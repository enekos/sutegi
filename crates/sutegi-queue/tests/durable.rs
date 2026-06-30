//! Live integration test for the durable PostgreSQL-backed queue. Runs only
//! when `SUTEGI_PG_TEST_URL` is set.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use sutegi_json::Json;
use sutegi_pg::Pool;
use sutegi_queue::Queue;

// Both tests share one `sutegi_jobs` table, so they must not run concurrently
// (one's DROP would nuke the other's rows). Serialize them.
static DB_LOCK: Mutex<()> = Mutex::new(());

fn pool() -> Option<Pool> {
    let url = std::env::var("SUTEGI_PG_TEST_URL").ok()?;
    Some(Pool::new(sutegi_pg::Config::from_url(&url).unwrap(), 8))
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
    let Some(pool) = pool() else {
        eprintln!("skipping: SUTEGI_PG_TEST_URL not set");
        return;
    };
    pool.batch("DROP TABLE IF EXISTS sutegi_jobs").unwrap();

    let processed = Arc::new(Mutex::new(Vec::<String>::new()));
    let fail_first = Arc::new(AtomicU32::new(0));

    let mut queue = Queue::new(pool.clone())
        .poll_interval(Duration::from_millis(20))
        .retry_backoff(Duration::from_millis(1)); // tiny backoff so the test is quick

    let seen = Arc::clone(&processed);
    queue.register("greet", move |payload: &Json| {
        let who = payload.get("who").and_then(Json::as_str).unwrap_or("?");
        seen.lock().unwrap().push(who.to_string());
        Ok(())
    });

    let counter = Arc::clone(&fail_first);
    queue.register("flaky", move |_payload: &Json| {
        // Fail on the first attempt, succeed on the second.
        if counter.fetch_add(1, Ordering::SeqCst) == 0 {
            Err("transient".into())
        } else {
            Ok(())
        }
    });

    queue.migrate().unwrap();

    queue
        .dispatch("greet", Json::obj(vec![("who", Json::str("world"))]))
        .unwrap();
    queue
        .dispatch_with(
            "flaky",
            Json::Null,
            3, // up to 3 attempts
            Duration::ZERO,
        )
        .unwrap();

    let queue = Arc::new(queue);
    let workers = Arc::clone(&queue).start(2);

    // The greet job runs once; the flaky job fails then succeeds on retry.
    assert!(
        wait_until(|| processed.lock().unwrap().contains(&"world".to_string())),
        "greet should have been processed"
    );
    assert!(
        wait_until(|| fail_first.load(Ordering::SeqCst) >= 2),
        "flaky should have been retried and then succeeded"
    );
    // Once both complete, the table drains to empty (no failed dead-letters).
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
    pool.batch("DROP TABLE sutegi_jobs").unwrap();
}

#[test]
fn terminal_failure_becomes_dead_letter() {
    let _guard = DB_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let Some(pool) = pool() else {
        eprintln!("skipping: SUTEGI_PG_TEST_URL not set");
        return;
    };
    pool.batch("DROP TABLE IF EXISTS sutegi_jobs").unwrap();

    let mut queue = Queue::new(pool.clone())
        .poll_interval(Duration::from_millis(20))
        .retry_backoff(Duration::from_millis(1));
    queue.register("always_fails", |_: &Json| Err("nope".into()));
    queue.migrate().unwrap();
    queue
        .dispatch_with("always_fails", Json::Null, 2, Duration::ZERO)
        .unwrap();

    let queue = Arc::new(queue);
    let workers = Arc::clone(&queue).start(1);

    // After exhausting 2 attempts the job is kept as a failed dead-letter.
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
    pool.batch("DROP TABLE sutegi_jobs").unwrap();
}
