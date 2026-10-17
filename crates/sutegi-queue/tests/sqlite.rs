//! The queue's behaviour, exercised end to end on the bundled SQLite backend —
//! no server, no environment variables, so this runs everywhere `cargo test`
//! does. The Postgres leg of the same contract lives in `durable.rs`.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use sutegi_json::Json;
use sutegi_orm::db::Db;
use sutegi_orm::Backend;
use sutegi_orm::Value;
use sutegi_queue::{now_ms, Queue};

/// A fresh database file per test. WAL is what production runs, and an
/// in-memory database would not be shared across the pool's connections.
struct TempDb {
    path: String,
}

impl TempDb {
    fn new(tag: &str) -> TempDb {
        let path = std::env::temp_dir()
            .join(format!("sutegi-queue-{tag}-{}.db", std::process::id()))
            .to_string_lossy()
            .into_owned();
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{path}{suffix}"));
        }
        TempDb { path }
    }

    fn open(&self) -> Db {
        Db::open(&self.path).expect("open db")
    }
}

impl Drop for TempDb {
    fn drop(&mut self) {
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", self.path));
        }
    }
}

fn queue(tag: &str) -> (TempDb, Db, Queue) {
    let tmp = TempDb::new(tag);
    let db = tmp.open();
    let q = Queue::new(db.clone())
        .poll_interval(Duration::from_millis(10))
        .retry_backoff(Duration::from_millis(1));
    q.migrate().expect("migrate");
    (tmp, db, q)
}

fn wait_until(cond: impl Fn() -> bool) -> bool {
    for _ in 0..400 {
        if cond() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    cond()
}

fn count(db: &Db, sql: &str) -> i64 {
    db.query(sql, &[])
        .expect("count")
        .first()
        .and_then(|r| r.get("n").and_then(Json::as_i64))
        .unwrap_or(-1)
}

#[test]
fn migrate_is_idempotent_and_creates_the_table() {
    let (_tmp, db, q) = queue("migrate");
    q.migrate()
        .expect("second migrate is a no-op, not an error");
    assert_eq!(count(&db, "SELECT COUNT(*) AS n FROM sutegi_jobs"), 0);
}

#[test]
fn a_dispatched_job_runs_with_its_payload_and_leaves_no_row() {
    let (_tmp, db, mut q) = queue("dispatch");
    let seen = Arc::new(Mutex::new(Vec::<String>::new()));
    let sink = Arc::clone(&seen);
    q.register("greet", move |job| {
        let who = job
            .payload()
            .get("who")
            .and_then(Json::as_str)
            .unwrap_or("?");
        sink.lock().unwrap().push(who.to_string());
        Ok(())
    });

    q.dispatch("greet", Json::obj(vec![("who", Json::str("eneko"))]))
        .expect("dispatch");
    assert!(q.run_once().expect("run"), "a job was waiting");
    assert!(!q.run_once().expect("run"), "queue is drained");

    assert_eq!(seen.lock().unwrap().as_slice(), ["eneko"]);
    // Success deletes the row — the queue is not a history table.
    assert_eq!(count(&db, "SELECT COUNT(*) AS n FROM sutegi_jobs"), 0);
}

#[test]
fn a_failing_job_retries_until_its_budget_runs_out_then_dead_letters() {
    let (_tmp, db, mut q) = queue("retry");
    let attempts = Arc::new(AtomicU32::new(0));
    let counter = Arc::clone(&attempts);
    q.register("flaky", move |job| {
        // Fail the first attempt, succeed on the second.
        if counter.fetch_add(1, Ordering::SeqCst) == 0 {
            assert!(!job.is_last_attempt(), "attempt 1 of 2 is not terminal");
            Err("transient".into())
        } else {
            Ok(())
        }
    });
    q.register("doomed", |_job| Err("always".into()));

    q.job("flaky", Json::Null)
        .max_attempts(2)
        .dispatch()
        .expect("dispatch");
    q.run_once().expect("attempt 1");
    assert_eq!(
        count(
            &db,
            "SELECT COUNT(*) AS n FROM sutegi_jobs WHERE failed_at IS NULL"
        ),
        1,
        "still queued for a retry"
    );
    // The retry is scheduled in the future; the backoff here is 1ms.
    assert!(wait_until(|| q.run_once().unwrap_or(false)), "retry ran");
    assert_eq!(attempts.load(Ordering::SeqCst), 2);
    assert_eq!(count(&db, "SELECT COUNT(*) AS n FROM sutegi_jobs"), 0);

    // Terminal failure keeps the row as a dead letter, with the reason.
    let id = q.job("doomed", Json::Null).dispatch().expect("dispatch");
    q.run_once().expect("attempt");
    let failed = q.failed(10).expect("failed list");
    assert_eq!(failed.len(), 1);
    assert_eq!(failed[0].get("id").and_then(Json::as_i64), Some(id));
    assert_eq!(
        failed[0].get("last_error").and_then(Json::as_str),
        Some("always")
    );
    assert!(
        !q.run_once().expect("run"),
        "a dead letter is not claimable"
    );

    // …and can be put back by hand.
    assert!(q.retry(id, 1).expect("retry"));
    assert!(q.run_once().expect("run"), "revived job ran again");
    assert!(!q.retry(id + 999, 1).expect("retry"), "no such job");
}

#[test]
fn a_panicking_handler_dead_letters_instead_of_taking_the_worker() {
    let (_tmp, _db, mut q) = queue("panic");
    q.register("boom", |_job| panic!("handler exploded"));
    q.register("ok", |_job| Ok(()));

    q.dispatch("boom", Json::Null).expect("dispatch");
    q.run_once().expect("the panic is caught, not propagated");
    let failed = q.failed(10).expect("failed");
    assert_eq!(failed.len(), 1);
    assert!(failed[0]
        .get("last_error")
        .and_then(Json::as_str)
        .unwrap_or_default()
        .contains("panicked"));

    // The queue still works afterwards.
    q.dispatch("ok", Json::Null).expect("dispatch");
    assert!(q.run_once().expect("run"));
}

#[test]
fn an_unregistered_name_is_a_failure_not_a_silent_drop() {
    let (_tmp, _db, q) = queue("unregistered");
    q.dispatch("nobody.handles.this", Json::Null)
        .expect("dispatch");
    q.run_once().expect("run");
    let failed = q.failed(10).expect("failed");
    assert_eq!(failed.len(), 1);
    assert!(failed[0]
        .get("last_error")
        .and_then(Json::as_str)
        .unwrap_or_default()
        .contains("no handler registered"));
}

#[test]
fn a_delayed_job_is_not_claimable_before_its_time() {
    let (_tmp, _db, mut q) = queue("delay");
    q.register("later", |_job| Ok(()));
    q.job("later", Json::Null)
        .delay(Duration::from_secs(60))
        .dispatch()
        .expect("dispatch");
    assert!(!q.run_once().expect("run"), "scheduled for the future");

    let stats = q.stats().expect("stats");
    assert_eq!(stats.get("scheduled").and_then(Json::as_i64), Some(1));
    assert_eq!(stats.get("ready").and_then(Json::as_i64), Some(0));
}

#[test]
fn priority_wins_over_arrival_order() {
    let (_tmp, _db, mut q) = queue("priority");
    let order = Arc::new(Mutex::new(Vec::<String>::new()));
    let sink = Arc::clone(&order);
    q.register("task", move |job| {
        let tag = job
            .payload()
            .get("tag")
            .and_then(Json::as_str)
            .unwrap_or("");
        sink.lock().unwrap().push(tag.to_string());
        Ok(())
    });

    for (tag, prio) in [("low", 0), ("urgent", 10), ("mid", 5)] {
        q.job("task", Json::obj(vec![("tag", Json::str(tag))]))
            .priority(prio)
            .dispatch()
            .expect("dispatch");
    }
    while q.run_once().expect("run") {}
    assert_eq!(order.lock().unwrap().as_slice(), ["urgent", "mid", "low"]);
}

#[test]
fn named_queues_do_not_see_each_others_work() {
    let (_tmp, _db, mut q) = queue("queues");
    let ran = Arc::new(Mutex::new(Vec::<String>::new()));
    let sink = Arc::clone(&ran);
    q.register("job", move |job| {
        sink.lock().unwrap().push(job.queue.to_string());
        Ok(())
    });

    q.job("job", Json::Null)
        .queue("video")
        .dispatch()
        .expect("dispatch");
    q.dispatch("job", Json::Null).expect("dispatch");

    // The default pool must not drain the video queue…
    assert!(q.run_once().expect("run"));
    assert!(!q.run_once().expect("run"));
    assert_eq!(ran.lock().unwrap().as_slice(), ["default"]);

    // …and the video queue still has its own row.
    let stop = AtomicBool::new(false);
    assert!(q.run_once_on("video", &stop).expect("run"));
    assert_eq!(ran.lock().unwrap().as_slice(), ["default", "video"]);
    assert_eq!(
        q.stats_for("video")
            .unwrap()
            .get("total")
            .and_then(Json::as_i64),
        Some(0)
    );
}

#[test]
fn a_dedupe_key_collapses_duplicates_while_one_is_live() {
    let (_tmp, db, mut q) = queue("dedupe");
    q.register("ingest", |_job| Ok(()));

    let first = q
        .job("ingest", Json::Null)
        .unique("yt:abc")
        .dispatch()
        .expect("dispatch");
    let second = q
        .job("ingest", Json::Null)
        .unique("yt:abc")
        .dispatch()
        .expect("dispatch is not an error — it returns the live row");
    assert_eq!(first, second, "same key while in flight = same job");
    assert_eq!(count(&db, "SELECT COUNT(*) AS n FROM sutegi_jobs"), 1);

    // A different key is a different job.
    let other = q
        .job("ingest", Json::Null)
        .unique("yt:xyz")
        .dispatch()
        .expect("dispatch");
    assert_ne!(first, other);

    // Once the work is done the key is free again.
    while q.run_once().expect("run") {}
    let third = q
        .job("ingest", Json::Null)
        .unique("yt:abc")
        .dispatch()
        .expect("dispatch");
    assert_ne!(third, first, "a finished job does not block the next one");
}

#[test]
fn a_dead_lettered_key_does_not_block_a_fresh_dispatch() {
    let (_tmp, _db, mut q) = queue("dedupe-failed");
    q.register("ingest", |_job| Err("nope".into()));
    let first = q
        .job("ingest", Json::Null)
        .unique("yt:abc")
        .dispatch()
        .expect("dispatch");
    q.run_once().expect("run");
    assert_eq!(q.failed(10).unwrap().len(), 1);

    let again = q
        .job("ingest", Json::Null)
        .unique("yt:abc")
        .dispatch()
        .expect("the dead letter must not own the key forever");
    assert_ne!(again, first);
}

#[test]
fn a_job_whose_worker_died_is_reclaimed_after_the_visibility_timeout() {
    let tmp = TempDb::new("crash");
    let db = tmp.open();
    let mut q = Queue::new(db.clone()).visibility_timeout(Duration::from_millis(50));
    q.register("resume", |_job| Ok(()));
    q.migrate().expect("migrate");

    let id = q.dispatch("resume", Json::Null).expect("dispatch");
    // Simulate a worker that claimed the row and then died: locked, never
    // settled.
    db.execute(
        "UPDATE sutegi_jobs SET locked_at = ?, attempts = 1 WHERE id = ?",
        &[Value::Int(now_ms()), Value::Int(id)],
    )
    .expect("lock");
    assert!(!q.run_once().expect("run"), "still inside the lease");

    std::thread::sleep(Duration::from_millis(70));
    assert!(q.run_once().expect("run"), "lease expired, work resumed");
    assert_eq!(count(&db, "SELECT COUNT(*) AS n FROM sutegi_jobs"), 0);
}

#[test]
fn a_heartbeat_keeps_a_long_job_from_being_stolen() {
    let tmp = TempDb::new("heartbeat");
    let db = tmp.open();
    let mut q = Queue::new(db.clone()).visibility_timeout(Duration::from_millis(50));
    q.register("slow", |job| {
        // Outlive the visibility timeout, but say so while doing it.
        for _ in 0..4 {
            std::thread::sleep(Duration::from_millis(20));
            job.heartbeat()?;
            assert!(!job.should_stop());
        }
        Ok(())
    });
    q.migrate().expect("migrate");
    q.dispatch("slow", Json::Null).expect("dispatch");

    let q = Arc::new(q);
    let runner = Arc::clone(&q);
    let worker = std::thread::spawn(move || runner.run_once().expect("run"));
    // While the handler heartbeats, a second worker must find nothing.
    std::thread::sleep(Duration::from_millis(60));
    assert!(!q.run_once().expect("run"), "lease was kept alive");
    assert!(worker.join().expect("join"));
    assert_eq!(count(&db, "SELECT COUNT(*) AS n FROM sutegi_jobs"), 0);
}

#[test]
fn concurrent_workers_run_each_job_exactly_once() {
    let (_tmp, db, mut q) = queue("concurrency");
    let done = Arc::new(Mutex::new(Vec::<i64>::new()));
    let sink = Arc::clone(&done);
    q.register("work", move |job| {
        let n = job.payload().get("n").and_then(Json::as_i64).unwrap_or(-1);
        // Long enough that workers genuinely overlap.
        std::thread::sleep(Duration::from_millis(5));
        sink.lock().unwrap().push(n);
        Ok(())
    });

    const JOBS: i64 = 40;
    for n in 0..JOBS {
        q.dispatch("work", Json::obj(vec![("n", Json::int(n))]))
            .expect("dispatch");
    }

    let q = Arc::new(q);
    let workers = Arc::clone(&q).start(6);
    assert!(
        wait_until(|| count(&db, "SELECT COUNT(*) AS n FROM sutegi_jobs") == 0),
        "queue drained"
    );
    workers.stop();

    let mut seen = done.lock().unwrap().clone();
    seen.sort_unstable();
    assert_eq!(
        seen.len(),
        JOBS as usize,
        "every job ran exactly once: {seen:?}"
    );
    assert_eq!(seen, (0..JOBS).collect::<Vec<_>>());
}

#[test]
fn a_dispatch_wakes_an_idle_worker_without_waiting_out_the_poll_interval() {
    let tmp = TempDb::new("wakeup");
    let db = tmp.open();
    // A poll interval far longer than the assertion window: if the job runs at
    // all, it is because the dispatch notified the sleeping worker.
    let mut q = Queue::new(db.clone()).poll_interval(Duration::from_secs(30));
    q.register("ping", |_job| Ok(()));
    q.migrate().expect("migrate");

    let q = Arc::new(q);
    let workers = Arc::clone(&q).start(1);
    std::thread::sleep(Duration::from_millis(50)); // let the worker reach its wait

    q.dispatch("ping", Json::Null).expect("dispatch");
    let drained = wait_until(|| count(&db, "SELECT COUNT(*) AS n FROM sutegi_jobs") == 0);
    workers.stop();
    assert!(drained, "the worker woke on dispatch, not on the 30s poll");
}

#[test]
fn stats_and_purge_report_the_states_an_operator_asks_about() {
    let (_tmp, _db, mut q) = queue("stats");
    q.register("ok", |_job| Ok(()));
    q.register("bad", |_job| Err("nope".into()));

    q.dispatch("ok", Json::Null).expect("dispatch");
    q.job("ok", Json::Null)
        .delay(Duration::from_secs(300))
        .dispatch()
        .expect("dispatch");
    q.dispatch("bad", Json::Null).expect("dispatch");
    while q.run_once().expect("run") {}

    let stats = q.stats().expect("stats");
    let n = |k: &str| stats.get(k).and_then(Json::as_i64).unwrap_or(-1);
    assert_eq!(n("ready"), 0);
    assert_eq!(n("running"), 0);
    assert_eq!(n("scheduled"), 1, "the delayed job");
    assert_eq!(n("failed"), 1, "the dead letter");
    assert_eq!(n("total"), 2);

    assert_eq!(q.purge_failed(Duration::from_secs(3600)).expect("purge"), 0);
    assert_eq!(q.purge_failed(Duration::ZERO).expect("purge"), 1);
    assert_eq!(
        q.stats().unwrap().get("failed").and_then(Json::as_i64),
        Some(0)
    );
}

#[test]
fn sqlite_claims_are_process_scoped_and_say_so() {
    let (_tmp, _db, q) = queue("caps");
    assert!(
        !q.cross_pod(),
        "SQLite relies on serialized writers, not SKIP LOCKED — the capability must not overclaim"
    );
}

#[test]
fn a_job_logs_under_the_context_it_was_dispatched_from() {
    let (_tmp, db, mut q) = queue("trace");
    q.register("mail", |_job| {
        sutegi_log::info!("app", "sending");
        Ok(())
    });
    {
        let _request = sutegi_log::scope(&[
            ("rid", sutegi_log::Value::Str("req-42".into())),
            ("ws", sutegi_log::Value::Int(9)),
        ]);
        q.dispatch("mail", Json::Null).expect("dispatch");
    }
    assert_eq!(
        count(
            &db,
            "SELECT COUNT(*) AS n FROM sutegi_jobs WHERE trace IS NOT NULL"
        ),
        1
    );

    let (ran, records) = sutegi_log::capture(|| q.run_once().expect("run"));
    assert!(ran);
    let sending = records
        .iter()
        .find(|r| r.msg == "sending")
        .expect("the handler logged");
    assert_eq!(
        sending.field("rid").and_then(|v| v.as_str()),
        Some("req-42")
    );
    assert_eq!(sending.field("ws"), Some(&sutegi_log::Value::Int(9)));
    assert_eq!(sending.field("job").and_then(|v| v.as_str()), Some("mail"));
    assert_eq!(sending.field("attempt"), Some(&sutegi_log::Value::Int(1)));
    let done = records
        .iter()
        .find(|r| r.msg == "job done")
        .expect("the outcome is logged");
    assert_eq!(done.field("rid").and_then(|v| v.as_str()), Some("req-42"));
    assert!(sutegi_log::get("rid").is_none());
}

#[test]
fn a_failed_attempt_is_logged_before_it_retries() {
    let (_tmp, _db, mut q) = queue("trace-retry");
    q.register("flaky", |_job| Err("upstream 503".into()));
    q.job("flaky", Json::Null)
        .max_attempts(2)
        .dispatch()
        .expect("dispatch");
    let (_, records) = sutegi_log::capture(|| q.run_once().expect("run"));
    let failed = records
        .iter()
        .find(|r| r.target == "queue")
        .expect("the failure is logged");
    assert_eq!(failed.level, sutegi_log::Level::Warn);
    assert_eq!(
        failed.field("error").and_then(|v| v.as_str()),
        Some("upstream 503")
    );
}

#[test]
fn a_dispatch_outside_any_context_stores_no_trace() {
    let (_tmp, db, q) = queue("trace-none");
    q.dispatch("x", Json::Null).expect("dispatch");
    assert_eq!(
        count(
            &db,
            "SELECT COUNT(*) AS n FROM sutegi_jobs WHERE trace IS NULL"
        ),
        1
    );
}

#[test]
fn migrate_adds_the_trace_column_to_a_table_created_before_it() {
    let tmp = TempDb::new("trace-upgrade");
    let db = tmp.open();
    db.execute(
        "CREATE TABLE sutegi_jobs (id INTEGER PRIMARY KEY AUTOINCREMENT, \
         queue TEXT NOT NULL DEFAULT 'default', name TEXT NOT NULL, payload TEXT NOT NULL, \
         priority INTEGER NOT NULL DEFAULT 0, attempts INTEGER NOT NULL DEFAULT 0, \
         max_attempts INTEGER NOT NULL DEFAULT 1, unique_key TEXT, \
         run_at BIGINT NOT NULL DEFAULT 0, locked_at BIGINT, failed_at BIGINT, \
         last_error TEXT, created_at BIGINT NOT NULL DEFAULT 0)",
        &[],
    )
    .expect("old schema");
    db.execute(
        "INSERT INTO sutegi_jobs (name, payload) VALUES ('old', 'null')",
        &[],
    )
    .expect("queued before the upgrade");

    let mut q = Queue::new(db.clone());
    q.migrate().expect("migrate upgrades");
    q.migrate().expect("and stays idempotent");
    let ran = Arc::new(AtomicU32::new(0));
    let seen = Arc::clone(&ran);
    q.register("old", move |_job| {
        seen.fetch_add(1, Ordering::Relaxed);
        Ok(())
    });
    assert!(q.run_once().expect("the old row still runs"));
    assert_eq!(ran.load(Ordering::Relaxed), 1);

    let _s = sutegi_log::scope(&[("rid", sutegi_log::Value::Str("r".into()))]);
    q.dispatch("old", Json::Null)
        .expect("new rows carry a trace");
}
