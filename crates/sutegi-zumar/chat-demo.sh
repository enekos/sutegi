#!/usr/bin/env bash
# chat-demo.sh — multi-client realtime, authored in zumar-lang (P4 payoff).
#
# 1. compile examples-zu/chat.zu with `zuc build --target live`
# 2. scaffold a server that mounts `chat::program()` in the bridge (the
#    default per-endpoint PubSub fans messages across connections)
# 3. connect TWO clients through the real zumar-live.js: one sends, the
#    OTHER receives — that is the whole point of pubsub subscriptions.
#
# Everything lands in /tmp. Needs `zuc` on PATH and node.
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
bridge="$here"
sutegi="$(cd "$here/../.." && pwd)"
zumar="$(cd "$sutegi/../zumar" && pwd)"
work="/tmp/chat-live-demo"
port="${PORT:-8799}"

rm -rf "$work"; mkdir -p "$work/server/src"

echo "==> zuc build chat.zu --target live"
zuc build "$here/examples-zu/chat.zu" --target live --out "$work/chat" --zumar "$zumar"

echo "==> scaffolding the chat server (mounts chat::program())"
cat > "$work/server/Cargo.toml" <<EOF
[package]
name = "chat-live-server"
version = "0.1.0"
edition = "2021"

[dependencies]
chat = { path = "../chat" }
sutegi-web = { path = "$sutegi/crates/sutegi-web", features = ["ws"] }
sutegi-zumar = { path = "$bridge" }
EOF
cat > "$work/server/src/main.rs" <<'EOF'
use sutegi_web::{html, App};
use sutegi_zumar::Live;

fn main() -> std::io::Result<()> {
    let www = "ZUMAR_WWW";
    App::new("chat-live")
        .get("/", "page", |_c| html(200, PAGE))
        // one endpoint, one default PubSub shared across its connections —
        // the program came from chat.zu, not hand-written Rust
        .ws("/live", "chat socket", Live::new(|_r| chat::program()).ws())
        .static_dir("/www", www)
        .serve()
}

const PAGE: &str = r#"<!doctype html><html><head><meta charset="utf-8">
<title>chat.zu</title></head><body><div id="app"></div>
<script type="module">
import { mountLive } from "/www/zumar-live.js";
mountLive(`ws://${location.host}/live`, document.getElementById("app"), { session: false });
</script></body></html>"#;
EOF
sed -i.bak "s#ZUMAR_WWW#$zumar/www#" "$work/server/src/main.rs" && rm -f "$work/server/src/main.rs.bak"

echo "==> cargo run (background)"
( cd "$work/server" && HOST=127.0.0.1 PORT="$port" cargo run -q ) &
srv=$!
trap 'kill $srv 2>/dev/null || true' EXIT
for i in $(seq 1 120); do
  curl -fsS -o /dev/null "http://127.0.0.1:$port/" 2>/dev/null && break
  sleep 1
done

echo "==> driving TWO clients through zumar-live.js"
ZUMAR="$zumar" PORT="$port" node "$here/chat-check.mjs"
