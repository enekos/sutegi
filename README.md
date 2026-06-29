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
| `sutegi-orm`  | Typed schema, fluent parameterized query builder, migration emitter. |
| `sutegi-ai`   | `Tool` trait, registry, LLM manifest, `/__tools` endpoints. |
| `sutegi`      | Facade crate + `prelude`. |
| `sutegi-cli`  | The `sutegi` command: scaffold apps/models/routes, `introspect` a live app. |

## The agent contract

A sutegi app is drivable by an LLM with no source access and no integration code:

1. `GET /__introspect` → discover routes, data models, and tools.
2. `GET /__tools` → an Anthropic-style `{name, description, input_schema}` manifest.
3. `POST /__tools/:name` with a JSON body → invoke a tool; required-field
   validation rejects malformed calls with a clear error.

See [`AGENTS.md`](./AGENTS.md) for the full agent-facing contract.

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

Early. The query builder emits SQL but ships no bundled driver (keeping the
binary tiny is the point); wire your own, or a future optional `rusqlite`
feature. HTTP is 1.1, connection-per-request, no TLS. See the roadmap in the
thinking-os project page.

MIT © 2026 Eneko Sarasola
