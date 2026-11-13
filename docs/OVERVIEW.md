<!-- Generated from landing/src/Landing.svelte. Run: cd landing && pnpm docs:export -->

# Batteries-included Rust — built from std up

[Documentation index](README.md) · [Tutorial](TUTORIAL.md)

Zero-dependency · Agent-native

**sutegi** (Basque: *forge*) is a batteries-included web framework with **zero third-party dependencies** — every layer hand-built on `std`. Handlers take one `&Ctx` and return anything; `.state()` shares a pooled DB, `.serve()` boots it. One **Backend** trait, two stores — SQLite single-node or Postgres multi-pod. A complete Todo app is ~60 lines, and an AI agent is a first-class user.

- **0** runtime deps
- **480 KB** core binary
- **std** only

```bash
cargo add sutegi
```

```text
> OPS_TOKEN=dev-token cargo run -p todo-example
[sutegi] todo-demo on http://0.0.0.0:8080
> curl -H "Authorization: Bearer dev-token" localhost:8080/__introspect
{ "framework": "sutegi", "routes": [...], "tools": [...] }
> curl -H "Authorization: Bearer dev-token" -H 'Content-Type: application/json' -X POST localhost:8080/__tools/create_todo -d '{"title":"ship sutegi"}'
{ "id": 1, "title": "ship sutegi", "done": false }
```

## On this page

- [What is sutegi?](#what)
- [How it works](#how)
- [Batteries included, std only](#features)
- [Who it’s for](#use-cases)
- [Quick start](#quickstart)
- [Introspect a live app](#live)

<a id="what"></a>

## What is sutegi?

Three goals usually in tension — held at once.

### The tension

Frameworks are either batteries-included but heavy, or tiny but bare. And almost none are designed for an LLM agent to operate directly.

### The forge

Build every component from **std**, with no async runtime. The result is small, auditable, and — because the whole surface is introspectable JSON — natively drivable by agents.

No tokio. No serde. No hyper — not even a Postgres driver crate. The HTTP/1.1 parser, JSON codec, router, ORM, the pure-std Postgres wire driver, and the tool layer are all original code you can read in an afternoon.

<a id="how"></a>

## How it works

Define it, run it, and let humans or agents drive it.

1

### Define

Routes as `&Ctx` closures, models with `#[derive(Model, Validate)]`, and AI tools as `.tool(...)` closures. Each registers its own metadata.

2

### Run

A std-only, thread-per-connection server. Tiny binary, graceful SIGTERM draining, health/readiness/metrics built in.

3

### Drive

Curl it like any API — or let an agent hit `/__introspect` + `/__tools` to discover and invoke the whole app.

<a id="features"></a>

## Batteries included, std only

Each pillar is an opt-in compile-time feature.

### Routing & Ctx

Path params, route groups, middleware; one &Ctx in, any IntoResponse out.

### Two backends, one trait

SQLite (single-node) or Postgres (multi-pod) behind the Backend trait — swap without touching handlers.

### ORM & shared state

Pooled Send+Sync Db via .state(); typed reads, save(), route-model binding, query builder.

### #\[derive(Model, Validate)\]

Schema, migrations, JSON and the validation ruleset — all from one struct, at build time.

### Kv store

Namespaced JSON key/value over either backend: set / get / scan / delete.

### Streaming & SSE

Stream bytes or Server-Sent Events with natural backpressure; same transport as stream tools.

### Durable queue

Over the Backend seam — SQLite or Postgres: leased claims, retries, priorities, dedupe, dead-letter.

### Agent-native

.tool() / .stream\_tool() closures auto-mount /\_\_tools; /\_\_introspect exposes the whole surface.

### Realtime

RFC 6455 sockets on a kqueue/epoll reactor (80k idle conns, zero threads), Phoenix-style channels + presence, cross-pod over PG.

### Actors & supervision

Typed mailboxes, OTP restart policies and strategies, live status at /\_\_actors.

### Search & embeddings

One grammar over tsvector and FTS5, vector columns, and RRF hybrid retrieval in one call.

### Production auth

PBKDF2 passwords, remember-me, login throttling, CSRF, hash-bound sessions, hashed agent tokens.

<a id="use-cases"></a>

## Who it’s for

When small, legible, and agent-friendly matters.

### Agent tool servers

Expose capabilities to an LLM with a built-in manifest and validated invocation — no glue layer.

### Internal microservices

Start single-node on SQLite; scale to many pods on Postgres + the durable queue without rewriting handlers.

### Edge & embedded

A ~480 KB binary with no async runtime and one embedded SQLite file fits where a full stack will not.

### LLM-generated apps

Rigid scaffolding conventions mean a model can extend the codebase correctly with minimal context.

### JSON APIs & CRUD

Routing + typed models + validation cover the everyday backend without pulling a framework zoo.

### Streaming endpoints

SSE token streams for chat/AI UIs, backpressured by the thread-per-connection model.

<a id="quickstart"></a>

## Quick start

From zero to a running, introspectable app in under a minute.

### 1. Add the dependency

Pick the features you need; the core is always there.

```toml
[dependencies]
# default = ["derive", "orm", "validate"] — the agent tool surface is core
sutegi = { version = "*", features = ["sqlite", "graceful"] }  # single-node
# multi-pod: Postgres, the durable queue, cross-pod realtime
# sutegi = { version = "*", features = [
#     "postgres", "queue", "channels", "pubsub-postgres", "graceful" ] }
# minimal HTTP service, nothing else compiled in:
# sutegi = { version = "*", default-features = false }
```

### 2. Write your app

A handler is a closure; run it with graceful shutdown.

```rust
use sutegi::prelude::*;

fn main() -> std::io::Result<()> {
    let ops_token = std::env::var("OPS_TOKEN").expect("set OPS_TOKEN");
    App::new("hello")
        .ops_guard(bearer(&ops_token))
        // handler: one &Ctx in, anything IntoResponse out
        .get("/", "Health check", |_| "sutegi up")
        .get("/hello/:name", "Greet", |c| {
            format!("hi, {}", c.param("name").unwrap_or("world"))
        })
        .serve()   // reads HOST/PORT/WORKERS; graceful drain on SIGTERM
}
```

### 3. Talk to it

As a plain HTTP API — or as an agent surface.

```bash
OPS_TOKEN=dev-token cargo run

# From another terminal:
AUTH="Authorization: Bearer dev-token"
curl -H "$AUTH" localhost:8080/__introspect   # full app surface as JSON
curl -H "$AUTH" localhost:8080/__tools        # LLM tool-calling manifest

# After adding create_todo (tutorial Step 6):
curl -H "$AUTH" -H 'Content-Type: application/json' -X POST localhost:8080/__tools/create_todo -d '{"title":"ship sutegi"}'
```

## Build your first app

Follow the complete [Todo tutorial](TUTORIAL.md), from installation to deployment.

<a id="live"></a>

## Introspect a live app

Run a sutegi app locally, then read its whole surface from here.

The [website](https://enekos.github.io/sutegi/#live) provides an interactive URL field (default: `http://localhost:8080`). It shows the app name and version, route/model/tool counts, and the first eight routes with their descriptions. Browser access requires CORS. The panel currently sends no authorization header; for guarded apps, use the authenticated terminal request from [The agent surface](reference/agents.md).

## Light the forge.

Zero dependencies, a tiny binary, and an agent-native surface — from std up.

[Get started](OVERVIEW.md#quickstart) [Star on GitHub](https://github.com/enekos/sutegi)

sutegi

[GitHub](https://github.com/enekos/sutegi) [README](../README.md) [Examples](../examples)

MIT · Built by [enekos](https://github.com/enekos)
