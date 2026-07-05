//! A chat room — the canonical WebSocket demo.
//!
//! ```sh
//! cargo run -p ws-chat-example
//! # open http://127.0.0.1:8080 in two tabs
//! ```
//!
//! One `App::ws` endpoint, a roster keyed by connection id, broadcast via a
//! single shared encoded frame. The page is a dependency-free inline client.

use std::collections::HashMap;
use std::sync::Mutex;

use sutegi::prelude::*;
use sutegi::ws::broadcast;

/// name + handle per live connection. `Conn` is cheap to clone and safe to
/// use from any thread, so a plain map is the whole "presence" story here.
static ROSTER: Mutex<Option<HashMap<u64, (String, Conn)>>> = Mutex::new(None);

/// Fan a line out to the whole room. `broadcast` encodes once and takes each
/// shard's lock once — the right primitive even when the room is large.
fn say(text: &str) {
    let frame = text_frame(text);
    if let Some(roster) = ROSTER.lock().unwrap().as_ref() {
        broadcast(roster.values().map(|(_, conn)| conn), &frame);
    }
}

fn main() -> std::io::Result<()> {
    *ROSTER.lock().unwrap() = Some(HashMap::new());

    App::new("ws-chat")
        .get("/", "The chat page.", |_c| html(200, PAGE))
        .ws(
            "/ws",
            "Chat socket. Pass ?name=<nick>; every text message is broadcast to the room.",
            // Public, cookieless demo, so no origin gate. An app that
            // authenticates the socket by cookie MUST add one or it's open to
            // cross-site hijacking: `.check_origin(["https://app.example.com"])`
            // (or `.authorize(|req| ...)` for a token check) — both refuse
            // before the 101.
            Ws::new()
                .on_open(|conn: &Conn, req: &Request| {
                    let name = req
                        .query
                        .split('&')
                        .find_map(|kv| kv.strip_prefix("name="))
                        .unwrap_or("anon")
                        .to_string();
                    if let Some(roster) = ROSTER.lock().unwrap().as_mut() {
                        roster.insert(conn.id(), (name.clone(), conn.clone()));
                    }
                    say(&format!("* {name} joined"));
                })
                .on_message(|conn: &Conn, msg: Msg| {
                    if let Msg::Text(text) = msg {
                        let name = ROSTER
                            .lock()
                            .unwrap()
                            .as_ref()
                            .and_then(|r| r.get(&conn.id()).map(|(n, _)| n.clone()))
                            .unwrap_or_else(|| "anon".into());
                        say(&format!("{name}: {text}"));
                    }
                })
                .on_close(|conn: &Conn, _code| {
                    let name = ROSTER
                        .lock()
                        .unwrap()
                        .as_mut()
                        .and_then(|r| r.remove(&conn.id()))
                        .map(|(n, _)| n);
                    if let Some(name) = name {
                        say(&format!("* {name} left"));
                    }
                }),
        )
        .serve()
}

const PAGE: &str = r#"<!doctype html>
<html><head><meta charset="utf-8"><title>sutegi chat</title>
<style>
  body { font: 15px/1.4 ui-monospace, monospace; background: #16130f; color: #e8ddcf;
         max-width: 640px; margin: 2rem auto; padding: 0 1rem; }
  h1 { color: #e2725b; font-size: 1.2rem; }
  #log { border: 1px solid #3a332a; padding: .6rem; height: 20rem; overflow-y: auto; }
  #log .sys { color: #8a7f6e; }
  form { display: flex; gap: .5rem; margin-top: .6rem; }
  input { flex: 1; background: #211c15; color: inherit; border: 1px solid #3a332a; padding: .4rem; }
  button { background: #e2725b; color: #16130f; border: 0; padding: .4rem .9rem; cursor: pointer; }
</style></head><body>
<h1>ws-chat</h1>
<div id="log"></div>
<form id="f"><input id="m" autocomplete="off" placeholder="say something"><button>send</button></form>
<script>
  const name = prompt("name?") || "anon";
  const ws = new WebSocket(`ws://${location.host}/ws?name=${encodeURIComponent(name)}`);
  const log = (t, cls) => {
    const d = document.createElement("div");
    if (cls) d.className = cls;
    d.textContent = t;
    document.getElementById("log").append(d);
    d.scrollIntoView();
  };
  ws.onmessage = e => log(e.data, e.data.startsWith("*") ? "sys" : "");
  ws.onclose = () => log("* disconnected", "sys");
  document.getElementById("f").onsubmit = e => {
    e.preventDefault();
    const m = document.getElementById("m");
    if (m.value) ws.send(m.value);
    m.value = "";
  };
</script></body></html>"#;
