//! sutegi performance benchmarks, driven by the aatxe statistical microbench
//! SDK (adaptive sampling, CV-gated, emits an aatxe RunReport on stdout).
//!
//! Covers the real hot paths — JSON codec, HTTP/1.1 request parsing, routing,
//! ORM query building, validation, real SQLite ops — plus **full end-to-end
//! requests** over a live TCP socket, both connection-per-request and
//! keep-alive (10 requests over one connection, reconnecting whenever the
//! server closes — so the same bench name measures honestly before and after
//! keep-alive support lands).
//!
//! Postgres benches run only when `SUTEGI_PG_TEST_URL` is set (same contract
//! as the integration tests), so the suite stays runnable without a database.
//!
//! ```text
//! cargo run --release --bin sutegi-bench            # emits RunReport JSON
//! cargo run --release --bin sutegi-bench | jq .     # pretty
//! ```

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::thread;
use std::time::Duration;

use aatxe_bench::{bench, keep, Suite};

use sutegi::http::{parse_request, Limits, Method};
use sutegi::json::Json;
use sutegi::orm::db::Db;
use sutegi::orm::{QueryBuilder, UpdateBuilder, Value};
use sutegi::prelude::*;
use sutegi::validate::{validate_schema, Rule, Ruleset};

const ADDR: &str = "127.0.0.1:18099";

fn main() {
    // Spin up a real sutegi server for the end-to-end benches.
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

    // --- Router: match the last of 100 registered routes, in-process ---
    let svc = routing_app(100).service();
    bench(&mut suite, "route_match_100", || {
        keep(svc(bench_request(Method::Get, "/bench99/12345")));
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
            .build()
            .unwrap();
        keep(built);
    });

    // --- ORM update-builder emission ---
    bench(&mut suite, "update_builder", || {
        let built = UpdateBuilder::table("todos")
            .set("title", Value::Text("new".into()))
            .set("done", Value::Bool(true))
            .filter("id", "=", Value::Int(5))
            .build()
            .unwrap();
        keep(built);
    });

    // --- ORM count-query emission ---
    let count_qb = QueryBuilder::table("todos")
        .join("users", "users.id", "todos.user_id")
        .filter("done", "=", Value::Bool(false));
    bench(&mut suite, "count_builder", || {
        keep(count_qb.build_count().unwrap());
    });

    // --- ORM wide builder: stresses the per-setter identifier/operator
    // validation added by the injection guard (~16 identifier + 3 operator
    // checks per build), so the guard's cost is visible under load. ---
    bench(&mut suite, "query_builder_wide", || {
        let built = QueryBuilder::table("events")
            .select(&[
                "id",
                "user_id",
                "kind",
                "payload",
                "created_at",
                "updated_at",
                "status",
                "source",
                "ip",
                "note",
            ])
            .filter("status", "=", Value::Text("active".into()))
            .filter("kind", "!=", Value::Text("noise".into()))
            .filter_in("source", vec![Value::Int(1), Value::Int(2), Value::Int(3)])
            .order_by("created_at", true)
            .order_by("id", true)
            .limit(50)
            .build()
            .unwrap();
        keep(built);
    });

    // --- Validation (Ruleset) ---
    let rules = Ruleset::new()
        .field(
            "title",
            &[
                Rule::Required,
                Rule::Str,
                Rule::MinLen(1),
                Rule::MaxLen(200),
            ],
        )
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
                db.insert(
                    "todos",
                    &[
                        ("title", Value::Text("x".into())),
                        ("done", Value::Bool(false)),
                    ],
                    "id",
                )
                .unwrap(),
            );
        });
        let select = QueryBuilder::table("todos")
            .select(&["id", "title", "done"])
            .limit(20);
        bench(&mut suite, "sqlite_select_20", || {
            keep(db.select(&select).unwrap());
        });
        // Count a fixed-size table: counting `todos` would scan however many
        // rows the adaptive insert bench happened to leave behind.
        db.execute(
            "CREATE TABLE todos_fixed (id INTEGER PRIMARY KEY, title TEXT NOT NULL, done BOOLEAN NOT NULL)",
            &[],
        )
        .unwrap();
        for i in 0..100 {
            db.insert(
                "todos_fixed",
                &[
                    ("title", Value::Text(format!("t{i}"))),
                    ("done", Value::Bool(i % 2 == 0)),
                ],
                "id",
            )
            .unwrap();
        }
        let count_q = QueryBuilder::table("todos_fixed").filter("done", "=", Value::Bool(false));
        bench(&mut suite, "sqlite_count", || {
            keep(db.count(&count_q).unwrap());
        });
    }

    // --- Real Postgres operations (opt-in: SUTEGI_PG_TEST_URL) ---
    #[cfg(feature = "postgres")]
    if let Ok(url) = std::env::var("SUTEGI_PG_TEST_URL") {
        use sutegi::orm::pg::Pg;
        let pg = Pg::connect(&url, 2).expect("pg connect");
        pg.execute("DROP TABLE IF EXISTS bench_todos", &[]).unwrap();
        pg.execute(
            "CREATE TABLE bench_todos (id BIGSERIAL PRIMARY KEY, title TEXT NOT NULL, done BOOLEAN NOT NULL)",
            &[],
        )
        .unwrap();
        bench(&mut suite, "pg_insert", || {
            keep(
                pg.insert(
                    "bench_todos",
                    &[
                        ("title", Value::Text("x".into())),
                        ("done", Value::Bool(false)),
                    ],
                    "id",
                )
                .unwrap(),
            );
        });
        let select = QueryBuilder::table("bench_todos")
            .select(&["id", "title", "done"])
            .limit(20);
        bench(&mut suite, "pg_select_20", || {
            keep(pg.select(&select).unwrap());
        });
        // Fixed-size table for count, for the same reason as sqlite_count.
        pg.execute("DROP TABLE IF EXISTS bench_todos_fixed", &[])
            .unwrap();
        pg.execute(
            "CREATE TABLE bench_todos_fixed (id BIGSERIAL PRIMARY KEY, title TEXT NOT NULL, done BOOLEAN NOT NULL)",
            &[],
        )
        .unwrap();
        for i in 0..100 {
            pg.insert(
                "bench_todos_fixed",
                &[
                    ("title", Value::Text(format!("t{i}"))),
                    ("done", Value::Bool(i % 2 == 0)),
                ],
                "id",
            )
            .unwrap();
        }
        let count_q =
            QueryBuilder::table("bench_todos_fixed").filter("done", "=", Value::Bool(false));
        bench(&mut suite, "pg_count", || {
            keep(pg.count(&count_q).unwrap());
        });
        pg.execute("DROP TABLE IF EXISTS bench_todos", &[]).unwrap();
        pg.execute("DROP TABLE IF EXISTS bench_todos_fixed", &[])
            .unwrap();
    } else {
        eprintln!("skipping pg_* benches: SUTEGI_PG_TEST_URL not set");
    }

    // --- End-to-end over a live TCP socket ---
    bench(&mut suite, "e2e_request", || {
        keep(http_get("/bench"));
    });
    bench(&mut suite, "e2e_keepalive_10", || {
        keep(http_get_10_reusing_connection("/bench"));
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

/// An app with `n` distinct parameterized routes, for router benches.
fn routing_app(n: usize) -> App {
    let mut app = App::new("route-bench");
    for i in 0..n {
        app = app.get(
            &format!("/bench{i}/:id"),
            "route-match bench endpoint",
            |c| text(200, c.param("id").unwrap_or("")),
        );
    }
    app
}

/// A synthetic in-process request for service-closure benches.
fn bench_request(method: Method, path: &str) -> Request {
    Request {
        method,
        path: path.to_string(),
        query: String::new(),
        version: "HTTP/1.1".to_string(),
        headers: vec![("host".to_string(), "localhost".to_string())],
        body: Vec::new(),
        peer: None,
    }
}

/// Start a sutegi server on ADDR in a background thread.
fn spawn_server() {
    thread::spawn(|| {
        let app = App::new("bench-server")
            .get("/bench", "Bench endpoint", |_c| text(200, "ok"))
            .post("/echo", "Echo a JSON body back", |c| {
                let body = c.json()?;
                Ok::<_, Error>(json(200, &body))
            })
            // An echo tool so e2e_tool_call exercises the AI invoke + validate path.
            .tool(
                "echo",
                "Echo a message back.",
                schema::object(vec![("msg", schema::string("text to echo"))], &["msg"]),
                |_ctx, args| {
                    Ok(Json::obj(vec![(
                        "echo",
                        Json::str(args.get("msg").and_then(Json::as_str).unwrap_or("")),
                    )]))
                },
            );
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

/// One full HTTP/1.1 round-trip (connection-per-request).
fn http_get(path: &str) -> usize {
    let mut stream = TcpStream::connect(ADDR).expect("connect");
    let req = format!(
        "GET {} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
        path
    );
    stream.write_all(req.as_bytes()).expect("write");
    let mut buf = Vec::new();
    stream.read_to_end(&mut buf).expect("read");
    buf.len()
}

/// Ten GETs that *try* to reuse one connection: each response is framed by
/// content-length, and the client reconnects whenever the server signals (or
/// enforces) connection-close. Before keep-alive support this degenerates to
/// ten connects; after, it's one — the same bench measures both worlds.
fn http_get_10_reusing_connection(path: &str) -> usize {
    let mut total = 0;
    let mut conn: Option<BufReader<TcpStream>> = None;
    for _ in 0..10 {
        let mut reader = match conn.take() {
            Some(r) => r,
            None => BufReader::new(TcpStream::connect(ADDR).expect("connect")),
        };
        let req = format!("GET {} HTTP/1.1\r\nHost: localhost\r\n\r\n", path);
        reader.get_mut().write_all(req.as_bytes()).expect("write");

        // Read the status line + headers.
        let mut content_length = 0usize;
        let mut close = false;
        let mut line = String::new();
        loop {
            line.clear();
            if reader.read_line(&mut line).expect("read headers") == 0 {
                panic!("connection closed mid-response");
            }
            let l = line.trim_end();
            if l.is_empty() {
                break;
            }
            let lower = l.to_ascii_lowercase();
            if let Some(v) = lower.strip_prefix("content-length:") {
                content_length = v.trim().parse().unwrap_or(0);
            }
            if lower.starts_with("connection:") && lower.contains("close") {
                close = true;
            }
            total += l.len();
        }

        // Read the body by content-length.
        let mut body = vec![0u8; content_length];
        reader.read_exact(&mut body).expect("read body");
        total += body.len();

        if !close {
            conn = Some(reader);
        }
    }
    total
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
