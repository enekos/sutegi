//! Reliability tests for the migration runner on the conditions production
//! actually has: a **pooled, file-backed** database (where a statement-level
//! `BEGIN`/`COMMIT` would spray across connections) and **concurrent runners**
//! (many pods, or two processes on one SQLite file). Every test here asserts
//! the same contract: whatever fails or races, the database is either fully
//! before a migration or fully after it — never in between.

#![cfg(feature = "sqlite")]

use sutegi_orm::db::Db;
use sutegi_orm::migrate::{Migration, Migrator};
use sutegi_orm::{Backend, QueryBuilder};

/// A unique throwaway database file per test (pooled connections, WAL) —
/// `Db::memory()` pins its pool to one connection, which would hide every
/// cross-connection bug this suite exists to catch.
struct TempDb {
    path: String,
}

impl TempDb {
    fn new(tag: &str) -> TempDb {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "sutegi_mig_rel_{tag}_{}_{nanos}.db",
            std::process::id()
        ));
        TempDb {
            path: path.to_str().unwrap().to_string(),
        }
    }

    fn open(&self) -> Db {
        Db::open_pool(&self.path, 4).unwrap()
    }
}

impl Drop for TempDb {
    fn drop(&mut self) {
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", self.path));
        }
    }
}

fn table_exists(db: &Db, table: &str) -> bool {
    db.select(&QueryBuilder::table(table)).is_ok()
}

fn history_count(db: &Db) -> i64 {
    db.count(&QueryBuilder::table("_sutegi_migrations"))
        .unwrap_or(0)
}

#[test]
fn failing_migration_is_atomic_on_a_pooled_file_db() {
    let tmp = TempDb::new("atomic");
    let db = tmp.open();

    let m = Migrator::new().add(Migration::new("0001_boom", "boom", |ops| {
        ops.execute("CREATE TABLE half_done (id INTEGER PRIMARY KEY)", &[])?;
        ops.execute("INSERT INTO half_done (id) VALUES (1)", &[])?;
        Err("deliberate failure".into())
    }));

    let err = m.run(&db).unwrap_err();
    assert!(err.contains("deliberate failure"), "got: {err}");

    // The whole body rolled back on the ONE connection that ran it — no
    // table, no rows, no history entry.
    assert!(!table_exists(&db, "half_done"));
    assert_eq!(history_count(&db), 0);
}

#[test]
fn failing_rollback_is_atomic_on_a_pooled_file_db() {
    let tmp = TempDb::new("rb_atomic");
    let db = tmp.open();

    let m = Migrator::new().add(Migration::reversible(
        "0001_t",
        "t",
        |ops| {
            ops.execute("CREATE TABLE t (id INTEGER PRIMARY KEY)", &[])
                .map(|_| ())
        },
        |ops| {
            ops.execute("DROP TABLE t", &[])?;
            Err("down blew up after the drop".into())
        },
    ));
    m.run(&db).unwrap();

    let err = m.rollback(&db, 1).unwrap_err();
    assert!(err.contains("down blew up"), "got: {err}");

    // The failed `down` rolled back wholesale: the table is still there and
    // the migration is still recorded as applied.
    assert!(table_exists(&db, "t"));
    assert_eq!(history_count(&db), 1);
    assert!(m.status(&db).unwrap()[0].applied);
}

#[test]
fn partial_batch_failure_keeps_the_applied_prefix() {
    let tmp = TempDb::new("prefix");
    let db = tmp.open();

    let broken = Migrator::new()
        .add(Migration::new("0001_ok", "ok", |ops| {
            ops.execute("CREATE TABLE ok_t (id INTEGER PRIMARY KEY)", &[])
                .map(|_| ())
        }))
        .add(Migration::new("0002_bad", "bad", |_| Err("nope".into())));

    assert!(broken.run(&db).unwrap_err().contains("nope"));
    // 0001 committed (its own transaction); 0002 left no trace.
    assert!(table_exists(&db, "ok_t"));
    assert_eq!(history_count(&db), 1);

    // Ship the fix: only the failed migration runs, in a fresh batch.
    let fixed = Migrator::new()
        .add(Migration::new("0001_ok", "ok", |_| Ok(())))
        .add(Migration::new("0002_bad", "bad", |ops| {
            ops.execute("CREATE TABLE bad_t (id INTEGER PRIMARY KEY)", &[])
                .map(|_| ())
        }));
    assert_eq!(fixed.run(&db).unwrap(), vec!["0002_bad"]);
    assert!(table_exists(&db, "bad_t"));
    assert_eq!(history_count(&db), 2);
}

/// The migrations a racing-runner test applies. Plain `CREATE TABLE` (no
/// `IF NOT EXISTS`) and an unguarded marker INSERT: a double-apply cannot
/// pass silently — it either errors on the DDL or doubles the marker count.
fn racing_migrator() -> Migrator {
    Migrator::new()
        .add(Migration::new("0000_markers", "markers", |ops| {
            ops.execute("CREATE TABLE markers (version TEXT NOT NULL)", &[])
                .map(|_| ())
        }))
        .add(Migration::new("0001_a", "a", |ops| {
            ops.execute("CREATE TABLE race_a (id INTEGER PRIMARY KEY)", &[])?;
            ops.execute("INSERT INTO markers (version) VALUES ('0001_a')", &[])
                .map(|_| ())
        }))
        .add(Migration::new("0002_b", "b", |ops| {
            ops.execute("CREATE TABLE race_b (id INTEGER PRIMARY KEY)", &[])?;
            ops.execute("INSERT INTO markers (version) VALUES ('0002_b')", &[])
                .map(|_| ())
        }))
        .add(Migration::new("0003_c", "c", |ops| {
            ops.execute("CREATE TABLE race_c (id INTEGER PRIMARY KEY)", &[])?;
            ops.execute("INSERT INTO markers (version) VALUES ('0003_c')", &[])
                .map(|_| ())
        }))
}

fn assert_applied_exactly_once(db: &Db, applied_by_runners: Vec<Vec<String>>) {
    // Across all runners, every version was applied by exactly one of them.
    let mut all: Vec<String> = applied_by_runners.into_iter().flatten().collect();
    all.sort();
    assert_eq!(all, vec!["0000_markers", "0001_a", "0002_b", "0003_c"]);

    // And the database agrees: one marker row per migration, one history row
    // per migration, all tables present.
    let markers = db.count(&QueryBuilder::table("markers")).unwrap();
    assert_eq!(markers, 3, "a migration body ran more than once");
    assert_eq!(history_count(db), 4);
    for t in ["race_a", "race_b", "race_c"] {
        assert!(table_exists(db, t));
    }
}

#[test]
fn racing_runners_on_one_handle_apply_each_migration_exactly_once() {
    let tmp = TempDb::new("race_one_handle");
    let db = tmp.open();

    let results: Vec<Vec<String>> = std::thread::scope(|s| {
        (0..8)
            .map(|_| {
                let db = db.clone();
                s.spawn(move || racing_migrator().run(&db).unwrap())
            })
            .collect::<Vec<_>>()
            .into_iter()
            .map(|h| h.join().unwrap())
            .collect()
    });

    assert_applied_exactly_once(&db, results);
}

#[test]
fn racing_runners_on_separate_handles_apply_each_migration_exactly_once() {
    // Separate `Db` handles = separate pools, like two processes sharing the
    // file. Each opens its pool at boot (before migrating, as a real pod
    // does — opening mid-race can itself hit SQLITE_BUSY); the process-scope
    // lock registry plus the in-transaction history re-check must still give
    // exactly-once.
    let tmp = TempDb::new("race_two_handles");
    let handles: Vec<Db> = (0..4)
        .map(|_| Db::open_pool(&tmp.path, 2).unwrap())
        .collect();

    let results: Vec<Vec<String>> = std::thread::scope(|s| {
        handles
            .iter()
            .map(|db| s.spawn(move || racing_migrator().run(db).unwrap()))
            .collect::<Vec<_>>()
            .into_iter()
            .map(|h| h.join().unwrap())
            .collect()
    });

    assert_applied_exactly_once(&tmp.open(), results);
}

#[test]
fn racing_rollbacks_undo_each_migration_exactly_once() {
    let tmp = TempDb::new("race_rollback");
    let db = tmp.open();

    let up = Migrator::new()
        .add(Migration::reversible(
            "0001_x",
            "x",
            |ops| {
                ops.execute("CREATE TABLE x (id INTEGER PRIMARY KEY)", &[])
                    .map(|_| ())
            },
            // A second DROP of the same table errors, so a double-rollback
            // cannot pass silently.
            |ops| ops.execute("DROP TABLE x", &[]).map(|_| ()),
        ))
        .add(Migration::reversible(
            "0002_y",
            "y",
            |ops| {
                ops.execute("CREATE TABLE y (id INTEGER PRIMARY KEY)", &[])
                    .map(|_| ())
            },
            |ops| ops.execute("DROP TABLE y", &[]).map(|_| ()),
        ));
    up.run(&db).unwrap();

    let results: Vec<Vec<String>> = std::thread::scope(|s| {
        (0..6)
            .map(|_| {
                let db = db.clone();
                s.spawn(move || {
                    Migrator::new()
                        .add(Migration::reversible(
                            "0001_x",
                            "x",
                            |_| Ok(()),
                            |ops| ops.execute("DROP TABLE x", &[]).map(|_| ()),
                        ))
                        .add(Migration::reversible(
                            "0002_y",
                            "y",
                            |_| Ok(()),
                            |ops| ops.execute("DROP TABLE y", &[]).map(|_| ()),
                        ))
                        .rollback(&db, 1)
                        .unwrap()
                })
            })
            .collect::<Vec<_>>()
            .into_iter()
            .map(|h| h.join().unwrap())
            .collect()
    });

    let mut all: Vec<String> = results.into_iter().flatten().collect();
    all.sort();
    assert_eq!(all, vec!["0001_x", "0002_y"]);
    assert!(!table_exists(&db, "x"));
    assert!(!table_exists(&db, "y"));
    assert_eq!(history_count(&db), 0);
}

#[test]
fn a_held_migration_lock_times_out_with_guidance() {
    let tmp = TempDb::new("lock_timeout");
    let db = tmp.open();

    // Simulate a stuck runner: hold the migration lock from "elsewhere".
    let held = db
        .try_lock("sutegi:migrations")
        .unwrap()
        .expect("free lock");

    let m = Migrator::new()
        .add(Migration::new("0001_x", "x", |_| Ok(())))
        .lock_timeout(std::time::Duration::from_millis(80));
    let err = m.run(&db).unwrap_err();
    assert!(err.contains("migration lock"), "got: {err}");
    assert!(err.contains("lock_timeout"), "got: {err}");
    assert_eq!(history_count(&db), 0);

    // Once the stuck runner releases, the same migrator goes through.
    drop(held);
    assert_eq!(m.run(&db).unwrap(), vec!["0001_x"]);
}

#[test]
fn concurrent_dev_syncs_converge_on_a_pooled_file_db() {
    use sutegi_orm::{ColType, Column, TableSchema};

    let tmp = TempDb::new("sync_race");
    let db = tmp.open();
    let schema = || {
        vec![TableSchema::new("sync_t")
            .column(Column::new("id", ColType::Integer).primary())
            .column(Column::new("title", ColType::Text))]
    };

    std::thread::scope(|s| {
        let handles: Vec<_> = (0..6)
            .map(|_| {
                let db = db.clone();
                s.spawn(move || {
                    sutegi_orm::migrate::sync(
                        &db,
                        &[TableSchema::new("sync_t")
                            .column(Column::new("id", ColType::Integer).primary())
                            .column(Column::new("title", ColType::Text))],
                    )
                })
            })
            .collect();
        for h in handles {
            // Two syncs can race the same CREATE TABLE: losers may error on
            // the duplicate, but no outcome may corrupt the schema.
            let _ = h.join().unwrap();
        }
    });

    let live = db.introspect().unwrap();
    assert_eq!(live.len(), 1);
    assert_eq!(live[0], schema()[0].normalized());
    // And a follow-up sync agrees there is nothing left to do.
    assert!(sutegi_orm::migrate::sync(&db, &schema())
        .unwrap()
        .is_empty());
}

#[test]
fn failing_migration_under_pool_contention_leaks_nothing() {
    use std::sync::atomic::{AtomicBool, Ordering};

    // The scenario that broke the statement-level BEGIN/COMMIT of old:
    // concurrent traffic shuffles the pool's connections, so a transaction
    // faked with `execute("BEGIN")` spans connections and its ROLLBACK rolls
    // back nothing (measured on the old runner: 7/60 failing migrations left
    // their half-applied schema behind under this exact load).
    let tmp = TempDb::new("contention");
    let db = tmp.open();
    Backend::execute(&db, "CREATE TABLE noise (id INTEGER PRIMARY KEY)", &[]).unwrap();

    let stop = AtomicBool::new(false);
    let mut leaks = 0;
    std::thread::scope(|s| {
        for _ in 0..3 {
            let db = db.clone();
            let stop = &stop;
            s.spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    let _ = db.select(&QueryBuilder::table("noise"));
                }
            });
        }
        for i in 0..30 {
            let m = Migrator::new().add(Migration::new(format!("{i:04}_boom"), "boom", |ops| {
                ops.execute("CREATE TABLE half_done (id INTEGER PRIMARY KEY)", &[])?;
                ops.execute("INSERT INTO half_done (id) VALUES (1)", &[])?;
                Err("deliberate".into())
            }));
            let _ = m.run(&db);
            if table_exists(&db, "half_done") {
                leaks += 1;
                let _ = Backend::execute(&db, "DROP TABLE half_done", &[]);
            }
        }
        stop.store(true, Ordering::Relaxed);
    });
    assert_eq!(
        leaks, 0,
        "{leaks}/30 failing migrations left a half-applied schema behind"
    );
    assert_eq!(history_count(&db), 0);
}
