//! Single-node, two-person WebRTC signalling. Media travels browser-to-browser
//! or through a configured TURN service; Sutegi owns signalling and recordings.
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use sutegi::crypto::{constant_time_eq, hex, random_bytes};
use sutegi::prelude::*;
use sutegi::storage::{FsStorage, Storage};

struct Session {
    room: String,
    expires: Instant,
    connection: Option<Conn>,
    connected_before: bool,
}
#[derive(Default)]
struct Rooms {
    sessions: HashMap<String, Session>,
}
fn token(req: &Request) -> Option<&str> {
    req.header("cookie")?
        .split(';')
        .find_map(|v| v.trim().strip_prefix("call_session="))
}
fn random_id() -> Result<String, String> {
    random_bytes(24).map(|v| hex(&v))
}
fn send(conn: &Conn, message: Json) {
    if conn.try_send_text(&message.to_string()) == SendStatus::Full {
        conn.close(1013, "signalling overloaded");
    }
}
fn pair_generation(a: &Conn, b: &Conn) -> String {
    format!("{}:{}", a.id().min(b.id()), a.id().max(b.id()))
}
impl Rooms {
    fn session(&self, req: &Request) -> Option<&Session> {
        self.sessions
            .get(token(req)?)
            .filter(|s| s.expires > Instant::now())
    }
    fn peer(&self, room: &str, id: u64) -> Option<Conn> {
        self.sessions
            .values()
            .filter(|s| s.room == room && s.expires > Instant::now())
            .filter_map(|s| s.connection.as_ref())
            .find(|c| c.id() != id && !c.is_closed())
            .cloned()
    }
    fn open(&mut self, conn: &Conn, req: &Request) {
        let Some(session) = self.session(req) else {
            conn.close(1008, "session expired");
            return;
        };
        let room = session.room.clone();
        let session_token = token(req).unwrap().to_string();
        let occupants = self
            .sessions
            .iter()
            .filter(|(key, s)| **key != session_token && s.room == room)
            .filter(|(_, s)| s.connection.as_ref().is_some_and(|c| !c.is_closed()))
            .count();
        if occupants >= 2 {
            conn.close(1008, "room is full");
            return;
        }
        let session = self.sessions.get_mut(&session_token).unwrap();
        if let Some(old) = session.connection.replace(conn.clone()) {
            old.close(1000, "reconnected");
        }
        session.connected_before = true;
        if let Some(peer) = self.peer(&room, conn.id()) {
            let generation = pair_generation(conn, &peer);
            for (current, other) in [(conn, &peer), (&peer, conn)] {
                send(
                    current,
                    Json::obj(vec![
                        ("type", Json::str("ready")),
                        ("generation", Json::str(&generation)),
                        ("initiator", Json::Bool(current.id() < other.id())),
                    ]),
                );
            }
        } else {
            send(conn, Json::obj(vec![("type", Json::str("waiting"))]));
        }
    }
    fn message(&self, conn: &Conn, msg: Msg) {
        let Some(session) = self
            .sessions
            .values()
            .find(|s| s.connection.as_ref().is_some_and(|c| c.id() == conn.id()))
        else {
            return;
        };
        if session.expires <= Instant::now() {
            conn.close(1008, "session expired");
            return;
        }
        let Msg::Text(text) = msg else {
            conn.close(1003, "text signalling only");
            return;
        };
        let Ok(message) = Json::parse(&text) else {
            conn.close(1008, "invalid JSON");
            return;
        };
        if message.get("type").and_then(Json::as_str) != Some("signal") {
            conn.close(1008, "unknown event");
            return;
        }
        let Some(peer) = self.peer(&session.room, conn.id()) else {
            return;
        };
        if message.get("generation").and_then(Json::as_str)
            != Some(pair_generation(conn, &peer).as_str())
        {
            return;
        }
        let Some(payload) = message.get("payload").filter(|p| p.as_object().is_some()) else {
            conn.close(1008, "invalid signal");
            return;
        };
        // The server chooses the peer; clients cannot address another room or
        // forge identity. Old generations cannot renegotiate a replacement call.
        send(
            &peer,
            Json::obj(vec![
                ("type", Json::str("signal")),
                ("generation", Json::str(pair_generation(conn, &peer))),
                ("payload", payload.clone()),
            ]),
        );
    }
    fn close(&mut self, conn: &Conn) {
        let Some(session) = self
            .sessions
            .values_mut()
            .find(|s| s.connection.as_ref().is_some_and(|c| c.id() == conn.id()))
        else {
            return;
        };
        session.connection = None;
        let room = session.room.clone();
        if let Some(peer) = self.peer(&room, conn.id()) {
            send(&peer, Json::obj(vec![("type", Json::str("peer-left"))]));
        }
    }
}
fn same_origin(req: &Request) -> bool {
    let Some(origin) = req.header("origin") else {
        return false;
    };
    let Some((scheme, host)) = origin.split_once("://") else {
        return false;
    };
    matches!(scheme, "http" | "https") && req.header("host") == Some(host)
}
fn main() -> std::io::Result<()> {
    let password = std::env::var("CALL_TOKEN")
        .map_err(|_| std::io::Error::other("set CALL_TOKEN to a strong shared room access key"))?;
    if password.len() < 16 {
        return Err(std::io::Error::other(
            "CALL_TOKEN must be at least 16 characters",
        ));
    }
    let ice = Json::parse(&std::env::var("CALL_ICE_SERVERS").unwrap_or_else(|_| "[]".into()))
        .map_err(std::io::Error::other)?;
    if ice.as_array().is_none() {
        return Err(std::io::Error::other(
            "CALL_ICE_SERVERS must be a JSON array",
        ));
    }
    let rtc_config = Json::obj(vec![
        ("iceServers", ice),
        (
            "iceTransportPolicy",
            Json::str(if std::env::var("CALL_RELAY_ONLY").as_deref() == Ok("1") {
                "relay"
            } else {
                "all"
            }),
        ),
    ]);
    let secure_cookie = std::env::var("CALL_SECURE_COOKIE").as_deref() == Ok("1");
    let store = FsStorage::new(
        std::env::var("CALL_DATA").unwrap_or_else(|_| "data/call-recordings".into()),
    )
    .map_err(std::io::Error::other)?;
    let rooms = Arc::new(Mutex::new(Rooms::default()));
    let app = App::new("call")
        .max_body(4096)
        .drain_timeout(Duration::from_secs(5));
    let lifecycle = app.lifecycle();
    let sessions = rooms.clone();
    let ws_auth = rooms.clone();
    let ws_drain = rooms.clone();
    let ws_open = rooms.clone();
    let ws_msg = rooms.clone();
    let ws_close = rooms.clone();
    let upload_auth = rooms.clone();
    let upload_rooms = rooms.clone();
    let upload_store = store.clone();
    let playback = move |ctx: &Ctx| {
        let sessions = rooms.lock().unwrap();
        let Some(session) = sessions.session(ctx.req) else {
            return text(403, "sign in first");
        };
        let id = ctx.param("id").unwrap_or("");
        if id.len() != 48 || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
            return text(404, "not found");
        }
        let key = format!("{}/{id}.webm", session.room);
        drop(sessions);
        match store.open(&key) {
            Ok(Some(object)) => download(ctx.req, object.reader, &object.meta.content_type)
                .with_header("cache-control", "private, no-store"),
            Ok(None) => text(404, "not found"),
            Err(_) => text(500, "could not open recording"),
        }
    };
    let playback = Arc::new(playback);
    let head_playback = playback.clone();
    app.get("/", "Two-person call demo", |_| {
        html(200, include_str!("index.html"))
    })
    .get("/client.js", "WebRTC client", |_| {
        Response::new(200)
            .with_header("content-type", "text/javascript")
            .with_body(include_str!("client.js"))
    })
    .post(
        "/session",
        "Authenticate a one-hour room session",
        move |ctx| {
            if lifecycle.phase() != Phase::Running {
                return text(503, "server is draining");
            }
            if !same_origin(ctx.req) {
                return text(403, "same-origin request required");
            }
            let Ok(input) = json_body(ctx.req) else {
                return text(400, "JSON required");
            };
            let key = input.get("key").and_then(Json::as_str).unwrap_or("");
            if !constant_time_eq(key.as_bytes(), password.as_bytes()) {
                return text(403, "invalid access key");
            }
            let room = input.get("room").and_then(Json::as_str).unwrap_or("");
            if room.is_empty()
                || room.len() > 64
                || !room.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
            {
                return text(400, "room must be 1–64 letters, digits or hyphens");
            }
            let mut rooms = sessions.lock().unwrap();
            rooms.sessions.retain(|_, s| {
                if s.expires <= Instant::now() {
                    if let Some(c) = &s.connection {
                        c.close(1008, "session expired");
                    }
                    false
                } else {
                    true
                }
            });
            if rooms.sessions.len() >= 128 {
                return text(503, "session capacity reached");
            }
            let Ok(id) = random_id() else {
                return text(500, "could not create session");
            };
            // Re-authentication in this browser retires its previous socket.
            if let Some(old_token) = token(ctx.req) {
                if let Some(conn) = rooms
                    .sessions
                    .get(old_token)
                    .and_then(|s| s.connection.clone())
                {
                    rooms.close(&conn);
                    conn.close(1000, "session replaced");
                }
                rooms.sessions.remove(old_token);
            }
            rooms.sessions.insert(
                id.clone(),
                Session {
                    room: room.into(),
                    expires: Instant::now() + Duration::from_secs(3600),
                    connection: None,
                    connected_before: false,
                },
            );
            json(200, &rtc_config)
                .with_header("cache-control", "no-store")
                .with_header(
                    "set-cookie",
                    &format!(
                        "call_session={id}; HttpOnly; SameSite=Strict; Path=/; Max-Age=3600{}",
                        if secure_cookie { "; Secure" } else { "" }
                    ),
                )
        },
    )
    .ws_config(WsConfig {
        shards: 2,
        max_frame: 64 * 1024,
        max_message: 64 * 1024,
        max_connections: 128,
        max_buffered: 256 * 1024,
        ..WsConfig::default()
    })
    .ws(
        "/signal",
        "Authenticated SDP/ICE relay; server selects room peer",
        Ws::new()
            .authorize(move |req| {
                same_origin(req) && ws_auth.lock().unwrap().session(req).is_some()
            })
            .admit_during_drain(move |req| {
                ws_drain
                    .lock()
                    .unwrap()
                    .session(req)
                    .is_some_and(|s| s.connected_before)
            })
            .on_open(move |c, r| ws_open.lock().unwrap().open(c, r))
            .on_message(move |c, m| ws_msg.lock().unwrap().message(c, m))
            .on_close(move |c, _| ws_close.lock().unwrap().close(c))
            .dispatch(DispatchConfig {
                workers: 2,
                max_connections: 128,
                ..DispatchConfig::default()
            })?,
    )
    .upload(
        Method::Post,
        "/recordings",
        "Upload this room's WebM recording",
        UploadLimits {
            max_bytes: 64 * 1024 * 1024,
            ..UploadLimits::default()
        },
        move |req| {
            if !same_origin(req) || upload_auth.lock().unwrap().session(req).is_none() {
                return Some(text(403, "sign in first"));
            }
            if req.content_type().unwrap_or("").split(';').next() != Some("video/webm") {
                return Some(text(415, "WebM required"));
            }
            None
        },
        move |ctx, body| {
            let sessions = upload_rooms.lock().unwrap();
            let Some(session) = sessions.session(ctx.req) else {
                return text(403, "session expired");
            };
            let room = session.room.clone();
            drop(sessions);
            let Ok(id) = random_id() else {
                return text(500, "could not name recording");
            };
            let length = body.remaining() as u64;
            if length == 0 {
                return text(400, "empty recording");
            }
            match upload_store.put_reader(&format!("{room}/{id}.webm"), body, length, "video/webm")
            {
                Ok(()) => json(
                    201,
                    &Json::obj(vec![("url", Json::str(format!("/recordings/{id}")))]),
                ),
                Err(_) => text(500, "recording was not saved"),
            }
        },
    )
    .get(
        "/recordings/:id",
        "Play a recording authorized for this room",
        move |ctx| playback(ctx),
    )
    .route(
        Method::Head,
        "/recordings/:id",
        "Recording metadata",
        move |ctx| head_playback(ctx),
    )
    .serve()
}
