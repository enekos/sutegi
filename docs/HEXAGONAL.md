# Building clean, hexagonal apps in sutegi

This is the **opinionated** way to structure a non-trivial sutegi app. It's
hexagonal architecture (a.k.a. ports & adapters / clean architecture), adapted
to sutegi's pieces. The worked reference is [`examples/hexagonal`](../examples/hexagonal).

## The one rule

> **Source dependencies point inward.** Outer layers know about inner layers,
> never the reverse.

```
            inbound adapters                       outbound adapters
         (HTTP handlers, AI tools)              (SQLite repo, HTTP client)
                    │  call                            ▲  implement
                    ▼                                  │
            ┌───────────────┐     depends on    ┌──────────────┐
            │  application  │ ─────────────────▶│    ports     │
            │  (use cases)  │                   │   (traits)   │
            └───────┬───────┘                   └──────────────┘
                    │ uses
                    ▼
            ┌───────────────┐
            │    domain     │   pure: entities + rules, no framework
            └───────────────┘
```

If you ever find yourself writing `use sutegi::...` in the domain layer, stop —
that's the rule being broken.

## Layers and where code goes

| Layer | Directory | May depend on | Never touches |
|-------|-----------|---------------|---------------|
| **domain** | `src/domain.rs` | nothing | sutegi, JSON, SQL, HTTP |
| **ports** | `src/ports.rs` | domain, `sutegi::hex::AppResult` | concrete adapters |
| **application** (use cases) | `src/application.rs` | domain, ports, `sutegi::hex` | HTTP/JSON/SQL |
| **adapters** | `src/adapters/` | application, ports, domain, sutegi | — |
| **composition root** | `src/main.rs` | everything (it wires it) | business logic |

- **Domain** — entities and invariants. `Todo::new` rejects empty titles;
  `Todo::complete()` is a domain operation, not an `UPDATE`.
- **Ports** — traits the application needs, in *domain* terms. The outbound
  `TodoRepository` says nothing about SQL. Inbound ports are the use cases.
- **Application** — one struct per use case implementing
  [`sutegi::hex::UseCase`]. It orchestrates the domain over the ports and is
  unit-testable with an in-memory adapter — no server required.
- **Adapters** — all the framework/IO code:
  - *inbound* drive the app: `inbound_http.rs` (routes → use cases),
    `inbound_ai.rs` (a `.tool(...)` closure → the **same** use case).
  - *outbound* are driven by it: `outbound_memory.rs`, `outbound_sqlite.rs`,
    `outbound_postgres.rs` — interchangeable because all implement
    `TodoRepository`. The SQLite and Postgres adapters are near-identical: both
    are written against the `Backend` trait (`Db` vs `Pg`), differing only in
    which handle the composition root hands them.
- **Composition root** (`main.rs`) — the only place that names concrete
  adapters. It picks one (`REPO=memory|sqlite`), injects it, mounts inbound
  adapters, runs.

## What `sutegi::hex` gives you

Enable the `hex` feature. You get three primitives:

- **`AppError`** — a transport-agnostic error (`NotFound`, `Invalid`,
  `Conflict`, `Unauthorized`, `Internal`) with a canonical HTTP mapping
  (`.status()`, `.to_response()` → `{ "error", "kind" }`). The domain and
  application speak `AppError`; adapters translate it.
- **`UseCase`** — `type Input; type Output; fn execute(&self, Input) -> AppResult<Output>`.
  The inbound port every application service implements.
- **`respond` / `respond_created`** — map an `AppResult<T>` to a `Response`
  (`T: IntoJson`), so an HTTP handler is one line:
  ```rust
  .post("/todos", "Create", move |c| {
      let title = c.json().ok()
          .and_then(|b| b.get("title").and_then(Json::as_str).map(str::to_string))
          .unwrap_or_default();
      respond_created(create.execute(title))
  })
  ```
  Handlers take one `&Ctx` and return anything `IntoResponse`; the inbound
  adapter translates the request into a use-case input, then `respond` /
  `respond_created` map the `AppResult` back to a `Response`.

`IntoJson` is the presenter seam — implement it for your domain types **in the
adapter layer** so the domain stays JSON-free.

## Why bother

- **Swap adapters freely.** `REPO=memory` for tests and local dev, `REPO=sqlite`
  single-node, `REPO=postgres` for multi-pod — the use cases don't change. The
  outbound repo is written against sutegi's `Backend` trait, so the same
  `TodoRepository` impl runs over `Db` (SQLite) or `Pg` (Postgres) unchanged;
  the composition root picks one at boot.
- **Two transports, one core.** The HTTP `POST /api/todos` and the AI tool
  `create_todo` both call the *same* `CreateTodo` use case. Business rules live
  in exactly one place, and `/__introspect` + `/__tools` advertise both.
- **Fast, honest tests.** Use cases are tested against the in-memory repo with
  no HTTP and no database (see `application.rs` tests). The domain is tested with
  no dependencies at all.

## Testing strategy

| Test | Exercises | Needs |
|------|-----------|-------|
| domain unit tests | invariants (`Todo::new`) | nothing |
| use-case tests | orchestration + error mapping | in-memory adapter |
| adapter tests | SQL mapping, HTTP shape | a `Backend` (SQLite/Postgres) or a live server |

Push as much logic inward as possible: the more your rules live in the domain
and application, the more of your suite runs in microseconds with no IO.

## Starting your own

```
src/
  domain.rs            # entities + rules (pure)
  ports.rs             # trait TheRepository, etc.
  application.rs       # struct DoTheThing impl UseCase
  adapters/
    mod.rs
    inbound_http.rs    # pub fn register(app, use_cases...) -> App
    inbound_ai.rs      # .tool(...) closure over a use case
    outbound_memory.rs   # impl TheRepository (tests/dev)
    outbound_sqlite.rs   # impl TheRepository over Db  (single-node)
    outbound_postgres.rs # impl TheRepository over Pg  (multi-pod)
  main.rs              # composition root: choose adapter, wire, run
```

Keep `main.rs` boring. If it has an `if` about business rules, that `if` belongs
in a use case or the domain.
