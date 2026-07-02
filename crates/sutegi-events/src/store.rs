//! The event store: append-only writes with optimistic concurrency, stream
//! reads, and aggregate folding, over any ORM backend.

use std::time::{SystemTime, UNIX_EPOCH};

use sutegi_json::Json;
use sutegi_orm::{row, Backend, Transactional, Value};

use crate::Aggregate;

/// How often a racing append is re-attempted before giving up. Each retry does
/// real work (a fresh transaction), so this is a contention ceiling, not a spin.
const MAX_APPEND_RETRIES: usize = 16;

/// One event as stored: its global log `position`, per-stream `version`, and
/// the decoded JSON `payload`/`meta`.
#[derive(Clone, Debug, PartialEq)]
pub struct StoredEvent {
    /// Global, gap-free log position (1-based). Total order across streams.
    pub position: i64,
    /// The stream this event belongs to, e.g. `account:42`.
    pub stream: String,
    /// Per-stream sequence (1-based).
    pub version: i64,
    /// The event name, e.g. `deposited`.
    pub name: String,
    pub payload: Json,
    /// Correlation ids, actor, … — [`Json::Null`] when none was attached.
    pub meta: Json,
    /// Epoch milliseconds at append time.
    pub created_at: i64,
}

impl StoredEvent {
    /// The event as a JSON object — for history endpoints and agent tools.
    pub fn to_json(&self) -> Json {
        Json::obj(vec![
            ("position", Json::int(self.position)),
            ("stream", Json::str(&self.stream)),
            ("version", Json::int(self.version)),
            ("name", Json::str(&self.name)),
            ("payload", self.payload.clone()),
            ("meta", self.meta.clone()),
            ("created_at", Json::int(self.created_at)),
        ])
    }
}

/// An event to append: a name, a JSON payload, and optional metadata. Build
/// with [`event`], attach metadata with [`NewEvent::meta`].
#[derive(Clone, Debug)]
pub struct NewEvent {
    pub name: String,
    pub payload: Json,
    pub meta: Json,
}

impl NewEvent {
    /// Attach metadata (correlation id, actor, …) to the event.
    pub fn meta(mut self, meta: Json) -> NewEvent {
        self.meta = meta;
        self
    }
}

/// Shorthand constructor for a [`NewEvent`] with no metadata.
pub fn event(name: impl Into<String>, payload: Json) -> NewEvent {
    NewEvent {
        name: name.into(),
        payload,
        meta: Json::Null,
    }
}

/// The optimistic-concurrency expectation for an append: what the stream's
/// current version must be for the write to go through.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Expected {
    /// Append regardless of the stream's current version.
    Any,
    /// The stream must not exist yet (version 0) — creation semantics.
    NoStream,
    /// The stream must be exactly at this version — the value returned by the
    /// [`EventStore::load`] / [`EventStore::version`] that informed the decision.
    Version(i64),
}

impl Expected {
    fn matches(self, current: i64) -> bool {
        match self {
            Expected::Any => true,
            Expected::NoStream => current == 0,
            Expected::Version(v) => current == v,
        }
    }
}

/// Why an append failed: a real concurrency [`Conflict`](EventError::Conflict)
/// (someone else wrote the stream first — reload and re-decide), or a store
/// error.
#[derive(Clone, Debug, PartialEq)]
pub enum EventError {
    Conflict {
        stream: String,
        expected: Expected,
        actual: i64,
    },
    Store(String),
}

impl std::fmt::Display for EventError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EventError::Conflict {
                stream,
                expected,
                actual,
            } => write!(
                f,
                "version conflict on '{stream}': expected {expected:?}, stream is at {actual}"
            ),
            EventError::Store(e) => write!(f, "{e}"),
        }
    }
}

impl From<EventError> for String {
    fn from(e: EventError) -> String {
        e.to_string()
    }
}

/// The append-only event store over any [`Backend`]. Reads work on a plain
/// backend; [`append`](EventStore::append) additionally needs
/// [`Transactional`] (SQLite's `Db` and Postgres' `Pg` both are).
#[derive(Clone)]
pub struct EventStore<B: Backend> {
    backend: B,
}

impl<B: Backend> EventStore<B> {
    pub fn new(backend: B) -> EventStore<B> {
        EventStore { backend }
    }

    /// The underlying backend, for mixing event and relational access.
    pub fn backend(&self) -> &B {
        &self.backend
    }

    /// Create the `sutegi_events` + `sutegi_projections` tables if missing.
    /// Safe to call from every pod on boot (concurrent `CREATE IF NOT EXISTS`
    /// catalog races are treated as success, like the queue's migrate).
    pub fn migrate(&self) -> Result<(), String> {
        ensure_tables(&self.backend)
    }

    /// A stream's events after `after_version` (pass 0 for the whole stream),
    /// in version order.
    pub fn read_stream(
        &self,
        stream: &str,
        after_version: i64,
    ) -> Result<Vec<StoredEvent>, String> {
        let rows = self.backend.query(
            "SELECT * FROM sutegi_events WHERE stream = ? AND version > ? ORDER BY version",
            &[Value::Text(stream.to_string()), Value::Int(after_version)],
        )?;
        rows.iter().map(decode_event).collect()
    }

    /// Up to `limit` events after global `after_position`, in log order — the
    /// paging read projections use.
    pub fn read_all(&self, after_position: i64, limit: i64) -> Result<Vec<StoredEvent>, String> {
        let rows = self.backend.query(
            "SELECT * FROM sutegi_events WHERE position > ? ORDER BY position LIMIT ?",
            &[Value::Int(after_position), Value::Int(limit.max(1))],
        )?;
        rows.iter().map(decode_event).collect()
    }

    /// A stream's current version (0 if the stream doesn't exist).
    pub fn version(&self, stream: &str) -> Result<i64, String> {
        stream_version(&self.backend, stream)
    }

    /// The global head position (0 if the log is empty).
    pub fn head(&self) -> Result<i64, String> {
        head_position(&self.backend)
    }

    /// Fold a stream into an aggregate: `(state, version)`. Version 0 with a
    /// `Default` state means the stream doesn't exist yet.
    pub fn load<A: Aggregate>(&self, stream: &str) -> Result<(A, i64), String> {
        let events = self.read_stream(stream, 0)?;
        let version = events.last().map(|e| e.version).unwrap_or(0);
        let mut aggregate = A::default();
        for e in &events {
            aggregate.apply(e);
        }
        Ok((aggregate, version))
    }

    /// Store totals plus per-projection positions and lag, as JSON — wire it
    /// into a `/__events` route.
    pub fn stats(&self) -> Result<Json, String> {
        let head = self.head()?;
        let totals = self
            .backend
            .query_one(
                "SELECT COUNT(*) AS events, COUNT(DISTINCT stream) AS streams FROM sutegi_events",
                &[],
            )?
            .unwrap_or(Json::Null);
        let n = |k: &str| Json::int(totals.get(k).and_then(Json::as_i64).unwrap_or(0));
        let projections = self
            .backend
            .query(
                "SELECT name, position, updated_at FROM sutegi_projections ORDER BY name",
                &[],
            )?
            .iter()
            .map(|r| {
                let position = r.get("position").and_then(Json::as_i64).unwrap_or(0);
                Json::obj(vec![
                    (
                        "name",
                        Json::str(r.get("name").and_then(Json::as_str).unwrap_or("")),
                    ),
                    ("position", Json::int(position)),
                    ("lag", Json::int(head - position)),
                ])
            })
            .collect();
        Ok(Json::obj(vec![
            ("head", Json::int(head)),
            ("events", n("events")),
            ("streams", n("streams")),
            ("projections", Json::Arr(projections)),
        ]))
    }
}

impl<B: Transactional> EventStore<B> {
    /// Append `events` to `stream` if its current version matches `expected`;
    /// returns the stream's new version. Runs in its own transaction.
    ///
    /// Two kinds of races are told apart: a **version conflict** (the stream
    /// moved past `expected` — someone else's decision won) comes back as
    /// [`EventError::Conflict`] for the caller to reload and re-decide; a
    /// **position race** (a concurrent append to *any* stream claimed the same
    /// global position) is retried internally with fresh positions.
    pub fn append(
        &self,
        stream: &str,
        expected: Expected,
        events: &[NewEvent],
    ) -> Result<i64, EventError> {
        let mut attempt = 0;
        loop {
            let outcome = self.backend.transact(|tx| {
                match append_tx(tx, stream, expected, events) {
                    Ok(version) => Ok(Ok(version)),
                    // Nothing was written before the version check, so letting
                    // the empty transaction commit is harmless.
                    Err(conflict @ EventError::Conflict { .. }) => Ok(Err(conflict)),
                    Err(EventError::Store(e)) => Err(e), // roll back
                }
            });
            match outcome {
                Ok(result) => return result,
                Err(e) if is_unique_violation(&e) && attempt + 1 < MAX_APPEND_RETRIES => {
                    attempt += 1;
                }
                Err(e) => return Err(EventError::Store(e)),
            }
        }
    }
}

/// [`EventStore::append`] as a building block inside a transaction **you**
/// own — e.g. appending events atomically with a relational write. No retry:
/// a position race aborts your transaction (the store error will satisfy
/// `is-unique-violation`), and re-running it is the caller's call.
pub fn append_tx(
    tx: &dyn Backend,
    stream: &str,
    expected: Expected,
    events: &[NewEvent],
) -> Result<i64, EventError> {
    let current = stream_version(tx, stream).map_err(EventError::Store)?;
    if !expected.matches(current) {
        return Err(EventError::Conflict {
            stream: stream.to_string(),
            expected,
            actual: current,
        });
    }
    if events.is_empty() {
        return Ok(current);
    }
    let head = head_position(tx).map_err(EventError::Store)?;
    let now = now_millis();
    for (i, e) in events.iter().enumerate() {
        let meta = match &e.meta {
            Json::Null => Value::Null,
            other => Value::Text(other.to_string()),
        };
        tx.execute(
            "INSERT INTO sutegi_events (position, stream, version, name, payload, meta, created_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?)",
            &[
                Value::Int(head + 1 + i as i64),
                Value::Text(stream.to_string()),
                Value::Int(current + 1 + i as i64),
                Value::Text(e.name.clone()),
                Value::Text(e.payload.to_string()),
                meta,
                Value::Int(now),
            ],
        )
        .map_err(EventError::Store)?;
    }
    Ok(current + events.len() as i64)
}

pub(crate) fn ensure_tables(backend: &dyn Backend) -> Result<(), String> {
    // Portable DDL: positions and versions are assigned by the store, so no
    // AUTOINCREMENT/IDENTITY dialect split. UNIQUE (stream, version) *is* the
    // optimistic-concurrency check; the position primary key *is* the global
    // ordering guarantee.
    const TABLES: [&str; 2] = [
        "CREATE TABLE IF NOT EXISTS sutegi_events (\
            position BIGINT PRIMARY KEY, \
            stream TEXT NOT NULL, \
            version BIGINT NOT NULL, \
            name TEXT NOT NULL, \
            payload TEXT NOT NULL, \
            meta TEXT, \
            created_at BIGINT NOT NULL, \
            UNIQUE (stream, version))",
        "CREATE TABLE IF NOT EXISTS sutegi_projections (\
            name TEXT PRIMARY KEY, \
            position BIGINT NOT NULL DEFAULT 0, \
            locked_at BIGINT, \
            updated_at BIGINT NOT NULL DEFAULT 0)",
    ];
    for sql in TABLES {
        match backend.execute(sql, &[]) {
            Ok(_) => {}
            // Concurrent pods racing CREATE IF NOT EXISTS against the Postgres
            // catalog can raise a spurious duplicate error; the table exists
            // either way.
            Err(e) if e.contains("23505") || e.contains("already exists") => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

pub(crate) fn decode_event(row: &Json) -> Result<StoredEvent, String> {
    Ok(StoredEvent {
        position: row::get_i64(row, "position")?,
        stream: row::get_string(row, "stream")?,
        version: row::get_i64(row, "version")?,
        name: row::get_string(row, "name")?,
        payload: row::get_json(row, "payload")?,
        meta: row::opt_json(row, "meta")?.unwrap_or(Json::Null),
        created_at: row::get_i64(row, "created_at")?,
    })
}

fn stream_version(backend: &dyn Backend, stream: &str) -> Result<i64, String> {
    scalar(
        backend,
        "SELECT COALESCE(MAX(version), 0) AS n FROM sutegi_events WHERE stream = ?",
        &[Value::Text(stream.to_string())],
    )
}

fn head_position(backend: &dyn Backend) -> Result<i64, String> {
    scalar(
        backend,
        "SELECT COALESCE(MAX(position), 0) AS n FROM sutegi_events",
        &[],
    )
}

fn scalar(backend: &dyn Backend, sql: &str, params: &[Value]) -> Result<i64, String> {
    Ok(backend
        .query_one(sql, params)?
        .as_ref()
        .and_then(|r| r.get("n"))
        .and_then(Json::as_i64)
        .unwrap_or(0))
}

pub(crate) fn is_unique_violation(e: &str) -> bool {
    e.contains("UNIQUE constraint failed") || e.contains("23505") || e.contains("duplicate key")
}

pub(crate) fn now_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sutegi_orm::db::Db;

    fn store() -> EventStore<Db> {
        let store = EventStore::new(Db::memory().unwrap());
        store.migrate().unwrap();
        store
    }

    fn deposited(amount: i64) -> NewEvent {
        event("deposited", Json::obj(vec![("amount", Json::int(amount))]))
    }

    #[test]
    fn append_and_read_roundtrip() {
        let store = store();
        let v = store
            .append(
                "account:1",
                Expected::NoStream,
                &[
                    event("opened", Json::obj(vec![("owner", Json::str("eneko"))]))
                        .meta(Json::obj(vec![("actor", Json::str("test"))])),
                    deposited(50),
                ],
            )
            .unwrap();
        assert_eq!(v, 2);

        let events = store.read_stream("account:1", 0).unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].name, "opened");
        assert_eq!(events[0].version, 1);
        assert_eq!(events[0].position, 1);
        assert_eq!(
            events[0].payload.get("owner").and_then(Json::as_str),
            Some("eneko")
        );
        assert_eq!(
            events[0].meta.get("actor").and_then(Json::as_str),
            Some("test")
        );
        assert_eq!(events[1].meta, Json::Null);
        assert_eq!(events[1].version, 2);
        assert_eq!(store.version("account:1").unwrap(), 2);
        assert_eq!(store.version("account:none").unwrap(), 0);
    }

    #[test]
    fn optimistic_concurrency_conflicts() {
        let store = store();
        store
            .append("a", Expected::NoStream, &[deposited(1)])
            .unwrap();

        // Stale version → conflict carrying the actual version.
        let err = store
            .append("a", Expected::Version(0), &[deposited(2)])
            .unwrap_err();
        assert_eq!(
            err,
            EventError::Conflict {
                stream: "a".to_string(),
                expected: Expected::Version(0),
                actual: 1,
            }
        );

        // NoStream on an existing stream → conflict.
        assert!(matches!(
            store.append("a", Expected::NoStream, &[deposited(2)]),
            Err(EventError::Conflict { actual: 1, .. })
        ));

        // The right version and Any both go through.
        assert_eq!(
            store
                .append("a", Expected::Version(1), &[deposited(2)])
                .unwrap(),
            2
        );
        assert_eq!(
            store.append("a", Expected::Any, &[deposited(3)]).unwrap(),
            3
        );
        // Nothing from the conflicting attempts leaked into the stream.
        assert_eq!(store.read_stream("a", 0).unwrap().len(), 3);
    }

    #[test]
    fn positions_are_global_and_gap_free_across_streams() {
        let store = store();
        store.append("a", Expected::Any, &[deposited(1)]).unwrap();
        store
            .append("b", Expected::Any, &[deposited(2), deposited(3)])
            .unwrap();
        store.append("a", Expected::Any, &[deposited(4)]).unwrap();

        let all = store.read_all(0, 100).unwrap();
        assert_eq!(
            all.iter().map(|e| e.position).collect::<Vec<_>>(),
            vec![1, 2, 3, 4]
        );
        assert_eq!(
            all.iter().map(|e| e.version).collect::<Vec<_>>(),
            vec![1, 1, 2, 2]
        );
        assert_eq!(store.head().unwrap(), 4);

        // Paging picks up exactly where it left off.
        let page = store.read_all(2, 1).unwrap();
        assert_eq!(page.len(), 1);
        assert_eq!(page[0].position, 3);
    }

    #[test]
    fn empty_append_checks_expectation_and_returns_version() {
        let store = store();
        assert_eq!(store.append("a", Expected::NoStream, &[]).unwrap(), 0);
        store.append("a", Expected::Any, &[deposited(1)]).unwrap();
        assert_eq!(store.append("a", Expected::Any, &[]).unwrap(), 1);
        assert!(store.append("a", Expected::NoStream, &[]).is_err());
    }

    #[derive(Default)]
    struct Account {
        balance: i64,
    }

    impl Aggregate for Account {
        fn apply(&mut self, event: &StoredEvent) {
            let amount = event
                .payload
                .get("amount")
                .and_then(Json::as_i64)
                .unwrap_or(0);
            match event.name.as_str() {
                "deposited" => self.balance += amount,
                "withdrawn" => self.balance -= amount,
                _ => {}
            }
        }
    }

    #[test]
    fn load_folds_the_stream() {
        let store = store();
        let (fresh, v) = store.load::<Account>("account:9").unwrap();
        assert_eq!((fresh.balance, v), (0, 0));

        store
            .append(
                "account:9",
                Expected::NoStream,
                &[
                    deposited(100),
                    event("withdrawn", Json::obj(vec![("amount", Json::int(30))])),
                ],
            )
            .unwrap();
        let (account, version) = store.load::<Account>("account:9").unwrap();
        assert_eq!(account.balance, 70);
        assert_eq!(version, 2);
    }

    #[test]
    fn append_tx_composes_with_caller_transactions() {
        use sutegi_orm::Transactional;

        let store = store();
        let db = store.backend().clone();
        db.execute("CREATE TABLE ledger (total BIGINT NOT NULL)", &[])
            .unwrap();
        db.execute("INSERT INTO ledger (total) VALUES (0)", &[])
            .unwrap();

        // Event + relational write commit together…
        db.transact(|tx| {
            append_tx(tx, "account:7", Expected::NoStream, &[deposited(10)])?;
            tx.execute("UPDATE ledger SET total = total + 10", &[])?;
            Ok(())
        })
        .unwrap();

        // …and roll back together.
        let failed: Result<(), String> = db.transact(|tx| {
            append_tx(tx, "account:7", Expected::Any, &[deposited(99)])?;
            tx.execute("UPDATE ledger SET total = total + 99", &[])?;
            Err("boom".to_string())
        });
        assert!(failed.is_err());

        assert_eq!(store.version("account:7").unwrap(), 1);
        let total = db
            .query_one("SELECT total AS n FROM ledger", &[])
            .unwrap()
            .and_then(|r| r.get("n").and_then(Json::as_i64))
            .unwrap();
        assert_eq!(total, 10);
    }

    #[test]
    fn stats_reports_head_streams_and_lag() {
        let store = store();
        store.append("a", Expected::Any, &[deposited(1)]).unwrap();
        store.append("b", Expected::Any, &[deposited(2)]).unwrap();
        store
            .backend()
            .execute(
                "INSERT INTO sutegi_projections (name, position, updated_at) VALUES ('p', 1, 0)",
                &[],
            )
            .unwrap();

        let stats = store.stats().unwrap();
        assert_eq!(stats.get("head").and_then(Json::as_i64), Some(2));
        assert_eq!(stats.get("events").and_then(Json::as_i64), Some(2));
        assert_eq!(stats.get("streams").and_then(Json::as_i64), Some(2));
        let projections = match stats.get("projections") {
            Some(Json::Arr(items)) => items.clone(),
            other => panic!("projections not an array: {other:?}"),
        };
        assert_eq!(projections.len(), 1);
        assert_eq!(projections[0].get("lag").and_then(Json::as_i64), Some(1));
    }
}
