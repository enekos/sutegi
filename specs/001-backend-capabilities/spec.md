# 001 — backend capabilities

## What

A `BackendCaps` descriptor on the `Backend` trait, surfaced in
`/__introspect`, so callers — human or agent — can read what the store behind
an app can actually do before trying it. Plus two gate chores: fix the flaky
live-PG events test (done: quadratic retry backoff) and re-record the stale
`benches/baselines/local.json` so `bench-compare` is green on an unmodified
tree again.

## Why

Milestones 002–006 ship Postgres-strong features that SQLite either matches
(FTS5, JSON1, RETURNING), approximates (process-scoped locks, whole-db
transactions), or lacks (LISTEN/NOTIFY, pgvector, `@>` containment). Without a
capability descriptor each of those becomes either a lying abstraction or a
runtime dialect error. Caps land first; features flip their bits as they ship.

## Design

`sutegi-orm/src/backend.rs`:

```rust
#[derive(Clone, Debug, PartialEq)]
pub struct BackendCaps {
    pub backend: &'static str, // "sqlite" | "postgres" | impl-defined
    // coordination
    pub advisory_locks: CapScope, // None | Process | Cluster (002)
    pub row_locks: bool,           // FOR UPDATE/SHARE (003)
    pub skip_locked: bool,         // (003)
    pub isolation_levels: bool,    // transaction_with (003)
    // dml
    pub returning_dml: bool,       // RETURNING on update/delete (003)
    pub bulk_copy: bool,           // native bulk path, e.g. COPY (003)
    // documents & search
    pub json_path: bool,           // where_json / select_json (004)
    pub json_contains: bool,       // @> containment (004)
    pub fts: bool,                 // full-text search (005)
    // realtime & vectors
    pub listen_notify: bool,       // true on Pg today
    pub vector: bool,              // pgvector columns; true on Pg today
    pub live_queries: CapScope,    // watch() scope (006)
}
```

- `BackendCaps::none(name)` — everything off; the default
  `Backend::capabilities()` returns `none("unknown")` so third-party backends
  are honest by default.
- `to_json()` — stable, sorted keys; scopes serialize as
  `"none" | "process" | "cluster"`.
- Overrides in this milestone reflect only what is **already shipped**:
  - `Pg`/`Tx`: `listen_notify: true`, `vector: true`, rest off.
  - `Db` (SQLite): everything off (JSON1/FTS5/RETURNING exist in the bundled
    lib but have no framework surface yet — the cap describes the framework,
    not the C library).
- `sutegi-web`: `App::register_capabilities(caps)` (mirrors
  `register_model`); when set, `introspection()` includes a `capabilities`
  key. The `orm` feature's docs point at
  `app.register_capabilities(db.capabilities())`.
- Typed error helper for later milestones:
  `Unsupported(capability)`-style `String` constructor
  (`caps_error("advisory_locks", backend)`) so gate errors are uniform.

## Out of scope

Flipping any capability on for a feature that doesn't exist yet; auto-wiring
caps from `.state(db)` (state is type-erased at registration time — explicit
registration keeps it simple).

## Acceptance

- `Backend::capabilities()` default + overrides unit-tested on both backends.
- `/__introspect` includes the block when registered; a doc/introspection test
  asserts the JSON shape and stable key order.
- Events live-PG test 10/10 locally (was ~1-in-3 flaky).
- `make bench-compare` exits 0 on the unmodified branch after re-record.
- Gate: fmt, clippy `-D warnings`, workspace suite, live-PG suite.
