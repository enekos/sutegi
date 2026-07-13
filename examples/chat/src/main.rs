//! The canonical channels demo — Phoenix's chat, on sutegi.
//!
//! ```sh
//! cargo run -p chat-example                       # single pod, in-process broker
//! DATABASE_URL=postgres://… cargo run -p chat-example -- 127.0.0.1:8080
//! DATABASE_URL=postgres://… cargo run -p chat-example -- 127.0.0.1:8081
//! # two pods: open a room on each port — messages and the user list cross pods
//! ```
//!
//! One channel (`room:*`) with join auth, message broadcast, and presence;
//! the browser page uses the bundled dependency-free JS client. Agents can
//! discover everything at `GET /__channels` and join over a raw WebSocket.

use sutegi::channels::JS_CLIENT;
use sutegi::prelude::*;
use sutegi::pubsub::{sutegi_pg, PgPubSub};

fn room() -> Channel {
    Channel::new("room:*")
        .doc(
            "A chat room. Anyone with a nick may join; messages fan out to the room, \
              presence tracks who is online.",
        )
        .join_schema(
            "A display name, 1–24 chars.",
            schema::object(vec![("nick", schema::string("Display name"))], &["nick"]),
        )
        .on_join(|socket: &Socket, payload: &Json| {
            let nick = payload
                .pointer("/nick")
                .and_then(Json::as_str)
                .map(str::trim)
                .filter(|n| !n.is_empty() && n.len() <= 24)
                .ok_or_else(|| Json::str("nick required (1-24 chars)"))?
                .to_string();
            socket.assign("nick", Json::str(nick.clone()));
            let meta = Json::obj(vec![("nick", Json::str(nick.clone()))]);
            Presence::track(socket, &nick, meta);
            Ok(Json::Null)
        })
        .event_schema(
            "new_msg",
            "Say something to the room.",
            schema::object(
                vec![("body", schema::string("The message text"))],
                &["body"],
            ),
        )
        .on("new_msg", |socket: &Socket, payload: &Json| {
            let Some(body) = payload.pointer("/body").and_then(Json::as_str) else {
                return Reply::Err(Json::str("body required"));
            };
            let nick = socket
                .assign_get("nick")
                .and_then(|n| n.as_str().map(str::to_string))
                .unwrap_or_else(|| "anon".into());
            socket.broadcast(
                "new_msg",
                &Json::obj(vec![("nick", Json::str(nick)), ("body", Json::str(body))]),
            );
            Reply::None
        })
        .emits(
            "new_msg",
            "A message said by anyone in the room.",
            schema::object(
                vec![
                    ("nick", schema::string("Who said it")),
                    ("body", schema::string("What they said")),
                ],
                &[],
            ),
        )
        .emits(
            "presence_state",
            "Pushed to you after joining: the full {key: {metas}} online list.",
            Json::Null,
        )
        .emits(
            "presence_diff",
            "Pushed on every join/leave: {joins: {…}, leaves: {…}}.",
            Json::Null,
        )
}

fn main() -> std::io::Result<()> {
    let addr = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "127.0.0.1:8080".into());

    // Cross-pod when a database is configured, in-process otherwise — the
    // channel code is identical either way; only the broker changes.
    let mut channels = Channels::new().channel(room());
    match std::env::var("DATABASE_URL") {
        Ok(url) => {
            let cfg = PgPubSub::connect(
                &sutegi_pg::Config::from_url(&url).expect("DATABASE_URL must parse"),
            )
            .expect("connect PgPubSub");
            println!("chat: cross-pod fan-out via PostgreSQL LISTEN/NOTIFY");
            channels = channels.broker(cfg);
        }
        Err(_) => println!("chat: single pod (set DATABASE_URL for cross-pod fan-out)"),
    }
    let hub = channels.build();

    App::new("chat")
        .get("/", "The chat page.", |_c| html(200, PAGE))
        .get("/channels.js", "The bundled channels client.", |_c| {
            Response::new(200)
                .with_header("content-type", "application/javascript; charset=utf-8")
                .with_body(JS_CLIENT.as_bytes())
        })
        .channels(
            "/channels",
            "The chat socket: join room:<name> with a nick, then new_msg away.",
            hub,
        )
        .run(&addr)
}

const PAGE: &str = r#"<!doctype html>
<html><head><meta charset="utf-8"><title>sutegi channels chat</title>
<style>
  body { font: 15px/1.4 ui-monospace, monospace; background: #16130f; color: #e8ddcf;
         max-width: 720px; margin: 2rem auto; padding: 0 1rem; }
  h1 { color: #e2725b; font-size: 1.2rem; }
  #main { display: flex; gap: .8rem; }
  #log { border: 1px solid #3a332a; padding: .6rem; height: 20rem; overflow-y: auto; flex: 1; }
  #who { border: 1px solid #3a332a; padding: .6rem; height: 20rem; overflow-y: auto; width: 10rem; }
  #who h2 { font-size: .9rem; color: #8a7f6e; margin: 0 0 .4rem; }
  #log .sys { color: #8a7f6e; }
  #log .nick { color: #e2725b; }
  form { display: flex; gap: .5rem; margin-top: .6rem; }
  input { flex: 1; background: #211c15; color: inherit; border: 1px solid #3a332a; padding: .4rem; }
  button { background: #e2725b; color: #16130f; border: 0; padding: .4rem .9rem; cursor: pointer; }
</style></head><body>
<h1>channels chat</h1>
<div id="main">
  <div id="log"></div>
  <div id="who"><h2>online</h2><div id="list"></div></div>
</div>
<form id="f"><input id="m" autocomplete="off" placeholder="say something"><button>send</button></form>
<script src="/channels.js"></script>
<script>
  const nick = (prompt("nick?") || "anon").slice(0, 24);
  const roomName = "room:" + (location.hash.slice(1) || "lobby");
  const log = (html, cls) => {
    const d = document.createElement("div");
    if (cls) d.className = cls;
    d.innerHTML = html;
    document.getElementById("log").append(d);
    d.scrollIntoView();
  };
  const esc = (s) => s.replace(/[&<>"]/g, c => ({"&":"&amp;","<":"&lt;",">":"&gt;",'"':"&quot;"}[c]));

  const presence = {};
  const renderWho = () => {
    document.getElementById("list").innerHTML =
      Object.keys(presence).sort().map(esc).join("<br>");
  };

  const socket = new SutegiSocket("/channels");
  socket.connect();
  const room = socket.channel(roomName, {nick});
  room.on("new_msg", p => log(`<span class="nick">${esc(p.nick)}:</span> ${esc(p.body)}`));
  room.on("presence_state", p => {
    for (const k of Object.keys(presence)) delete presence[k];
    Object.assign(presence, p);
    renderWho();
  });
  room.on("presence_diff", p => {
    Object.assign(presence, p.joins);
    for (const k in p.leaves) delete presence[k];
    renderWho();
    for (const k in p.joins) log(`* ${esc(k)} joined`, "sys");
    for (const k in p.leaves) log(`* ${esc(k)} left`, "sys");
  });
  room.join()
    .receive("ok", () => log(`* joined ${esc(roomName)} as ${esc(nick)}`, "sys"))
    .receive("error", (e) => log(`* refused: ${esc(JSON.stringify(e))}`, "sys"));

  document.getElementById("f").onsubmit = e => {
    e.preventDefault();
    const m = document.getElementById("m");
    if (m.value) room.push("new_msg", {body: m.value});
    m.value = "";
  };
</script></body></html>"#;
