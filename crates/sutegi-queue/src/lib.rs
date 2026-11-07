//! A **durable job queue** that survives restarts — and, on Postgres, spans
//! replicas.
//!
//! Jobs live in a `sutegi_jobs` table reached through the ORM's [`Backend`]
//! seam, so the same queue runs on bundled SQLite (single box) and on Postgres
//! (many pods) with **one** set of SQL. Swap the backend, not the call sites.
//!
//! A worker claims a job with a single `UPDATE … RETURNING` statement. The
//! claim stamps `locked_at` instead of deleting the row, so a worker that dies
//! mid-job leaves a row that becomes visible again after the visibility
//! timeout — **at-least-once** delivery. Retries, delays and priorities are
//! columns, not in-memory timers, so a restart forgets nothing.
//!
//! A worker death spends an attempt like any failure: when the lease of a job
//! whose budget is already used up expires, the next claim dead-letters it
//! (`last_error = "exceeded max_attempts (worker died)"`) instead of running a
//! poison job forever. Give jobs that must survive a crash `max_attempts > 1`.
//!
//! Every write a worker makes after claiming — heartbeat and settle — is
//! fenced on its lease (`attempts` + `locked_at`, both rewritten by each
//! claim), so a worker that outlived its visibility timeout can't delete,
//! unlock, extend or fail a row another worker has since reclaimed. Its late
//! writes match nothing and are logged as a lost lease.

//!
//! How the claim stays exclusive differs by backend, and that is the *only*
//! dialect-aware line in this crate:
//!
//! - **Postgres** — `FOR UPDATE SKIP LOCKED` in the picking subquery, so
//!   concurrent workers step over each other's rows instead of blocking.
//! - **SQLite** — nothing needed. Writers are serialized by the database, so
//!   the second worker's `UPDATE` runs after the first one committed and its
//!   subquery no longer sees the claimed row.
//!
//! Time is stored as **epoch milliseconds** supplied by the caller, not by
//! `now()`. That keeps the SQL dialect-free and makes the schedule testable
//! without sleeping.
//!
//! ```no_run
//! use std::sync::Arc;
//! use sutegi_orm::db::Db;
//! use sutegi_queue::Queue;
//! use sutegi_json::Json;
//! # fn demo() -> Result<(), String> {
//! let db = Db::open("app.db")?;
//! let mut queue = Queue::new(db);
//! queue.register("send_email", |job| {
//!     let to = job.payload().get("to").and_then(Json::as_str).unwrap_or("");
//!     let _ = to; // … do the work; return Err to retry …
//!     Ok(())
//! });
//! queue.migrate()?;
//! queue.dispatch("send_email", Json::obj(vec![("to", Json::str("a@b.c"))]))?;
//!
//! let queue = Arc::new(queue);
//! let _workers = Arc::clone(&queue).start(4); // background until dropped
//! # Ok(())
//! # }
//! ```
//!
//! ## Long jobs
//!
//! The visibility timeout is a crash-recovery window, not a deadline — but a
//! job that outruns it gets picked up *twice*. Either raise
//! [`Queue::visibility_timeout`] past the worst-case runtime, or call
//! [`JobCtx::heartbeat`] from inside the handler, which pushes the window
//! forward while the work is still alive. Handlers that loop should also check
//! [`JobCtx::should_stop`] so shutdown doesn't wait for them.

use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use sutegi_json::Json;
use sutegi_log::Value as LogValue;
use sutegi_orm::Backend;
use sutegi_orm::Value;

/// The store a queue runs on: any [`Backend`] that can cross threads.
pub type Store = Arc<dyn Backend + Send + Sync>;

/// A handler for a named job. Returning `Err` triggers a retry until the job's
/// attempt budget is exhausted, after which the row is dead-lettered.
pub type Handler = Arc<dyn Fn(&JobCtx) -> Result<(), String> + Send + Sync>;

/// The queue every [`dispatch`](Queue::dispatch) lands on unless told
/// otherwise.
pub const DEFAULT_QUEUE: &str = "default";

/// Milliseconds since the Unix epoch — the queue's only clock.
pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// What a running handler is told about its job, plus the two things it can do
/// back: extend its lease and notice shutdown.
pub struct JobCtx<'a> {
    /// The job's row id.
    pub id: i64,
    /// The job name it was dispatched under.
    pub name: &'a str,
    /// Which attempt this is, 1-based.
    pub attempts: i64,
    /// The attempt budget; `attempts == max_attempts` is the last try.
    pub max_attempts: i64,
    /// The queue the job was taken from.
    pub queue: &'a str,
    payload: &'a Json,
    store: &'a Store,
    stop: &'a AtomicBool,
    /// The `locked_at` this worker's lease currently holds; heartbeats move it.
    lease: &'a AtomicI64,
}

impl JobCtx<'_> {
    /// The JSON payload the job was dispatched with.
    pub fn payload(&self) -> &Json {
        self.payload
    }

    /// Whether a failure now is terminal (dead-letter) rather than a retry —
    /// worth knowing before writing a user-visible error.
    pub fn is_last_attempt(&self) -> bool {
        self.attempts >= self.max_attempts
    }

    /// Push the visibility window forward: the job is still alive. Call this
    /// from long handlers, or a second worker will reclaim the row while the
    /// first is still working.
    ///
    /// # Errors
    ///
    /// A store error, or a **lost lease**: the window already expired and
    /// another worker reclaimed the job. The handler should stop — whatever
    /// it returns afterwards is discarded, since the row is no longer its own.
    pub fn heartbeat(&self) -> Result<(), String> {
        let held = self.lease.load(Ordering::Relaxed);
        let now = now_ms().max(held);
        let n = self.store.execute(
            "UPDATE sutegi_jobs SET locked_at = ? \
             WHERE id = ? AND attempts = ? AND locked_at = ?",
            &[
                Value::Int(now),
                Value::Int(self.id),
                Value::Int(self.attempts),
                Value::Int(held),
            ],
        )?;
        if n == 0 {
            return Err(format!(
                "lease lost: job {} was reclaimed by another worker",
                self.id
            ));
        }
        self.lease.store(now, Ordering::Relaxed);
        Ok(())
    }

    /// True once the worker pool has been asked to shut down. Long loops should
    /// check this and return early — returning `Err` schedules a retry, which
    /// is usually what you want for interrupted work.
    pub fn should_stop(&self) -> bool {
        self.stop.load(Ordering::Relaxed)
    }
}

/// A durable queue over any [`Backend`].
pub struct Queue {
    store: Store,
    handlers: HashMap<String, Handler>,
    /// How long a claimed-but-unfinished job stays invisible before another
    /// worker may reclaim it (crash recovery).
    visibility_timeout: Duration,
    /// How long an idle worker sleeps before polling again. Local dispatches
    /// wake workers immediately; this bounds the wait for *scheduled* jobs and
    /// for work enqueued by another pod.
    poll_interval: Duration,
    /// Base retry backoff; the delay before attempt N is `base * N`.
    retry_backoff: Duration,
    /// Bumped on every local dispatch so idle workers wake at once.
    wakeup: Arc<(Mutex<u64>, Condvar)>,
}

impl Queue {
    /// Create a queue over `store` with sensible defaults (30 s visibility
    /// timeout, 1 s poll interval, 5 s base retry backoff).
    pub fn new(store: impl Backend + Send + Sync + 'static) -> Queue {
        Queue::with_store(Arc::new(store))
    }

    /// Create a queue over a store you already hold behind an [`Arc`] — e.g.
    /// the same handle the rest of the app writes through.
    pub fn with_store(store: Store) -> Queue {
        Queue {
            store,
            handlers: HashMap::new(),
            visibility_timeout: Duration::from_secs(30),
            poll_interval: Duration::from_secs(1),
            retry_backoff: Duration::from_secs(5),
            wakeup: Arc::new((Mutex::new(0), Condvar::new())),
        }
    }

    /// Override the visibility timeout (crash-recovery window).
    pub fn visibility_timeout(mut self, d: Duration) -> Queue {
        self.visibility_timeout = d;
        self
    }

    /// Override the idle poll interval.
    pub fn poll_interval(mut self, d: Duration) -> Queue {
        self.poll_interval = d;
        self
    }

    /// Override the base retry backoff.
    pub fn retry_backoff(mut self, d: Duration) -> Queue {
        self.retry_backoff = d;
        self
    }

    /// Register a handler for jobs dispatched under `name`.
    pub fn register(
        &mut self,
        name: impl Into<String>,
        handler: impl Fn(&JobCtx) -> Result<(), String> + Send + Sync + 'static,
    ) {
        self.handlers.insert(name.into(), Arc::new(handler));
    }

    /// The underlying store, for sharing or advanced use.
    pub fn store(&self) -> &Store {
        &self.store
    }

    /// Whether claims are exclusive across *pods* (Postgres `SKIP LOCKED`) or
    /// only within this database file's writer serialization (SQLite).
    pub fn cross_pod(&self) -> bool {
        self.store.capabilities().skip_locked
    }

    // --- schema -----------------------------------------------------------

    /// Create the `sutegi_jobs` table and its indexes if they are missing.
    ///
    /// Safe to call from every pod on boot: Postgres can raise a spurious
    /// unique violation when several backends run `CREATE … IF NOT EXISTS`
    /// against the catalog at the same instant, so that race counts as success
    /// (the table ends up created either way).
    pub fn migrate(&self) -> Result<(), String> {
        for stmt in self.schema_sql() {
            match self.store.execute(&stmt, &[]) {
                Ok(_) => {}
                Err(e) if is_already_exists(&e) => {}
                Err(e) => return Err(e),
            }
        }
        self.add_trace_column()
    }

    fn add_trace_column(&self) -> Result<(), String> {
        if self
            .store
            .query("SELECT trace FROM sutegi_jobs WHERE 1 = 0", &[])
            .is_ok()
        {
            return Ok(());
        }
        match self
            .store
            .execute("ALTER TABLE sutegi_jobs ADD COLUMN trace TEXT", &[])
        {
            Ok(_) => Ok(()),
            Err(e) if is_already_exists(&e) || is_duplicate_column(&e) => Ok(()),
            Err(e) => Err(e),
        }
    }

    fn schema_sql(&self) -> Vec<String> {
        let pg = self.store.capabilities().backend == "postgres";
        let id = if pg {
            "id BIGINT GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY"
        } else {
            "id INTEGER PRIMARY KEY AUTOINCREMENT"
        };
        vec![
            format!(
                "CREATE TABLE IF NOT EXISTS sutegi_jobs ({id}, \
                 queue TEXT NOT NULL DEFAULT 'default', \
                 name TEXT NOT NULL, \
                 payload TEXT NOT NULL, \
                 priority INTEGER NOT NULL DEFAULT 0, \
                 attempts INTEGER NOT NULL DEFAULT 0, \
                 max_attempts INTEGER NOT NULL DEFAULT 1, \
                 unique_key TEXT, \
                 run_at BIGINT NOT NULL DEFAULT 0, \
                 locked_at BIGINT, \
                 failed_at BIGINT, \
                 last_error TEXT, \
                 trace TEXT, \
                 created_at BIGINT NOT NULL DEFAULT 0)"
            ),
            // The claim's exact access path: one queue, live rows only, best
            // priority then oldest schedule.
            "CREATE INDEX IF NOT EXISTS sutegi_jobs_claim_idx ON sutegi_jobs \
             (queue, priority, run_at, id) WHERE failed_at IS NULL"
                .into(),
            // Dedupe is per queue and only among live rows: a dead-lettered
            // job must not block re-dispatch of the same key.
            "CREATE UNIQUE INDEX IF NOT EXISTS sutegi_jobs_unique_idx ON sutegi_jobs \
             (queue, unique_key) WHERE unique_key IS NOT NULL AND failed_at IS NULL"
                .into(),
        ]
    }

    // --- dispatch ---------------------------------------------------------

    /// Enqueue a job to run as soon as a worker is free. Returns its row id.
    pub fn dispatch(&self, name: &str, payload: Json) -> Result<i64, String> {
        self.job(name, payload).dispatch()
    }

    /// Enqueue with a retry budget and an optional start delay.
    pub fn dispatch_with(
        &self,
        name: &str,
        payload: Json,
        max_attempts: u32,
        delay: Duration,
    ) -> Result<i64, String> {
        self.job(name, payload)
            .max_attempts(max_attempts)
            .delay(delay)
            .dispatch()
    }

    /// Start building a dispatch: queue, priority, retries, delay, dedupe key.
    ///
    /// ```no_run
    /// # fn demo(queue: &sutegi_queue::Queue) -> Result<(), String> {
    /// # use sutegi_json::Json;
    /// queue
    ///     .job("video.ingest", Json::obj(vec![("id", Json::str("abc"))]))
    ///     .queue("video")      // its own pool, so it can't starve the fast work
    ///     .unique("yt:abc")    // at most one in flight per video
    ///     .max_attempts(3)
    ///     .dispatch()?;
    /// # Ok(()) }
    /// ```
    pub fn job(&self, name: &str, payload: Json) -> Dispatch<'_> {
        Dispatch {
            queue: self,
            name: name.to_string(),
            payload,
            on: DEFAULT_QUEUE.to_string(),
            priority: 0,
            max_attempts: 1,
            delay: Duration::ZERO,
            unique_key: None,
        }
    }

    fn enqueue(&self, d: &Dispatch) -> Result<i64, String> {
        let now = now_ms();
        let mut cols: Vec<(&str, Value)> = vec![
            ("queue", Value::Text(d.on.clone())),
            ("name", Value::Text(d.name.clone())),
            ("payload", Value::Text(d.payload.to_string())),
            ("priority", Value::Int(d.priority as i64)),
            ("max_attempts", Value::Int(d.max_attempts.max(1) as i64)),
            (
                "unique_key",
                match &d.unique_key {
                    Some(k) => Value::Text(k.clone()),
                    None => Value::Null,
                },
            ),
            ("run_at", Value::Int(now + d.delay.as_millis() as i64)),
            ("created_at", Value::Int(now)),
        ];
        if let Some(trace) = current_trace() {
            cols.push(("trace", Value::Text(trace)));
        }
        match self.store.insert("sutegi_jobs", &cols, "id") {
            Ok(id) => {
                self.notify();
                Ok(id)
            }
            // A dedupe key already in flight is the *point* of the key, not an
            // error: hand back the row that is already queued.
            Err(e) if d.unique_key.is_some() && is_unique_violation(&e) => {
                match self.find_unique(&d.on, d.unique_key.as_deref().unwrap_or(""))? {
                    Some(id) => Ok(id),
                    // Lost the race with a worker that just finished it —
                    // nothing is in flight, so enqueue for real.
                    None => {
                        let id = self.store.insert("sutegi_jobs", &cols, "id")?;
                        self.notify();
                        Ok(id)
                    }
                }
            }
            Err(e) => Err(e),
        }
    }

    fn find_unique(&self, queue: &str, key: &str) -> Result<Option<i64>, String> {
        Ok(self
            .store
            .query(
                "SELECT id FROM sutegi_jobs WHERE queue = ? AND unique_key = ? \
                 AND failed_at IS NULL LIMIT 1",
                &[Value::Text(queue.into()), Value::Text(key.into())],
            )?
            .first()
            .and_then(|r| r.get("id").and_then(Json::as_i64)))
    }

    /// Wake idle workers; a local dispatch shouldn't wait out a poll interval.
    fn notify(&self) {
        let (lock, cv) = &*self.wakeup;
        if let Ok(mut generation) = lock.lock() {
            *generation += 1;
        }
        cv.notify_all();
    }

    // --- running ----------------------------------------------------------

    /// Claim and run at most one ready job from the default queue. Returns
    /// `true` if a job was taken off the queue — run, or dead-lettered because
    /// its budget was already spent — so a caller can keep draining, `false`
    /// if the queue was idle.
    pub fn run_once(&self) -> Result<bool, String> {
        self.run_once_on(DEFAULT_QUEUE, &AtomicBool::new(false))
    }

    /// [`run_once`](Queue::run_once) against a named queue. `stop` is what the
    /// handler sees through [`JobCtx::should_stop`].
    pub fn run_once_on(&self, queue: &str, stop: &AtomicBool) -> Result<bool, String> {
        let job = match self.claim(queue)? {
            Claim::Idle => return Ok(false),
            Claim::DeadLettered { id, name, attempts } => {
                sutegi_log::error!(
                    "queue",
                    "job dead-lettered: its worker died on the last attempt",
                    queue = queue,
                    job = name,
                    job_id = id,
                    attempts = attempts
                );
                return Ok(true);
            }
            Claim::Run(job) => job,
        };
        let payload = Json::parse(&job.payload).unwrap_or(Json::Null);
        let inherited = trace_fields(job.trace.as_deref());
        let fields: Vec<(&str, LogValue)> = inherited
            .iter()
            .map(|(k, v)| (k.as_str(), v.clone()))
            .chain([
                ("job", LogValue::Str(Cow::Borrowed(job.name.as_str()))),
                ("job_id", LogValue::Int(job.id)),
                ("attempt", LogValue::Int(job.attempts)),
            ])
            .collect();
        let _scope = sutegi_log::scope(&fields);
        let started = Instant::now();
        let result = match self.handlers.get(&job.name) {
            Some(handler) => {
                let ctx = JobCtx {
                    id: job.id,
                    name: &job.name,
                    attempts: job.attempts,
                    max_attempts: job.max_attempts,
                    queue,
                    payload: &payload,
                    store: &self.store,
                    stop,
                    lease: &job.lease,
                };
                // A panicking handler must not take the worker thread down, and
                // must not vanish silently either: treat it as a failure so the
                // row retries or dead-letters like any other.
                match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| handler(&ctx))) {
                    Ok(r) => r,
                    Err(_) => Err(format!("handler for '{}' panicked", job.name)),
                }
            }
            None => Err(format!("no handler registered for job '{}'", job.name)),
        };
        let ms = started.elapsed().as_millis();
        match &result {
            Ok(()) => sutegi_log::debug!("queue", "job done", queue = queue, ms = ms),
            Err(e) if job.attempts >= job.max_attempts => sutegi_log::error!(
                "queue",
                "job failed terminally",
                queue = queue,
                ms = ms,
                max_attempts = job.max_attempts,
                error = e
            ),
            Err(e) => sutegi_log::warn!(
                "queue",
                "job failed, will retry",
                queue = queue,
                ms = ms,
                max_attempts = job.max_attempts,
                error = e
            ),
        }
        self.settle(&job, result)?;
        Ok(true)
    }

    /// Claim the next ready job in `queue`. One statement, so the claim is
    /// atomic on both backends.
    ///
    /// A row whose lease expired with its attempt budget already spent belongs
    /// to a worker that died mid-job (abort, OOM kill, segfault) on its last
    /// try — no handler ever reached `settle`. Running it again would loop a
    /// poison job forever, so the same `UPDATE` dead-letters it instead. Every
    /// `CASE` reads the row as it was before the update, on both dialects.
    fn claim(&self, queue: &str) -> Result<Claim, String> {
        let now = now_ms();
        let cutoff = now - self.visibility_timeout.as_millis() as i64;
        // Postgres needs SKIP LOCKED so concurrent workers step over rows
        // another backend is already claiming. SQLite serializes writers, so
        // the second UPDATE simply no longer sees the row.
        let skip = if self.store.capabilities().skip_locked {
            " FOR UPDATE SKIP LOCKED"
        } else {
            ""
        };
        let sql = format!(
            "UPDATE sutegi_jobs SET \
               locked_at = CASE WHEN attempts >= max_attempts THEN NULL \
                                ELSE CAST(? AS BIGINT) END, \
               failed_at = CASE WHEN attempts >= max_attempts THEN CAST(? AS BIGINT) \
                                ELSE NULL END, \
               last_error = CASE WHEN attempts >= max_attempts THEN CAST(? AS TEXT) \
                                 ELSE last_error END, \
               attempts = CASE WHEN attempts >= max_attempts THEN attempts \
                               ELSE attempts + 1 END \
             WHERE id = (SELECT id FROM sutegi_jobs \
                         WHERE failed_at IS NULL AND queue = ? AND run_at <= ? \
                           AND (locked_at IS NULL OR locked_at < ?) \
                         ORDER BY priority DESC, run_at, id LIMIT 1{skip}) \
             RETURNING id, name, payload, attempts, max_attempts, trace, failed_at, locked_at"
        );
        let rows = self.store.query(
            &sql,
            &[
                Value::Int(now),
                Value::Int(now),
                Value::Text(WORKER_DIED.to_string()),
                Value::Text(queue.to_string()),
                Value::Int(now),
                Value::Int(cutoff),
            ],
        )?;
        let Some(row) = rows.into_iter().next() else {
            return Ok(Claim::Idle);
        };
        let text = |k: &str| {
            row.get(k)
                .and_then(Json::as_str)
                .unwrap_or_default()
                .to_string()
        };
        let int = |k: &str, d: i64| row.get(k).and_then(Json::as_i64).unwrap_or(d);
        if row.get("failed_at").and_then(Json::as_i64).is_some() {
            return Ok(Claim::DeadLettered {
                id: int("id", 0),
                name: text("name"),
                attempts: int("attempts", 0),
            });
        }
        Ok(Claim::Run(Claimed {
            id: int("id", 0),
            name: text("name"),
            payload: text("payload"),
            attempts: int("attempts", 1),
            max_attempts: int("max_attempts", 1),
            trace: row.get("trace").and_then(Json::as_str).map(str::to_string),
            lease: AtomicI64::new(int("locked_at", now)),
        }))
    }

    /// Land a finished attempt: delete on success, retry with backoff while the
    /// budget lasts, dead-letter when it runs out.
    ///
    /// Every write is fenced on the lease this worker holds. If the visibility
    /// timeout expired and another worker reclaimed the row, nothing matches:
    /// the late result is dropped and logged, never applied over the new
    /// owner's run — and it is not a worker error.
    fn settle(&self, job: &Claimed, result: Result<(), String>) -> Result<(), String> {
        const FENCE: &str = "id = ? AND attempts = ? AND locked_at = ?";
        let fence = [
            Value::Int(job.id),
            Value::Int(job.attempts),
            Value::Int(job.lease.load(Ordering::Relaxed)),
        ];
        let (sql, mut params) = match result {
            Ok(()) => (format!("DELETE FROM sutegi_jobs WHERE {FENCE}"), vec![]),
            Err(err) if job.attempts >= job.max_attempts => (
                format!(
                    "UPDATE sutegi_jobs SET failed_at = ?, locked_at = NULL, \
                     last_error = ? WHERE {FENCE}"
                ),
                vec![Value::Int(now_ms()), Value::Text(err)],
            ),
            Err(err) => {
                let backoff = self.retry_backoff.as_millis() as i64 * job.attempts.max(1);
                (
                    format!(
                        "UPDATE sutegi_jobs SET locked_at = NULL, last_error = ?, \
                         run_at = ? WHERE {FENCE}"
                    ),
                    vec![Value::Text(err), Value::Int(now_ms() + backoff)],
                )
            }
        };
        params.extend(fence);
        if self.store.execute(&sql, &params)? == 0 {
            sutegi_log::warn!(
                "queue",
                "lease lost: job was reclaimed after its visibility timeout; result dropped",
                job = job.name,
                job_id = job.id,
                attempt = job.attempts
            );
        }
        Ok(())
    }

    /// Spawn `workers` threads that drain the default queue until the returned
    /// [`Workers`] handle is dropped (or `stop()` is called).
    pub fn start(self: Arc<Self>, workers: usize) -> Workers {
        self.start_on(DEFAULT_QUEUE, workers)
    }

    /// [`start`](Queue::start) against a named queue. Separate pools are how a
    /// slow job class (transcoding a video) is kept from starving a fast one
    /// (fetching a page): give each its own queue and its own worker count.
    pub fn start_on(self: Arc<Self>, queue: &str, workers: usize) -> Workers {
        let stop = Arc::new(AtomicBool::new(false));
        let mut handles = Vec::new();
        for _ in 0..workers.max(1) {
            let queue_name = queue.to_string();
            let q = Arc::clone(&self);
            let stop = Arc::clone(&stop);
            handles.push(thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    match q.run_once_on(&queue_name, &stop) {
                        Ok(true) => continue, // keep draining while there's work
                        Ok(false) => q.wait_for_work(),
                        Err(e) => {
                            sutegi_log::error!(
                                "queue",
                                "worker error",
                                queue = queue_name,
                                error = e
                            );
                            thread::sleep(q.poll_interval);
                        }
                    }
                }
            }));
        }
        Workers { stop, handles }
    }

    /// Sleep until a local dispatch wakes us or the poll interval expires —
    /// whichever comes first. The timeout is what covers delayed jobs and work
    /// enqueued by another pod, which no local notify can announce.
    fn wait_for_work(&self) {
        let (lock, cv) = &*self.wakeup;
        if let Ok(generation) = lock.lock() {
            let _ = cv.wait_timeout(generation, self.poll_interval);
        }
    }

    // --- introspection & ops ---------------------------------------------

    /// Queue depth by state, as JSON — wire it into an ops endpoint.
    pub fn stats(&self) -> Result<Json, String> {
        self.stats_where("", &[])
    }

    /// [`stats`](Queue::stats) for one named queue.
    pub fn stats_for(&self, queue: &str) -> Result<Json, String> {
        self.stats_where(" WHERE queue = ?", &[Value::Text(queue.to_string())])
    }

    fn stats_where(&self, filter: &str, params: &[Value]) -> Result<Json, String> {
        // `count(*) FILTER (WHERE …)` is not portable; CASE is. `now` is
        // interpolated rather than bound so the placeholder order stays
        // independent of the optional filter.
        let now = now_ms();
        let sql = format!(
            "SELECT \
               SUM(CASE WHEN failed_at IS NULL AND run_at <= {now} AND locked_at IS NULL \
                        THEN 1 ELSE 0 END) AS ready, \
               SUM(CASE WHEN failed_at IS NULL AND locked_at IS NOT NULL THEN 1 ELSE 0 END) \
                        AS running, \
               SUM(CASE WHEN failed_at IS NULL AND run_at > {now} THEN 1 ELSE 0 END) \
                        AS scheduled, \
               SUM(CASE WHEN failed_at IS NOT NULL THEN 1 ELSE 0 END) AS failed, \
               COUNT(*) AS total \
             FROM sutegi_jobs{filter}"
        );
        let row = self
            .store
            .query(&sql, params)?
            .into_iter()
            .next()
            .unwrap_or(Json::Null);
        let n = |k: &str| Json::int(row.get(k).and_then(Json::as_i64).unwrap_or(0));
        Ok(Json::obj(vec![
            ("ready", n("ready")),
            ("running", n("running")),
            ("scheduled", n("scheduled")),
            ("failed", n("failed")),
            ("total", n("total")),
        ]))
    }

    /// Dead-lettered jobs, newest first — the rows a dev screen should show.
    pub fn failed(&self, limit: i64) -> Result<Vec<Json>, String> {
        self.store.query(
            "SELECT id, queue, name, payload, attempts, max_attempts, last_error, failed_at \
             FROM sutegi_jobs WHERE failed_at IS NOT NULL ORDER BY failed_at DESC LIMIT ?",
            &[Value::Int(limit.max(1))],
        )
    }

    /// Put a dead-lettered job back in the queue with a fresh attempt budget.
    /// Returns whether there was such a job to revive.
    pub fn retry(&self, id: i64, max_attempts: u32) -> Result<bool, String> {
        let n = self.store.execute(
            "UPDATE sutegi_jobs SET failed_at = NULL, locked_at = NULL, attempts = 0, \
             max_attempts = ?, run_at = ?, last_error = NULL \
             WHERE id = ? AND failed_at IS NOT NULL",
            &[
                Value::Int(max_attempts.max(1) as i64),
                Value::Int(now_ms()),
                Value::Int(id),
            ],
        )?;
        if n > 0 {
            self.notify();
        }
        Ok(n > 0)
    }

    /// Drop dead-letter rows that failed at least `age` ago. Returns how many
    /// were removed. The bound is inclusive, so `purge_failed(Duration::ZERO)`
    /// clears the lot — including a row stamped this same millisecond.
    pub fn purge_failed(&self, age: Duration) -> Result<usize, String> {
        self.store.execute(
            "DELETE FROM sutegi_jobs WHERE failed_at IS NOT NULL AND failed_at <= ?",
            &[Value::Int(now_ms() - age.as_millis() as i64)],
        )
    }
}

/// A dispatch under construction — see [`Queue::job`].
pub struct Dispatch<'q> {
    queue: &'q Queue,
    name: String,
    payload: Json,
    on: String,
    priority: i32,
    max_attempts: u32,
    delay: Duration,
    unique_key: Option<String>,
}

impl Dispatch<'_> {
    /// Put the job on a named queue (default `"default"`).
    pub fn queue(mut self, name: &str) -> Self {
        self.on = name.to_string();
        self
    }

    /// Higher runs first within a queue. Equal priorities run oldest-first.
    pub fn priority(mut self, p: i32) -> Self {
        self.priority = p;
        self
    }

    /// How many attempts the job gets before it is dead-lettered.
    pub fn max_attempts(mut self, n: u32) -> Self {
        self.max_attempts = n;
        self
    }

    /// Hold the job back for `d` before it becomes claimable.
    pub fn delay(mut self, d: Duration) -> Self {
        self.delay = d;
        self
    }

    /// Collapse duplicates: while a job with this key is live on the queue, a
    /// second dispatch returns the existing row's id instead of enqueueing
    /// again. Dead-lettered rows don't block a fresh dispatch.
    pub fn unique(mut self, key: &str) -> Self {
        self.unique_key = Some(key.to_string());
        self
    }

    /// Enqueue it. Returns the row id.
    pub fn dispatch(self) -> Result<i64, String> {
        self.queue.enqueue(&self)
    }
}

/// The `last_error` of a job dead-lettered by [`Queue::claim`] rather than by
/// a handler's own failure.
const WORKER_DIED: &str = "exceeded max_attempts (worker died)";

/// What one claim attempt found.
enum Claim {
    /// Nothing ready.
    Idle,
    /// A job to run.
    Run(Claimed),
    /// An expired lease on a spent budget, moved to the dead letter instead.
    DeadLettered {
        id: i64,
        name: String,
        attempts: i64,
    },
}

/// A row this worker has claimed.
struct Claimed {
    id: i64,
    name: String,
    payload: String,
    attempts: i64,
    max_attempts: i64,
    trace: Option<String>,
    /// The `locked_at` written by this claim (or this worker's last
    /// heartbeat). With `attempts`, it fences every later write to the row.
    lease: AtomicI64,
}

const MAX_TRACE_BYTES: usize = 2048;

fn current_trace() -> Option<String> {
    let fields = sutegi_log::context();
    if fields.is_empty() {
        return None;
    }
    let pairs: Vec<(&str, Json)> = fields
        .iter()
        .map(|(k, v)| (k.as_str(), log_value_to_json(v)))
        .collect();
    let trace = Json::obj(pairs).to_string();
    (trace.len() <= MAX_TRACE_BYTES).then_some(trace)
}

fn log_value_to_json(value: &LogValue<'_>) -> Json {
    match value {
        LogValue::Null => Json::Null,
        LogValue::Bool(b) => Json::Bool(*b),
        LogValue::Int(i) => Json::int(*i),
        LogValue::Uint(u) => i64::try_from(*u).map_or_else(|_| Json::str(u.to_string()), Json::int),
        LogValue::Float(f) => Json::num(*f),
        LogValue::Str(s) => Json::str(s.as_ref()),
    }
}

fn trace_fields(trace: Option<&str>) -> Vec<(String, LogValue<'static>)> {
    let Some(Json::Obj(map)) = trace.and_then(|t| Json::parse(t).ok()) else {
        return Vec::new();
    };
    map.into_iter()
        .filter_map(|(k, v)| {
            let value = match v {
                Json::Null => LogValue::Null,
                Json::Bool(b) => LogValue::Bool(b),
                Json::Num(n) if n.fract() == 0.0 && n.abs() < 9.0e15 => LogValue::Int(n as i64),
                Json::Num(n) => LogValue::Float(n),
                Json::Str(s) => LogValue::Str(Cow::Owned(s)),
                Json::Arr(_) | Json::Obj(_) => return None,
            };
            Some((k, value))
        })
        .collect()
}

/// A running set of queue workers. Dropping it (or calling
/// [`stop`](Workers::stop)) signals shutdown and joins the threads.
pub struct Workers {
    stop: Arc<AtomicBool>,
    handles: Vec<thread::JoinHandle<()>>,
}

impl Workers {
    /// Signal the workers to stop and wait for in-flight jobs to finish.
    pub fn stop(mut self) {
        self.shutdown();
    }

    /// Ask the workers to stop without waiting — this is the flag handlers see
    /// through [`JobCtx::should_stop`]. Joining still happens on drop.
    pub fn signal_stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }

    fn shutdown(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        for h in self.handles.drain(..) {
            let _ = h.join();
        }
    }
}

impl Drop for Workers {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// `CREATE … IF NOT EXISTS` racing itself across pods, in either dialect.
fn is_already_exists(e: &str) -> bool {
    let e = e.to_ascii_lowercase();
    e.contains("already exists") || e.contains("23505") || e.contains("duplicate key")
}

fn is_duplicate_column(e: &str) -> bool {
    e.to_ascii_lowercase().contains("duplicate column")
}

/// A unique-index violation, in either dialect.
fn is_unique_violation(e: &str) -> bool {
    let e = e.to_ascii_lowercase();
    e.contains("unique constraint") // SQLite
        || e.contains("23505") // Postgres SQLSTATE
        || e.contains("duplicate key")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dialect_error_shapes_are_both_recognised() {
        assert!(is_unique_violation(
            "UNIQUE constraint failed: sutegi_jobs.unique_key"
        ));
        assert!(is_unique_violation(
            "ERROR 23505: duplicate key value violates unique constraint"
        ));
        assert!(!is_unique_violation("no such table: sutegi_jobs"));
        assert!(is_already_exists("relation \"sutegi_jobs\" already exists"));
        assert!(!is_already_exists("syntax error at or near \"CREATE\""));
    }

    // -- lease fencing ------------------------------------------------------
    //
    // Driven through `claim`/`settle` directly so the interleaving is forced,
    // not timed: worker A claims, its lease expires, worker B reclaims, then A
    // finally reports back.

    struct TempDb(String);

    impl Drop for TempDb {
        fn drop(&mut self) {
            for suffix in ["", "-wal", "-shm"] {
                // Best-effort cleanup; a leftover temp file is harmless.
                let _ = std::fs::remove_file(format!("{}{suffix}", self.0));
            }
        }
    }

    fn stolen_job(tag: &str) -> (TempDb, Queue, Claimed, Claimed) {
        let path = std::env::temp_dir()
            .join(format!("sutegi-queue-unit-{tag}-{}.db", std::process::id()))
            .to_string_lossy()
            .into_owned();
        let tmp = TempDb(path);
        drop(TempDb(tmp.0.clone())); // clear any leftover from a crashed run
        let db = sutegi_orm::db::Db::open(&tmp.0).expect("open db");
        let q = Queue::new(db).visibility_timeout(Duration::from_millis(30));
        q.migrate().expect("migrate");
        q.dispatch_with("job", Json::Null, 5, Duration::ZERO)
            .expect("dispatch");
        let Claim::Run(a) = q.claim(DEFAULT_QUEUE).expect("claim") else {
            panic!("A should claim");
        };
        std::thread::sleep(Duration::from_millis(50));
        let Claim::Run(b) = q.claim(DEFAULT_QUEUE).expect("claim") else {
            panic!("B should reclaim after A's lease expired");
        };
        assert_eq!(a.id, b.id);
        (tmp, q, a, b)
    }

    fn row(q: &Queue, id: i64) -> Option<Json> {
        q.store()
            .query(
                "SELECT locked_at, last_error, failed_at FROM sutegi_jobs WHERE id = ?",
                &[Value::Int(id)],
            )
            .expect("select")
            .into_iter()
            .next()
    }

    #[test]
    fn a_stale_worker_cannot_delete_a_reclaimed_job() {
        let (_tmp, q, a, b) = stolen_job("fence-ok");
        q.settle(&a, Ok(()))
            .expect("a lost lease is not a worker error");
        assert!(row(&q, a.id).is_some(), "A's late success deleted B's job");
        q.settle(&b, Ok(())).expect("settle");
        assert!(row(&q, b.id).is_none());
    }

    #[test]
    fn a_stale_worker_cannot_unlock_or_fail_a_reclaimed_job() {
        let (_tmp, q, a, b) = stolen_job("fence-err");
        q.settle(&a, Err("stale".into())).expect("no-op");
        let r = row(&q, a.id).expect("row");
        assert_eq!(
            r.get("locked_at").and_then(Json::as_i64),
            Some(b.lease.load(Ordering::Relaxed)),
            "A's late failure unlocked B's running job"
        );
        assert_eq!(r.get("last_error"), Some(&Json::Null));

        // B's own failure still lands.
        q.settle(&b, Err("real".into())).expect("settle");
        let r = row(&q, b.id).expect("row");
        assert_eq!(r.get("last_error").and_then(Json::as_str), Some("real"));
    }

    #[test]
    fn a_stale_heartbeat_reports_the_lost_lease_and_extends_nothing() {
        let (_tmp, q, a, b) = stolen_job("fence-hb");
        let stop = AtomicBool::new(false);
        let payload = Json::Null;
        let ctx = |job: &Claimed| -> Result<(), String> {
            JobCtx {
                id: job.id,
                name: &job.name,
                attempts: job.attempts,
                max_attempts: job.max_attempts,
                queue: DEFAULT_QUEUE,
                payload: &payload,
                store: q.store(),
                stop: &stop,
                lease: &job.lease,
            }
            .heartbeat()
        };
        let err = ctx(&a).expect_err("A no longer holds the lease");
        assert!(err.contains("lease lost"), "got: {err}");
        let held = b.lease.load(Ordering::Relaxed);
        let locked = row(&q, b.id).and_then(|r| r.get("locked_at").and_then(Json::as_i64));
        assert_eq!(locked, Some(held), "A's heartbeat moved B's lease");

        // B's heartbeat works, and settle follows the moved lease.
        std::thread::sleep(Duration::from_millis(2));
        ctx(&b).expect("B holds the lease");
        assert!(b.lease.load(Ordering::Relaxed) > held);
        q.settle(&b, Ok(())).expect("settle");
        assert!(
            row(&q, b.id).is_none(),
            "settle must use the heartbeat's lease"
        );
    }

    #[test]
    fn now_ms_is_a_plausible_wall_clock() {
        // Sanity: past 2020, not in the far future — catches a seconds/millis
        // mixup, which would silently make every delay 1000× wrong.
        let now = now_ms();
        assert!(now > 1_577_836_800_000, "{now} is before 2020");
        assert!(now < 4_102_444_800_000, "{now} is after 2100");
    }
}
