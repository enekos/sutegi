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
| `sutegi-web`  | Router, `App` builder, middleware, extractors, `/__introspect`. |
| `sutegi-orm`  | Typed schema, fluent parameterized query builder, migration emitter, optional runnable SQLite layer (`sqlite` feature). |
| `sutegi-validate` | Laravel-`Validator`-style rule sets **and** a JSON Schema subset validator, with structured errors. |
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

Rows come back as JSON objects (no derive macro needed) — consistent with the
"machine-readable everything" stance. Enabling `sqlite` grows the binary to
~1.3 MB (bundled SQLite); the zero-dep core remains ~378 KB.

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

Early but usable. The ORM now runs against SQLite behind the opt-in `sqlite`
feature (migrate / `all` / `find` / `create`); validation covers requests and
AI tool args. HTTP is 1.1, connection-per-request, no TLS. Booleans round-trip
through SQLite as `0`/`1` (no native bool). Next: typed row mapping / a derive
macro, form-encoded bodies, keep-alive.

MIT © 2026 Eneko Sarasola
