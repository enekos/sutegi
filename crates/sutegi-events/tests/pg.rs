//! Live integration test: the event store + projections against a real
//! PostgreSQL — the multi-pod path. Runs only when `SUTEGI_PG_TEST_URL` is set
//! (CI provides a postgres:16 service). One sequential test fn: the store
//! tables are shared fixtures, so the phases must not interleave.

use std::sync::Arc;
use std::thread;

use sutegi_events::{append_tx, event, EventError, EventStore, Expected, Projections};
use sutegi_json::Json;
use sutegi_orm::pg::Pg;
use sutegi_orm::{Backend, Transactional, Value};

fn db() -> Option<Pg> {
    let url = std::env::var("SUTEGI_PG_TEST_URL").ok()?;
    Some(Pg::connect(&url, 8).unwrap())
}

fn deposited(amount: i64) -> sutegi_events::NewEvent {
    event("deposited", Json::obj(vec![("amount", Json::int(amount))]))
}

#[test]
fn event_sourcing_over_postgres() {
    let Some(pg) = db() else {
        eprintln!("skipping: SUTEGI_PG_TEST_URL not set");
        return;
    };
    pg.pool()
        .batch(
            "DROP TABLE IF EXISTS sutegi_events; \
             DROP TABLE IF EXISTS sutegi_projections; \
             DROP TABLE IF EXISTS es_totals",
        )
        .unwrap();

    let store = EventStore::new(pg.clone());
    store.migrate().unwrap();
    store.migrate().unwrap(); // idempotent

    // --- optimistic concurrency over PG ---
    assert_eq!(
        store
            .append("account:1", Expected::NoStream, &[deposited(10)])
            .unwrap(),
        1
    );
    assert!(matches!(
        store.append("account:1", Expected::Version(0), &[deposited(1)]),
        Err(EventError::Conflict { actual: 1, .. })
    ));
    assert_eq!(
        store
            .append("account:1", Expected::Version(1), &[deposited(5)])
            .unwrap(),
        2
    );

    // --- concurrent appends: racing writers, gap-free global positions ---
    // Distinct streams so every collision is a *position* race (retried
    // internally), exercising the head-of-log serialization.
    let threads: Vec<_> = (0..4)
        .map(|t| {
            let store = EventStore::new(pg.clone());
            thread::spawn(move || {
                for i in 0..10 {
                    store
                        .append(&format!("racer:{t}"), Expected::Any, &[deposited(i)])
                        .unwrap();
                }
            })
        })
        .collect();
    for t in threads {
        t.join().unwrap();
    }
    let all = store.read_all(0, 1_000).unwrap();
    assert_eq!(all.len(), 42); // 2 setup events + 4 threads * 10
    let positions: Vec<i64> = all.iter().map(|e| e.position).collect();
    assert_eq!(positions, (1..=42).collect::<Vec<i64>>());
    for t in 0..4 {
        let versions: Vec<i64> = store
            .read_stream(&format!("racer:{t}"), 0)
            .unwrap()
            .iter()
            .map(|e| e.version)
            .collect();
        assert_eq!(versions, (1..=10).collect::<Vec<i64>>());
    }

    // --- projection with a transactional read model ---
    pg.execute(
        "CREATE TABLE es_totals (stream TEXT PRIMARY KEY, total BIGINT NOT NULL DEFAULT 0)",
        &[],
    )
    .unwrap();
    let mut projections = Projections::new(pg.clone());
    projections.register("es_totals", |e, tx| {
        let amount = e.payload.get("amount").and_then(Json::as_i64).unwrap_or(0);
        tx.execute(
            "INSERT INTO es_totals (stream, total) VALUES (?, ?) \
             ON CONFLICT (stream) DO UPDATE SET total = es_totals.total + excluded.total",
            &[Value::Text(e.stream.clone()), Value::Int(amount)],
        )?;
        Ok(())
    });
    projections.migrate().unwrap();
    let projections = Arc::new(projections);
    while projections.run_once().unwrap() > 0 {}
    let total = |stream: &str| {
        pg.query_one(
            "SELECT total FROM es_totals WHERE stream = ?",
            &[Value::Text(stream.to_string())],
        )
        .unwrap()
        .as_ref()
        .and_then(|r| r.get("total"))
        .and_then(Json::as_i64)
        .unwrap_or(0)
    };
    assert_eq!(total("account:1"), 15);
    assert_eq!(total("racer:0"), 45); // 0+1+…+9
    assert_eq!(projections.position("es_totals").unwrap(), 42);

    // --- rebuild from the log ---
    pg.execute("DELETE FROM es_totals", &[]).unwrap();
    projections.reset("es_totals").unwrap();
    while projections.run_once().unwrap() > 0 {}
    assert_eq!(total("account:1"), 15);

    // --- append_tx composes with a caller-owned PG transaction ---
    let failed: Result<(), String> = pg.transact(|tx| {
        append_tx(tx, "account:1", Expected::Any, &[deposited(999)])?;
        Err("boom".to_string())
    });
    assert!(failed.is_err());
    assert_eq!(store.version("account:1").unwrap(), 2); // rolled back

    let stats = store.stats().unwrap();
    assert_eq!(stats.get("head").and_then(Json::as_i64), Some(42));
    assert_eq!(stats.get("streams").and_then(Json::as_i64), Some(5));
}
