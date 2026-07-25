# 006 — reactive queries

## What

`watch(query)` → an initial result set plus **pushed diffs** whenever the
underlying data changes: `Change { added, updated, removed }`. Cluster-scoped
on Postgres (any pod's write wakes every pod's watchers), process-scoped on
SQLite. The primitive that unifies LISTEN/NOTIFY, the event-store wakeups,
and PgPubSub into one shape — and turns channels into live queries.

```rust
let watcher = Watcher::postgres(&pg)?;            // one per process
let sub = watcher.watch(QueryBuilder::table("todos").filter(...), "id")?;
for row in sub.rows() { … }                       // initial result
while let Some(change) = sub.recv_timeout(d) {    // pushed diffs
    hub.broadcast("todos:lobby", change.to_json()); // → channels → browsers
}
```

## Design

### Change detection

- **Postgres** (`live_queries: cluster`): `watch()` idempotently installs a
  shared `_sutegi_notify_change()` plpgsql function + a **statement-level**
  `AFTER INSERT OR UPDATE OR DELETE` trigger per watched table that
  `pg_notify('sutegi_changes', TG_TABLE_NAME)`. A dedicated `Listener`
  session (already in `sutegi-pg`) receives the stream on a background
  thread. NOTIFY is transactional — uncommitted writes never fire. Payload is
  just the table name: v1 invalidation is table-coarse by design, so pks in
  the payload would be dead weight (and the 8KB payload cap stops mattering).
  Triggers/functions are `_sutegi_`-named; introspection reflects tables, so
  drift stays clean.
- **SQLite** (`live_queries: process`): rusqlite's `update_hook` (the
  `hooks` feature — same dep, one flag; constitution-sanctioned), installed
  on every pooled connection — current *and* future — via a hook slot on the
  pool. Hooks only see writes through this process's pool: exactly what
  `process` scope promises. A connection checked out across the instant of
  attach misses the hook until its next checkout (documented; attach before
  serving).

### Invalidation: table-coarse requery-diff (v1)

On a change to table T (debounced 25ms, coalesced across tables), each
watcher on T re-runs its query on the watcher's own backend handle and diffs
by pk against its last snapshot: new pk → `added`, gone → `removed`, same pk
with different row JSON → `updated`. Empty diffs are swallowed (a write that
doesn't move the watched result emits nothing). Row-level invalidation
(trigger payloads with row images) is the v2 escape hatch if requery cost
ever bites; the guardrails make it a non-issue at v1 scale.

### Guardrails

- Debounce window 25ms per wake, coalescing bursts into one requery.
- Watcher cap (default 1024): `watch()` errors past it.
- One watcher registry + one background thread per `Watcher`; drop the
  `Watcher` and the thread shuts down (`ListenerShutdown` on PG); drop a
  `Subscription` and its entry unregisters.

### Placement

`sutegi_orm::watch` — not a new crate. It needs the concrete backends
(hooks on `Db`'s pool, `Listener` from `sutegi-pg`) which are right here
behind the same features; the facade already re-exports orm. Caps flip:
`live_queries` cluster/process.

## Acceptance

- SQLite: watch → insert/update/delete produce correct added/updated/removed;
  irrelevant-table writes emit nothing; unrelated-row writes emit nothing
  (diff swallows); subscription drop unregisters; watcher cap errors.
- Live PG: two independent `Pg` handles (two sessions = the two-pod wire
  path), one watches, the other writes — the diff arrives; a write from a
  third raw session (the "psql from outside" case) also arrives; trigger
  setup idempotent; introspect stays clean.
- Debounce: a burst of N writes produces ≤ a couple of Change events, not N.
- Gate per constitution.
