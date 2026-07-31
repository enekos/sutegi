//! The Postgres leg of the queue contract — the same behaviour `sqlite.rs`
//! pins, but claimed with `FOR UPDATE SKIP LOCKED` across pods.
//!
//! Needs a live server: `cargo test -p sutegi-queue --features postgres` with
//! `SUTEGI_PG_TEST_URL` set. Without the feature the file compiles to nothing,
//! so the default `cargo test` stays dependency-free.
#![cfg(feature = "postgres")]

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use sutegi_json::Json;
use sutegi_orm::pg::Pg;
use sutegi_orm::Backend;
use sutegi_queue::Queue;

// Both tests share one `sutegi_jobs` table, so they must not run concurrently
// (one's DROP would nuke the other's rows). Serialize them.
static DB_LOCK: Mutex<()> = Mutex::new(());

fn store() -> Option<Pg> {
    let url = std::env::var("SUTEGI_PG_TEST_URL").ok()?;
    Pg::connect(&url, 8).ok()
}

fn wait_until(cond: impl Fn() -> bool) -> bool {
    for _ in 0..200 {
        if cond() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    cond()
}

#[test]
fn dispatch_process_and_retry() {
    let _guard = DB_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let Some(pg) = store() else {
        eprintln!("skipping: SUTEGI_PG_TEST_URL not set");
        return;
    };
    pg.execute("DROP TABLE IF EXISTS sutegi_jobs", &[]).unwrap();

    let processed = Arc::new(Mutex::new(Vec::<String>::new()));
    let fail_first = Arc::new(AtomicU32::new(0));

    let mut queue = Queue::new(pg.clone())
        .poll_interval(Duration::from_millis(20))
        .retry_backoff(Duration::from_millis(1)); // tiny backoff so the test is quick

    let seen = Arc::clone(&processed);
    queue.register("greet", move |job| {
        let who = job
            .payload()
            .get("who")
            .and_then(Json::as_str)
            .unwrap_or("?");
        seen.lock().unwrap().push(who.to_string());
        Ok(())
    });

    let counter = Arc::clone(&fail_first);
    queue.register("flaky", move |_job| {
        // Fail on the first attempt, succeed on the second.
        if counter.fetch_add(1, Ordering::SeqCst) == 0 {
            Err("transient".into())
        } else {
            Ok(())
        }
    });

    queue.migrate().unwrap();
    assert!(
        queue.cross_pod(),
        "Postgres claims must be cluster-scoped (SKIP LOCKED)"
    );

    queue
        .dispatch("greet", Json::obj(vec![("who", Json::str("world"))]))
        .unwrap();
    queue
        .dispatch_with("flaky", Json::Null, 3, Duration::ZERO)
        .unwrap();

    let queue = Arc::new(queue);
    let workers = Arc::clone(&queue).start(2);

    assert!(
        wait_until(|| processed.lock().unwrap().contains(&"world".to_string())),
        "greet should have been processed"
    );
    assert!(
        wait_until(|| fail_first.load(Ordering::SeqCst) >= 2),
        "flaky should have been retried and then succeeded"
    );
    assert!(
        wait_until(|| {
            queue
                .stats()
                .map(|s| s.get("total").and_then(Json::as_i64) == Some(0))
                .unwrap_or(false)
        }),
        "queue should drain to empty; stats: {:?}",
        queue.stats()
    );

    workers.stop();
    queue
        .store()
        .execute("DROP TABLE sutegi_jobs", &[])
        .unwrap();
}

#[test]
fn terminal_failure_becomes_dead_letter() {
    let _guard = DB_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let Some(pg) = store() else {
        eprintln!("skipping: SUTEGI_PG_TEST_URL not set");
        return;
    };
    pg.execute("DROP TABLE IF EXISTS sutegi_jobs", &[]).unwrap();

    let mut queue = Queue::new(pg.clone())
        .poll_interval(Duration::from_millis(20))
        .retry_backoff(Duration::from_millis(1));
    queue.register("always_fails", |_job| Err("nope".into()));
    queue.migrate().unwrap();
    queue
        .dispatch_with("always_fails", Json::Null, 2, Duration::ZERO)
        .unwrap();

    let queue = Arc::new(queue);
    let workers = Arc::clone(&queue).start(1);

    assert!(
        wait_until(|| {
            queue
                .stats()
                .map(|s| s.get("failed").and_then(Json::as_i64) == Some(1))
                .unwrap_or(false)
        }),
        "job should land in the dead-letter (failed) state; stats: {:?}",
        queue.stats()
    );

    workers.stop();
    pg.execute("DROP TABLE sutegi_jobs", &[]).unwrap();
}

#[test]
fn dedupe_keys_and_named_queues_behave_as_on_sqlite() {
    let _guard = DB_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let Some(pg) = store() else {
        eprintln!("skipping: SUTEGI_PG_TEST_URL not set");
        return;
    };
    pg.execute("DROP TABLE IF EXISTS sutegi_jobs", &[]).unwrap();

    let mut queue = Queue::new(pg.clone());
    queue.register("ingest", |_job| Ok(()));
    queue.migrate().unwrap();

    let first = queue
        .job("ingest", Json::Null)
        .queue("video")
        .unique("yt:abc")
        .dispatch()
        .unwrap();
    let second = queue
        .job("ingest", Json::Null)
        .queue("video")
        .unique("yt:abc")
        .dispatch()
        .unwrap();
    assert_eq!(first, second, "the live row is returned, not a duplicate");
    assert!(!queue.run_once().unwrap(), "default queue is empty");
    assert_eq!(
        queue
            .stats_for("video")
            .unwrap()
            .get("ready")
            .and_then(Json::as_i64),
        Some(1)
    );

    pg.execute("DROP TABLE sutegi_jobs", &[]).unwrap();
}
