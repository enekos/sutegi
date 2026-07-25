//! Live integration tests for the concurrency cluster on Postgres: row locks
//! (`FOR UPDATE SKIP LOCKED`), isolation levels, RETURNING on DML, and bulk
//! `COPY FROM STDIN`. Runs only when `SUTEGI_PG_TEST_URL` is set.

#![cfg(feature = "postgres")]

use sutegi_json::Json;
use sutegi_orm::pg::Pg;
use sutegi_orm::{
    Backend, DeleteBuilder, Isolation, QueryBuilder, Transactional, UpdateBuilder, Value,
};

fn db() -> Option<Pg> {
    let url = std::env::var("SUTEGI_PG_TEST_URL").ok()?;
    Some(Pg::connect(&url, 4).unwrap())
}

fn fresh_table(pg: &Pg, table: &str, ddl: &str) {
    pg.pool()
        .batch(&format!("DROP TABLE IF EXISTS {table}; {ddl}"))
        .unwrap();
}

#[test]
fn skip_locked_claims_dont_overlap() {
    let Some(pg) = db() else {
        eprintln!("skipping: SUTEGI_PG_TEST_URL not set");
        return;
    };
    fresh_table(
        &pg,
        "conc_jobs",
        "CREATE TABLE conc_jobs (id BIGINT PRIMARY KEY, taken BOOLEAN NOT NULL DEFAULT FALSE)",
    );
    pg.insert_many(
        "conc_jobs",
        &["id"],
        &(1..=2).map(|i| vec![Value::Int(i)]).collect::<Vec<_>>(),
    )
    .unwrap();

    // Session A claims one row FOR UPDATE SKIP LOCKED inside a transaction…
    let other = db().unwrap();
    pg.transact(|tx| {
        let claim = |exec: &dyn Backend| {
            exec.select(
                &QueryBuilder::table("conc_jobs")
                    .order_by("id", false)
                    .limit(1)
                    .for_update()
                    .skip_locked(),
            )
        };
        let a = claim(tx)?;
        assert_eq!(a[0].get("id").and_then(Json::as_i64), Some(1));
        // …session B's identical claim skips the locked row and gets the next.
        let b = other.transact(|tx2| claim(tx2))?;
        assert_eq!(b[0].get("id").and_then(Json::as_i64), Some(2));
        Ok(())
    })
    .unwrap();
}

#[test]
fn serializable_conflict_surfaces_as_error() {
    let Some(pg) = db() else {
        eprintln!("skipping: SUTEGI_PG_TEST_URL not set");
        return;
    };
    fresh_table(
        &pg,
        "conc_counter",
        "CREATE TABLE conc_counter (id BIGINT PRIMARY KEY, n BIGINT NOT NULL)",
    );
    pg.execute("INSERT INTO conc_counter VALUES (1, 0)", &[])
        .unwrap();

    // Two serializable read-modify-write transactions overlap: one must fail
    // with a serialization error (40001) rather than silently losing a write.
    let other = db().unwrap();
    let read = |exec: &dyn Backend| -> Result<i64, String> {
        Ok(exec
            .query_one("SELECT n FROM conc_counter WHERE id = 1", &[])?
            .and_then(|r| r.get("n").and_then(Json::as_i64))
            .unwrap_or(0))
    };
    let result = pg.transact_with(Isolation::Serializable, |tx| {
        let n = read(tx)?;
        // Overlapping serializable writer commits in between.
        other.transact_with(Isolation::Serializable, |tx2| {
            let m = read(tx2)?;
            tx2.execute(
                "UPDATE conc_counter SET n = ? WHERE id = 1",
                &[Value::Int(m + 1)],
            )
        })?;
        tx.execute(
            "UPDATE conc_counter SET n = ? WHERE id = 1",
            &[Value::Int(n + 1)],
        )
    });
    let err = result.expect_err("second overlapping write must not commit");
    assert!(
        err.contains("40001"),
        "expected serialization failure: {err}"
    );
}

#[test]
fn returning_dml_round_trips_on_pg() {
    let Some(pg) = db() else {
        eprintln!("skipping: SUTEGI_PG_TEST_URL not set");
        return;
    };
    fresh_table(
        &pg,
        "conc_todos",
        "CREATE TABLE conc_todos (id BIGSERIAL PRIMARY KEY, title TEXT NOT NULL, done BOOLEAN NOT NULL)",
    );
    pg.insert(
        "conc_todos",
        &[
            ("title", Value::Text("a".into())),
            ("done", Value::Bool(false)),
        ],
        "id",
    )
    .unwrap();

    let rows = pg
        .update_returning(
            &UpdateBuilder::table("conc_todos")
                .set("done", Value::Bool(true))
                .filter("title", "=", Value::Text("a".into()))
                .returning(&["id", "done"]),
        )
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].get("done").and_then(Json::as_bool), Some(true));

    let rows = pg
        .delete_returning(
            &DeleteBuilder::table("conc_todos")
                .filter("done", "=", Value::Bool(true))
                .returning(&["title"]),
        )
        .unwrap();
    assert_eq!(rows[0].get("title").and_then(Json::as_str), Some("a"));
}

#[test]
fn copy_bulk_insert_round_trips_and_outruns_row_at_a_time() {
    let Some(pg) = db() else {
        eprintln!("skipping: SUTEGI_PG_TEST_URL not set");
        return;
    };
    fresh_table(
        &pg,
        "conc_bulk",
        "CREATE TABLE conc_bulk (id BIGINT PRIMARY KEY, note TEXT, flag BOOLEAN)",
    );

    // Hostile content: tabs, newlines, backslashes, CRs, NULLs — the text
    // format's own metacharacters must survive the round-trip.
    let rows = vec![
        vec![
            Value::Int(1),
            Value::Text("plain".into()),
            Value::Bool(true),
        ],
        vec![
            Value::Int(2),
            Value::Text("tab\there \\ and\nnewline\rcr".into()),
            Value::Bool(false),
        ],
        vec![Value::Int(3), Value::Null, Value::Null],
    ];
    let n = pg
        .insert_many("conc_bulk", &["id", "note", "flag"], &rows)
        .unwrap();
    assert_eq!(n, 3);
    let back = pg
        .select(&QueryBuilder::table("conc_bulk").order_by("id", false))
        .unwrap();
    assert_eq!(
        back[1].get("note").and_then(Json::as_str),
        Some("tab\there \\ and\nnewline\rcr")
    );
    assert!(back[2].get("note").map(|j| j.is_null()).unwrap_or(false));

    // Speedup check: COPY vs one INSERT per row on 5k rows. The spec target
    // is ≥10× on 100k; assert a conservative ≥3× here so the test stays fast
    // and un-flaky on a loaded laptop.
    let big: Vec<Vec<Value>> = (10_000..15_000)
        .map(|i| {
            vec![
                Value::Int(i),
                Value::Text(format!("row {i}")),
                Value::Bool(i % 2 == 0),
            ]
        })
        .collect();
    let t0 = std::time::Instant::now();
    pg.insert_many("conc_bulk", &["id", "note", "flag"], &big)
        .unwrap();
    let copy_time = t0.elapsed();

    pg.pool()
        .batch("DELETE FROM conc_bulk WHERE id >= 10000")
        .unwrap();
    let t0 = std::time::Instant::now();
    for row in &big {
        pg.execute(
            "INSERT INTO conc_bulk (id, note, flag) VALUES (?, ?, ?)",
            row,
        )
        .unwrap();
    }
    let row_time = t0.elapsed();
    eprintln!(
        "COPY: {copy_time:?}  row-at-a-time: {row_time:?}  ratio: {:.1}x",
        row_time.as_secs_f64() / copy_time.as_secs_f64()
    );
    assert!(
        row_time > copy_time * 3,
        "COPY {copy_time:?} vs row-at-a-time {row_time:?}"
    );
}
