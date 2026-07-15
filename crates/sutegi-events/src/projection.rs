//! Projections: named, checkpointed consumers of the global event log that
//! maintain read models (or trigger side effects) in the background.
//!
//! Each tick claims the projection's checkpoint row, reads a batch of events
//! past it, runs the handler for each, and advances the checkpoint — **all in
//! one transaction**. A handler error rolls the whole batch back, so read-model
//! writes made through the handler's backend are exactly-once per projection.
//! The row claim serializes a projection across pods (two workers on the same
//! projection just take turns), while distinct projections run independently.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use sutegi_orm::{Backend, Transactional, Value};

use crate::store::{decode_event, ensure_tables, now_millis, StoredEvent};

/// A projection handler: receives each stored event **and the transaction the
/// checkpoint moves in** — write your read model through it and the update is
/// atomic with the checkpoint. Return `Err` to roll the batch back (it will be
/// retried next poll; the checkpoint won't move past a failing event).
pub type ProjectionFn = Arc<dyn Fn(&StoredEvent, &dyn Backend) -> Result<(), String> + Send + Sync>;

/// A set of named projections over one backend. Register handlers, `migrate()`
/// to create checkpoint rows, then either drive ticks yourself ([`run_once`]
/// (Projections::run_once) — handy in tests and cron jobs) or [`start`]
/// (Projections::start) background workers.
pub struct Projections<B: Transactional> {
    backend: B,
    projections: Vec<(String, ProjectionFn)>,
    batch: i64,
    poll_interval: Duration,
    wakeup: Arc<(std::sync::Mutex<()>, std::sync::Condvar)>,
}

impl<B: Transactional> Projections<B> {
    /// Projections over `backend` with sensible defaults (batches of 100,
    /// 500ms idle poll).
    pub fn new(backend: B) -> Projections<B> {
        Projections {
            backend,
            projections: Vec::new(),
            batch: 100,
            poll_interval: Duration::from_millis(500),
            wakeup: Arc::new((std::sync::Mutex::new(()), std::sync::Condvar::new())),
        }
    }

    /// Override how many events one tick processes at most.
    pub fn batch(mut self, n: i64) -> Projections<B> {
        self.batch = n.max(1);
        self
    }

    /// Override how long an idle worker sleeps before polling again.
    pub fn poll_interval(mut self, d: Duration) -> Projections<B> {
        self.poll_interval = d;
        self
    }

    /// Wake all sleeping projection workers immediately.
    /// Wire this to a pub/sub listener (e.g. `sutegi-pubsub/postgres` on the
    /// `sutegi_events` channel) to make your projections fully push-driven.
    pub fn wake(&self) {
        self.wakeup.1.notify_all();
    }

    /// Register `name` with its handler. Names key the durable checkpoints, so
    /// renaming a projection restarts it from position 0.
    pub fn register(
        &mut self,
        name: impl Into<String>,
        handler: impl Fn(&StoredEvent, &dyn Backend) -> Result<(), String> + Send + Sync + 'static,
    ) {
        self.projections.push((name.into(), Arc::new(handler)));
    }

    /// Ensure the store tables exist and every registered projection has a
    /// checkpoint row. Call on boot, after registering.
    pub fn migrate(&self) -> Result<(), String> {
        ensure_tables(&self.backend)?;
        for (name, _) in &self.projections {
            self.backend.execute(
                "INSERT INTO sutegi_projections (name, position, updated_at) VALUES (?, 0, ?) \
                 ON CONFLICT (name) DO NOTHING",
                &[Value::Text(name.clone()), Value::Int(now_millis())],
            )?;
        }
        Ok(())
    }

    /// Run one batch for projection `name`; returns how many events were
    /// processed (0 = caught up).
    pub fn tick(&self, name: &str) -> Result<usize, String> {
        let handler = self
            .projections
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, h)| Arc::clone(h))
            .ok_or_else(|| format!("unknown projection '{name}'"))?;
        self.backend.transact(|tx| {
            // The UPDATE takes the checkpoint's row lock for the duration of
            // the transaction — that's what serializes this projection across
            // pods; the value itself is just bookkeeping.
            let claimed = tx.execute(
                "UPDATE sutegi_projections SET locked_at = ? WHERE name = ?",
                &[Value::Int(now_millis()), Value::Text(name.to_string())],
            )?;
            if claimed == 0 {
                return Err(format!(
                    "projection '{name}' has no checkpoint row — call migrate() first"
                ));
            }
            let position = tx
                .query_one(
                    "SELECT position FROM sutegi_projections WHERE name = ?",
                    &[Value::Text(name.to_string())],
                )?
                .as_ref()
                .and_then(|r| r.get("position"))
                .and_then(sutegi_json::Json::as_i64)
                .unwrap_or(0);
            let rows = tx.query(
                "SELECT * FROM sutegi_events WHERE position > ? ORDER BY position LIMIT ?",
                &[Value::Int(position), Value::Int(self.batch)],
            )?;
            let events: Vec<StoredEvent> = rows
                .iter()
                .map(decode_event)
                .collect::<Result<_, String>>()?;
            for event in &events {
                handler(event, tx)?;
            }
            if let Some(last) = events.last() {
                tx.execute(
                    "UPDATE sutegi_projections SET position = ?, updated_at = ? WHERE name = ?",
                    &[
                        Value::Int(last.position),
                        Value::Int(now_millis()),
                        Value::Text(name.to_string()),
                    ],
                )?;
            }
            Ok(events.len())
        })
    }

    /// Tick every registered projection once; returns the total events
    /// processed. Loop it to drain: `while p.run_once()? > 0 {}`.
    pub fn run_once(&self) -> Result<usize, String> {
        let mut total = 0;
        for (name, _) in &self.projections {
            total += self.tick(name)?;
        }
        Ok(total)
    }

    /// A projection's current checkpoint position.
    pub fn position(&self, name: &str) -> Result<i64, String> {
        Ok(self
            .backend
            .query_one(
                "SELECT position FROM sutegi_projections WHERE name = ?",
                &[Value::Text(name.to_string())],
            )?
            .as_ref()
            .and_then(|r| r.get("position"))
            .and_then(sutegi_json::Json::as_i64)
            .unwrap_or(0))
    }

    /// Rewind `name` to position 0 — the rebuild switch. Clear the read model
    /// first; the next ticks replay the whole log into it.
    pub fn reset(&self, name: &str) -> Result<(), String> {
        self.backend
            .execute(
                "UPDATE sutegi_projections SET position = 0, updated_at = ? WHERE name = ?",
                &[Value::Int(now_millis()), Value::Text(name.to_string())],
            )
            .map(|_| ())
    }

    /// Spawn one background thread per registered projection, polling until
    /// the returned [`ProjectionWorkers`] handle is dropped (or `stop()` is
    /// called). A failing projection logs and keeps retrying without blocking
    /// the others.
    pub fn start(self: Arc<Self>) -> ProjectionWorkers
    where
        B: Send + Sync + 'static,
    {
        let stop = Arc::new(AtomicBool::new(false));
        let mut handles = Vec::new();
        for (name, _) in &self.projections {
            let projections = Arc::clone(&self);
            let name = name.clone();
            let stop = Arc::clone(&stop);
            handles.push(thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    match projections.tick(&name) {
                        Ok(0) => {
                            let (lock, cvar) = &*projections.wakeup;
                            let guard = lock.lock().unwrap();
                            let _ = cvar.wait_timeout(guard, projections.poll_interval).unwrap();
                        }
                        Ok(_) => continue, // keep draining while behind
                        Err(e) => {
                            eprintln!("[events] projection '{name}': {e}");
                            let (lock, cvar) = &*projections.wakeup;
                            let guard = lock.lock().unwrap();
                            let _ = cvar.wait_timeout(guard, projections.poll_interval).unwrap();
                        }
                    }
                }
            }));
        }
        ProjectionWorkers { stop, handles }
    }
}

/// Running projection workers. Dropping it (or calling
/// [`stop`](ProjectionWorkers::stop)) signals shutdown and joins the threads.
pub struct ProjectionWorkers {
    stop: Arc<AtomicBool>,
    handles: Vec<thread::JoinHandle<()>>,
}

impl ProjectionWorkers {
    /// Signal the workers to stop and wait for in-flight batches to finish.
    pub fn stop(mut self) {
        self.shutdown();
    }

    fn shutdown(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        for h in self.handles.drain(..) {
            let _ = h.join();
        }
    }
}

impl Drop for ProjectionWorkers {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{event, EventStore, Expected};
    use std::sync::atomic::AtomicUsize;
    use sutegi_json::Json;
    use sutegi_orm::db::Db;

    fn setup() -> (EventStore<Db>, Db) {
        let db = Db::memory().unwrap();
        let store = EventStore::new(db.clone());
        store.migrate().unwrap();
        db.execute(
            "CREATE TABLE totals (stream TEXT PRIMARY KEY, total BIGINT NOT NULL DEFAULT 0)",
            &[],
        )
        .unwrap();
        (store, db)
    }

    fn deposited(stream: &str, amount: i64, store: &EventStore<Db>) {
        store
            .append(
                stream,
                Expected::Any,
                &[event(
                    "deposited",
                    Json::obj(vec![("amount", Json::int(amount))]),
                )],
            )
            .unwrap();
    }

    fn totals_projection(projections: &mut Projections<Db>) {
        projections.register("totals", |e, tx| {
            let amount = e.payload.get("amount").and_then(Json::as_i64).unwrap_or(0);
            tx.execute(
                "INSERT INTO totals (stream, total) VALUES (?, ?) \
                 ON CONFLICT (stream) DO UPDATE SET total = totals.total + excluded.total",
                &[Value::Text(e.stream.clone()), Value::Int(amount)],
            )?;
            Ok(())
        });
    }

    fn total(db: &Db, stream: &str) -> i64 {
        db.query_one(
            "SELECT total FROM totals WHERE stream = ?",
            &[Value::Text(stream.to_string())],
        )
        .unwrap()
        .as_ref()
        .and_then(|r| r.get("total"))
        .and_then(Json::as_i64)
        .unwrap_or(0)
    }

    #[test]
    fn builds_a_read_model_and_checkpoints() {
        let (store, db) = setup();
        let mut projections = Projections::new(db.clone());
        totals_projection(&mut projections);
        projections.migrate().unwrap();

        deposited("a", 10, &store);
        deposited("b", 5, &store);
        deposited("a", 7, &store);

        assert_eq!(projections.run_once().unwrap(), 3);
        assert_eq!(total(&db, "a"), 17);
        assert_eq!(total(&db, "b"), 5);
        assert_eq!(projections.position("totals").unwrap(), 3);

        // Caught up: nothing to do, checkpoint holds.
        assert_eq!(projections.run_once().unwrap(), 0);

        // New events picked up from the checkpoint, not replayed.
        deposited("a", 3, &store);
        assert_eq!(projections.run_once().unwrap(), 1);
        assert_eq!(total(&db, "a"), 20);
    }

    #[test]
    fn failing_handler_rolls_back_and_holds_the_checkpoint() {
        let (store, db) = setup();
        let failures = Arc::new(AtomicUsize::new(1));
        let mut projections = Projections::new(db.clone());
        let flaky = Arc::clone(&failures);
        projections.register("totals", move |e, tx| {
            if flaky.load(Ordering::SeqCst) > 0 {
                flaky.fetch_sub(1, Ordering::SeqCst);
                return Err("transient".to_string());
            }
            let amount = e.payload.get("amount").and_then(Json::as_i64).unwrap_or(0);
            tx.execute(
                "INSERT INTO totals (stream, total) VALUES (?, ?) \
                 ON CONFLICT (stream) DO UPDATE SET total = totals.total + excluded.total",
                &[Value::Text(e.stream.clone()), Value::Int(amount)],
            )?;
            Ok(())
        });
        projections.migrate().unwrap();
        deposited("a", 10, &store);

        // First tick fails: no read-model write, checkpoint unmoved.
        assert!(projections.tick("totals").is_err());
        assert_eq!(total(&db, "a"), 0);
        assert_eq!(projections.position("totals").unwrap(), 0);

        // Retry succeeds and applies the event exactly once.
        assert_eq!(projections.tick("totals").unwrap(), 1);
        assert_eq!(total(&db, "a"), 10);
        assert_eq!(projections.position("totals").unwrap(), 1);
    }

    #[test]
    fn batches_respect_the_limit() {
        let (store, db) = setup();
        let mut projections = Projections::new(db.clone()).batch(2);
        totals_projection(&mut projections);
        projections.migrate().unwrap();
        for _ in 0..5 {
            deposited("a", 1, &store);
        }

        assert_eq!(projections.tick("totals").unwrap(), 2);
        assert_eq!(projections.position("totals").unwrap(), 2);
        assert_eq!(projections.tick("totals").unwrap(), 2);
        assert_eq!(projections.tick("totals").unwrap(), 1);
        assert_eq!(projections.tick("totals").unwrap(), 0);
        assert_eq!(total(&db, "a"), 5);
    }

    #[test]
    fn reset_replays_the_log() {
        let (store, db) = setup();
        let mut projections = Projections::new(db.clone());
        totals_projection(&mut projections);
        projections.migrate().unwrap();
        deposited("a", 10, &store);
        assert_eq!(projections.run_once().unwrap(), 1);
        assert_eq!(total(&db, "a"), 10);

        // Rebuild: clear the read model, rewind, replay.
        db.execute("DELETE FROM totals", &[]).unwrap();
        projections.reset("totals").unwrap();
        assert_eq!(projections.run_once().unwrap(), 1);
        assert_eq!(total(&db, "a"), 10);
    }

    #[test]
    fn unknown_and_unmigrated_projections_error() {
        let (_, db) = setup();
        let mut projections = Projections::new(db.clone());
        totals_projection(&mut projections);
        assert!(projections.tick("nope").is_err());
        // Registered but no checkpoint row yet (migrate not called).
        assert!(projections.tick("totals").unwrap_err().contains("migrate"));
    }

    #[test]
    fn background_workers_catch_up() {
        let (store, db) = setup();
        let mut projections = Projections::new(db.clone()).poll_interval(Duration::from_millis(10));
        totals_projection(&mut projections);
        projections.migrate().unwrap();
        deposited("a", 4, &store);

        let workers = Arc::new(projections).start();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while total(&db, "a") != 4 && std::time::Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        workers.stop();
        assert_eq!(total(&db, "a"), 4);
    }
}
