//! counter-live — the LiveView-analog demo, everything server-side.
//!
//! - the model lives in a per-connection `Program` (browser holds no state)
//! - "lazy +10" is a **server** `delay`; the ticker is a **server** `every`
//!   on the bridge's timer thread; "fetch" is a **server** `httpGet`
//!   against this same app's API
//! - every input is journaled to SQLite via sutegi-events: reload the tab
//!   (same localStorage session) and the state comes back — replay, not
//!   persistence of the model itself
//!
//! ```sh
//! PORT=8796 cargo run --example counter_live
//! ```

use sutegi_web::{html, text, App};
use sutegi_zumar::{EventJournal, Live};
use zumar_core::{el, text as t, VNode};
use zumar_runtime::effects::{delay, every, http_get, Cmds, HttpResult, Sub};
use zumar_runtime::Program;

#[derive(Clone)]
enum Msg {
    Inc,
    Dec,
    Lazy,
    Bump,
    Toggle,
    Tick,
    Fetch,
    Got(HttpResult),
}

struct Model {
    count: i64,
    running: bool,
    greeting: String,
    pending: bool,
}

fn update(model: &mut Model, msg: Msg) -> Cmds<Msg> {
    match msg {
        Msg::Inc => model.count += 1,
        Msg::Dec => model.count -= 1,
        Msg::Lazy => {
            model.pending = true;
            return vec![delay(1000, Msg::Bump)];
        }
        Msg::Bump => {
            model.pending = false;
            model.count += 10;
        }
        Msg::Toggle => model.running = !model.running,
        Msg::Tick => model.count += 1,
        Msg::Fetch => return vec![http_get("/api/hello", Msg::Got)],
        Msg::Got(r) => model.greeting = r.body,
    }
    Vec::new()
}

fn view(model: &Model) -> VNode<Msg> {
    el("div")
        .attr("class", "counter")
        .child(el("h1").child(t("counter-live")))
        .child(
            el("p")
                .attr("class", "sub")
                .child(t("state, effects and timers all live in the server")),
        )
        .child(
            el("div")
                .attr("class", "row")
                .child(el("button").on("click", Msg::Dec).child(t("-")))
                .child(
                    el("span")
                        .attr("class", "count")
                        .child(t(model.count.to_string())),
                )
                .child(el("button").on("click", Msg::Inc).child(t("+"))),
        )
        .child(
            el("div")
                .attr("class", "row")
                .child(
                    el("button")
                        .attr("class", "lazy")
                        .on("click", Msg::Lazy)
                        .child(t(if model.pending {
                            "…"
                        } else {
                            "lazy +10 (server delay)"
                        })),
                )
                .child(
                    el("button")
                        .attr("class", "tick")
                        .on("click", Msg::Toggle)
                        .child(t(if model.running {
                            "stop ticker"
                        } else {
                            "start ticker (server every)"
                        })),
                )
                .child(
                    el("button")
                        .attr("class", "fetch")
                        .on("click", Msg::Fetch)
                        .child(t("fetch (server httpGet)")),
                ),
        )
        .child(
            el("p")
                .attr("class", "greeting")
                .child(t(model.greeting.clone())),
        )
        .into()
}

fn subs(model: &Model) -> Vec<Sub<Msg>> {
    if model.running {
        vec![every(1000, Msg::Tick)]
    } else {
        Vec::new()
    }
}

fn program() -> Program<Model, Msg> {
    Program::new(
        Model {
            count: 0,
            running: false,
            greeting: String::new(),
            pending: false,
        },
        update,
        view,
    )
    .with_subscriptions(subs)
}

fn main() -> std::io::Result<()> {
    let db_path = std::env::var("LIVE_DB").unwrap_or_else(|_| "counter-live.db".to_string());
    let db = sutegi_orm::db::Db::open(&db_path).expect("sqlite open");
    let journal = EventJournal::new(db).expect("journal migrate");

    // the shim JS comes straight from the sibling zumar checkout
    let zumar_www = concat!(env!("CARGO_MANIFEST_DIR"), "/../../../zumar/www");

    App::new("counter-live")
        .get("/", "The live counter page.", |_c| html(200, PAGE))
        .get(
            "/api/hello",
            "What the server-side httpGet fetches.",
            |_c| text(200, "aupa from the same server, fetched server-side"),
        )
        .ws(
            "/live",
            "zumar live socket: server-side Program per connection, \
             journaled per session, replayed on reconnect.",
            Live::new(|_req| program()).journal(journal).ws(),
        )
        .static_dir("/www", zumar_www)
        .serve()
}

const PAGE: &str = r#"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8" />
  <meta name="viewport" content="width=device-width, initial-scale=1" />
  <title>counter-live</title>
  <style>
    :root { color-scheme: dark; }
    body { margin: 0; min-height: 100vh; display: grid; place-items: center;
           background: #16130f; color: #e8ddcf;
           font-family: ui-monospace, "SF Mono", Menlo, monospace; }
    .counter { text-align: center; }
    .counter h1 { letter-spacing: 0.25em; color: #d9a76a; }
    .sub { color: #6f7362; font-size: 0.8rem; }
    .row { display: flex; align-items: center; justify-content: center; gap: 1rem; margin: 1.4rem 0; }
    .count { font-size: 3rem; min-width: 4ch; font-variant-numeric: tabular-nums; }
    .greeting { color: #d9a76a; min-height: 1em; font-size: 0.85rem; }
    button { background: #1e2318; color: #e8ddcf; border: 1px solid #3a4230;
             border-radius: 8px; font: inherit; cursor: pointer;
             font-size: 1.2rem; width: 3rem; height: 3rem; }
    button:hover { border-color: #d9a76a; }
    button.lazy, button.tick, button.fetch { width: auto; height: auto;
             font-size: 0.8rem; padding: 0.5rem 0.9rem; }
    .live { position: fixed; bottom: 1rem; width: 100%; text-align: center;
            color: #6f7362; font-size: 0.75rem; }
  </style>
</head>
<body>
  <div id="app"></div>
  <p class="live">live: reload the tab — the session replays from the journal</p>
  <script type="module">
    import { mountLive } from "/www/zumar-live.js";
    const proto = location.protocol === "https:" ? "wss" : "ws";
    mountLive(`${proto}://${location.host}/live`, document.getElementById("app"));
  </script>
</body>
</html>
"#;
