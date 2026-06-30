//! sutegi performance benchmarks, driven by the aatxe statistical microbench
//! SDK (adaptive sampling, CV-gated, emits an aatxe RunReport on stdout).
//!
//! Covers the real hot paths — JSON codec, HTTP/1.1 request parsing,
//! routing-adjacent ORM query building, validation, real SQLite ops — plus a
//! **full end-to-end request** over a live TCP socket (connect → GET →
//! response → close), which is the closest thing to real-world latency.
//!
//! ```text
//! cargo run --release --bin sutegi-bench            # emits RunReport JSON
//! cargo run --release --bin sutegi-bench | jq .     # pretty
//! ```

use std::io::{BufReader, Read, Write};
use std::net::TcpStream;
use std::thread;
use std::time::Duration;

use aatxe_bench::{bench, keep, Suite};

use sutegi::http::{parse_request, Limits};
use sutegi::json::Json;
use sutegi::orm::db::Db;
use sutegi::orm::{QueryBuilder, UpdateBuilder, Value};
use sutegi::prelude::*;
use sutegi::validate::{validate_schema, Rule, Ruleset};

const ADDR: &str = "127.0.0.1:18099";

fn main() {
    // Spin up a real sutegi server for the end-to-end bench.
    spawn_server();
    wait_ready();

    let mut suite = Suite::new("sutegi");

    // --- JSON codec ---
    let doc = r#"{"name":"sutegi","tags":["a","b","c"],"nested":{"x":1,"y":2.5,"ok":true},"items":[1,2,3,4,5,6,7,8]}"#;
    bench(&mut suite, "json_parse", || {
        keep(Json::parse(doc).unwrap());
    });
    let value = Json::parse(doc).unwrap();
    bench(&mut suite, "json_serialize", || {
        keep(value.to_string());
    });
    bench(&mut suite, "json_pretty", || {
        keep(value.to_pretty());
    });

    // --- HTTP/1.1 request parsing ---
    let raw = b"GET /users/42?page=2 HTTP/1.1\r\nHost: localhost\r\nUser-Agent: bench\r\nAccept: */*\r\n\r\n";
    let limits = Limits::default();
    bench(&mut suite, "http_parse_request", || {
        let mut reader = BufReader::new(&raw[..]);
        keep(parse_request(&mut reader, &limits).unwrap());
    });

    // --- ORM query builder (parameterized SQL emission) ---
    bench(&mut suite, "query_builder", || {
        let built = QueryBuilder::table("todos")
            .select(&["id", "title", "done"])
            .filter("done", "=", Value::Bool(false))
            .filter_in("id", vec![Value::Int(1), Value::Int(2), Value::Int(3)])
            .order_by("id", true)
            .limit(20)
            .offset(40)
            .build();
        keep(built);
    });

    // --- ORM update-builder emission ---
    bench(&mut suite, "update_builder", || {
        let built = UpdateBuilder::table("todos")
            .set("title", Value::Text("new".into()))
            .set("done", Value::Bool(true))
            .filter("id", "=", Value::Int(5))
            .build();
        keep(built);
    });

    // --- ORM count-query emission ---
    let count_qb = QueryBuilder::table("todos")
        .join("users", "users.id", "todos.user_id")
        .filter("done", "=", Value::Bool(false));
    bench(&mut suite, "count_builder", || {
        keep(count_qb.build_count());
    });

    // --- Validation (Ruleset) ---
    let rules = Ruleset::new()
        .field("title", &[Rule::Required, Rule::Str, Rule::MinLen(1), Rule::MaxLen(200)])
        .field("email", &[Rule::Required, Rule::Email]);
    let body = Json::parse(r#"{"title":"ship sutegi","email":"a@b.com"}"#).unwrap();
    bench(&mut suite, "validate_ruleset", || {
        keep(rules.validate(&body).is_ok());
    });

    // --- Validation (JSON Schema subset — the AI tool-arg path) ---
    let arg_schema = Json::parse(
        r#"{"type":"object","required":["title"],"properties":{"title":{"type":"string","minLength":1},"count":{"type":"integer","minimum":0}}}"#,
    )
    .unwrap();
    let args = Json::parse(r#"{"title":"ship sutegi","count":3}"#).unwrap();
    bench(&mut suite, "validate_schema", || {
        keep(validate_schema(&arg_schema, &args).is_ok());
    });

    // --- Real SQLite operations (in-memory) ---
    {
        let db = Db::memory().unwrap();
        db.execute(
            "CREATE TABLE todos (id INTEGER PRIMARY KEY, title TEXT NOT NULL, done BOOLEAN NOT NULL)",
            &[],
        )
        .unwrap();
        bench(&mut suite, "sqlite_insert", || {
            keep(
                db.insert("todos", &[("title", Value::Text("x".into())), ("done", Value::Bool(false))])
                    .unwrap(),
            );
        });
        let select = QueryBuilder::table("todos").select(&["id", "title", "done"]).limit(20);
        bench(&mut suite, "sqlite_select_20", || {
            keep(db.select(&select).unwrap());
        });
        let count_q = QueryBuilder::table("todos").filter("done", "=", Value::Bool(false));
        bench(&mut suite, "sqlite_count", || {
            keep(db.count(&count_q).unwrap());
        });
    }

    // --- End-to-end over a live TCP socket (connection-per-request) ---
    bench(&mut suite, "e2e_request", || {
        keep(http_get("/bench"));
    });
    bench(&mut suite, "e2e_introspect", || {
        keep(http_get("/__introspect"));
    });
    bench(&mut suite, "e2e_post_echo", || {
        keep(http_post("/echo", r#"{"x":1}"#));
    });
    bench(&mut suite, "e2e_tool_call", || {
        keep(http_post("/__tools/echo", r#"{"msg":"hi"}"#));
    });

    suite.emit_stdout();
}

/// An echo tool so the e2e bench can exercise the AI invoke + validate path.
struct BenchEcho;
impl Tool for BenchEcho {
    fn name(&self) -> &str {
        "echo"
    }
    fn description(&self) -> &str {
        "Echo a message back."
    }
    fn parameters(&self) -> Json {
        schema::object(vec![("msg", schema::string("text to echo"))], &["msg"])
    }
    fn call(&self, args: Json) -> Result<Json, String> {
        Ok(Json::obj(vec![("echo", Json::str(args.get("msg").and_then(Json::as_str).unwrap_or("")))]))
    }
}

/// Start a sutegi server on ADDR in a background thread.
fn spawn_server() {
    thread::spawn(|| {
        let app = App::new("bench-server")
            .get("/bench", "Bench endpoint", |_req, _p| text(200, "ok"))
            .post("/echo", "Echo a JSON body back", |req, _p| {
                json(200, &json_body(req).unwrap_or(Json::Null))
            });
        // Mount the AI tool surface so e2e_tool_call has a real endpoint.
        let app = sutegi::ai::mount(app, ToolRegistry::new().add(BenchEcho));
        let _ = app.run(ADDR);
    });
}

/// Block until the server accepts connections (or give up after ~2s).
fn wait_ready() {
    for _ in 0..200 {
        if TcpStream::connect(ADDR).is_ok() {
            return;
        }
        thread::sleep(Duration::from_millis(10));
    }
    eprintln!("warning: bench server did not become ready");
}

/// One full HTTP/1.1 round-trip (connection-per-request, matching sutegi's model).
fn http_get(path: &str) -> usize {
    let mut stream = TcpStream::connect(ADDR).expect("connect");
    let req = format!("GET {} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n", path);
    stream.write_all(req.as_bytes()).expect("write");
    let mut buf = Vec::new();
    stream.read_to_end(&mut buf).expect("read");
    buf.len()
}

/// One full HTTP/1.1 POST round-trip with a JSON body.
fn http_post(path: &str, body: &str) -> usize {
    let mut stream = TcpStream::connect(ADDR).expect("connect");
    let req = format!(
        "POST {} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
        path,
        body.len(),
        body
    );
    stream.write_all(req.as_bytes()).expect("write");
    let mut buf = Vec::new();
    stream.read_to_end(&mut buf).expect("read");
    buf.len()
}
