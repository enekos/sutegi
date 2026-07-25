# 003 — concurrency & throughput cluster

## What

Four mechanical Postgres-strong primitives through the seam, each two-sided or
capability-gated:

1. **Row locks** — `QueryBuilder::for_update() / for_share() / skip_locked() /
   nowait()`.
2. **Isolation levels** — `transact_with(Isolation, f)` on `Transactional`.
3. **RETURNING on DML** — `.returning(&cols)` on Update/DeleteBuilder +
   `Backend::update_returning / delete_returning`.
4. **Bulk insert** — `Backend::insert_many(table, cols, rows)`; Postgres
   overrides with wire-native `COPY FROM STDIN`.

## Design

### Row locks

The builder stores the requested clause; **emission happens in
`Backend::select`**, which knows the dialect and caps — `build()` stays
dialect-blind and unchanged for existing callers.

- Postgres (`row_locks: true`, `skip_locked: true`): appends
  `FOR UPDATE|SHARE [SKIP LOCKED|NOWAIT]`.
- SQLite (`row_locks: false`): `for_update`/`for_share` are a documented
  no-op — a SQLite write transaction already holds the whole-database lock,
  strictly coarser than a row lock. `skip_locked`/`nowait` **error**
  (`unsupported`): their entire point is altered semantics under contention,
  which SQLite cannot express — generalizes what `sutegi-queue` hand-rolls.

### Isolation

`Isolation::{ReadCommitted, RepeatableRead, Serializable}`.
`Transactional::run_in_tx_with` (object-safe) + `transact_with` wrapper;
default **errors** with `unsupported("isolation_levels")` — silently running
at a weaker level than asked is a lie. Postgres: `BEGIN ISOLATION LEVEL …`.
SQLite: always serializable by nature; `Serializable` → `BEGIN EXCLUSIVE`,
`RepeatableRead` → `BEGIN IMMEDIATE` (deferred lock upgrades can deadlock-race
under busy_timeout; IMMEDIATE takes the write intent up front),
`ReadCommitted` → plain `BEGIN` (still serializable — stronger than asked is
honest, weaker is not).

### RETURNING

`returning(&["id", "title"])` on both DML builders (idents validated).
`build()` appends the clause itself — both engines speak the same syntax
(SQLite ≥3.35, bundled is newer). Executing needs rows back, so:
`Backend::update_returning(ub)` / `delete_returning(db)` route through
`query` instead of `execute`, gated on `returning_dml` (true on Pg **and**
Db). Calling `execute` with a RETURNING clause on a backend whose driver
discards rows is the caller's bug; the typed helpers are the API.

### Bulk insert

Default `insert_many`: multi-row `INSERT … VALUES (…), (…), …` batches sized
to the placeholder budget (500 rows/statement) — works on any backend, no cap
needed. Postgres override: `Client::copy_in` — `CopyInResponse` → `CopyData`
(text format: tab-separated, `\N` null, `\\ \t \n \r` escaped) → `CopyDone`.
`bulk_copy: true` on Pg only (the cap = native path exists; the method works
everywhere).

Measured once at spec time (not a committed bench): COPY vs row-at-a-time
inserts on 100k rows, target ≥10×.

## Acceptance

- Unit: builder emits/withholds clauses per dialect+caps; skip_locked errors
  on SQLite; returning idents validated; insert_many batches correctly (odd
  remainder, empty rows, param budget).
- SQLite: update/delete returning round-trip; transact_with all three levels.
- Live PG: FOR UPDATE SKIP LOCKED two-session claim test (second session
  skips the locked row); serializable conflict surfaces as an error; COPY
  inserts N rows with nulls/tabs/newlines intact and ≥10× row-at-a-time.
- Caps flipped: Pg row_locks/skip_locked/isolation_levels/returning_dml/
  bulk_copy; Db isolation_levels/returning_dml.
- Gate per constitution.
