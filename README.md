# sutegi — the forge

> Laravel's batteries-included ergonomics, in Rust — built from `std` up, with a
> tiny binary and an AI agent as a first-class user.

`sutegi` (Basque: *forge / smithy*) is a web framework for Rust with **zero
third-party dependencies**. The HTTP/1.1 server, JSON codec, router, ORM query
builder, and LLM tool layer are all hand-built on the standard library. No
tokio, no hyper, no serde, no `clap`.

Three design goals, held simultaneously:

| Goal | How |
|------|-----|
| **From the ground up** | Every component is original std-only code you can read in one sitting. |
| **Minimum binary size** | No async runtime; size-optimized release profile. The demo app + server is **~378 KB**. |
| **Agent-native** | Routes, models, and tools are introspectable as JSON at runtime; tools are a first-class concept with a built-in LLM manifest + invocation endpoint. |

## Quickstart

```bash
cargo run -p todo-example -- 127.0.0.1:8080
```

```bash
curl localhost:8080/__introspect              # full app surface as JSON
curl localhost:8080/__tools                   # LLM tool-calling manifest
curl -X POST localhost:8080/__tools/create_todo -d '{"title":"ship sutegi"}'
curl localhost:8080/todos
```

A minimal app:

```rust
use sutegi::prelude::*;

fn main() -> std::io::Result<()> {
    App::new("hello")
        .get("/", "Health check", |_req, _params| text(200, "sutegi up"))
        .run("127.0.0.1:8080")
}
```

## Workspace

| Crate | Responsibility |
|-------|----------------|
| `sutegi-json` | JSON value, serializer, parser (deterministic key order). |
| `sutegi-http` | HTTP/1.1 parsing + thread-pool server on `std::net`. |
| `sutegi-web`  | Router, `App` builder, middleware, groups, extractors, streaming (`sse`/`stream`), `/__introspect`. |
| `sutegi-orm`  | Typed schema, fluent parameterized query builder, migration emitter, optional runnable SQLite layer (`sqlite` feature). |
| `sutegi-macros` | `#[derive(Model)]` — schema, typed JSON hydration, inserts. Compile-time only (syn/quote never reach your binary). |
| `sutegi-validate` | Laravel-`Validator`-style rule sets **and** a JSON Schema subset validator, with structured errors. |
| `sutegi-queue` | Zero-dep in-process job queue: background workers, retries, delayed dispatch, introspectable stats. |
| `sutegi-ai`   | `Tool` trait, registry, LLM manifest, `/__tools` endpoints (args validated against each tool's schema). |
| `sutegi`      | Facade crate + `prelude`. |
| `sutegi-cli`  | The `sutegi` command: scaffold apps/models/routes, `introspect` a live app. |

## The agent contract

A sutegi app is drivable by an LLM with no source access and no integration code:

1. `GET /__introspect` → discover routes, data models, and tools.
2. `GET /__tools` → an Anthropic-style `{name, description, input_schema}` manifest.
3. `POST /__tools/:name` with a JSON body → invoke a tool; required-field
   validation rejects malformed calls with a clear error.

See [`AGENTS.md`](./AGENTS.md) for the full agent-facing contract.

## Database (the `sqlite` feature)

The core ships **no** database driver — the query builder just emits
`(sql, params)`, keeping the default binary tiny. Opt in to a runnable, bundled
SQLite layer with `--features sqlite`:

```rust
use sutegi::prelude::*;          // brings in Db when the feature is on

let db = Db::memory()?;          // or Db::open("app.db")?
Todo::migrate(&db)?;             // CREATE TABLE from the model schema

let id = Todo::create(&db, &[("title", Value::Text("ship sutegi".into()))])?;
let one  = Todo::find(&db, Value::Int(id))?;   // Option<Json>
let all  = Todo::all(&db)?;                     // Vec<Json>
```

Rows come back as JSON objects. Enabling `sqlite` grows the binary to ~1.3 MB
(bundled SQLite); the zero-dep core remains ~378 KB.

### Typed models with `#[derive(Model)]`

```rust
#[derive(Model)]
#[model(table = "todos")]
struct Todo {
    #[model(primary)]
    id: i64,
    title: String,
    done: bool,          // round-trips cleanly (SQLite stores 0/1, you get a bool)
    note: Option<String>,// Option<T> => nullable column
}

let todos: Vec<Todo> = Todo::all_typed(&db)?;     // hydrated structs
let one: Option<Todo> = Todo::find_typed(&db, Value::Int(1))?;
let id = Todo::create(&db, &Todo { id: 0, title: "x".into(), done: false, note: None }.to_values()[1..])?;
let body: Json = one.unwrap().to_json();          // booleans serialize as real booleans
```

The derive generates the schema, `FromRow` hydration, `to_values()` (inserts),
and `to_json()`. Its build-time deps don't affect your runtime binary; turn it
off with `default-features = false` if you prefer hand-written models.

### Route groups + middleware

```rust
let auth = mw(|req: &Request| {
    if req.header("authorization").is_some() { None }          // continue
    else { Some(text(401, "unauthorized")) }                   // short-circuit
});

App::new("api")
    .group("/api", vec![auth], |g| {
        g.get("/todos", "List", list)
         .post("/todos", "Create", create)
    })
```

Group middleware runs before each route in the group; patterns are prefixed.

### Route-model binding (with `sqlite`)

```rust
// GET /api/todos/:id  — hydrate a Todo from the path param, or return 404/500.
move |_req, p| match sutegi::binding::model::<Todo>(&db.lock().unwrap(), p, "id") {
    Ok(todo) => json(200, &todo.to_json()),
    Err(resp) => resp,
}
```

### Background jobs

```rust
struct Notify { to: String }
impl Job for Notify {
    fn name(&self) -> &str { "notify" }
    fn handle(&self) -> Result<(), String> { /* send … */ Ok(()) }
    fn tries(&self) -> u32 { 3 }     // retried on Err
}

let queue = Queue::new(4);           // Arc<Queue>, 4 workers
queue.dispatch(Notify { to: "a@b.com".into() });
let stats = queue.stats();           // { dispatched, processed, failed, retried } — JSON-able
```

## Validation

Two entry points, one structured error shape (`{ field: [messages] }`):

```rust
// Laravel-style request validation
let rules = Ruleset::new()
    .field("title", &[Rule::Required, Rule::Str, Rule::MinLen(1), Rule::MaxLen(200)])
    .field("done",  &[Rule::Bool]);
rules.validate(&body)?;          // Err(ValidationErrors) -> errs.to_json()
```

AI tool arguments are validated automatically against each tool's declared
`input_schema` (type, `required`, `enum`, bounds), so a malformed agent call
gets a precise `422`:

```json
{ "error": "validation failed", "errors": { "title": ["expected type 'string'"] } }
```

## Streaming (SSE & raw)

Because the server is blocking thread-per-connection, streaming is trivial: a
handler just writes and flushes over time, and the worker thread provides
natural backpressure. No async, no chunked encoding (framing is "read until
close", valid HTTP/1.1).

```rust
// Server-Sent Events — the natural transport for LLM tokens.
.get("/stream", "SSE demo", |_req, _p| sse(|sink| {
    for i in 1..=3 {
        sink.data(&format!("tick {i}"))?;     // each frame is flushed immediately
        std::thread::sleep(std::time::Duration::from_millis(80));
    }
    sink.event("done", "bye")
}))

// Or raw byte streaming (NDJSON, large exports, …):
.get("/export", "stream rows", |_req, _p| stream(200, "application/x-ndjson", |sink| {
    for row in rows() { sink.write_str(&format!("{}\n", row.to_json()))?; }
    Ok(())
}))
```

Streaming tools for agents implement `StreamTool` and are invoked over SSE at
`POST /__tools/:name/stream`:

```rust
struct StreamAnswer;
impl StreamTool for StreamAnswer {
    fn name(&self) -> &str { "stream_answer" }
    fn description(&self) -> &str { "Stream an answer token-by-token." }
    fn parameters(&self) -> Json { schema::object(vec![("prompt", schema::string("…"))], &["prompt"]) }
    fn run(&self, args: Json, sink: &mut SseSink) -> std::io::Result<()> {
        for tok in answer(&args).split(' ') { sink.data(tok)?; }
        sink.event("done", "{}")
    }
}
// registry.add_stream(StreamAnswer)
```

The `/__tools` manifest marks these with `"streaming": true`, so an agent knows
to hit the SSE endpoint. Argument validation happens *before* the stream opens,
so malformed calls still get a normal JSON `422`.

## CLI

```bash
sutegi new blog            # scaffold a new app
sutegi make:model Post     # src/models/post.rs (table: posts)
sutegi make:route health   # src/routes/health.rs with register(app)
sutegi introspect          # pretty-print a running app's /__introspect
```

Scaffolding follows rigid conventions on purpose — one right shape per artifact —
so an LLM can extend a sutegi app correctly with minimal context.

## Status

Early but increasingly capable. Typed models (`#[derive(Model)]`), a runnable
SQLite ORM (opt-in `sqlite`), validation (requests + AI tool args), route groups
+ middleware, route-model binding, and a background job queue all work and are
exercised by the `todo` example. Streaming responses (SSE + raw) and streaming
AI tools are supported. HTTP is 1.1, connection-per-request, no TLS. Next:
form-encoded bodies, keep-alive, a durable (SQLite-backed) queue driver,
relations/joins in the query builder.

MIT © 2026 Eneko Sarasola
