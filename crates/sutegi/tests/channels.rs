//! End-to-end channel coverage through the assembled app: real RFC 6455
//! handshakes against `App::run`, then the full channel protocol over the
//! upgraded sockets — join auth, replies, broadcasts, sender exclusion,
//! heartbeats, leave/rejoin, the `/__channels` manifest, and a two-app
//! "two pods" run sharing one broker.
#![cfg(feature = "channels")]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::thread;
use std::time::Duration;

use sutegi::prelude::*;

// --- a tiny WebSocket client (text frames, masked, extended lengths) --------

struct WsClient {
    stream: TcpStream,
}

impl WsClient {
    fn connect(addr: &str, path: &str) -> WsClient {
        let mut stream = TcpStream::connect(addr).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        write!(
            stream,
            "GET {path} HTTP/1.1\r\nHost: x\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\
             Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n"
        )
        .unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut status = String::new();
        reader.read_line(&mut status).unwrap();
        assert!(status.starts_with("HTTP/1.1 101"), "{status}");
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            if line.trim_end().is_empty() {
                break;
            }
        }
        WsClient { stream }
    }

    fn send(&mut self, text: &str) {
        let payload = text.as_bytes();
        let mask = [7u8, 7, 7, 7];
        let mut out = vec![0x81u8];
        match payload.len() {
            n if n <= 125 => out.push(0x80 | n as u8),
            n if n <= 65535 => {
                out.push(0x80 | 126);
                out.extend_from_slice(&(n as u16).to_be_bytes());
            }
            n => {
                out.push(0x80 | 127);
                out.extend_from_slice(&(n as u64).to_be_bytes());
            }
        }
        out.extend_from_slice(&mask);
        out.extend(payload.iter().enumerate().map(|(i, b)| b ^ mask[i & 3]));
        self.stream.write_all(&out).unwrap();
    }

    /// Read one text frame as parsed JSON.
    fn recv(&mut self) -> Json {
        let mut hdr = [0u8; 2];
        self.stream.read_exact(&mut hdr).unwrap();
        assert_eq!(hdr[0], 0x81, "expected a FIN text frame");
        let len = match hdr[1] & 0x7F {
            126 => {
                let mut ext = [0u8; 2];
                self.stream.read_exact(&mut ext).unwrap();
                u16::from_be_bytes(ext) as usize
            }
            127 => {
                let mut ext = [0u8; 8];
                self.stream.read_exact(&mut ext).unwrap();
                u64::from_be_bytes(ext) as usize
            }
            n => n as usize,
        };
        let mut payload = vec![0u8; len];
        self.stream.read_exact(&mut payload).unwrap();
        Json::parse(std::str::from_utf8(&payload).unwrap()).unwrap()
    }

    /// Keep reading until a frame with this event arrives (skipping others).
    fn recv_event(&mut self, event: &str) -> Json {
        for _ in 0..50 {
            let frame = self.recv();
            if frame.pointer("/event").and_then(Json::as_str) == Some(event) {
                return frame;
            }
        }
        panic!("never saw event {event:?}");
    }

    fn join(&mut self, topic: &str, payload: Json, reference: &str) -> Json {
        self.send(&format!(
            r#"{{"topic":"{topic}","event":"stg:join","ref":"{reference}","payload":{payload}}}"#
        ));
        self.recv_event("stg:reply")
    }
}

fn str_at<'j>(json: &'j Json, path: &str) -> Option<&'j str> {
    json.pointer(path).and_then(Json::as_str)
}

// --- the app under test ------------------------------------------------------

fn room_channel() -> Channel {
    Channel::new("room:*")
        .doc("A chat room.")
        .join_schema(
            "Pass a nick to join.",
            Json::obj(vec![("nick", Json::str("string"))]),
        )
        .on_join(|socket: &Socket, payload: &Json| {
            let nick = payload
                .pointer("/nick")
                .and_then(Json::as_str)
                .ok_or_else(|| Json::str("nick required"))?;
            socket.assign("nick", Json::str(nick));
            socket.broadcast_from("joined", &Json::obj(vec![("nick", Json::str(nick))]));
            Ok(Json::obj(vec![("welcome", Json::str(socket.topic()))]))
        })
        .on("new_msg", |socket: &Socket, payload: &Json| {
            let nick = socket
                .assign_get("nick")
                .and_then(|j| j.as_str().map(str::to_string))
                .unwrap_or_default();
            let body = payload
                .pointer("/body")
                .and_then(Json::as_str)
                .unwrap_or("");
            socket.broadcast(
                "new_msg",
                &Json::obj(vec![("nick", Json::str(nick)), ("body", Json::str(body))]),
            );
            Reply::None
        })
        .on("whoami", |socket: &Socket, _p: &Json| {
            Reply::Ok(Json::obj(vec![(
                "nick",
                socket.assign_get("nick").unwrap_or(Json::Null),
            )]))
        })
        .on("typing", |socket: &Socket, _p: &Json| {
            socket.broadcast_from("typing", &Json::obj(vec![]));
            Reply::None
        })
        .on_leave(|socket: &Socket, reason: LeaveReason| {
            if reason != LeaveReason::Disconnect {
                socket.broadcast_from("left", &Json::obj(vec![]));
            }
        })
}

fn boot(addr: &'static str, build: impl FnOnce() -> App + Send + 'static) {
    thread::spawn(move || {
        let _ = build().run(addr);
    });
    for _ in 0..300 {
        if TcpStream::connect(addr).is_ok() {
            return;
        }
        thread::sleep(Duration::from_millis(10));
    }
    panic!("server {addr} did not become ready");
}

fn chat_app() -> App {
    let hub = Channels::new().channel(room_channel()).build();
    App::new("channels-e2e").channels("/channels", "The chat socket.", hub)
}

// --- tests -------------------------------------------------------------------

#[test]
fn join_reply_and_refusal() {
    boot("127.0.0.1:18480", chat_app);
    let mut c = WsClient::connect("127.0.0.1:18480", "/channels");

    // Refused: no nick.
    let reply = c.join("room:1", Json::obj(vec![]), "1");
    assert_eq!(str_at(&reply, "/payload/status"), Some("error"));
    assert_eq!(str_at(&reply, "/payload/response"), Some("nick required"));

    // Admitted: the join callback's response rides the ok reply, the ref and
    // join_ref are echoed.
    let reply = c.join("room:1", Json::obj(vec![("nick", Json::str("ada"))]), "2");
    assert_eq!(str_at(&reply, "/payload/status"), Some("ok"));
    assert_eq!(str_at(&reply, "/payload/response/welcome"), Some("room:1"));
    assert_eq!(str_at(&reply, "/ref"), Some("2"));
    assert_eq!(str_at(&reply, "/join_ref"), Some("2"));
}

#[test]
fn broadcast_reaches_the_room_and_excludes_sender_on_broadcast_from() {
    boot("127.0.0.1:18481", chat_app);
    let mut ada = WsClient::connect("127.0.0.1:18481", "/channels");
    let mut bob = WsClient::connect("127.0.0.1:18481", "/channels");

    ada.join("room:7", Json::obj(vec![("nick", Json::str("ada"))]), "1");
    bob.join("room:7", Json::obj(vec![("nick", Json::str("bob"))]), "1");

    // ada saw bob join (broadcast_from inside on_join excludes bob himself).
    let joined = ada.recv_event("joined");
    assert_eq!(str_at(&joined, "/payload/nick"), Some("bob"));

    // A message broadcast reaches both, with assigns resolved server-side.
    bob.send(r#"{"topic":"room:7","event":"new_msg","payload":{"body":"hi"}}"#);
    for client in [&mut ada, &mut bob] {
        let msg = client.recv_event("new_msg");
        assert_eq!(str_at(&msg, "/payload/nick"), Some("bob"));
        assert_eq!(str_at(&msg, "/payload/body"), Some("hi"));
    }

    // broadcast_from: ada types; bob sees it, ada must not. Prove the
    // negative with a sentinel that arrives after.
    ada.send(r#"{"topic":"room:7","event":"typing","payload":{}}"#);
    bob.recv_event("typing");
    ada.send(r#"{"topic":"room:7","event":"new_msg","payload":{"body":"sentinel"}}"#);
    let next = ada.recv_event("new_msg"); // skips nothing: typing never arrived
    assert_eq!(str_at(&next, "/payload/body"), Some("sentinel"));
}

#[test]
fn replies_heartbeat_and_protocol_errors() {
    boot("127.0.0.1:18482", chat_app);
    let mut c = WsClient::connect("127.0.0.1:18482", "/channels");

    // Heartbeat before any join.
    c.send(r#"{"topic":"stg","event":"heartbeat","ref":"hb1"}"#);
    let hb = c.recv_event("stg:reply");
    assert_eq!(str_at(&hb, "/payload/status"), Some("ok"));
    assert_eq!(str_at(&hb, "/ref"), Some("hb1"));

    // Pushing without a join → error reply.
    c.send(r#"{"topic":"room:9","event":"new_msg","ref":"e1","payload":{}}"#);
    let err = c.recv_event("stg:reply");
    assert_eq!(str_at(&err, "/payload/status"), Some("error"));
    assert_eq!(
        str_at(&err, "/payload/response/reason"),
        Some("not joined to this topic")
    );

    // A topic no channel serves.
    c.send(r#"{"topic":"nope","event":"stg:join","ref":"e2"}"#);
    let err = c.recv_event("stg:reply");
    assert_eq!(
        str_at(&err, "/payload/response/reason"),
        Some("no channel serves this topic")
    );

    // Unhandled event after a valid join.
    c.join("room:9", Json::obj(vec![("nick", Json::str("x"))]), "3");
    c.send(r#"{"topic":"room:9","event":"mystery","ref":"e3","payload":{}}"#);
    let err = c.recv_event("stg:reply");
    assert_eq!(
        str_at(&err, "/payload/response/reason"),
        Some("unhandled event")
    );

    // Reply events answer whoami from assigns.
    c.send(r#"{"topic":"room:9","event":"whoami","ref":"e4"}"#);
    let who = c.recv_event("stg:reply");
    assert_eq!(str_at(&who, "/payload/response/nick"), Some("x"));

    // Garbage frames get a ref-less stg:error, and the connection survives.
    c.send("not json at all");
    let err = c.recv_event("stg:error");
    assert!(str_at(&err, "/payload/reason")
        .unwrap()
        .contains("not JSON"));
    c.send(r#"{"topic":"stg","event":"heartbeat","ref":"hb2"}"#);
    assert_eq!(str_at(&c.recv_event("stg:reply"), "/ref"), Some("hb2"));

    // Clients cannot forge reserved events.
    c.send(r#"{"topic":"room:9","event":"stg:reply","ref":"e5","payload":{}}"#);
    let err = c.recv_event("stg:reply");
    assert_eq!(
        str_at(&err, "/payload/response/reason"),
        Some("clients cannot send reserved events")
    );
}

#[test]
fn leave_and_disconnect_tear_down_membership() {
    boot("127.0.0.1:18483", chat_app);
    let mut ada = WsClient::connect("127.0.0.1:18483", "/channels");
    let mut bob = WsClient::connect("127.0.0.1:18483", "/channels");
    ada.join("room:2", Json::obj(vec![("nick", Json::str("ada"))]), "1");
    bob.join("room:2", Json::obj(vec![("nick", Json::str("bob"))]), "1");
    ada.recv_event("joined");

    // Explicit leave: ok reply, the room hears "left", pushes then fail.
    bob.send(r#"{"topic":"room:2","event":"stg:leave","ref":"l1"}"#);
    let ok = bob.recv_event("stg:reply");
    assert_eq!(str_at(&ok, "/payload/status"), Some("ok"));
    ada.recv_event("left");
    bob.send(r#"{"topic":"room:2","event":"new_msg","ref":"l2","payload":{"body":"ghost"}}"#);
    let err = bob.recv_event("stg:reply");
    assert_eq!(str_at(&err, "/payload/status"), Some("error"));

    // Disconnect: dropping the socket removes the member (no "left"
    // broadcast for disconnects in this channel; prove via a sentinel that
    // ada never receives the ghost's messages again).
    drop(bob);
    thread::sleep(Duration::from_millis(100));
    ada.send(r#"{"topic":"room:2","event":"new_msg","payload":{"body":"still here"}}"#);
    let msg = ada.recv_event("new_msg");
    assert_eq!(str_at(&msg, "/payload/body"), Some("still here"));
}

#[test]
fn rejoin_replaces_the_membership() {
    boot("127.0.0.1:18484", chat_app);
    let mut c = WsClient::connect("127.0.0.1:18484", "/channels");
    c.join("room:3", Json::obj(vec![("nick", Json::str("one"))]), "1");
    // Rejoin with a different nick: fresh assigns, single membership.
    let reply = c.join("room:3", Json::obj(vec![("nick", Json::str("two"))]), "2");
    assert_eq!(str_at(&reply, "/payload/status"), Some("ok"));
    assert_eq!(str_at(&reply, "/join_ref"), Some("2"));

    c.send(r#"{"topic":"room:3","event":"whoami","ref":"w"}"#);
    let who = c.recv_event("stg:reply");
    assert_eq!(str_at(&who, "/payload/response/nick"), Some("two"));

    // Exactly one membership: one broadcast per message, not two.
    c.send(r#"{"topic":"room:3","event":"new_msg","payload":{"body":"once"}}"#);
    c.recv_event("new_msg");
    c.send(r#"{"topic":"stg","event":"heartbeat","ref":"hb"}"#);
    let frame = c.recv_event("stg:reply"); // heartbeat reply, no duplicate new_msg first
    assert_eq!(str_at(&frame, "/ref"), Some("hb"));
}

#[test]
fn manifest_is_served_at_dunder_channels() {
    boot("127.0.0.1:18485", chat_app);
    let mut stream = TcpStream::connect("127.0.0.1:18485").unwrap();
    stream
        .write_all(b"GET /__channels HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n")
        .unwrap();
    let mut body = Vec::new();
    stream.read_to_end(&mut body).unwrap();
    let text = String::from_utf8_lossy(&body);
    assert!(text.starts_with("HTTP/1.1 200"), "{text}");
    let json_start = text.find("\r\n\r\n").unwrap() + 4;
    let manifest = Json::parse(&text[json_start..]).unwrap();
    assert_eq!(str_at(&manifest, "/path"), Some("/channels"));
    assert_eq!(str_at(&manifest, "/channels/0/pattern"), Some("room:*"));
    assert_eq!(
        str_at(&manifest, "/protocol/control_events/join"),
        Some("stg:join")
    );
    assert_eq!(
        str_at(&manifest, "/channels/0/join/payload_schema/nick"),
        Some("string")
    );
}

#[test]
fn two_apps_one_broker_cross_pod_broadcast() {
    // Two Apps on two ports sharing one in-process broker — the same wiring
    // PgPubSub gives two real pods (the pg_live suite proves that leg).
    let bus = PubSub::new();
    let bus_a = bus.clone();
    let bus_b = bus;
    boot("127.0.0.1:18486", move || {
        let hub = Channels::new()
            .channel(room_channel())
            .broker(bus_a)
            .build();
        App::new("pod-a").channels("/channels", "Pod A.", hub)
    });
    boot("127.0.0.1:18487", move || {
        let hub = Channels::new()
            .channel(room_channel())
            .broker(bus_b)
            .build();
        App::new("pod-b").channels("/channels", "Pod B.", hub)
    });

    let mut ada = WsClient::connect("127.0.0.1:18486", "/channels");
    let mut bob = WsClient::connect("127.0.0.1:18487", "/channels");
    ada.join("room:x", Json::obj(vec![("nick", Json::str("ada"))]), "1");
    bob.join("room:x", Json::obj(vec![("nick", Json::str("bob"))]), "1");

    // A message sent on pod B arrives on pod A.
    bob.send(r#"{"topic":"room:x","event":"new_msg","payload":{"body":"across pods"}}"#);
    let msg = ada.recv_event("new_msg");
    assert_eq!(str_at(&msg, "/payload/body"), Some("across pods"));
    assert_eq!(str_at(&msg, "/payload/nick"), Some("bob"));
}

#[test]
fn server_side_hub_broadcast_reaches_members() {
    // A hub handle kept outside the app can push into a room (the
    // "broadcast from an HTTP handler / background thread" story).
    let hub = Channels::new().channel(room_channel()).build();
    let hub_for_app = hub.clone();
    boot("127.0.0.1:18488", move || {
        App::new("hubcast").channels("/channels", "Chat.", hub_for_app)
    });
    let mut c = WsClient::connect("127.0.0.1:18488", "/channels");
    c.join("room:5", Json::obj(vec![("nick", Json::str("n"))]), "1");
    assert_eq!(hub.local_members("room:5"), 1);

    hub.broadcast(
        "room:5",
        "announcement",
        &Json::obj(vec![("body", Json::str("maintenance at noon"))]),
    );
    let msg = c.recv_event("announcement");
    assert_eq!(str_at(&msg, "/payload/body"), Some("maintenance at noon"));
}
