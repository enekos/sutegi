//! Live integration test for the PostgreSQL backend driving the same query
//! builder + Model surface as SQLite. Runs only when `SUTEGI_PG_TEST_URL` is
//! set (see sutegi-pg's integration tests for how to start a server).

#![cfg(feature = "postgres")]

use sutegi_json::Json;
use sutegi_orm::pg::Pg;
use sutegi_orm::row;
use sutegi_orm::{Backend, ColType, Column, FromRow, Model, QueryBuilder, TableSchema, Value};

struct Todo {
    id: i64,
    title: String,
    done: bool,
}

impl Model for Todo {
    fn schema() -> TableSchema {
        TableSchema {
            table: "orm_pg_todos",
            columns: vec![
                Column {
                    name: "id",
                    ty: ColType::Integer,
                    nullable: false,
                    primary: true,
                },
                Column {
                    name: "title",
                    ty: ColType::Text,
                    nullable: false,
                    primary: false,
                },
                Column {
                    name: "done",
                    ty: ColType::Boolean,
                    nullable: false,
                    primary: false,
                },
            ],
        }
    }
}

impl FromRow for Todo {
    fn from_row(r: &Json) -> Result<Self, String> {
        Ok(Todo {
            id: row::get_i64(r, "id")?,
            title: row::get_string(r, "title")?,
            done: row::get_bool(r, "done")?,
        })
    }
}

fn db() -> Option<Pg> {
    let url = std::env::var("SUTEGI_PG_TEST_URL").ok()?;
    Some(Pg::connect(&url, 4).unwrap())
}

#[test]
fn model_crud_over_postgres() {
    let Some(db) = db() else {
        eprintln!("skipping: SUTEGI_PG_TEST_URL not set");
        return;
    };

    db.pool()
        .batch("DROP TABLE IF EXISTS orm_pg_todos")
        .unwrap();
    Todo::migrate(&db).unwrap();

    // create returns the IDENTITY-generated primary key via RETURNING.
    let id1 = Todo::create(
        &db,
        &[
            ("title", Value::Text("ship".into())),
            ("done", Value::Bool(false)),
        ],
    )
    .unwrap();
    assert_eq!(id1, 1);
    let id2 = Todo::create(
        &db,
        &[
            ("title", Value::Text("test".into())),
            ("done", Value::Bool(true)),
        ],
    )
    .unwrap();
    assert_eq!(id2, 2);

    // count + typed all + find_typed.
    assert_eq!(Todo::count(&db).unwrap(), 2);
    let all: Vec<Todo> = Todo::all_typed(&db).unwrap();
    assert_eq!(all.len(), 2);
    assert!(all.iter().any(|t| t.id == id1) && all.iter().any(|t| t.id == id2));
    let found = Todo::find_typed(&db, Value::Int(1)).unwrap().unwrap();
    assert_eq!(found.title, "ship");
    assert!(!found.done);

    // update + delete by primary key.
    assert_eq!(
        Todo::update(&db, Value::Int(1), &[("done", Value::Bool(true))]).unwrap(),
        1
    );
    assert!(Todo::find_typed(&db, Value::Int(1)).unwrap().unwrap().done);
    assert!(Todo::delete(&db, Value::Int(2)).unwrap());
    assert_eq!(Todo::count(&db).unwrap(), 1);

    // query builder: filter + order survive placeholder translation.
    let rows = db
        .select(
            &QueryBuilder::table("orm_pg_todos")
                .filter("done", "=", Value::Bool(true))
                .order_by("id", false),
        )
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].get("title").and_then(Json::as_str), Some("ship"));

    // upsert: same pk updates in place rather than inserting a duplicate.
    db.upsert(
        "orm_pg_todos",
        &[
            ("id", Value::Int(1)),
            ("title", Value::Text("upserted".into())),
            ("done", Value::Bool(false)),
        ],
        "id",
        "id",
    )
    .unwrap();
    assert_eq!(Todo::count(&db).unwrap(), 1);
    assert_eq!(
        Todo::find_typed(&db, Value::Int(1)).unwrap().unwrap().title,
        "upserted"
    );

    db.pool().batch("DROP TABLE orm_pg_todos").unwrap();
}

#[test]
fn transaction_commits_and_rolls_back() {
    let Some(db) = db() else {
        eprintln!("skipping: SUTEGI_PG_TEST_URL not set");
        return;
    };
    db.pool().batch("DROP TABLE IF EXISTS orm_pg_tx").unwrap();
    db.pool()
        .batch("CREATE TABLE orm_pg_tx (n INTEGER NOT NULL)")
        .unwrap();

    // The transaction closure now receives a `Tx`, which is a `Backend`: the
    // `?`-placeholder API (and the whole query builder / Model surface) works
    // inside the transaction, pinned to one connection.

    // Rolled-back work leaves no trace.
    let _ = db.transaction(|tx| {
        tx.execute("INSERT INTO orm_pg_tx (n) VALUES (?)", &[Value::Int(1)])?;
        Err::<(), String>("boom".into())
    });
    assert_eq!(db.count(&QueryBuilder::table("orm_pg_tx")).unwrap(), 0);

    // Committed work persists — and is visible via the query builder inside the
    // same transaction before commit.
    db.transaction(|tx| {
        tx.execute("INSERT INTO orm_pg_tx (n) VALUES (?)", &[Value::Int(2)])?;
        assert_eq!(tx.count(&QueryBuilder::table("orm_pg_tx"))?, 1);
        Ok(())
    })
    .unwrap();
    assert_eq!(db.count(&QueryBuilder::table("orm_pg_tx")).unwrap(), 1);

    db.pool().batch("DROP TABLE orm_pg_tx").unwrap();
}

#[test]
fn migrations_run_and_roll_back_over_postgres() {
    use sutegi_orm::migrate::{Migration, Migrator};

    let Some(db) = db() else {
        eprintln!("skipping: SUTEGI_PG_TEST_URL not set");
        return;
    };
    db.pool().batch("DROP TABLE IF EXISTS orm_pg_mig").unwrap();
    db.pool()
        .batch("DROP TABLE IF EXISTS _sutegi_migrations")
        .unwrap();

    let migrator = || {
        Migrator::new().add(Migration::reversible(
            "20260701_000001",
            "create_orm_pg_mig",
            |c| {
                c.execute(
                    "CREATE TABLE orm_pg_mig (id BIGINT PRIMARY KEY, label TEXT NOT NULL)",
                    &[],
                )
                .map(|_| ())
            },
            |c| c.execute("DROP TABLE orm_pg_mig", &[]).map(|_| ()),
        ))
    };

    // Forward: creates the table + records history (INSERT with ? → $n, and the
    // BEGIN/COMMIT wrapper both go over the wire protocol).
    assert_eq!(migrator().run(&db).unwrap(), vec!["20260701_000001"]);
    db.execute(
        "INSERT INTO orm_pg_mig (id, label) VALUES (?, ?)",
        &[Value::Int(1), Value::Text("ok".into())],
    )
    .unwrap();
    assert_eq!(
        db.query("SELECT COUNT(*) AS c FROM orm_pg_mig", &[])
            .unwrap()[0]
            .get("c")
            .and_then(Json::as_i64),
        Some(1)
    );
    // Idempotent.
    assert!(migrator().run(&db).unwrap().is_empty());

    // Rollback drops the table and clears the history row.
    assert_eq!(
        migrator().rollback(&db, 1).unwrap(),
        vec!["20260701_000001"]
    );
    assert!(db.query("SELECT 1 FROM orm_pg_mig", &[]).is_err());
    assert!(migrator().status(&db).unwrap().iter().all(|s| !s.applied));

    db.pool()
        .batch("DROP TABLE IF EXISTS _sutegi_migrations")
        .unwrap();
}

#[test]
fn kv_store_over_postgres() {
    use sutegi_orm::kv::Kv;

    let Some(db) = db() else {
        eprintln!("skipping: SUTEGI_PG_TEST_URL not set");
        return;
    };
    db.pool().batch("DROP TABLE IF EXISTS orm_pg_kv").unwrap();

    // The same Kv layer that backs single-node SQLite runs unchanged on the
    // multi-pod Postgres backend — useful for small shared state (flags/config).
    let kv = Kv::with_table(db.clone(), "orm_pg_kv");
    kv.migrate().unwrap();

    kv.set("flags", "beta", &Json::Bool(true)).unwrap();
    kv.set("flags", "rollout", &Json::obj(vec![("pct", Json::int(25))]))
        .unwrap();
    assert_eq!(kv.get("flags", "beta").unwrap(), Some(Json::Bool(true)));

    // Overwrite in place (ON CONFLICT), then confirm no duplicate row.
    kv.set("flags", "beta", &Json::Bool(false)).unwrap();
    assert_eq!(kv.get("flags", "beta").unwrap(), Some(Json::Bool(false)));
    assert_eq!(kv.count("flags").unwrap(), 2);
    assert_eq!(kv.keys("flags").unwrap(), vec!["beta", "rollout"]);

    assert!(kv.delete("flags", "beta").unwrap());
    assert_eq!(kv.count("flags").unwrap(), 1);

    db.pool().batch("DROP TABLE orm_pg_kv").unwrap();
}
