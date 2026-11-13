<!-- Generated from landing/src/Landing.svelte. Run: cd landing && pnpm docs:export -->

# Build your first app

[Documentation index](README.md) · [Overview](OVERVIEW.md)

Follow along and build a complete **Todo API** — routes, a database, validation, an agent interface, streaming and a production deploy — one step at a time.

## On this page

### Getting started

- [Step 1: Install & scaffold](#install)
- [Step 2: Your first route](#route)

### The basics

- [Step 3: Model your data](#model)
- [Step 4: Talk to the database](#orm)
- [Step 4b: Two backends, one trait](#backend)
- [Step 5: Validate the input](#validate)

### Agents & realtime

- [Step 6: Make it agent-native](#agent)
- [Step 7: Stream responses](#stream)
- [Step 7b: Push to the browser](#realtime)
- [Step 8: Defer the slow work](#jobs)

### Going to production

- [Step 9: Structure for growth](#hex)
- [Step 10: Ship to production](#ops)
- [Step 11: Deploy with ontzi](#deploy)

## Getting started

<a id="install"></a>

### Step 1: Install & scaffold

Every sutegi app is an ordinary Rust binary. Add the crate, switch on the feature pillars you want, and you’re ready — there are no runtime dependencies trailing behind it. Only `json`, `http` and `web` are always compiled; everything else is opt-in, so the binary carries exactly what you use.

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

> [!TIP]
> In a hurry? `sutegi new todo-api` scaffolds the whole app with the conventional layout. Add pieces later with `sutegi make:model Todo` and `sutegi make:route todos`.

<a id="route"></a>

### Step 2: Your first route

A handler is a closure that takes one `&Ctx` and returns anything that is `IntoResponse` — a `&str`/`String`, a `Json`, a `(u16, Json)` pair, `()` for a 204, an `Error`, or a `Result<T, Error>` so `?` just works. Read path params off the `Ctx`. Call `.serve()` and you have a live HTTP/1.1 server.

```rust
use sutegi::prelude::*;

fn main() -> std::io::Result<()> {
    App::new("todo-api")
        .get("/", "Health check", |_| "sutegi up")
        .get("/todos/:id", "Show one todo", |c| {
            format!("todo #{}", c.param("id").unwrap_or("?"))
        })
        .serve()   // reads HOST/PORT/WORKERS (or argv[1])
}
```

> [!NOTE]
> Every route you register also shows up in `/__introspect`. You never write that endpoint — sutegi assembles your app’s whole surface for you. We’ll lean on it in Step 6.

## The basics

<a id="model"></a>

### Step 3: Model your data

Echoing a path param is fun for about ten seconds. Let’s give the app something real to store. Derive `Model` *and* `Validate` on a plain struct: `Model` gives you schema, migrations, typed reads and `save()`; `Validate` reads the `#[validate(...)]` field attrs and generates the model’s ruleset. Bools round-trip cleanly and `Option<T>` becomes a nullable column. Both macros run at build time, so none of their machinery reaches your binary.

```rust
#[derive(Model, Validate)]
#[model(table = "todos")]
struct Todo {
    #[model(primary)]
    id: i64,
    #[validate(required, str, min_len = 1, max_len = 200)]
    title: String,
    done: bool,            // round-trips as a real bool
    note: Option<String>,  // Option<T> => nullable column
    #[model(skip)]
    cached: bool,          // not persisted; default-initialized
}
```

> [!TIP]
> Drop the `#[model(table = "…")]` line and the name is inferred as snake\_case + plural — `Todo` becomes `todos` automatically. The struct is the single source of truth: table, JSON, `save()` and validation all derive from it.

<a id="orm"></a>

### Step 4: Talk to the database

Shared state replaces the hand-rolled `Arc<Mutex<Db>>`. `Db` is now a pooled, `Send + Sync + Clone` handle — no Mutex. Register it once with `.state(db)`, then reach it from any handler via `c.db::<Db>()`. Migrate the table, then read and write through your typed model: `Todo::all_typed`, `Todo::find_typed`, `todo.save` (insert, DB assigns the pk). `c.model::<Todo, Db>("id")` is route-model binding — it parses the path param, loads the row, and hands you a typed `Todo` or a 404.

```rust
let db = Db::open_or_memory("DATABASE_PATH");  // or Db::memory()
Todo::migrate(&db).unwrap();

App::new("todo")
    .state(db)                                // shared, no Mutex
    .get("/todos", "list", |c| -> Result<Json, Error> {
        let all = Todo::all_typed(c.db::<Db>())?;
        Ok(Json::arr(all.iter().map(Todo::to_json).collect()))
    })
    .get("/todos/:id", "show", |c| {          // route-model binding
        c.model::<Todo, Db>("id").map(|t| t.to_json())
    })
    .post("/todos", "create", |c| {
        let todo: Todo = c.validated()?;      // parse + validate -> 422
        let id = todo.save(c.db::<Db>())?;    // typed insert; DB assigns id
        Ok::<_, Error>((201, Todo { id, ..todo }.to_json()))
    })
    .serve()
```

> [!TIP]
> Prefer raw SQL? The parameterized query builder is still there — OR groups, `IS NULL`, `LIKE`, joins, transactions and pagination — `db.query("SELECT 1", &[])` is the escape hatch.

<a id="backend"></a>

### Step 4b: Two backends, one trait

This is the key story. Everything above is written against the `Backend` trait, not a concrete engine. `Db` (SQLite, `sqlite` feature) is the single-node store — one file on disk, zero ops. `Pg` (Postgres, a **pure-std** wire driver, `postgres` feature) is the multi-pod store. Both implement `Backend`, so `Model` is written once against it: swap `Db` for `Pg::from_env(8)?` and every handler above is unchanged. There is also `Kv<B>` — a namespaced JSON key/value store over either backend.

```rust
// single node — an embedded file, nothing to run
let db = Db::open_pool("app.db", 8);

// multi-pod — same Model code, Postgres underneath
let pg = Pg::from_env(8)?;      // reads DATABASE_URL
Todo::migrate(&pg).unwrap();
let all = Todo::all_typed(&pg)?;   // identical call site

// a namespaced JSON key/value store over either backend
let kv = Kv::new(db);              // or Kv::new(pg)
kv.migrate()?;
kv.set("config", "theme", &Json::str("dark"))?;
let theme = kv.get("config", "theme")?;   // Option<Json>
let all_flags = kv.scan("flags")?;        // Vec<(String, Json)>
```

> [!NOTE]
> Write your domain against `Backend`, choose the store at boot. SQLite for local dev, edge and single-node; Postgres when you scale to many pods — the handlers never learn which one they got.

<a id="validate"></a>

### Step 5: Validate the input

Never trust the request body. In Step 3 the `#[validate(...)]` attrs already generated the model’s ruleset, so the create handler just calls `c.validated::<Todo>()` — parse the body, validate it, hydrate a typed model, or return a `422` with structured per-field errors. Need an ad-hoc ruleset (a login form, a filter)? Build one and call `c.validate(&rules)`. The available rules cover the common cases.

```rust
// (a) model-driven: the ruleset comes from #[derive(Validate)]
.post("/todos", "create", |c| {
    let todo: Todo = c.validated()?;   // 422 on any field error
    /* … */
})

// (b) ad-hoc ruleset for a shape without a model
let rules = Ruleset::new()
    .field("email", &[Rule::Required, Rule::Email])
    .field("age",   &[Rule::Integer, Rule::Between(18.0, 120.0)])
    .field("site",  &[Rule::Url])
    .field("password_confirmation", &[Rule::Same("password".into())]);
let body = c.validate(&rules)?;   // Err -> { "email": ["… valid email …"] }
```

> [!NOTE]
> You get this on the agent surface for free too: AI tool arguments are validated against each tool’s schema automatically — more on that next.

## Agents & realtime

<a id="agent"></a>

### Step 6: Make it agent-native

Here’s where sutegi differs from every other framework. Tools are first-class on the `App`: register a closure with `.tool(...)` and it’s instantly exposed to an LLM — a manifest at `/__tools`, a validated invocation endpoint, and an SSE variant via `.stream_tool(...)`. The closure shares app state through an owned `ToolCtx` (`c.db::<Db>()`, `c.state::<T>()`) and gets the already schema-validated `Json` args. An agent discovers your whole app via `/__introspect` and acts on it over plain JSON — no SDK, no glue layer.

```rust
// Add to the App builder; set OPS_TOKEN before running the app.
.ops_guard(bearer(&std::env::var("OPS_TOKEN").expect("set OPS_TOKEN")))
.tool("create_todo", "Create a todo",
    schema::object(vec![("title", schema::string("the title"))], &["title"]),
    |c, args| {
        let todo = Todo::from_input(&args)?;   // args already schema-validated
        let id = todo.save(c.db::<Db>())?;
        Ok(Todo { id, ..todo }.to_json())
    })
.stream_tool("stream_answer", "Stream tokens over SSE",
    schema::object(vec![("prompt", schema::string("the prompt"))], &["prompt"]),
    |_c, args, sink| {
        let prompt = args.get("prompt").and_then(Json::as_str).unwrap_or("");
        for tok in prompt.split(' ') { sink.data(tok)?; }
        sink.event("done", "{}")
    })
// GET  /__tools               -> manifest { name, description, input_schema }
// POST /__tools/create_todo   -> invoke (args validated -> 422 on failure)
// POST /__tools/:name/stream  -> SSE for streaming tools
```

> [!TIP]
> That’s the entire integration — the old `Tool`/`StreamTool` traits and `ToolRegistry` are gone. Point any agent at `/__introspect` for the surface and `/__tools` for the call manifest — the same app you built for humans is now drivable by a model.

> [!NOTE]
> The agent surface is closed by default (`403` without an `ops_guard`). Send `Authorization: Bearer <OPS_TOKEN>` when calling these endpoints, and `Content-Type: application/json` when posting tool arguments.

<a id="stream"></a>

### Step 7: Stream responses

Because the server is blocking and thread-per-connection, streaming is trivial and naturally backpressured — there’s no executor to fight. Send raw bytes or Server-Sent Events; each frame flushes immediately. It’s the same transport that carries live LLM tokens back to a UI (and the one `.stream_tool` rides on).

```rust
.get("/stream", "SSE demo", |_| sse(|sink| {
    for token in answer().split(' ') {
        sink.data(token)?;        // each frame flushed immediately
    }
    sink.event("done", "{}")
}))
```

<a id="realtime"></a>

### Step 7b: Push to the browser

For live UIs, upgrade instead of polling. An upgraded socket **detaches** from the HTTP worker into a sharded kqueue/epoll reactor — no async runtime — so an idle connection costs ~340 bytes and zero threads (measured: 80k live sockets at 0.0% idle CPU). On top of that, `channels` gives you Phoenix-style topics, joins, replies and presence, and broadcasts ride the pubsub `Broker` seam: adding `.broker(PgPubSub::connect(&cfg)?)` is the *only* change between one pod and a fleet.

```rust
let hub = Channels::new()
    .channel(Channel::new("room:*")
        .doc("A chat room. Join with a nick.")
        .on_join(|socket, payload| {
            socket.assign("nick", payload.pointer("/nick").cloned().unwrap());
            Ok(Json::Null)
        })
        .on("new_msg", |socket, payload| {
            socket.broadcast("new_msg", payload);   // all members, all pods
            Reply::None
        }))
    // .broker(PgPubSub::connect(&pg_cfg)?)         // <- cross-pod, that's it
    .check_origin(["https://app.example.com"])      // MUST set if cookies auth it
    .build();

App::new("chat")
    .channels("/channels", "The chat socket.", hub.clone())
    .serve()   // + GET /__channels: the manifest an agent joins from
```

> [!TIP]
> A bundled ~4 KB dependency-free JS client (`sutegi_channels::JS_CLIENT`) handles reconnect, rejoin and heartbeats. And `watch(query)` turns a topic into a live query: any pod’s committed write pushes a `Change {added, updated, removed}` diff you can broadcast straight through.

<a id="jobs"></a>

### Step 8: Defer the slow work

Some work shouldn’t block the response — sending mail, calling a webhook. The durable queue (`queue` feature) runs over the **same `Backend` seam**, so one jobs table and one set of SQL work on bundled SQLite *and* on Postgres. The claim stamps a lease instead of deleting the row, so a crashed worker’s job becomes visible again after the visibility timeout. Register a handler by name, `dispatch` a payload, and `start` N worker threads.

```rust
use std::sync::Arc;
use sutegi::queue::Queue;

let mut queue = Queue::new(db.clone());        // any Backend: Db or Pg
queue.migrate()?;                              // creates sutegi_jobs
queue.register("notify", |job| {
    let to = job.payload().get("to").and_then(Json::as_str).unwrap_or("");
    /* send … */ Ok(())                        // Err -> retried w/ backoff
});

queue.dispatch("notify", Json::obj(vec![("to", Json::str("a@b.com"))]))?;

// …or shape it: its own pool, one in flight per key, 3 tries.
queue.job("video.ingest", payload).queue("video").unique("yt:abc")
     .priority(10).max_attempts(3).dispatch()?;

let queue = Arc::new(queue);
let fast = Arc::clone(&queue).start(4);             // 4 on "default"
let slow = Arc::clone(&queue).start_on("video", 1); // 1 on the slow queue
```

> [!NOTE]
> Claims are exclusive on both backends — `FOR UPDATE SKIP LOCKED` on Postgres, and on SQLite the serialized writer already gives it. Postgres additionally makes that exclusivity *cross-pod*; `queue.cross_pod()` tells you which guarantee you actually have.

## Going to production

<a id="hex"></a>

### Step 9: Structure for growth

As the app grows, the hexagonal toolkit keeps it honest: your domain stays pure, the application layer depends on port traits, and adapters (HTTP, AI, and a repo over either `Backend`) plug in at the edges. One use case, many transports — and fully testable without ever starting a server.

```rust
impl UseCase for CreateTodo {
    type Input = String;          // title
    type Output = Todo;
    fn execute(&self, title: String) -> AppResult<Todo> {
        let todo = Todo::new(title).map_err(AppError::invalid)?;
        let id = self.repo.insert(&todo)?;   // outbound port (Db or Pg)
        Ok(Todo { id, ..todo })
    }
}
// inbound HTTP adapter — new &Ctx handler, respond_created maps AppResult:
.post("/todos", "Create", move |c| {
    let title = c.json().ok()
        .and_then(|b| b.get("title").and_then(Json::as_str).map(str::to_string))
        .unwrap_or_default();
    respond_created(create.execute(title))
})
```

> [!NOTE]
> The very same `CreateTodo` use case can back both an HTTP route and the AI tool from Step 6, over whichever `Backend` the composition root injects. Write the logic once; expose it everywhere.

<a id="ops"></a>

### Step 10: Ship to production

You’re ready to deploy. The operational endpoints are always on, and `.serve()` drains in-flight requests on SIGTERM before exiting — exactly what a Kubernetes rolling update needs. `Db` is `Clone`, so clone a handle for the readiness probe before you hand ownership to `.state()`, and wire it to whatever “healthy” means for your app.

```rust
let ready = db.clone();   // probe keeps its own pooled handle
App::new("api")
    .state(db)
    .readiness(move || ready.query("SELECT 1", &[]).is_ok())
    .serve()?;   // reads HOST/PORT/WORKERS; graceful drain on SIGTERM
                 // (.run(addr) / .run_graceful(addr) still exist)

// GET /__health   liveness        GET /__ready    readiness (200/503)
// GET /__metrics  Prometheus      GET /__introspect  full surface
```

<a id="deploy"></a>

### Step 11: Deploy with ontzi

Finally, `ontzi` (Basque: vessel / container) wraps Docker Compose: spin up N replicas behind an nginx load balancer (with `proxy_buffering off`, so the SSE streams from Step 7 pass straight through), then promote the same shape to Kubernetes — the manifests ship with probes, graceful drain and Prometheus annotations already wired.

```bash
./ontzi up 3            # 3 replicas + LB on http://localhost:8080
./ontzi curl /api/todos
./ontzi logs
./ontzi k8s apply      # apply deploy/k8s/ (probes, drain, metrics)
```

> [!TIP]
> That’s the whole journey: a route, a database, validation, an agent interface, streaming, jobs and a production deploy — and your binary still has zero runtime dependencies.

## Keep reading

- [AGENTS.md → The complete agent-facing contract: discover, manifest, invoke, stream.](../AGENTS.md)
- [Hexagonal guide → The dependency rule, layer responsibilities, layout, and testing strategy.](HEXAGONAL.md)
- [Inside the server → The request lifecycle, panic isolation, keep-alive, and every tuning knob.](reference/internals.md)
- [Backends & capabilities → Locks, isolation, JSON paths, search, live queries — and which ones your store actually has.](reference/backend.md)
