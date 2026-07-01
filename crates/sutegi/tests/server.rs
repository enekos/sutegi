//! End-to-end coverage of the assembled app: real HTTP requests over a live
//! TCP socket against `App::run`. This is the only place that exercises the
//! request service closure built by `App::into_service` — routing, path params,
//! 404-vs-405, the always-on operational endpoints, pre/after middleware, and
//! the mounted AI tool surface — which the per-crate unit tests can't reach.
//!
//! The AI portion requires the `ai` feature; inert in a minimal build.
#![cfg(feature = "ai")]

use std::io::{Read, Write};
use std::net::TcpStream;
use std::thread;
use std::time::Duration;

use sutegi::prelude::*;

/// Boot `build()` on `addr` in a background thread and wait until it accepts.
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

struct Resp {
    status: u16,
    head: String,
    body: String,
}

impl Resp {
    fn has_header(&self, name: &str) -> bool {
        self.head.lines().any(|l| {
            l.to_ascii_lowercase()
                .starts_with(&format!("{}:", name.to_ascii_lowercase()))
        })
    }
}

/// One `Connection: close` request/response round-trip.
fn send(addr: &str, method: &str, path: &str, headers: &[(&str, &str)], body: &str) -> Resp {
    let mut stream = TcpStream::connect(addr).unwrap();
    let mut req = format!("{method} {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n");
    for (k, v) in headers {
        req.push_str(&format!("{k}: {v}\r\n"));
    }
    if !body.is_empty() {
        req.push_str(&format!("Content-Length: {}\r\n", body.len()));
    }
    req.push_str("\r\n");
    req.push_str(body);
    stream.write_all(req.as_bytes()).unwrap();
    let mut raw = String::new();
    stream.read_to_string(&mut raw).unwrap();
    let (head, body) = raw.split_once("\r\n\r\n").unwrap_or((raw.as_str(), ""));
    let status = head
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|s| s.parse().ok())
        .unwrap();
    Resp {
        status,
        head: head.to_string(),
        body: body.to_string(),
    }
}

fn get(addr: &str, path: &str) -> Resp {
    send(addr, "GET", path, &[], "")
}

const MAIN: &str = "127.0.0.1:18151";
const AI: &str = "127.0.0.1:18152";

#[test]
fn routing_ops_and_middleware_end_to_end() {
    boot(MAIN, || {
        App::new("itest")
            .state(String::from("app-state"))
            .get("/", "root", |_c| "hello")
            .get("/users/:id", "fetch a user", |c| {
                json(
                    200,
                    &Json::obj(vec![("id", Json::str(c.param("id").unwrap_or("")))]),
                )
            })
            .get("/state", "read shared state", |c| {
                c.state::<String>().clone()
            })
            .post("/echo", "echo json back", |c| {
                // Handlers can use `?`: a bad body becomes a 400 automatically.
                let body = c.json()?;
                Ok::<_, Error>(json(200, &body))
            })
            .middleware(|r: &Request| {
                if r.path == "/blocked" {
                    Some(text(403, "denied"))
                } else {
                    None
                }
            })
            .get("/blocked", "should never run", |_c| {
                text(200, "unreachable")
            })
            .after(cors("*"))
    });

    // Plain route.
    let root = get(MAIN, "/");
    assert_eq!(root.status, 200);
    assert_eq!(root.body, "hello");
    // After-middleware (CORS) stamps every response.
    assert!(root.has_header("access-control-allow-origin"));

    // Path parameter capture.
    let user = get(MAIN, "/users/42");
    assert_eq!(user.status, 200);
    assert!(user.body.contains("\"42\""));

    // POST body round-trips through the json_body extractor.
    let echo = send(
        MAIN,
        "POST",
        "/echo",
        &[("content-type", "application/json")],
        r#"{"x":1}"#,
    );
    assert_eq!(echo.status, 200);
    assert!(echo.body.contains("\"x\":1"));

    // Unknown path → 404; known path with wrong method → 405.
    assert_eq!(get(MAIN, "/nope").status, 404);
    assert_eq!(send(MAIN, "POST", "/", &[], "").status, 405);

    // Pre-middleware short-circuits before the handler.
    assert_eq!(get(MAIN, "/blocked").status, 403);

    // Always-on operational endpoints.
    let health = get(MAIN, "/__health");
    assert_eq!(health.status, 200);
    assert!(health.body.contains("ok"));

    assert_eq!(get(MAIN, "/__ready").status, 200);

    let metrics = get(MAIN, "/__metrics");
    assert_eq!(metrics.status, 200);
    assert!(metrics.body.contains("sutegi_requests_total"));

    let intro = get(MAIN, "/__introspect");
    assert_eq!(intro.status, 200);
    assert!(intro.body.contains("\"name\":\"itest\""));
    assert!(intro.body.contains("/users/:id"));
    assert!(intro.body.contains("\"framework\":\"sutegi\""));

    // Shared state is reachable from a handler.
    let state = get(MAIN, "/state");
    assert_eq!(state.status, 200);
    assert_eq!(state.body, "app-state");

    // A malformed JSON body → 400 via `?` on `Ctx::json`.
    let bad = send(
        MAIN,
        "POST",
        "/echo",
        &[("content-type", "application/json")],
        "{not json",
    );
    assert_eq!(bad.status, 400);
    assert!(bad.body.contains("error"));
}

#[test]
fn ai_tool_surface_over_http() {
    boot(AI, || {
        App::new("ai-itest").tool(
            "echo",
            "Echo a message back.",
            schema::object(vec![("msg", schema::string("text to echo"))], &["msg"]),
            |_c, args| {
                Ok(Json::obj(vec![(
                    "echo",
                    Json::str(args.get("msg").and_then(Json::as_str).unwrap_or("")),
                )]))
            },
        )
    });

    // Manifest lists the tool with an input schema.
    let manifest = get(AI, "/__tools");
    assert_eq!(manifest.status, 200);
    assert!(manifest.body.contains("\"echo\""));
    assert!(manifest.body.contains("input_schema"));

    // Valid invocation succeeds.
    let ok = send(
        AI,
        "POST",
        "/__tools/echo",
        &[("content-type", "application/json")],
        r#"{"msg":"hi"}"#,
    );
    assert_eq!(ok.status, 200);
    assert!(ok.body.contains("\"echo\":\"hi\""));

    // Missing required arg → 422 with structured errors.
    let invalid = send(
        AI,
        "POST",
        "/__tools/echo",
        &[("content-type", "application/json")],
        "{}",
    );
    assert_eq!(invalid.status, 422);
    assert!(invalid.body.contains("msg"));

    // Unknown tool → 404.
    let unknown = send(
        AI,
        "POST",
        "/__tools/ghost",
        &[("content-type", "application/json")],
        "{}",
    );
    assert_eq!(unknown.status, 404);

    // The tool is also advertised in the app-wide introspection surface.
    let intro = get(AI, "/__introspect");
    assert!(intro.body.contains("/__tools/:name"));
}
