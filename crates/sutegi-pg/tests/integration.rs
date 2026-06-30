//! Live integration tests against a real PostgreSQL server.
//!
//! They run only when `SUTEGI_PG_TEST_URL` is set, so `cargo test` stays green
//! in environments without a database. Spin one up with:
//!
//! ```text
//! docker run -d --name pg -e POSTGRES_PASSWORD=secret -e POSTGRES_USER=sutegi \
//!     -e POSTGRES_DB=sutegi_test -p 5544:5432 postgres:16
//! export SUTEGI_PG_TEST_URL=postgres://sutegi:secret@localhost:5544/sutegi_test
//! ```

use sutegi_json::Json;
use sutegi_pg::{Client, Config, PgValue, Pool};

fn url() -> Option<String> {
    std::env::var("SUTEGI_PG_TEST_URL").ok()
}

macro_rules! require_db {
    () => {
        match url() {
            Some(u) => u,
            None => {
                eprintln!("skipping: SUTEGI_PG_TEST_URL not set");
                return;
            }
        }
    };
}

#[test]
fn scram_auth_and_roundtrip() {
    let url = require_db!();
    let mut client = Client::connect(&Config::from_url(&url).unwrap())
        .expect("connect + SCRAM-SHA-256 auth should succeed");

    client.batch("DROP TABLE IF EXISTS pg_it_todos").unwrap();
    client
        .batch(
            "CREATE TABLE pg_it_todos (\
                id BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY, \
                title TEXT NOT NULL, \
                done BOOLEAN NOT NULL, \
                score DOUBLE PRECISION)",
        )
        .unwrap();

    // Parameterized insert with mixed types, including a NULL.
    let affected = client
        .execute(
            "INSERT INTO pg_it_todos (title, done, score) VALUES ($1, $2, $3)",
            &[
                PgValue::Text("ship pg driver".into()),
                PgValue::Bool(false),
                PgValue::Null,
            ],
        )
        .unwrap();
    assert_eq!(affected, 1);

    client
        .execute(
            "INSERT INTO pg_it_todos (title, done, score) VALUES ($1, $2, $3)",
            &[
                PgValue::Text("write tests".into()),
                PgValue::Bool(true),
                PgValue::Real(9.5),
            ],
        )
        .unwrap();

    // Typed decoding: bool stays bool, bigint stays int, float stays float.
    let rows = client
        .query(
            "SELECT id, title, done, score FROM pg_it_todos ORDER BY id",
            &[],
        )
        .unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(
        rows[0].get("title").and_then(Json::as_str),
        Some("ship pg driver")
    );
    assert_eq!(rows[0].get("done").and_then(Json::as_bool), Some(false));
    assert_eq!(rows[0].get("score"), Some(&Json::Null));
    assert_eq!(rows[1].get("done").and_then(Json::as_bool), Some(true));
    assert_eq!(rows[1].get("score").and_then(Json::as_f64), Some(9.5));
    // IDENTITY column produced real integers.
    assert_eq!(rows[0].get("id").and_then(Json::as_i64), Some(1));

    // RETURNING flows back through the same row path.
    let returned = client
        .query(
            "INSERT INTO pg_it_todos (title, done) VALUES ($1, $2) RETURNING id",
            &[PgValue::Text("third".into()), PgValue::Bool(false)],
        )
        .unwrap();
    assert_eq!(returned[0].get("id").and_then(Json::as_i64), Some(3));

    // UPDATE / DELETE affected-row counts.
    let updated = client
        .execute(
            "UPDATE pg_it_todos SET done = $1 WHERE done = $2",
            &[PgValue::Bool(true), PgValue::Bool(false)],
        )
        .unwrap();
    assert_eq!(updated, 2);

    let deleted = client
        .execute("DELETE FROM pg_it_todos WHERE id = $1", &[PgValue::Int(1)])
        .unwrap();
    assert_eq!(deleted, 1);

    client.batch("DROP TABLE pg_it_todos").unwrap();
}

#[test]
fn pool_shares_connections_across_threads() {
    let url = require_db!();
    let pool = Pool::new(Config::from_url(&url).unwrap(), 4);
    pool.batch("DROP TABLE IF EXISTS pg_it_pool").unwrap();
    pool.batch("CREATE TABLE pg_it_pool (n INTEGER NOT NULL)")
        .unwrap();

    let handles: Vec<_> = (0..16)
        .map(|i| {
            let pool = pool.clone();
            std::thread::spawn(move || {
                pool.execute("INSERT INTO pg_it_pool (n) VALUES ($1)", &[PgValue::Int(i)])
                    .unwrap();
            })
        })
        .collect();
    for h in handles {
        h.join().unwrap();
    }

    let rows = pool
        .query("SELECT COUNT(*) AS c FROM pg_it_pool", &[])
        .unwrap();
    assert_eq!(rows[0].get("c").and_then(Json::as_i64), Some(16));
    pool.batch("DROP TABLE pg_it_pool").unwrap();
}

#[test]
fn reports_sql_errors() {
    let url = require_db!();
    let mut client = Client::connect(&Config::from_url(&url).unwrap()).unwrap();
    let err = client
        .query("SELECT * FROM table_that_does_not_exist", &[])
        .unwrap_err();
    assert!(err.contains("postgres error"), "got: {err}");
    // Connection must still be usable after a query error (Sync recovers it).
    let ok = client.query("SELECT 1 AS one", &[]).unwrap();
    assert_eq!(ok[0].get("one").and_then(Json::as_i64), Some(1));
}
