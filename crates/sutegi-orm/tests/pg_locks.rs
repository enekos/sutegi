//! Live integration tests for Postgres advisory locks through the `Backend`
//! seam. Runs only when `SUTEGI_PG_TEST_URL` is set. Each `Pg` handle takes
//! locks on a dedicated session, so two handles in one process exercise the
//! same paths as two pods.

#![cfg(feature = "postgres")]

use std::time::Duration;

use sutegi_orm::pg::Pg;
use sutegi_orm::{Backend, CapScope, Transactional};

fn db() -> Option<Pg> {
    let url = std::env::var("SUTEGI_PG_TEST_URL").ok()?;
    Some(Pg::connect(&url, 4).unwrap())
}

#[test]
fn advisory_locks_are_cluster_scoped_sessions() {
    let Some(pg) = db() else {
        eprintln!("skipping: SUTEGI_PG_TEST_URL not set");
        return;
    };
    assert_eq!(pg.capabilities().advisory_locks, CapScope::Cluster);
    let other = db().unwrap(); // an independent handle — "another pod"

    let guard = pg.try_lock("pg-lock-a").unwrap().expect("first acquire");
    assert_eq!(guard.name(), "pg-lock-a");
    // Held against every other session, including this handle's own pool.
    assert!(other.try_lock("pg-lock-a").unwrap().is_none());
    assert!(pg.try_lock("pg-lock-a").unwrap().is_none());
    // Distinct name is free.
    let _b = other.try_lock("pg-lock-b").unwrap().expect("other name");

    // Dropping the guard closes the session; the server releases the lock —
    // the same mechanism a crashed holder relies on.
    drop(guard);
    let reacquired = other.try_lock("pg-lock-a").unwrap();
    assert!(reacquired.is_some());
}

#[test]
fn blocking_lock_times_out_and_then_acquires() {
    let Some(pg) = db() else {
        eprintln!("skipping: SUTEGI_PG_TEST_URL not set");
        return;
    };
    let other = db().unwrap();

    let held = pg.try_lock("pg-lock-wait").unwrap().expect("acquire");
    // Queued behind the holder until statement_timeout cancels the wait.
    let miss = other
        .lock("pg-lock-wait", Duration::from_millis(150))
        .unwrap();
    assert!(miss.is_none());

    // Release from another thread mid-wait; the blocked lock() should win.
    let t = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(200));
        drop(held);
    });
    let won = other.lock("pg-lock-wait", Duration::from_secs(5)).unwrap();
    assert!(won.is_some());
    t.join().unwrap();
}

#[test]
fn xact_lock_releases_at_commit() {
    let Some(pg) = db() else {
        eprintln!("skipping: SUTEGI_PG_TEST_URL not set");
        return;
    };
    let other = db().unwrap();

    pg.transact(|tx| {
        let guard = tx.try_lock("pg-lock-xact")?.expect("xact acquire");
        // Dropping the guard does NOT release a transaction-scoped lock…
        drop(guard);
        assert!(other.try_lock("pg-lock-xact").unwrap().is_none());
        Ok(())
    })
    .unwrap();
    // …the COMMIT does.
    assert!(other.try_lock("pg-lock-xact").unwrap().is_some());
}

#[test]
fn with_lock_runs_exactly_one_of_two_racers() {
    let Some(_pg) = db() else {
        eprintln!("skipping: SUTEGI_PG_TEST_URL not set");
        return;
    };
    // Two "pods" race a singleton job with no wait: exactly one runs.
    let ran: Vec<bool> = [db().unwrap(), db().unwrap()]
        .into_iter()
        .map(|handle| {
            std::thread::spawn(move || {
                handle
                    .with_lock("pg-lock-singleton", Duration::ZERO, || {
                        std::thread::sleep(Duration::from_millis(100));
                        Ok(true)
                    })
                    .unwrap()
                    .is_some()
            })
        })
        .collect::<Vec<_>>()
        .into_iter()
        .map(|t| t.join().unwrap())
        .collect();
    assert_eq!(ran.iter().filter(|r| **r).count(), 1, "ran: {ran:?}");
}
