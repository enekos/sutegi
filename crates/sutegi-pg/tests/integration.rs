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
fn prepared_statement_cache_reuse_and_invalidation() {
    let url = require_db!();
    let mut client = Client::connect(&Config::from_url(&url).unwrap()).unwrap();
    client.batch("DROP TABLE IF EXISTS pg_it_cache").unwrap();
    client
        .batch("CREATE TABLE pg_it_cache (id BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY, n INTEGER NOT NULL)")
        .unwrap();

    // Same SQL run repeatedly: the second+ calls reuse the cached prepared
    // statement (Bind/Execute only). Results must stay correct.
    let insert = "INSERT INTO pg_it_cache (n) VALUES ($1)";
    for i in 0..5 {
        assert_eq!(client.execute(insert, &[PgValue::Int(i)]).unwrap(), 1);
    }
    let count = "SELECT COUNT(*) AS c FROM pg_it_cache";
    for _ in 0..2 {
        assert_eq!(
            client.query(count, &[]).unwrap()[0]
                .get("c")
                .and_then(Json::as_i64),
            Some(5)
        );
    }

    // Prime a `SELECT *` plan, then change the table shape on the SAME
    // connection. PostgreSQL invalidates the cached plan; the driver must
    // evict and retry transparently rather than surface the error.
    let star = "SELECT * FROM pg_it_cache ORDER BY id LIMIT 1";
    assert!(client.query(star, &[]).unwrap()[0].get("extra").is_none());
    client
        .batch("ALTER TABLE pg_it_cache ADD COLUMN extra TEXT")
        .unwrap();
    let row = client.query(star, &[]).unwrap();
    assert!(
        row[0].get("extra").is_some(),
        "cached-plan invalidation should transparently re-parse and see the new column"
    );

    client.batch("DROP TABLE pg_it_cache").unwrap();
}

#[test]
fn statement_cache_can_be_disabled() {
    let url = require_db!();
    let mut cfg = Config::from_url(&url).unwrap();
    cfg.statement_cache = false;
    let mut client = Client::connect(&cfg).unwrap();
    // With caching off, the same query still works (unnamed statement each time).
    for _ in 0..3 {
        assert_eq!(
            client.query("SELECT 7 AS n", &[]).unwrap()[0]
                .get("n")
                .and_then(Json::as_i64),
            Some(7)
        );
    }
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
