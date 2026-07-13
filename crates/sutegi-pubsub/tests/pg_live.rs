//! Live cross-broker tests against a real PostgreSQL server — the Phase 2
//! exit test: broadcast between two independent broker instances through
//! `LISTEN`/`NOTIFY`.
//!
//! They run only when `SUTEGI_PG_TEST_URL` is set (same switch as the other
//! live-PG suites), so `cargo test` stays green without a database.

#![cfg(feature = "postgres")]

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use sutegi_pg::Config;
use sutegi_pubsub::{Broker, BrokerExt, PgPubSub};

fn config() -> Option<Config> {
    let url = std::env::var("SUTEGI_PG_TEST_URL").ok()?;
    Some(Config::from_url(&url).expect("SUTEGI_PG_TEST_URL must parse"))
}

macro_rules! require_db {
    () => {
        match config() {
            Some(c) => c,
            None => {
                eprintln!("skipping: SUTEGI_PG_TEST_URL not set");
                return;
            }
        }
    };
}

fn sink() -> (Arc<Mutex<Vec<String>>>, impl Fn(&str) + Send + Sync) {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let s = Arc::clone(&seen);
    (seen, move |m: &str| s.lock().unwrap().push(m.to_string()))
}

/// Poll until `pred` holds or the deadline passes. NOTIFY delivery is fast
/// but asynchronous; a fixed sleep would be either flaky or slow.
fn wait_until(deadline: Duration, pred: impl Fn() -> bool) -> bool {
    let start = Instant::now();
    while start.elapsed() < deadline {
        if pred() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    pred()
}

#[test]
fn broadcast_crosses_broker_instances() {
    let cfg = require_db!();
    // Two brokers = two pods. A dedicated channel isolates this test run.
    let a = PgPubSub::connect_on(&cfg, "sutegi_pubsub_it_cross").unwrap();
    let b = PgPubSub::connect_on(&cfg, "sutegi_pubsub_it_cross").unwrap();
    assert_ne!(a.instance_id(), b.instance_id());

    let (seen_b, cb) = sink();
    let _ = b.on("room:42", cb);

    a.publish("room:42", "over the wire");
    assert!(
        wait_until(Duration::from_secs(5), || !seen_b
            .lock()
            .unwrap()
            .is_empty()),
        "message never crossed brokers"
    );
    assert_eq!(&*seen_b.lock().unwrap(), &["over the wire".to_string()]);
}

#[test]
fn local_delivery_is_synchronous_and_echo_is_suppressed() {
    let cfg = require_db!();
    let bus = PgPubSub::connect_on(&cfg, "sutegi_pubsub_it_echo").unwrap();
    let (seen, cb) = sink();
    let _ = bus.on("t", cb);

    bus.publish("t", "once");
    // Local delivery happened synchronously inside publish().
    assert_eq!(&*seen.lock().unwrap(), &["once".to_string()]);

    // The NOTIFY round-trip must NOT deliver it a second time. Prove the
    // round-trip has completed by sending a second message from a peer and
    // waiting for it, then check "once" is still there exactly once.
    let peer = PgPubSub::connect_on(&cfg, "sutegi_pubsub_it_echo").unwrap();
    peer.publish("t", "marker");
    assert!(wait_until(Duration::from_secs(5), || {
        seen.lock().unwrap().len() >= 2
    }));
    let log = seen.lock().unwrap().clone();
    assert_eq!(log, vec!["once".to_string(), "marker".to_string()]);
}

#[test]
fn topics_are_isolated_and_unsubscribe_works_across_pods() {
    let cfg = require_db!();
    let a = PgPubSub::connect_on(&cfg, "sutegi_pubsub_it_iso").unwrap();
    let b = PgPubSub::connect_on(&cfg, "sutegi_pubsub_it_iso").unwrap();

    let (seen_x, cb_x) = sink();
    let (seen_y, cb_y) = sink();
    let id = b.on("x", cb_x);
    let _ = b.on("y", cb_y);

    a.publish("x", "for x");
    a.publish("y", "for y");
    assert!(wait_until(Duration::from_secs(5), || {
        !seen_x.lock().unwrap().is_empty() && !seen_y.lock().unwrap().is_empty()
    }));
    assert_eq!(&*seen_x.lock().unwrap(), &["for x".to_string()]);
    assert_eq!(&*seen_y.lock().unwrap(), &["for y".to_string()]);

    // Unsubscribed topics stop delivering, even cross-pod.
    b.unsubscribe("x", id);
    a.publish("x", "after unsubscribe");
    a.publish("y", "still alive");
    assert!(wait_until(Duration::from_secs(5), || {
        seen_y.lock().unwrap().len() == 2
    }));
    assert_eq!(&*seen_x.lock().unwrap(), &["for x".to_string()]);
}

#[test]
fn oversized_payloads_fail_cross_pod_but_deliver_locally() {
    let cfg = require_db!();
    let bus = PgPubSub::connect_on(&cfg, "sutegi_pubsub_it_size").unwrap();
    let (seen, cb) = sink();
    let _ = bus.on("big", cb);

    let huge = "x".repeat(9000);
    let err = bus.try_publish("big", &huge).unwrap_err();
    assert!(err.contains("NOTIFY cap"), "got: {err}");
    // Local subscribers still got it (same-pod behavior is unaffected).
    assert_eq!(seen.lock().unwrap().len(), 1);
}
