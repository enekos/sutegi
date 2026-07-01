# sutegi — the forge

> Batteries-included web ergonomics, in Rust — built from `std` up, with a
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
| `sutegi-orm`  | Typed schema, fluent parameterized query builder, one `Backend` trait, a JSON key/value store, and two runnable backends: **SQLite** (`sqlite`, single-node) and **Postgres** (`postgres`, multi-pod). |
| `sutegi-pg`   | Pure-`std` PostgreSQL driver: wire protocol v3 over blocking TCP, SCRAM-SHA-256 auth, connection pool. No async runtime, no C library. |
| `sutegi-macros` | `#[derive(Model)]` — schema, typed JSON hydration, inserts. Compile-time only (syn/quote never reach your binary). |
| `sutegi-validate` | Fluent `Validator`-style rule sets **and** a JSON Schema subset validator, with structured errors. |
| `sutegi-queue` | Durable, cross-pod job queue backed by Postgres (`FOR UPDATE SKIP LOCKED` claim, visibility-timeout retries, dead-letter). |
| `sutegi-ai`   | `Tool` trait, registry, LLM manifest, `/__tools` endpoints (args validated against each tool's schema). |
| `sutegi-hex`  | Opinionated hexagonal/clean-architecture primitives: `AppError`, `UseCase` ports, `respond` adapter glue. |
| `sutegi`      | Facade crate + `prelude`. |
| `sutegi-cli`  | The `sutegi` command: scaffold apps/models/routes, `introspect` a live app. |

## Compile only what you ship

Only `sutegi-json` + `sutegi-http` + `sutegi-web` are always present. Every other
pillar is an opt-in feature on the `sutegi` facade, so the binary carries exactly
what you use:

| Feature | Default? | Pulls in |
|---------|:--------:|----------|
| `orm`      | ✓ | schema + query builder + `Backend` trait + KV store |
| `derive`   | ✓ | `#[derive(Model)]` (build-time syn/quote only) |
| `validate` | ✓ | request / tool validation |
| `ai`       | ✓ | `Tool`/`StreamTool` + `/__tools` |
| `sqlite`   |   | SQLite backend — the **single-node** runnable store (bundled) |
| `postgres` |   | Postgres backend — the **multi-pod** runnable store (pure std) |
| `queue`    |   | durable, cross-pod job queue (Postgres-backed) |
| `graceful` |   | SIGTERM/SIGINT draining (libc) |
| `hex`      |   | hexagonal/clean-architecture primitives |
| `auth`     |   | signed-cookie sessions (HMAC-SHA256) |

```toml
# Minimal HTTP service — core only:
sutegi = { version = "*", default-features = false }
# Single-node app with the SQLite backend + KV:
sutegi = { version = "*", default-features = false, features = ["sqlite"] }
# Multi-pod app on Postgres:
sutegi = { version = "*", default-features = false, features = ["postgres"] }
```

Measured: the core-only `hello` example is **~362 KB**; the full `todo` example
(every pillar + bundled SQLite) is **~1.28 MB**.

## Running at scale (pods)

Built-in operational endpoints (always on, no feature needed):

| Endpoint | Purpose |
|----------|---------|
| `GET /__health`  | liveness — always 200 while the process is up |
| `GET /__ready`   | readiness — 200/503 from your `App::readiness(...)` probe |
| `GET /__metrics` | Prometheus text (requests total, in-flight, by status class) |
| `GET /__introspect` | full app surface (routes/models/tools/endpoints) |

### Configuration

A std-only config layer (`sutegi::config::Config`): `.env` loading, typed
accessors, required-var validation, and prefix scoping.

```rust
use sutegi::config::Config;

let cfg = Config::load();                  // .env (if present) + process env (env wins)
let port = cfg.int("PORT", 8080);
let debug = cfg.bool("DEBUG", false);
let hosts = cfg.list("ALLOWED_HOSTS");     // comma-separated
cfg.require_all(&["DATABASE_URL", "API_KEY"])?;   // fail fast, lists all missing
let db = cfg.prefixed("DB_");              // DB_HOST/DB_PORT → HOST/PORT
```

```rust
App::new("api")
    .workers(Config::load().int("WORKERS", 8) as usize)     // 12-factor config
    .readiness(move || db.lock().unwrap().query("SELECT 1", &[]).is_ok())
    .get("/", "health", |_, _| text(200, "ok"))
    .run_graceful("0.0.0.0:8080")?;   // SIGTERM → stop accepting → drain in-flight
```

`run_graceful` (the `graceful` feature) traps SIGTERM/SIGINT, stops accepting new
connections, and lets in-flight requests finish before exit — exactly what a
Kubernetes rolling update needs. (`run_until(addr, flag)` gives you manual
control without the signal feature.)

**State: pick a backend for the deployment.** The request/route/AI surface is
stateless and scales horizontally. For data, sutegi is opinionated:

- **One instance → SQLite** (`sqlite`). Embedded, zero-ops, single writer. Plus
  the `Kv` key/value store for config/cache/sessions/flags.
- **Many pods → Postgres** (`postgres`) + the durable queue (`queue`). A shared,
  crash-safe source of truth all replicas talk to — pure-`std` driver, no async
  runtime, no C library.

Both backends implement the same `Backend` trait and drive the same query
builder + `Model` surface, so moving from one to the other changes the type you
hold, not your handlers.

## Ontzi — local multi-instance dev

`ontzi` (Basque: *vessel / container*) is a small harness that wraps Docker
Compose so you run the same horizontally-scaled shape locally: N app replicas
behind an nginx load balancer (configured `proxy_buffering off`, so SSE streams
pass straight through).

```bash
./ontzi up 3            # build + 3 app replicas + LB on http://localhost:8080
./ontzi curl /api/todos
./ontzi logs
./ontzi down
./ontzi k8s apply       # or apply the Kubernetes manifests (deploy/k8s/)
```

`deploy/k8s/deployment.yaml` shows the production shape: 3 replicas, liveness/
readiness probes on the built-ins, `terminationGracePeriodSeconds` + a `preStop`
hook for clean draining, Prometheus scrape annotations, and small resource asks
(the binary is tiny, so `requests: 32Mi`).

## Architecture: building clean apps (hexagonal)

For non-trivial apps, sutegi ships an opinionated **ports & adapters** structure
(the `hex` feature). The dependency rule — *source dependencies point inward* —
keeps your domain free of HTTP/SQL, makes business logic unit-testable without a
server, and lets you expose one use case over many transports.

```
inbound adapters (HTTP, AI tools) ──▶ application (use cases) ──▶ ports (traits)
                                              │                        ▲
                                              ▼                        │
                                          domain (pure)      outbound adapters (SQLite, …)
```

`sutegi::hex` provides `AppError` (with a canonical HTTP mapping), the `UseCase`
port trait, and `respond()`/`respond_created()` glue. The
[`examples/hexagonal`](./examples/hexagonal) app is a full worked reference:
domain → ports → use cases → adapters, with the **same `CreateTodo` use case
exposed via both HTTP and an AI tool**, and **two interchangeable repositories
(in-memory ↔ SQLite)** selected at the composition root (`REPO=memory|sqlite`).

Full guide: **[docs/HEXAGONAL.md](./docs/HEXAGONAL.md)**.

## The agent contract

A sutegi app is drivable by an LLM with no source access and no integration code:

1. `GET /__introspect` → discover routes, data models, and tools.
2. `GET /__tools` → an Anthropic-style `{name, description, input_schema}` manifest.
3. `POST /__tools/:name` with a JSON body → invoke a tool; required-field
   validation rejects malformed calls with a clear error.

See [`AGENTS.md`](./AGENTS.md) for the full agent-facing contract.

## Data: two backends, one API

The core ships **no** database driver — the query builder just emits
`(sql, params)`, keeping the default binary tiny. Opt in to a runnable backend,
and sutegi is opinionated about which:

- **SQLite** (`--features sqlite`) — the **single-node** store. Embedded,
  zero-ops, one writer. Bundled; grows the binary to ~1.3 MB.
- **Postgres** (`--features postgres`) — the **multi-pod** store. A shared,
  durable server many replicas talk to. Pure `std`, no async runtime, no C
  library, so the zero-dep core stays ~378 KB.

Both implement the same [`Backend`] trait, and `Model` is written once against
it — **swap the backend, not your handlers**:

```rust
use sutegi::prelude::*;          // brings in Db / Pg / Kv / Backend when enabled

let db = Db::memory()?;          // single-node: or Db::open("app.db")?
// let db = Pg::from_env(8)?;    // multi-pod: same code from here down
Todo::migrate(&db)?;             // CREATE TABLE from the model schema

let id = Todo::create(&db, &[("title", Value::Text("ship sutegi".into()))])?;
let one  = Todo::find(&db, Value::Int(id))?;   // Option<Json>
let all  = Todo::all(&db)?;                     // Vec<Json>
```

Rows come back as JSON objects.

The query layer covers reads and writes, all parameterized:

```rust
QueryBuilder::table("todos")
    .filter_in("id", vec![Value::Int(1), Value::Int(2)])
    .order_by("done", false).order_by("id", true)    // multi-column
    .limit(20).offset(40).build();                    // paging
QueryBuilder::table("todos").filter("done", "=", Value::Bool(true)).build_count();

QueryBuilder::table("todos")
    .filter("done", "=", Value::Bool(false))
    .or_group(&[("priority", "=", Value::Text("high".into())),   // AND (a OR b)
                ("pinned", "=", Value::Bool(true))])
    .where_not_null("title")
    .like("title", "%sutegi%")
    .join("users", "users.id", "todos.user_id")                  // JOIN / LEFT JOIN
    .group_by(&["users.name"]).distinct()
    .where_raw("created_at > ?", vec![Value::Int(0)]);           // escape hatch

UpdateBuilder::table("todos").set("done", Value::Bool(true)).filter("id", "=", Value::Int(5)).build();
DeleteBuilder::table("todos").filter("id", "=", Value::Int(5)).build();

// Runnable: transactions, counts, existence, upsert, pagination — all on the
// `Backend` trait, identical on SQLite and Postgres. The transaction closure
// receives a `Backend` (SQLite `&Db`, Postgres `Tx`), so the query builder and
// Model helpers work inside it.
db.transaction(|tx| { tx.insert("todos", &[/* … */], "id")?; Ok(()) })?;  // COMMIT / ROLLBACK
let n = Todo::count(&db)?;                                              // i64
let ok = db.exists(&Todo::query().filter("id", "=", Value::Int(1)))?;  // bool
db.upsert("todos", &[("id", Value::Int(1)), ("title", Value::Text("x".into()))], "id", "id")?;  // conflict col, pk
Todo::update(&db, Value::Int(1), &[("done", Value::Bool(true))])?;     // by primary key
Todo::delete(&db, Value::Int(1))?;
let page = db.paginate(&Todo::query().order_by("id", true), 2, 20)?;   // Page { items, total, page, … }
let one: Option<Todo> = db.fetch_one(&Todo::query().filter("id", "=", Value::Int(1)))?;
```

### `Kv` — a JSON key/value store (either backend)

Not everything wants a schema. `Kv` is a namespaced JSON key/value store over any
`Backend` — one table, single-statement reads/writes. It's the natural fit for
config, caches, feature flags, and sessions on a single SQLite node (and works on
Postgres for small *shared* state). See [`examples/kv`](./examples/kv).

```rust
use sutegi::prelude::*;

let kv = Kv::new(Db::open("app.db")?);
kv.migrate()?;
kv.set("config", "theme", &Json::str("dark"))?;
let theme = kv.get("config", "theme")?;              // Some(Json::Str("dark"))
let flags = kv.scan("flags")?;                        // Vec<(String, Json)>
kv.delete("config", "theme")?;
```

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

## Collections

`collect(..)` wraps any iterable in a `Collection<T>` — a fluent, chainable API
for the everyday shaping that raw `Iterator` makes verbose (`filter`/`reject`,
`map`/`filter_map`, `group_by`, `partition`, `chunk`, `unique`, `implode`,
`tap`/`pipe`, …). It's a thin layer over `Vec<T>`: it `Deref`s to `[T]` and
round-trips through `Vec`/iterators, so it adds no allocation over doing the
work by hand.

```rust
use sutegi::collect;

let report = collect(orders)
    .filter(|o| o.paid)
    .group_by(|o| o.country.clone())      // HashMap<String, Collection<Order>>
    .into_iter()
    .map(|(country, os)| format!("{country}: {}", os.sum_by(|o| o.total)))
    .collect::<Vec<_>>();

// Numeric chains read left-to-right:
let total: i64 = collect(vec![1, 2, 3, 4]).filter(|n| n % 2 == 0).map(|n| n * 10).sum();
```

## Validation

Two entry points, one structured error shape (`{ field: [messages] }`):

```rust
// Fluent request validation
let rules = Ruleset::new()
    .field("title",    &[Rule::Required, Rule::Str, Rule::MinLen(1), Rule::MaxLen(200)])
    .field("email",    &[Rule::Required, Rule::Email])
    .field("age",      &[Rule::Integer, Rule::Between(18.0, 120.0)])
    .field("website",  &[Rule::Url])
    .field("slug",     &[Rule::AlphaNum])
    .field("role",     &[Rule::In(vec!["admin".into(), "user".into()])])
    .field("password_confirmation", &[Rule::Same("password".into())]);
rules.validate(&body)?;          // Err(ValidationErrors) -> errs.to_json()
```

Rules: `Required`, `Str`, `Integer`, `Number`, `Bool`, `Email`, `Url`, `Alpha`,
`AlphaNum`, `Min`/`Max`, `Between`, `MinLen`/`MaxLen`, `In`, `Same`.

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

## Benchmarks

Hot-path microbenchmarks via [aatxe](https://github.com/enekos/aatxe) (adaptive,
CV-gated sampling; emits a statistical `RunReport`). Run with `make bench`
(needs the `aatxe` repo cloned as a sibling). Indicative `--release` numbers
from a dev machine:

| Bench | Median | ops/sec |
|-------|-------:|--------:|
| `validate_ruleset` (2 fields) | 69 ns | 14.4 M |
| `http_parse_request` | 1.01 µs | 994 K |
| `json_serialize` | 1.14 µs | 877 K |
| `sqlite_insert` (in-mem) | 1.78 µs | 561 K |
| `query_builder` | 1.96 µs | 510 K |
| `json_parse` (~150 B) | 2.42 µs | 414 K |
| `sqlite_select_20` | 5.75 µs | 174 K |
| **`e2e_request`** (full TCP round-trip) | **112 µs** | **~8.9 K** |

`e2e_request` is a complete connect → GET → response → close cycle from a single
sequential client (sutegi is connection-per-request); real throughput scales
with concurrency across the worker pool. Numbers are machine-dependent — run
`make bench` for yours.

## Status

Early but increasingly capable. Typed models (`#[derive(Model)]`), a query
builder + `Backend` trait over **two runnable backends** (SQLite single-node,
pure-`std` Postgres multi-pod), a JSON `Kv` store, validation (requests + AI tool
args), route groups + middleware, route-model binding, and a **durable, cross-pod
job queue** (Postgres) all work and are exercised by the `todo`/`kv` examples.
Streaming responses (SSE + raw) and streaming AI tools are supported. Every pillar
is an opt-in compile feature; the runtime ships health/readiness/metrics endpoints
and graceful shutdown for pods, with an `ontzi` Docker/k8s harness. HTTP is 1.1,
connection-per-request. Next: TLS to Postgres, form-encoded bodies, keep-alive,
relations/joins in the query builder.

MIT © 2026 Eneko Sarasola
