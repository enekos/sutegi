# Advisory locks

Named locks through the `Backend` seam — the coordination primitive for
"exactly one of us": singleton jobs, janitors, leader election, migration
mutexes.

```rust
use std::time::Duration;

// Try once; None = someone else holds it.
if let Some(_guard) = db.try_lock("nightly-report")? {
    run_report(&db)?;
} // dropping the guard releases

// Wait up to 5s, then give up.
let guard = db.lock("reindex", Duration::from_secs(5))?;

// The singleton-job shape: at most one pod runs f.
db.with_lock("janitor", Duration::ZERO, || sweep(&db))?; // Ok(None) = another pod ran it
```

## Scope — check `capabilities().advisory_locks`

| backend | scope | mechanism |
|---------|-------|-----------|
| Postgres | `cluster` — every pod sharing the database | `pg_try_advisory_lock` on a dedicated session |
| SQLite | `process` — every `Db` handle on the same file in one OS process | in-process registry |
| other | `none` — `try_lock` returns `Err(unsupported…)` | — |

The name is hashed to Postgres's 64-bit key space with
`sutegi_orm::lock_key(name)` (first 8 bytes of SHA-256, big-endian) — an
operator can inspect or take the same lock from psql:
`SELECT pg_try_advisory_lock(<key>)`, `SELECT * FROM pg_locks WHERE locktype =
'advisory'`.

## Semantics worth knowing

- **A held Postgres lock is one dedicated connection** (not from the pool — a
  leader lock held for the process lifetime can't starve request traffic).
  Release = closing that session, which is also why a crashed/OOM-killed
  holder releases automatically: the server drops session locks when the
  session dies. Don't take thousands of concurrent locks; they're for
  coordination, not hot paths.
- **`with_lock` reconnects per call** on Postgres. Fine for a janitor loop
  with a poll interval; wrong inside a request handler's hot path.
- **Inside a Postgres transaction** (`transact`/`transaction`), `try_lock`
  takes a *transaction-scoped* lock (`pg_try_advisory_xact_lock`): it releases
  at COMMIT/ROLLBACK, **not** at guard drop.
- **SQLite locks don't reach across processes.** Two OS processes on the same
  SQLite file do not contend — the capability says `process`, believe it. For
  cross-process coordination use the Postgres backend.

## Patterns

**Leader election** — hold the lock for the process lifetime; followers retry:

```rust
std::thread::spawn(move || loop {
    if let Ok(Some(_leader)) = db.try_lock("scheduler-leader") {
        run_scheduler(&db); // returns only if the scheduler stops
    }
    std::thread::sleep(Duration::from_secs(5)); // follower: retry
});
```

**Queue janitor on exactly one pod** — composes with `sutegi-queue` as-is:

```rust
loop {
    db.with_lock("queue-janitor", Duration::ZERO, || queue.requeue_stale())?;
    std::thread::sleep(Duration::from_secs(30));
}
```

**Migration mutex** — serialize deploy-time `migrate:run` across racing pods:

```rust
let _m = db.lock("migrations", Duration::from_secs(60))?
    .ok_or("another pod is still migrating")?;
apply_pending(&db)?;
```
