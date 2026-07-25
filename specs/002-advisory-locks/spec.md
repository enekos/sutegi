# 002 — advisory locks

## What

Named advisory locks through the `Backend` seam: `try_lock(name)` /
`lock(name, timeout)` returning an RAII `LockGuard`, plus `with_lock(name,
timeout, f)`. Cluster-scoped on Postgres (advisory locks), process-scoped on
SQLite (a named-mutex registry). The missing coordination primitive for the
cross-pod story: singleton jobs, janitors, leader election.

## Design

### API (`sutegi-orm::backend`)

```rust
pub struct LockGuard { /* name + an opaque hold; dropping it releases */ }

pub trait Backend {
    /// Ok(None) = held elsewhere. Err = backend has no advisory_locks cap.
    fn try_lock(&self, name: &str) -> Result<Option<LockGuard>, String>;
    /// Poll try_lock until acquired or timeout (default impl; Pg overrides
    /// with a server-side wait).
    fn lock(&self, name: &str, timeout: Duration) -> Result<Option<LockGuard>, String>;
    /// Acquire, run, release. Ok(None) = never acquired within timeout.
    fn with_lock<T>(&self, name: &str, timeout: Duration, f: …) -> Result<Option<T>, String>;
}
```

`lock_key(name) -> i64` — first 8 bytes of SHA-256(name), big-endian. Public
and documented: an operator can inspect/take the same lock from psql.

### Postgres — dedicated session per held lock

- A lock acquires on a **fresh dedicated connection** (`Client::connect` via
  the pool's `Config`), *not* a pooled one: a long-held leader lock must not
  starve the pool, and the server releasing session locks when the session
  dies makes crash-release automatic — the guard's release *is* dropping the
  connection (no explicit `pg_advisory_unlock` needed, no unlock-failure
  path).
- `try_lock`: `SELECT pg_try_advisory_lock($1)`; false → drop the connection,
  return None.
- `lock`: one dedicated connection, `SET statement_timeout`, blocking
  `SELECT pg_advisory_lock($1)`; cancellation (57014) → None. Timeout reset
  after acquire.
- `Tx::try_lock` uses `pg_try_advisory_xact_lock`: released at COMMIT/ROLLBACK,
  **not** at guard drop — documented on the impl.
- Cost note (documented): a held lock = one PG connection; `with_lock` in a
  tight loop reconnects per call. Locks are for coordination, not hot paths.

### SQLite — process-wide named-mutex registry

- A global registry of `(namespace, name)` entries; namespace =
  `file:<path>` / `mem:<pool-id>` so two `Db` handles on the same file
  contend, unrelated databases in one process don't.
- Guard drop removes the entry. `CapScope::Process` — honest about reach.

### Capabilities

`advisory_locks`: `Cluster` on Pg/Tx, `Process` on Db. Unsupported backends
keep the all-off default; `try_lock` default method returns
`Err(unsupported("advisory_locks", …))`.

### Consumers

Documented patterns in `docs/LOCKS.md` (singleton queue janitor, leader
election, migration mutex) — `with_lock` composes with `sutegi-queue` and
`sutegi-actors` as-is; no new API in those crates.

## Acceptance

- SQLite: same-process exclusion, cross-`Db`-clone exclusion, distinct-file
  isolation, guard-drop release, `with_lock` timeout → `Ok(None)`.
- Live PG: two sessions mutually exclude; guard drop (socket close) releases;
  blocking `lock` acquires within timeout when the holder releases, times out
  (None) when it doesn't; xact-scoped lock releases at commit.
- Gate per constitution.
