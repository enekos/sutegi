//! Live migration-reliability tests on Postgres — the backend where the old
//! statement-level `BEGIN`/`COMMIT` was most dangerous, because every
//! `execute` checks a connection out of a pool: a faked transaction spanned
//! connections and left one parked in the pool mid-`BEGIN`. Runs only when
//! `SUTEGI_PG_TEST_URL` is set (same server the other `pg_*` suites use).
//!
//! The history table is shared with the other suites, so every assertion here
//! is scoped to this file's `pgmig_`-prefixed versions and tables — no global
//! counts, no dropping `_sutegi_migrations`. Rollback is the one operation
//! that cannot be scoped — it undoes the newest batch in the table, whoever
//! wrote it — so the tests in this file hold [`serial`] and never interleave.

#![cfg(feature = "postgres")]

use sutegi_orm::migrate::{Migration, Migrator};
use sutegi_orm::pg::Pg;
use sutegi_orm::{Backend, Value};

fn db() -> Option<Pg> {
    let url = std::env::var("SUTEGI_PG_TEST_URL").ok()?;
    Some(Pg::connect(&url, 4).unwrap())
}

fn serial() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Remove this test's tables and history rows so reruns start clean.
fn scrub(pg: &Pg, prefix: &str, tables: &[&str]) {
    for t in tables {
        pg.pool()
            .batch(&format!("DROP TABLE IF EXISTS {t}"))
            .unwrap();
    }
    let _ = pg.execute(
        "DELETE FROM _sutegi_migrations WHERE version LIKE ?",
        &[Value::Text(format!("{prefix}%"))],
    );
}

fn applied_versions(pg: &Pg, prefix: &str) -> Vec<String> {
    let mut v: Vec<String> = pg
        .query(
            "SELECT version FROM _sutegi_migrations WHERE version LIKE ?",
            &[Value::Text(format!("{prefix}%"))],
        )
        .unwrap()
        .iter()
        .filter_map(|r| r.get("version").and_then(sutegi_json::Json::as_str))
        .map(str::to_string)
        .collect();
    v.sort();
    v
}

#[test]
fn failing_migration_is_atomic_on_the_pg_pool_under_traffic() {
    use std::sync::atomic::{AtomicBool, Ordering};

    let _serial = serial();

    let Some(pg) = db() else {
        eprintln!("skipping: SUTEGI_PG_TEST_URL not set");
        return;
    };
    scrub(
        &pg,
        "pgmig_atomic",
        &["pgmig_atomic_half", "pgmig_atomic_noise"],
    );
    Backend::execute(
        &pg,
        "CREATE TABLE pgmig_atomic_noise (id BIGINT PRIMARY KEY)",
        &[],
    )
    .unwrap();

    // Concurrent readers shuffle the pool while migrations run and fail —
    // the load that made a statement-level BEGIN span connections.
    let stop = AtomicBool::new(false);
    let problems = std::thread::scope(|s| {
        for _ in 0..3 {
            let pg = pg.clone();
            let stop = &stop;
            s.spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    let _ = pg.query("SELECT id FROM pgmig_atomic_noise", &[]);
                }
            });
        }

        let mut problems: Vec<String> = Vec::new();
        for i in 0..10 {
            let m = Migrator::new().add(Migration::new(
                format!("pgmig_atomic_{i:03}"),
                "boom",
                |ops| {
                    ops.execute(
                        "CREATE TABLE pgmig_atomic_half (id BIGINT PRIMARY KEY)",
                        &[],
                    )?;
                    ops.execute("INSERT INTO pgmig_atomic_half (id) VALUES (1)", &[])?;
                    Err("deliberate".into())
                },
            ));
            match m.run(&pg) {
                Err(e) if e.contains("deliberate") => {}
                other => problems.push(format!("iteration {i}: unexpected result {other:?}")),
            }
            if pg.query("SELECT 1 FROM pgmig_atomic_half", &[]).is_ok() {
                problems.push(format!(
                    "iteration {i}: failing migration leaked a half-applied table"
                ));
                let _ = pg.pool().batch("DROP TABLE pgmig_atomic_half");
            }
        }
        stop.store(true, Ordering::Relaxed);
        problems
    });

    assert!(problems.is_empty(), "{problems:?}");
    // And the pool came out healthy: no connection stuck inside a BEGIN.
    assert!(applied_versions(&pg, "pgmig_atomic").is_empty());
    Backend::execute(&pg, "INSERT INTO pgmig_atomic_noise (id) VALUES (1)", &[]).unwrap();
    scrub(
        &pg,
        "pgmig_atomic",
        &["pgmig_atomic_half", "pgmig_atomic_noise"],
    );
}

fn racing_migrator() -> Migrator {
    Migrator::new()
        .add(Migration::new("pgmig_race_000", "markers", |ops| {
            ops.execute(
                "CREATE TABLE pgmig_race_markers (version TEXT NOT NULL)",
                &[],
            )
            .map(|_| ())
        }))
        .add(Migration::new("pgmig_race_001", "a", |ops| {
            ops.execute("CREATE TABLE pgmig_race_a (id BIGINT PRIMARY KEY)", &[])?;
            ops.execute(
                "INSERT INTO pgmig_race_markers (version) VALUES ('pgmig_race_001')",
                &[],
            )
            .map(|_| ())
        }))
        .add(Migration::new("pgmig_race_002", "b", |ops| {
            ops.execute("CREATE TABLE pgmig_race_b (id BIGINT PRIMARY KEY)", &[])?;
            ops.execute(
                "INSERT INTO pgmig_race_markers (version) VALUES ('pgmig_race_002')",
                &[],
            )
            .map(|_| ())
        }))
}

#[test]
fn renamed_tables_keep_later_constraint_changes_reversible() {
    use sutegi_orm::schema_diff::SchemaOp;
    use sutegi_orm::{ColType, Column, FkAction, ForeignKey, TableSchema};

    let _serial = serial();
    let Some(pg) = db() else {
        eprintln!("skipping: SUTEGI_PG_TEST_URL not set");
        return;
    };
    scrub(
        &pg,
        "pgmig_rename",
        &["pgmig_old_items", "pgmig_new_items", "pgmig_parents"],
    );

    let parent =
        TableSchema::new("pgmig_parents").column(Column::new("id", ColType::Integer).primary());
    let original = TableSchema::new("pgmig_old_items")
        .column(Column::new("id", ColType::Integer).primary())
        .column(Column::new("slug", ColType::Text).unique())
        .column(Column::new("parent_id", ColType::Integer))
        .foreign_key("parent_id", "pgmig_parents", "id", FkAction::Cascade);
    let migration = Migrator::new().add(Migration::ops(
        "pgmig_rename_constraints",
        "rename_constraints",
        vec![
            SchemaOp::CreateTable(parent),
            SchemaOp::CreateTable(original),
            SchemaOp::RenameTable {
                from: "pgmig_old_items".into(),
                to: "pgmig_new_items".into(),
            },
            SchemaOp::AlterColumn {
                table: "pgmig_new_items".into(),
                from: Column::new("slug", ColType::Text).unique(),
                to: Column::new("slug", ColType::Text),
            },
            SchemaOp::DropForeignKey {
                table: "pgmig_new_items".into(),
                fk: ForeignKey {
                    column: "parent_id".into(),
                    ref_table: "pgmig_parents".into(),
                    ref_column: "id".into(),
                    on_delete: FkAction::Cascade,
                },
            },
        ],
    ));

    migration.run(&pg).unwrap();
    let renamed = pg
        .introspect()
        .unwrap()
        .into_iter()
        .find(|table| table.table == "pgmig_new_items")
        .unwrap();
    assert!(!renamed.col("slug").unwrap().unique);
    assert!(renamed.foreign_keys.is_empty());

    migration.rollback(&pg, 1).unwrap();
    assert!(
        pg.introspect()
            .unwrap()
            .iter()
            .all(|table| { table.table != "pgmig_new_items" && table.table != "pgmig_parents" })
    );
    scrub(
        &pg,
        "pgmig_rename",
        &["pgmig_old_items", "pgmig_new_items", "pgmig_parents"],
    );
}

#[test]
fn dropping_an_indexed_column_removes_the_index_first() {
    use sutegi_orm::schema_diff::diff;
    use sutegi_orm::{ColType, Column, Dialect, TableSchema};

    let _serial = serial();
    let Some(pg) = db() else {
        eprintln!("skipping: SUTEGI_PG_TEST_URL not set");
        return;
    };
    scrub(&pg, "pgmig_drop_indexed", &["pgmig_drop_items"]);
    let original = TableSchema::new("pgmig_drop_items")
        .column(Column::new("id", ColType::Integer).primary())
        .column(Column::new("slug", ColType::Text))
        .index(&["slug"]);
    let desired =
        TableSchema::new("pgmig_drop_items").column(Column::new("id", ColType::Integer).primary());
    let mut ops = diff(&[], std::slice::from_ref(&original), Dialect::Postgres).ops;
    ops.extend(diff(&[original], &[desired], Dialect::Postgres).ops);
    let migration = Migrator::new().add(Migration::ops(
        "pgmig_drop_indexed_column",
        "drop_indexed_column",
        ops,
    ));

    migration.run(&pg).unwrap();
    assert_eq!(
        pg.introspect()
            .unwrap()
            .into_iter()
            .find(|table| table.table == "pgmig_drop_items")
            .unwrap()
            .columns
            .len(),
        1
    );
    scrub(&pg, "pgmig_drop_indexed", &["pgmig_drop_items"]);
}

#[test]
fn racing_pg_runners_apply_each_migration_exactly_once() {
    let _serial = serial();
    let Some(pg) = db() else {
        eprintln!("skipping: SUTEGI_PG_TEST_URL not set");
        return;
    };
    let tables = ["pgmig_race_markers", "pgmig_race_a", "pgmig_race_b"];
    scrub(&pg, "pgmig_race", &tables);

    // Six independent handles = six pods booting at once. The cluster
    // advisory lock must serialize them; plain CREATE TABLE (no IF NOT
    // EXISTS) and unguarded marker INSERTs make any double-apply loud.
    let results: Vec<Vec<String>> = std::thread::scope(|s| {
        (0..6)
            .map(|_| {
                s.spawn(|| {
                    let pod = db().unwrap();
                    racing_migrator().run(&pod).unwrap()
                })
            })
            .collect::<Vec<_>>()
            .into_iter()
            .map(|h| h.join().unwrap())
            .collect()
    });

    let mut all: Vec<String> = results.into_iter().flatten().collect();
    all.sort();
    assert_eq!(
        all,
        vec!["pgmig_race_000", "pgmig_race_001", "pgmig_race_002"]
    );
    assert_eq!(
        applied_versions(&pg, "pgmig_race"),
        vec!["pgmig_race_000", "pgmig_race_001", "pgmig_race_002"]
    );
    let markers = pg
        .query("SELECT COUNT(*) AS c FROM pgmig_race_markers", &[])
        .unwrap()[0]
        .get("c")
        .and_then(sutegi_json::Json::as_i64);
    assert_eq!(markers, Some(2), "a migration body ran more than once");

    scrub(&pg, "pgmig_race", &tables);
}

#[test]
fn pg_rollback_of_a_failing_down_is_atomic() {
    let _serial = serial();
    let Some(pg) = db() else {
        eprintln!("skipping: SUTEGI_PG_TEST_URL not set");
        return;
    };
    scrub(&pg, "pgmig_rb", &["pgmig_rb_t"]);

    let m = Migrator::new().add(Migration::reversible(
        "pgmig_rb_001",
        "t",
        |ops| {
            ops.execute("CREATE TABLE pgmig_rb_t (id BIGINT PRIMARY KEY)", &[])
                .map(|_| ())
        },
        |ops| {
            ops.execute("DROP TABLE pgmig_rb_t", &[])?;
            Err("down blew up after the drop".into())
        },
    ));
    m.run(&pg).unwrap();

    let err = m.rollback(&pg, 1).unwrap_err();
    assert!(err.contains("down blew up"), "got: {err}");
    // Transactional DDL on PG: the DROP rolled back with the failure.
    assert!(pg.query("SELECT 1 FROM pgmig_rb_t", &[]).is_ok());
    assert_eq!(applied_versions(&pg, "pgmig_rb"), vec!["pgmig_rb_001"]);

    scrub(&pg, "pgmig_rb", &["pgmig_rb_t"]);
}

#[test]
fn pg_no_transaction_migration_runs_concurrent_index_ddl() {
    let _serial = serial();
    let Some(pg) = db() else {
        eprintln!("skipping: SUTEGI_PG_TEST_URL not set");
        return;
    };
    scrub(&pg, "pgmig_notx", &["pgmig_notx_t"]);

    // CREATE INDEX CONCURRENTLY refuses to run inside a transaction — the
    // whole reason Migration::no_transaction exists.
    let m = Migrator::new()
        .add(Migration::new("pgmig_notx_001", "table", |ops| {
            ops.execute(
                "CREATE TABLE pgmig_notx_t (id BIGINT PRIMARY KEY, k TEXT)",
                &[],
            )
            .map(|_| ())
        }))
        .add(
            Migration::new("pgmig_notx_002", "concurrent_index", |ops| {
                ops.execute(
                    "CREATE INDEX CONCURRENTLY IF NOT EXISTS pgmig_notx_k ON pgmig_notx_t (k)",
                    &[],
                )
                .map(|_| ())
            })
            .no_transaction(),
        );
    assert_eq!(
        m.run(&pg).unwrap(),
        vec!["pgmig_notx_001", "pgmig_notx_002"]
    );
    assert!(m.run(&pg).unwrap().is_empty());

    scrub(&pg, "pgmig_notx", &["pgmig_notx_t"]);
}
