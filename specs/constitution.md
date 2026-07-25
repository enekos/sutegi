# sutegi SDD constitution

Rules every spec under `specs/` inherits. A milestone that can't satisfy one of
these must say so in its spec and get the exception reviewed, not silently
diverge.

## Principles

1. **Two-sided or gated.** Every feature ships a real implementation on both
   `Backend`s — Postgres (`sutegi-pg`) and SQLite (bundled rusqlite) — or
   degrades explicitly through `BackendCaps` with a typed
   "unsupported: <capability>" error. A dialect SQL error reaching the caller
   at runtime is a bug.
2. **The zero-dep core stays zero-dep.** Postgres-side work lives in
   `sutegi-pg`/`sutegi-orm` on `std` only. SQLite-side work rides the existing
   bundled-rusqlite `sqlite` feature; enabling additional rusqlite *feature
   flags* (e.g. `hooks`) is allowed — adding a new dependency is not.
3. **Agent-native.** Every new capability is visible in `/__introspect`
   (`capabilities` block). Operations get an ops_guard-gated `__` surface.
4. **Injection-safe by construction.** New builder surfaces go through
   `valid_identifier`/parameter binding. User-supplied path/search syntax is
   parsed into an AST and compiled per dialect — never spliced into SQL.
5. **Gate per milestone**: `cargo fmt --check`, `clippy -D warnings`, the full
   workspace suite, the live-PG tests (`SUTEGI_PG_TEST_URL` against a local
   PG17), and `make bench-compare` green.
6. **Docs land with the code**: `docs/<FEATURE>.md` + a CHANGELOG entry per
   milestone. Breaking changes are acceptable pre-1.0 and must be called out.

## Milestones (one spec dir each, run in order)

| dir | milestone |
|-----|-----------|
| `001-backend-capabilities` | `BackendCaps` + `/__introspect` caps block + bench baseline re-record |
| `002-advisory-locks` | cluster-wide named locks over PG advisory locks / process mutexes |
| `003-concurrency` | row locks, isolation levels, RETURNING on DML, bulk insert (COPY) |
| `004-json-path` | JSON path queries (jsonb ↔ JSON1) |
| `005-fts-hybrid` | full-text search (tsvector ↔ FTS5) + hybrid search with pgvector |
| `006-reactive` | `db.watch(qb)` → pushed diffs (triggers+NOTIFY ↔ update_hook) |

Later milestones flip the capability bits they implement; `BackendCaps` always
describes what the running build can actually do.
