#!/usr/bin/env bash
# demo-zu-live.sh — proof that a .zu file runs live (the thesis, closed).
#
# 1. compile examples-zu/counter.zu with `zuc build --target live`
#    → a wasm-free crate exposing `pub fn program()`
# 2. scaffold a tiny server that mounts `counter::program()` in the bridge
# 3. drive it through the REAL client stack (zumar-live.js) over a WS:
#    clicks, a server-side `every` tick, and reconnect-by-replay
#
# Everything lands in /tmp — nothing is committed except the .zu source and
# this script. Needs `zuc` on PATH (cargo install zumar-cli) and node.
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
bridge="$here"
sutegi="$(cd "$here/../.." && pwd)"
zumar="$(cd "$sutegi/../zumar" && pwd)"
work="/tmp/zu-live-demo"
port="${PORT:-8798}"

rm -rf "$work"; mkdir -p "$work"

echo "==> zuc build counter.zu --target live"
zuc build "$here/examples-zu/counter.zu" --target live --out "$work/counter" --zumar "$zumar"

echo "==> scaffolding the server (mounts counter::program(), no hand-written Rust)"
mkdir -p "$work/server/src"
cat > "$work/server/Cargo.toml" <<EOF
[package]
name = "zu-live-server"
version = "0.1.0"
edition = "2021"

[dependencies]
counter = { path = "../counter" }
sutegi-web = { path = "$sutegi/crates/sutegi-web", features = ["ws"] }
sutegi-orm = { path = "$sutegi/crates/sutegi-orm", features = ["sqlite"] }
sutegi-zumar = { path = "$bridge" }
EOF
cat > "$work/server/src/main.rs" <<'EOF'
use sutegi_web::{html, text, App};
use sutegi_zumar::{EventJournal, Live};

fn main() -> std::io::Result<()> {
    let db = sutegi_orm::db::Db::open("/tmp/zu-live-demo/live.db").expect("db");
    let journal = EventJournal::new(db).expect("journal");
    let www = concat!(env!("CARGO_MANIFEST_DIR"), "/../../../../zumar/www");
    App::new("zu-live")
        .get("/", "page", |_c| html(200, PAGE))
        .get("/api/hello", "server-side fetch target", |_c| {
            text(200, "aupa, fetched server-side")
        })
        // the program comes from counter.zu — NOT hand-written Rust
        .ws("/live", "live socket", Live::new(|_r| counter::program()).journal(journal).ws())
        .static_dir("/www", www)
        .serve()
}

const PAGE: &str = r#"<!doctype html><html><head><meta charset="utf-8">
<title>counter.zu live</title></head><body><div id="app"></div>
<script type="module">
import { mountLive } from "/www/zumar-live.js";
mountLive(`ws://${location.host}/live`, document.getElementById("app"));
</script></body></html>"#;
EOF
# resolve the www path (the server is one dir deeper than the bridge)
sed -i.bak "s#/../../../../zumar/www#$zumar/www#" "$work/server/src/main.rs" && rm -f "$work/server/src/main.rs.bak"

echo "==> cargo run (background)"
( cd "$work/server" && HOST=127.0.0.1 PORT="$port" cargo run -q ) &
srv=$!
trap 'kill $srv 2>/dev/null || true' EXIT

for i in $(seq 1 120); do
  curl -fsS -o /dev/null "http://127.0.0.1:$port/api/hello" 2>/dev/null && break
  sleep 1
done

echo "==> driving the .zu-authored live app through zumar-live.js"
ZUMAR="$zumar" PORT="$port" node "$here/zu-live-check.mjs"
