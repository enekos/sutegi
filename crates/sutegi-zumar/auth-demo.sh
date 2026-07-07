#!/usr/bin/env bash
# auth-demo.sh — auth/session/forms, authored in zumar-lang (P5 payoff).
#
# 1. compile examples-zu/account.zu with `zuc build --target live`
# 2. scaffold a server that wires sutegi-auth (Users + signed-cookie
#    Sessions) and mounts account::program_with(seeded) behind Live::guard —
#    the live socket is private, and the factory reads the logged-in user
#    from the mount request's cookie and seeds their name into the model.
# 3. drive it over the real wire (auth-check.mjs, node): an anonymous socket
#    is refused by the guard; after logging in, the cookie rides the upgrade,
#    the mount is greeted by name, and a form save round-trips over the
#    authenticated socket with no CSRF token.
#
# Everything lands in /tmp. Needs `zuc` on PATH and node (>=22 for WebSocket).
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
bridge="$here"
sutegi="$(cd "$here/../.." && pwd)"
zumar="$(cd "$sutegi/../zumar" && pwd)"
work="/tmp/auth-live-demo"
port="${PORT:-8795}"
db="/tmp/auth-live-demo-${$}.db"

rm -rf "$work"; mkdir -p "$work/server/src"
rm -f "$db" "$db-shm" "$db-wal"

echo "==> zuc build account.zu --target live"
zuc build "$here/examples-zu/account.zu" --target live --out "$work/account" --zumar "$zumar"

echo "==> scaffolding the auth server (sutegi-auth + guarded live mount)"
cat > "$work/server/Cargo.toml" <<EOF
[package]
name = "auth-live-server"
version = "0.1.0"
edition = "2021"

[dependencies]
account = { path = "../account" }
sutegi = { path = "$sutegi/crates/sutegi", features = ["sqlite", "auth", "ws"] }
sutegi-zumar = { path = "$bridge" }
EOF
cat > "$work/server/src/main.rs" <<'EOF'
use std::sync::Arc;
use sutegi::prelude::*;
use sutegi_zumar::Live;

fn main() -> std::io::Result<()> {
    let db = Db::open(&std::env::var("AUTH_DB").expect("AUTH_DB")).expect("open db");

    // The user system. Demo work factor (10k) so debug builds stay snappy.
    let users = Users::new(db.clone()).iterations(10_000);
    users.migrate().expect("migrate users");
    // A known user so the browser test can log in.
    if users.count().unwrap_or(0) == 0 {
        users
            .register_with("eneko@join.com", "password1", "Eneko", "user")
            .expect("seed user");
    }
    let secret = "dev-only-secret-set-SESSION_SECRET";
    let auth = Arc::new(Auth::new(
        users,
        Sessions::new(secret.as_bytes()).insecure(), // local http://
    ));

    let a_login = auth.clone();
    let a_factory = auth.clone(); // seeds the mounted model from the cookie
    let a_guard = auth.clone(); // gates the socket

    App::new("auth-live")
        .state(auth.clone())
        .get("/", "login page", |_c| html(200, LOGIN))
        .get("/account", "the private live page", |_c| html(200, ACCOUNT))
        .post("/login", "log in (sets the signed session cookie)", move |c| {
            let body = c.json()?;
            let email = body.get("email").and_then(Json::as_str).unwrap_or("");
            let password = body.get("password").and_then(Json::as_str).unwrap_or("");
            match a_login.users.authenticate(email, password)? {
                Some(user) => {
                    Ok::<_, Error>(a_login.login(c.req, &user, json(200, &user.to_json())))
                }
                None => Err(Error::unauthorized("bad credentials")),
            }
        })
        // The form target. Called server-side by the bridge (next to the
        // data), so it takes the note as its raw body and echoes it back.
        .post("/api/note", "save a note", |c| {
            let note = String::from_utf8_lossy(&c.req.body).to_string();
            text(200, &note)
        })
        // The private live socket. `guard` refuses the upgrade unless the
        // browser's cookie names a valid session; the factory then reads that
        // same request and seeds the user's name into the mounted program.
        .ws(
            "/account/live",
            "guarded zumar live socket",
            Live::new(move |req| {
                let name = a_factory
                    .current(req)
                    .ok()
                    .flatten()
                    .map(|u| u.name)
                    .unwrap_or_default();
                let mut model = account::init_model();
                model.name = name;
                account::program_with(model)
            })
            .guard(move |req| a_guard.user_id(req).is_some())
            .ws(),
        )
        .static_dir("/www", concat!(env!("CARGO_MANIFEST_DIR"), "/WWW"))
        .serve()
}

const LOGIN: &str = r#"<!doctype html><html><head><meta charset="utf-8"><title>log in</title></head>
<body><h1>log in</h1>
<input class="email" value="eneko@join.com">
<input class="password" type="password" value="password1">
<button class="login">log in</button> <span class="result"></span>
<script>
document.querySelector(".login").addEventListener("click", async () => {
  const email = document.querySelector(".email").value;
  const password = document.querySelector(".password").value;
  const r = await fetch("/login", { method: "POST", credentials: "same-origin",
    headers: { "Content-Type": "application/json" }, body: JSON.stringify({ email, password }) });
  document.querySelector(".result").textContent = r.ok ? "logged-in" : "failed";
});
</script></body></html>"#;

const ACCOUNT: &str = r#"<!doctype html><html><head><meta charset="utf-8"><title>account.zu</title></head>
<body><div id="app"></div>
<script type="module">
import { mountLive } from "/www/zumar-live.js";
const app = document.getElementById("app");
mountLive(`ws://${location.host}/account/live`, app, {
  session: false,
  onUnauthorized: () => { app.innerHTML = '<p class="denied">not logged in</p>'; },
});
</script></body></html>"#;
EOF
# point the server's static dir at zumar's real www/ (framework JS shims)
sed -i.bak "s#/WWW#$zumar/www#" "$work/server/src/main.rs" && rm -f "$work/server/src/main.rs.bak"

echo "==> cargo run (background)"
( cd "$work/server" && HOST=127.0.0.1 PORT="$port" AUTH_DB="$db" cargo run -q ) &
srv=$!
trap 'kill $srv 2>/dev/null || true; rm -f "$db" "$db-shm" "$db-wal"' EXIT
for i in $(seq 1 180); do
  curl -fsS -o /dev/null "http://127.0.0.1:$port/" 2>/dev/null && break
  sleep 1
done

echo "==> driving the real wire (auth-check.mjs)"
ZUMAR="$zumar" PORT="$port" node "$here/auth-check.mjs"
