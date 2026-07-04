//! End-to-end REPL coverage: every command class driven through `eval()`
//! against a real app — in-process (with an attached SQLite backend) and
//! remote (over a live TCP socket, the agent-contract path).
#![cfg(feature = "orm")]

use std::net::TcpStream;
use std::thread;
use std::time::Duration;

use sutegi_json::Json;
use sutegi_orm::db::Db;
use sutegi_orm::Backend;
use sutegi_repl::Repl;
use sutegi_web::{schema, App, Error};

/// The shared fixture: one route group, a unary tool, a streaming tool, and a
/// `todos` table the tool writes to.
fn build(db: Db) -> App {
    let handle = db.clone();
    App::new("repl-fixture")
        .state(db)
        .get("/", "Health check.", |_| "up")
        .get("/todos", "List todos.", move |_| -> Result<Json, Error> {
            let rows = handle.query("SELECT id, title FROM todos ORDER BY id", &[])?;
            Ok(Json::arr(rows))
        })
        .tool(
            "create_todo",
            "Create a todo.",
            schema::object(vec![("title", schema::string("the title"))], &["title"]),
            |c, args| {
                let db = c.db::<Db>();
                let title = args.get("title").and_then(Json::as_str).unwrap_or("");
                db.execute(
                    "INSERT INTO todos (title) VALUES (?)",
                    &[sutegi_orm::Value::Text(title.to_string())],
                )?;
                Ok(Json::obj(vec![("created", Json::str(title))]))
            },
        )
        .stream_tool(
            "ticker",
            "Stream three ticks.",
            schema::object(vec![("label", schema::string("frame label"))], &["label"]),
            |_c, args, sink| {
                let label = args.get("label").and_then(Json::as_str).unwrap_or("t");
                for i in 1..=3 {
                    sink.data(&format!("{label}-{i}"))?;
                }
                sink.event("done", "{}")
            },
        )
}

fn fixture_db() -> Db {
    let db = Db::memory().unwrap();
    db.execute(
        "CREATE TABLE todos (id INTEGER PRIMARY KEY AUTOINCREMENT, title TEXT NOT NULL, done INTEGER NOT NULL DEFAULT 0)",
        &[],
    )
    .unwrap();
    db
}

// ---- in-process ------------------------------------------------------------

#[test]
fn surface_commands_in_process() {
    let db = fixture_db();
    let repl = Repl::new(build(db.clone())).db(db);

    let routes = repl.eval("routes");
    assert!(routes.contains("GET"), "routes lists methods: {routes}");
    assert!(routes.contains("/todos"), "routes lists patterns: {routes}");
    assert!(
        routes.contains("List todos."),
        "routes carries docs: {routes}"
    );

    let introspect = repl.eval("introspect");
    assert!(
        introspect.contains("\"framework\": \"sutegi\""),
        "{introspect}"
    );

    let tools = repl.eval("tools");
    assert!(
        tools.contains("create_todo") && tools.contains("[unary"),
        "{tools}"
    );
    assert!(
        tools.contains("ticker") && tools.contains("[streaming"),
        "{tools}"
    );

    let schema = repl.eval("tool create_todo");
    assert!(schema.contains("\"input_schema\""), "{schema}");
    assert!(
        repl.eval("tool nope").contains("unknown tool"),
        "graceful miss"
    );
}

#[test]
fn tool_call_route_and_data_commands_share_state() {
    let db = fixture_db();
    let repl = Repl::new(build(db.clone())).db(db);

    // A tool call writes...
    let created = repl.eval(r#"call create_todo {"title": "ship the repl"}"#);
    assert!(created.contains("ship the repl"), "{created}");

    // ...the app's own route reads it back...
    let listed = repl.eval("get /todos");
    assert!(listed.contains("ship the repl"), "{listed}");

    // ...and so does every flavor of data command against the same handle.
    assert!(repl
        .eval("sql select title from todos")
        .contains("ship the repl"));
    assert!(repl.eval("table todos").contains("ship the repl"));
    assert!(repl.eval("count todos").trim().ends_with('1'));
    let q = repl.eval("q todos select title where done = 0 order id desc limit 5");
    assert!(q.contains("ship the repl") && q.contains("(1 row)"), "{q}");

    // Write path + affected-count reporting.
    assert!(repl
        .eval("sql update todos set done = 1")
        .contains("1 row affected"));

    // Validation failures surface as the tool's own 422.
    let invalid = repl.eval("call create_todo {}");
    assert!(
        invalid.contains("[422]") || invalid.contains("validation"),
        "{invalid}"
    );
}

#[test]
fn streaming_tool_prints_sse_frames() {
    let db = fixture_db();
    let repl = Repl::new(build(db.clone())).db(db);
    let out = repl.eval(r#"call ticker {"label": "tick"}"#);
    for frame in [
        "data: tick-1",
        "data: tick-2",
        "data: tick-3",
        "event: done",
    ] {
        assert!(out.contains(frame), "missing {frame} in: {out}");
    }
}

#[test]
fn kv_commands_round_trip() {
    let db = fixture_db();
    sutegi_orm::kv::Kv::new(db.clone()).migrate().unwrap();
    let repl = Repl::new(build(db.clone())).db(db.clone());

    assert_eq!(
        repl.eval(r#"kv set app greeting {"msg": "kaixo"}"#).trim(),
        "ok"
    );
    assert!(repl.eval("kv get app greeting").contains("kaixo"));
    assert!(repl.eval("kv keys app").contains("greeting"));
    assert!(repl.eval("kv scan app").contains("kaixo"));
    assert_eq!(repl.eval("kv del app greeting").trim(), "deleted");
    assert_eq!(repl.eval("kv get app greeting").trim(), "(not set)");

    // The REPL wrote through the same table the app-side Kv layer reads.
    let kv = sutegi_orm::kv::Kv::new(db);
    repl.eval(r#"kv set app n {"v": 7}"#);
    assert_eq!(
        kv.get("app", "n")
            .unwrap()
            .unwrap()
            .pointer("/v")
            .and_then(Json::as_i64),
        Some(7)
    );
}

#[test]
fn helpful_errors_without_a_db_or_on_bad_input() {
    let db = fixture_db();
    let no_db = Repl::new(build(db.clone()));
    assert!(no_db.eval("sql select 1").contains("no database attached"));

    let repl = Repl::new(build(db.clone())).db(db);
    assert!(repl.eval("nonsense").contains("unknown command"));
    assert!(repl
        .eval("call create_todo not-json")
        .contains("must be valid JSON"));
    assert!(repl.eval("q todos frobnicate").contains("error:"));
    assert!(repl
        .eval("sql select * from missing_table")
        .contains("error:"));
    // 404s are labeled, not silent.
    assert!(repl.eval("get /nope").contains("[404]"));
    // events/jobs degrade to the backend's own error when the tables aren't migrated.
    assert!(repl.eval("events").contains("error:"));
}

// ---- remote ----------------------------------------------------------------

#[test]
fn remote_repl_drives_a_live_server() {
    let addr = "127.0.0.1:39217";
    let db = fixture_db();
    thread::spawn(move || {
        let _ = build(db).run(addr);
    });
    for _ in 0..300 {
        if TcpStream::connect(addr).is_ok() {
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }

    let repl = Repl::connect(addr);
    assert!(repl
        .eval("introspect")
        .contains("\"framework\": \"sutegi\""));
    assert!(repl.eval("routes").contains("/todos"));

    let created = repl.eval(r#"call create_todo {"title": "over the wire"}"#);
    assert!(created.contains("over the wire"), "{created}");
    assert!(repl.eval("get /todos").contains("over the wire"));

    // Streaming over a real socket: close-framed SSE, read incrementally.
    let out = repl.eval(r#"call ticker {"label": "net"}"#);
    for frame in ["data: net-1", "data: net-3", "event: done"] {
        assert!(out.contains(frame), "missing {frame} in: {out}");
    }

    // Remote REPLs have no backend: data commands explain themselves.
    assert!(repl.eval("sql select 1").contains("no database attached"));
    // And a dead server reports usefully instead of panicking.
    let dead = Repl::connect("127.0.0.1:1");
    assert!(dead.eval("introspect").contains("request failed"));
}
