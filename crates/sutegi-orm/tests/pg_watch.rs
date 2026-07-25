//! Live integration tests for reactive queries on Postgres: cluster-scoped
//! change delivery over LISTEN/NOTIFY + statement triggers. Two independent
//! `Pg` handles are two sessions — mechanically the same wire path as two
//! pods. Runs only when `SUTEGI_PG_TEST_URL` is set.

#![cfg(feature = "postgres")]

use std::time::Duration;

use sutegi_json::Json;
use sutegi_orm::pg::Pg;
use sutegi_orm::watch::Watcher;
use sutegi_orm::{Backend, QueryBuilder, Transactional, Value};

fn db() -> Option<Pg> {
    let url = std::env::var("SUTEGI_PG_TEST_URL").ok()?;
    Some(Pg::connect(&url, 4).unwrap())
}

#[test]
fn cross_session_writes_push_diffs() {
    let Some(pg) = db() else {
        eprintln!("skipping: SUTEGI_PG_TEST_URL not set");
        return;
    };
    pg.pool()
        .batch(
            "DROP TABLE IF EXISTS watch_todos; \
             CREATE TABLE watch_todos (id BIGSERIAL PRIMARY KEY, title TEXT, done BOOLEAN)",
        )
        .unwrap();

    // Watch on one handle…
    let watcher = Watcher::postgres(&pg).unwrap();
    let sub = watcher
        .watch(
            QueryBuilder::table("watch_todos").filter("done", "=", Value::Bool(false)),
            "id",
        )
        .unwrap();
    assert!(sub.rows().is_empty());
    // …trigger setup is idempotent across watchers.
    let watcher2 = Watcher::postgres(&pg).unwrap();
    let sub2 = watcher2
        .watch(QueryBuilder::table("watch_todos"), "id")
        .unwrap();

    // Write from an entirely different pool ("another pod").
    let other = db().unwrap();
    let id = other
        .insert(
            "watch_todos",
            &[
                ("title", Value::Text("from another pod".into())),
                ("done", Value::Bool(false)),
            ],
            "id",
        )
        .unwrap();
    let change = sub.recv_timeout(Duration::from_secs(5)).expect("added");
    assert_eq!(change.added.len(), 1);
    assert_eq!(
        change.added[0].get("title").and_then(Json::as_str),
        Some("from another pod")
    );
    // Both watchers (≈ both pods) saw it.
    assert!(sub2.recv_timeout(Duration::from_secs(5)).is_some());

    // The "psql from outside" case: a raw SQL write from a third session —
    // no framework code in the write path, the trigger still notifies.
    other
        .pool()
        .batch(&format!(
            "UPDATE watch_todos SET done = true WHERE id = {id}"
        ))
        .unwrap();
    let change = sub.recv_timeout(Duration::from_secs(5)).expect("removed");
    // The row left the watched (done = false) result.
    assert_eq!(change.removed.len(), 1);

    // Triggers/functions stay out of schema introspection (drift-clean).
    let tables: Vec<String> = pg
        .introspect()
        .unwrap()
        .into_iter()
        .map(|t| t.table)
        .collect();
    assert!(tables.contains(&"watch_todos".to_string()));

    // Uncommitted writes never fire: NOTIFY is transactional.
    let miss = other.transact(|tx| {
        tx.execute(
            "INSERT INTO watch_todos (title, done) VALUES (?, ?)",
            &[Value::Text("rolled back".into()), Value::Bool(false)],
        )?;
        Err::<(), String>("abort".into())
    });
    assert!(miss.is_err());
    assert!(sub.recv_timeout(Duration::from_millis(500)).is_none());
}
