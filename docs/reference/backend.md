<!-- Generated from landing/src/Docs.svelte. Run: cd landing && pnpm docs:export -->

[← The query builder](queries.md) · [Documentation index](../README.md) · [Locks, isolation, bulk →](concurrency.md)

# Backends & capabilities

This is the key architectural story. The ORM is written against a `Backend` trait, not a concrete engine. `Db` (SQLite, the `sqlite` feature) is the single-node store — one embedded file, zero operations. `Pg` (Postgres, via a **pure-std wire driver**, the `postgres` feature) is the multi-pod store. Both implement `Backend`, so `Model` is written once and every call site is identical.

```rust
// Single node — an embedded file, nothing to run.
let db = Db::open_or_memory("DATABASE_PATH");   // or Db::memory() / Db::open_pool("app.db", 8)

// Multi-pod — the SAME model code, Postgres underneath.
let pg = Pg::from_env(8)?;         // DATABASE_URL, else PGHOST/PGPORT/…; 8 pooled conns
Todo::migrate(&pg).unwrap();
let all = Todo::all_typed(&pg)?;   // an identical call site

// Write your domain against the trait and choose the store at boot:
fn active_count(store: &impl Backend) -> Result<i64, String> {
    store.count(&Todo::query().filter("done", "=", false.into()))
}

// Transactions work on either backend; the closure gets a Backend, so the
// query builder and Model helpers run inside the transaction:
db.transact(|tx| { tx.insert("todos", &[("title", "x".into())], "id")?; Ok(()) })?;
```

The trait is small — five required primitives (`query`, `execute`, `insert`, `upsert`, `migrate`) — and everything else (`select`, `count`, `paginate`, transactions, locks, bulk insert…) is a default method implemented once on top. It is object-safe, so `&dyn Backend` works; the typed helpers are `Self: Sized`-gated.

## Where the engines differ, a bit says so

Some things genuinely cannot be papered over. `Backend::capabilities()` returns a `BackendCaps` descriptor — the honest default is everything off, so a backend never advertises what it has not implemented — and a gated call returns `unsupported: <cap> is not available on <backend>` before any SQL is sent. `App::register_capabilities(db.capabilities())` publishes the block in `/__introspect`, so an agent reads what the store can do instead of finding out from a dialect error.

```rust
// Ask the store what it can do — never find out from a dialect SQL error.
let caps = db.capabilities();
if caps.skip_locked { /* the claim shape is safe to use */ }
if caps.json_contains { /* @> containment is available */ }
match caps.advisory_locks {
    CapScope::Cluster => elect_a_leader(&db)?,   // Postgres
    CapScope::Process => run_locally(&db)?,      // SQLite
    CapScope::None => run_unlocked(&db)?,
}

// Publish the block so an agent can read it too:
App::new("api")
    .register_capabilities(db.capabilities())   // -> "capabilities" in /__introspect
    .state(db)
    .serve()

// Gated features return one uniform error instead of leaking dialect SQL:
// Err("unsupported: json_contains is not available on sqlite")
```

| Capability | SQLite | Postgres |
| --- | --- | --- |
| `advisory_locks` | process | cluster |
| `live_queries` | process | cluster |
| `isolation_levels` | yes | yes |
| `returning_dml` | yes | yes |
| `json_path` | yes | yes |
| `fts` | yes | yes |
| `row_locks` / `skip_locked` | no | yes |
| `bulk_copy` | no | yes |
| `json_contains` | no | yes |
| `listen_notify` | no | yes |
| `vector` | no | yes |

A capability describes the *framework* surface, not the underlying C library: bundled SQLite ships JSON1, FTS5 and `RETURNING`, but a bit stays off until sutegi exposes the feature through the builder. Pick the backend for the deployment: SQLite for local dev, edge, and single-node; Postgres when you scale to many pods or need cluster-wide coordination.

---

[← The query builder](queries.md) · [Documentation index](../README.md) · [Locks, isolation, bulk →](concurrency.md)
