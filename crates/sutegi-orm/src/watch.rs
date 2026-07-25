//! Reactive queries: `watch(query)` → an initial result set plus **pushed
//! diffs** whenever the underlying data changes.
//!
//! ```ignore
//! let watcher = Watcher::postgres(&pg)?;                  // one per process
//! let sub = watcher.watch(QueryBuilder::table("todos"), "id")?;
//! for row in sub.rows() { /* initial result */ }
//! while let Some(change) = sub.recv_timeout(dur) {        // pushed diffs
//!     hub.broadcast("todos:lobby", change.to_json());     // → channels
//! }
//! ```
//!
//! Change detection is two-sided (`capabilities().live_queries`):
//! **Postgres = cluster** — a statement-level trigger per watched table
//! `pg_notify`s a shared channel, received on a dedicated [`Listener`]
//! session, so *any pod's* committed write wakes *every pod's* watchers.
//! **SQLite = process** — rusqlite's `update_hook` on every pooled
//! connection; writes from other processes on the same file are invisible
//! (believe the capability). A connection checked out across the instant of
//! attach picks the hook up on its next checkout — attach the watcher before
//! serving traffic.
//!
//! Invalidation is **table-coarse requery-diff** (v1): on a change to a
//! watched table (debounced, coalesced), each watcher re-runs its query and
//! diffs by pk — new pk → `added`, gone → `removed`, same pk with different
//! row → `updated`. A write that doesn't move the watched result emits
//! nothing. Row-level trigger payloads are the v2 escape hatch if requery
//! cost ever bites.
//!
//! [`Listener`]: sutegi_pg::Listener

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::backend::Backend;
use crate::builder::QueryBuilder;
use sutegi_json::Json;

/// One pushed diff for a watched query.
#[derive(Clone, Debug, PartialEq)]
pub struct Change {
    /// The watched table the change came from.
    pub table: String,
    /// Rows whose pk newly entered the result.
    pub added: Vec<Json>,
    /// Rows whose pk stayed but whose content changed.
    pub updated: Vec<Json>,
    /// Rows (as last seen) whose pk left the result.
    pub removed: Vec<Json>,
}

impl Change {
    /// The wire form: `{table, added, updated, removed}` — ready to
    /// broadcast on a channel topic.
    pub fn to_json(&self) -> Json {
        Json::obj(vec![
            ("table", Json::str(self.table.clone())),
            ("added", Json::arr(self.added.clone())),
            ("updated", Json::arr(self.updated.clone())),
            ("removed", Json::arr(self.removed.clone())),
        ])
    }
}

/// Bursts within this window coalesce into one requery.
const DEBOUNCE: Duration = Duration::from_millis(25);

/// A registered watched query and its last-seen state.
struct Sub {
    table: String,
    qb: QueryBuilder,
    pk: String,
    /// pk → row as last delivered.
    snapshot: HashMap<i64, Json>,
    out: Sender<Change>,
}

/// Shared watcher state — non-generic so [`Subscription`] can hold it
/// without knowing the backend.
struct Registry {
    subs: Mutex<HashMap<u64, Sub>>,
    next_id: AtomicU64,
    /// Watcher cap: `watch()` errors past it (requery cost is per-watcher).
    max_subs: usize,
}

enum Feed {
    Changed(String),
    Shutdown,
}

/// Idempotently make sure change detection covers a table (Postgres installs
/// the notify trigger; SQLite needs nothing per-table).
type EnsureTable<B> = Box<dyn Fn(&B, &str) -> Result<(), String> + Send + Sync>;

/// The change-feed fan-out for one backend handle. One per process is the
/// intended shape; every watched query is a [`Subscription`] on it.
pub struct Watcher<B: Backend + Clone + Send + Sync + 'static> {
    backend: B,
    registry: Arc<Registry>,
    feed: Sender<Feed>,
    ensure_table: EnsureTable<B>,
    /// Interrupts a blocked Postgres `Listener::recv` on drop.
    unblock: Option<Box<dyn Fn() + Send + Sync>>,
}

/// A live watched query: the initial rows plus a stream of [`Change`]s.
/// Dropping it unregisters the watch.
pub struct Subscription {
    id: u64,
    registry: Arc<Registry>,
    rows: Vec<Json>,
    rx: Receiver<Change>,
}

impl Subscription {
    /// The query's result at watch time.
    pub fn rows(&self) -> &[Json] {
        &self.rows
    }

    /// Wait up to `timeout` for the next change; `None` on timeout or if the
    /// watcher is gone.
    pub fn recv_timeout(&self, timeout: Duration) -> Option<Change> {
        match self.rx.recv_timeout(timeout) {
            Ok(c) => Some(c),
            Err(RecvTimeoutError::Timeout) | Err(RecvTimeoutError::Disconnected) => None,
        }
    }

    /// Non-blocking poll for the next change.
    pub fn try_recv(&self) -> Option<Change> {
        self.rx.try_recv().ok()
    }
}

impl Drop for Subscription {
    fn drop(&mut self) {
        self.registry.subs.lock().unwrap().remove(&self.id);
    }
}

impl<B: Backend + Clone + Send + Sync + 'static> Watcher<B> {
    /// Assemble a watcher around a change feed: spawn the worker thread that
    /// debounces `rx` and requery-diffs affected subscriptions.
    fn assemble(
        backend: B,
        feed: Sender<Feed>,
        rx: Receiver<Feed>,
        ensure_table: EnsureTable<B>,
        unblock: Option<Box<dyn Fn() + Send + Sync>>,
    ) -> Watcher<B> {
        let registry = Arc::new(Registry {
            subs: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(1),
            max_subs: 1024,
        });
        let worker_registry = registry.clone();
        let worker_backend = backend.clone();
        std::thread::spawn(move || worker(worker_backend, worker_registry, rx));
        Watcher {
            backend,
            registry,
            feed,
            ensure_table,
            unblock,
        }
    }

    /// Watch a query: returns its current rows plus a [`Change`] stream.
    /// `pk` names the primary-key column the diff keys on — it must be part
    /// of the selected columns.
    pub fn watch(&self, qb: QueryBuilder, pk: &str) -> Result<Subscription, String> {
        let table = qb.table_name().to_string();
        (self.ensure_table)(&self.backend, &table)?;
        {
            let subs = self.registry.subs.lock().unwrap();
            if subs.len() >= self.registry.max_subs {
                return Err(format!(
                    "watch: watcher cap reached ({} live subscriptions)",
                    self.registry.max_subs
                ));
            }
        }
        let rows = self.backend.select(&qb)?;
        let snapshot = snapshot_of(&rows, pk)?;
        let (out, rx) = channel();
        let id = self.registry.next_id.fetch_add(1, Ordering::Relaxed);
        self.registry.subs.lock().unwrap().insert(
            id,
            Sub {
                table,
                qb,
                pk: pk.to_string(),
                snapshot,
                out,
            },
        );
        Ok(Subscription {
            id,
            registry: self.registry.clone(),
            rows,
            rx,
        })
    }
}

impl<B: Backend + Clone + Send + Sync + 'static> Drop for Watcher<B> {
    fn drop(&mut self) {
        let _ = self.feed.send(Feed::Shutdown);
        if let Some(unblock) = &self.unblock {
            unblock();
        }
    }
}

/// Key `rows` by their pk column. A row without the pk is an error — the
/// diff cannot key on it (select the pk column in a watched query).
fn snapshot_of(rows: &[Json], pk: &str) -> Result<HashMap<i64, Json>, String> {
    rows.iter()
        .map(|r| {
            r.get(pk)
                .and_then(Json::as_i64)
                .map(|id| (id, r.clone()))
                .ok_or_else(|| format!("watch: row is missing pk column {pk:?}"))
        })
        .collect()
}

/// The requery-diff worker: debounce the feed, re-run affected queries, and
/// push non-empty diffs. Ends on `Shutdown` or when every feed sender hangs
/// up.
fn worker<B: Backend>(backend: B, registry: Arc<Registry>, rx: Receiver<Feed>) {
    loop {
        // Block for the first signal…
        let mut tables: HashSet<String> = HashSet::new();
        match rx.recv() {
            Ok(Feed::Changed(t)) => {
                tables.insert(t);
            }
            Ok(Feed::Shutdown) | Err(_) => return,
        }
        // …then coalesce the burst.
        let deadline = Instant::now() + DEBOUNCE;
        loop {
            let now = Instant::now();
            if now >= deadline {
                break;
            }
            match rx.recv_timeout(deadline - now) {
                Ok(Feed::Changed(t)) => {
                    tables.insert(t);
                }
                Ok(Feed::Shutdown) => return,
                Err(RecvTimeoutError::Timeout) => break,
                Err(RecvTimeoutError::Disconnected) => return,
            }
        }

        let mut dead: Vec<u64> = Vec::new();
        let mut subs = registry.subs.lock().unwrap();
        for (id, sub) in subs.iter_mut() {
            if !tables.contains(&sub.table) {
                continue;
            }
            // A failed requery skips this round rather than killing the
            // watch — the next change retries.
            let Ok(rows) = backend.select(&sub.qb) else {
                continue;
            };
            let Ok(next) = snapshot_of(&rows, &sub.pk) else {
                continue;
            };
            let change = diff(&sub.table, &sub.snapshot, &next);
            sub.snapshot = next;
            if let Some(change) = change {
                if sub.out.send(change).is_err() {
                    dead.push(*id); // subscriber dropped mid-flight
                }
            }
        }
        for id in dead {
            subs.remove(&id);
        }
    }
}

/// Diff two snapshots by pk; `None` when nothing moved.
fn diff(table: &str, prev: &HashMap<i64, Json>, next: &HashMap<i64, Json>) -> Option<Change> {
    let mut change = Change {
        table: table.to_string(),
        added: Vec::new(),
        updated: Vec::new(),
        removed: Vec::new(),
    };
    for (id, row) in next {
        match prev.get(id) {
            None => change.added.push(row.clone()),
            Some(old) if old != row => change.updated.push(row.clone()),
            Some(_) => {}
        }
    }
    for (id, row) in prev {
        if !next.contains_key(id) {
            change.removed.push(row.clone());
        }
    }
    if change.added.is_empty() && change.updated.is_empty() && change.removed.is_empty() {
        None
    } else {
        Some(change)
    }
}

#[cfg(feature = "sqlite")]
impl Watcher<crate::db::Db> {
    /// Watch through the SQLite backend: `update_hook` on every pooled
    /// connection feeds the worker. Process scope — see the module docs.
    pub fn sqlite(db: &crate::db::Db) -> Watcher<crate::db::Db> {
        let (tx, rx) = channel();
        let hook_tx = tx.clone();
        db.set_change_hook(Arc::new(move |table: &str| {
            let _ = hook_tx.send(Feed::Changed(table.to_string()));
        }));
        Watcher::assemble(db.clone(), tx, rx, Box::new(|_, _| Ok(())), None)
    }
}

#[cfg(feature = "postgres")]
impl Watcher<crate::pg::Pg> {
    /// The shared NOTIFY channel every watched table reports on.
    const CHANNEL: &'static str = "sutegi_changes";

    /// Watch through the Postgres backend: a dedicated LISTEN session on a
    /// pump thread feeds the worker, so any pod's committed write wakes this
    /// process's watchers. Cluster scope.
    pub fn postgres(pg: &crate::pg::Pg) -> Result<Watcher<crate::pg::Pg>, String> {
        let mut listener = sutegi_pg::Listener::connect(pg.pool().config())?;
        listener.listen(Self::CHANNEL)?;
        let shutdown = listener.shutdown_handle()?;
        let (tx, rx) = channel();
        let pump_tx = tx.clone();
        std::thread::spawn(move || {
            // recv() errors when the connection dies — including via the
            // shutdown handle on Watcher drop — which ends the pump.
            while let Ok(n) = listener.recv() {
                if pump_tx.send(Feed::Changed(n.payload)).is_err() {
                    return;
                }
            }
        });
        Ok(Watcher::assemble(
            pg.clone(),
            tx,
            rx,
            Box::new(ensure_pg_trigger),
            Some(Box::new(move || shutdown.shutdown())),
        ))
    }
}

/// Idempotently install the shared notify function + a statement-level
/// trigger on `table`. `_sutegi_`-named; triggers/functions are invisible to
/// schema introspection, so `migrate:drift` stays clean.
#[cfg(feature = "postgres")]
fn ensure_pg_trigger(pg: &crate::pg::Pg, table: &str) -> Result<(), String> {
    if !crate::builder::valid_identifier(table) {
        return Err(format!("invalid identifier: {table:?}"));
    }
    pg.pool().batch(
        "CREATE OR REPLACE FUNCTION _sutegi_notify_change() RETURNS trigger \
         LANGUAGE plpgsql AS $$ \
         BEGIN PERFORM pg_notify('sutegi_changes', TG_TABLE_NAME); RETURN NULL; END $$",
    )?;
    let installed = pg
        .query_one(
            "SELECT 1 AS one FROM pg_trigger WHERE tgname = ?",
            &[crate::value::Value::Text(format!("_sutegi_watch_{table}"))],
        )?
        .is_some();
    if !installed {
        // Two watchers racing the CREATE is fine: 42710 = already there.
        match pg.pool().batch(&format!(
            "CREATE TRIGGER _sutegi_watch_{table} \
             AFTER INSERT OR UPDATE OR DELETE ON {table} \
             FOR EACH STATEMENT EXECUTE FUNCTION _sutegi_notify_change()"
        )) {
            Ok(()) => {}
            Err(e) if e.contains("42710") => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diff_classifies_added_updated_removed() {
        let row = |id: i64, v: &str| Json::obj(vec![("id", Json::int(id)), ("v", Json::str(v))]);
        let prev: HashMap<i64, Json> = [(1, row(1, "a")), (2, row(2, "b"))].into_iter().collect();
        let next: HashMap<i64, Json> = [(2, row(2, "B")), (3, row(3, "c"))].into_iter().collect();
        let change = diff("t", &prev, &next).unwrap();
        assert_eq!(change.added, vec![row(3, "c")]);
        assert_eq!(change.updated, vec![row(2, "B")]);
        assert_eq!(change.removed, vec![row(1, "a")]);
        // Identical snapshots → no change at all.
        assert!(diff("t", &next, &next).is_none());
    }

    #[cfg(feature = "sqlite")]
    fn watched_db() -> (crate::db::Db, Watcher<crate::db::Db>) {
        use crate::value::{ColType, Column, TableSchema};
        let db = crate::db::Db::memory().unwrap();
        for table in ["todos", "other"] {
            db.migrate(
                &TableSchema::new(table)
                    .column(Column::new("id", ColType::Integer).primary())
                    .column(Column::new("title", ColType::Text))
                    .column(Column::new("done", ColType::Boolean)),
            )
            .unwrap();
        }
        let watcher = Watcher::sqlite(&db);
        (db, watcher)
    }

    #[cfg(feature = "sqlite")]
    #[test]
    fn sqlite_watch_pushes_added_updated_removed() {
        use crate::backend::Backend;
        use crate::value::Value;
        let (db, watcher) = watched_db();
        let sub = watcher.watch(QueryBuilder::table("todos"), "id").unwrap();
        assert!(sub.rows().is_empty());

        let id = db
            .insert(
                "todos",
                &[
                    ("title", Value::Text("a".into())),
                    ("done", Value::Bool(false)),
                ],
                "id",
            )
            .unwrap();
        let change = sub.recv_timeout(Duration::from_secs(2)).expect("added");
        assert_eq!(change.added.len(), 1);
        assert!(change.updated.is_empty() && change.removed.is_empty());

        db.execute(
            "UPDATE todos SET done = ? WHERE id = ?",
            &[Value::Bool(true), Value::Int(id)],
        )
        .unwrap();
        let change = sub.recv_timeout(Duration::from_secs(2)).expect("updated");
        assert_eq!(change.updated.len(), 1);

        db.execute("DELETE FROM todos WHERE id = ?", &[Value::Int(id)])
            .unwrap();
        let change = sub.recv_timeout(Duration::from_secs(2)).expect("removed");
        assert_eq!(change.removed.len(), 1);
        assert_eq!(
            change.to_json().get("table").and_then(Json::as_str),
            Some("todos")
        );
    }

    #[cfg(feature = "sqlite")]
    #[test]
    fn sqlite_watch_filters_noise() {
        use crate::backend::Backend;
        use crate::value::Value;
        let (db, watcher) = watched_db();
        // Watch only undone todos.
        let sub = watcher
            .watch(
                QueryBuilder::table("todos").filter("done", "=", Value::Bool(false)),
                "id",
            )
            .unwrap();

        // A write to another table emits nothing.
        db.insert(
            "other",
            &[
                ("title", Value::Text("x".into())),
                ("done", Value::Bool(false)),
            ],
            "id",
        )
        .unwrap();
        assert!(sub.recv_timeout(Duration::from_millis(300)).is_none());

        // A row outside the watched predicate emits nothing either — the
        // requery runs, the diff is empty, the diff is swallowed.
        db.insert(
            "todos",
            &[
                ("title", Value::Text("done already".into())),
                ("done", Value::Bool(true)),
            ],
            "id",
        )
        .unwrap();
        assert!(sub.recv_timeout(Duration::from_millis(300)).is_none());

        // A matching row lands as one change.
        db.insert(
            "todos",
            &[
                ("title", Value::Text("live".into())),
                ("done", Value::Bool(false)),
            ],
            "id",
        )
        .unwrap();
        let change = sub.recv_timeout(Duration::from_secs(2)).expect("added");
        assert_eq!(change.added.len(), 1);
    }

    #[cfg(feature = "sqlite")]
    #[test]
    fn sqlite_watch_debounces_bursts_and_unregisters_on_drop() {
        use crate::backend::Backend;
        use crate::value::Value;
        let (db, watcher) = watched_db();
        let sub = watcher.watch(QueryBuilder::table("todos"), "id").unwrap();

        // A burst of writes coalesces into far fewer Change events than
        // writes (25ms debounce): typically 1, allow up to 3 on a slow box.
        for i in 0..20 {
            db.insert(
                "todos",
                &[
                    ("title", Value::Text(format!("t{i}"))),
                    ("done", Value::Bool(false)),
                ],
                "id",
            )
            .unwrap();
        }
        std::thread::sleep(Duration::from_millis(200));
        let mut events = 0;
        let mut seen = 0;
        while let Some(c) = sub.try_recv() {
            events += 1;
            seen += c.added.len();
        }
        assert_eq!(seen, 20, "no change may be lost");
        assert!(events <= 3, "expected coalescing, got {events} events");

        // Dropping the subscription unregisters it.
        drop(sub);
        assert!(watcher.registry.subs.lock().unwrap().is_empty());
    }

    #[cfg(feature = "sqlite")]
    #[test]
    fn watch_requires_the_pk_in_the_selection() {
        use crate::backend::Backend;
        use crate::value::Value;
        let (db, watcher) = watched_db();
        db.insert(
            "todos",
            &[
                ("title", Value::Text("x".into())),
                ("done", Value::Bool(false)),
            ],
            "id",
        )
        .unwrap();
        // The diff keys on the pk, so a selection without it is an error.
        let err = watcher
            .watch(QueryBuilder::table("todos").select(&["title"]), "id")
            .map(|_| ())
            .unwrap_err();
        assert!(err.contains("missing pk"), "{err}");
    }
}
