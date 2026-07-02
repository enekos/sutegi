//! The bundled **SQLite** execution layer — sutegi's *single-node* store.
//! Available with the `sqlite` feature (pulls in bundled rusqlite).
//!
//! SQLite is embedded, zero-ops, and single-writer: perfect for one-instance
//! apps and for local state you don't want to run a server for. Use it for a
//! single-pod relational app, or as the backing store for the [`crate::kv`]
//! key/value layer (config, cache, sessions, flags). When you need many
//! replicas sharing one database, reach for the Postgres backend
//! ([`crate::pg::Pg`]) instead — same [`Backend`] API, so the app code doesn't
//! change.
//!
//! [`Db`] is a cheap-to-clone, `Send + Sync` handle over a small connection
//! pool, so it drops straight into [`App::state`](../../sutegi_web/struct.App.html)
//! with no user-visible `Mutex`. File-backed databases run in WAL mode so
//! several pooled connections can read while one writes; an in-memory database
//! is inherently a single connection (a second would be a *different*
//! database), so [`Db::memory`] pins the pool to size 1.

use crate::backend::Backend;
use crate::value::{create_table_sql, TableSchema, Value};
use rusqlite::{params_from_iter, types::Value as SqlValue, Connection};
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::sync::{Arc, Condvar, Mutex};
use sutegi_json::Json;

/// Where a pool's connections come from.
enum Source {
    /// A private in-memory database (size-1 pool).
    Memory,
    /// A file on disk, opened in WAL mode.
    File(String),
}

struct Inner {
    /// Idle, ready-to-use connections.
    idle: Vec<Connection>,
    /// Live connections (idle + checked out). Capped at `max_size`.
    open: usize,
}

/// A small blocking SQLite connection pool. Connections are opened lazily up to
/// `max_size` and returned on checkin.
struct SqlitePool {
    source: Source,
    max_size: usize,
    state: (Mutex<Inner>, Condvar),
}

impl SqlitePool {
    fn new(source: Source, max_size: usize) -> SqlitePool {
        SqlitePool {
            source,
            max_size: max_size.max(1),
            state: (
                Mutex::new(Inner {
                    idle: Vec::new(),
                    open: 0,
                }),
                Condvar::new(),
            ),
        }
    }

    fn connect(&self) -> Result<Connection, String> {
        let conn = match &self.source {
            Source::Memory => Connection::open_in_memory().map_err(|e| e.to_string())?,
            Source::File(path) => {
                let conn = Connection::open(path).map_err(|e| e.to_string())?;
                // WAL lets pooled readers proceed alongside a writer; the busy
                // timeout absorbs brief write contention instead of erroring.
                conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA busy_timeout=5000;")
                    .map_err(|e| e.to_string())?;
                conn
            }
        };
        // Room for an app's worth of distinct statement shapes (rusqlite's
        // default LRU holds 16). Schema changes are safe: prepare_v2
        // re-prepares stale statements transparently.
        conn.set_prepared_statement_cache_capacity(64);
        Ok(conn)
    }

    /// Check a connection out, run `f`, and return it to the pool. A connection
    /// is only reused, never silently swapped, so a transaction pinned inside
    /// `f` stays on one connection.
    fn with<T>(&self, f: impl FnOnce(&mut Connection) -> Result<T, String>) -> Result<T, String> {
        let mut conn = self.checkout()?;
        let result = f(&mut conn);
        self.checkin(conn);
        result
    }

    fn checkout(&self) -> Result<Connection, String> {
        let (lock, cvar) = &self.state;
        let mut inner = lock.lock().unwrap();
        loop {
            if let Some(conn) = inner.idle.pop() {
                return Ok(conn);
            }
            if inner.open < self.max_size {
                inner.open += 1;
                drop(inner);
                match self.connect() {
                    Ok(conn) => return Ok(conn),
                    Err(e) => {
                        let mut inner = lock.lock().unwrap();
                        inner.open -= 1;
                        cvar.notify_one();
                        return Err(e);
                    }
                }
            }
            inner = cvar.wait(inner).unwrap();
        }
    }

    fn checkin(&self, conn: Connection) {
        let (lock, cvar) = &self.state;
        let mut inner = lock.lock().unwrap();
        inner.idle.push(conn);
        cvar.notify_one();
    }
}

/// A pooled SQLite handle: cheap to [`clone`](Clone), `Send + Sync`, and a
/// [`Backend`]. Share it across worker threads by cloning (it's an `Arc`
/// inside) or by handing it to [`App::state`](../../sutegi_web/struct.App.html).
#[derive(Clone)]
pub struct Db {
    pool: Arc<SqlitePool>,
}

impl Db {
    /// Open a private in-memory database (great for tests and demos). Always a
    /// single connection — the data lives only as long as this `Db`.
    pub fn memory() -> Result<Db, String> {
        let db = Db {
            pool: Arc::new(SqlitePool::new(Source::Memory, 1)),
        };
        // Materialize the single connection now so the DB exists eagerly.
        db.pool.with(|_| Ok(()))?;
        Ok(db)
    }

    /// Open (or create) a database file with a small default pool (WAL mode).
    /// Use [`Db::open_pool`] to size the pool to your worker count.
    pub fn open(path: &str) -> Result<Db, String> {
        Db::open_pool(path, 4)
    }

    /// Open (or create) a database file with a pool of `size` connections.
    pub fn open_pool(path: &str, size: usize) -> Result<Db, String> {
        let db = Db {
            pool: Arc::new(SqlitePool::new(Source::File(path.to_string()), size)),
        };
        // Fail fast if the file can't be opened / WAL can't be set.
        db.pool.with(|_| Ok(()))?;
        Ok(db)
    }

    /// Open the file named by the `env_key` environment variable, or fall back
    /// to an in-memory database when it is unset — the common "persist in
    /// prod, ephemeral in dev/tests" shape.
    ///
    /// ```ignore
    /// let db = Db::open_or_memory("DATABASE_PATH");
    /// ```
    pub fn open_or_memory(env_key: &str) -> Db {
        match std::env::var(env_key) {
            Ok(path) => Db::open(&path).expect("open database"),
            Err(_) => Db::memory().expect("open in-memory database"),
        }
    }

    /// Run `f` inside a transaction on a single pinned connection: `BEGIN`, then
    /// `COMMIT` on `Ok` or `ROLLBACK` on `Err`. The closure receives a [`Tx`],
    /// which is a [`Backend`], so the whole query builder + `Model` surface
    /// works inside the transaction.
    pub fn transaction<T>(&self, f: impl FnOnce(&Tx) -> Result<T, String>) -> Result<T, String> {
        self.pool.with(|conn| {
            conn.execute_batch("BEGIN").map_err(|e| e.to_string())?;
            let tx = Tx {
                conn: RefCell::new(conn),
            };
            match f(&tx) {
                Ok(value) => {
                    tx.conn
                        .borrow_mut()
                        .execute_batch("COMMIT")
                        .map_err(|e| e.to_string())?;
                    Ok(value)
                }
                Err(e) => {
                    let _ = tx.conn.borrow_mut().execute_batch("ROLLBACK");
                    Err(e)
                }
            }
        })
    }
}

/// Run one query on a checked-out connection and collect rows as JSON objects.
/// Statements are prepared through the per-connection cache: repeated shapes
/// (the query builder emits stable SQL) skip the SQLite parse/plan step.
fn query_conn(conn: &Connection, sql: &str, params: &[Value]) -> Result<Vec<Json>, String> {
    let mut stmt = conn.prepare_cached(sql).map_err(|e| e.to_string())?;
    let col_names: Vec<String> = stmt.column_names().iter().map(|s| s.to_string()).collect();
    let bound: Vec<SqlValue> = params.iter().map(to_sql).collect();
    let rows = stmt
        .query_map(params_from_iter(bound), |row| {
            let mut obj = BTreeMap::new();
            for (i, name) in col_names.iter().enumerate() {
                let v: SqlValue = row.get(i)?;
                obj.insert(name.clone(), sql_to_json(v));
            }
            Ok(Json::Obj(obj))
        })
        .map_err(|e| e.to_string())?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())
}

fn execute_conn(conn: &Connection, sql: &str, params: &[Value]) -> Result<usize, String> {
    let bound: Vec<SqlValue> = params.iter().map(to_sql).collect();
    let mut stmt = conn.prepare_cached(sql).map_err(|e| e.to_string())?;
    stmt.execute(params_from_iter(bound))
        .map_err(|e| e.to_string())
}

/// `INSERT INTO … VALUES (…)` returning the new rowid — shared by [`Db`] / [`Tx`].
fn insert_conn(conn: &Connection, table: &str, cols: &[(&str, Value)]) -> Result<i64, String> {
    let names: Vec<&str> = cols.iter().map(|(n, _)| *n).collect();
    let placeholders = vec!["?"; cols.len()].join(", ");
    let sql = format!(
        "INSERT INTO {} ({}) VALUES ({})",
        table,
        names.join(", "),
        placeholders
    );
    let bound: Vec<SqlValue> = cols.iter().map(|(_, v)| to_sql(v)).collect();
    let mut stmt = conn.prepare_cached(&sql).map_err(|e| e.to_string())?;
    stmt.execute(params_from_iter(bound))
        .map_err(|e| e.to_string())?;
    Ok(conn.last_insert_rowid())
}

/// `INSERT … ON CONFLICT(conflict) DO UPDATE …` returning the rowid.
fn upsert_conn(
    conn: &Connection,
    table: &str,
    cols: &[(&str, Value)],
    conflict: &str,
) -> Result<i64, String> {
    let names: Vec<&str> = cols.iter().map(|(n, _)| *n).collect();
    let placeholders = vec!["?"; cols.len()].join(", ");
    let updates: Vec<String> = names
        .iter()
        .filter(|n| **n != conflict)
        .map(|n| format!("{} = excluded.{}", n, n))
        .collect();
    let sql = format!(
        "INSERT INTO {} ({}) VALUES ({}) ON CONFLICT({}) DO UPDATE SET {}",
        table,
        names.join(", "),
        placeholders,
        conflict,
        updates.join(", ")
    );
    let bound: Vec<SqlValue> = cols.iter().map(|(_, v)| to_sql(v)).collect();
    let mut stmt = conn.prepare_cached(&sql).map_err(|e| e.to_string())?;
    stmt.execute(params_from_iter(bound))
        .map_err(|e| e.to_string())?;
    Ok(conn.last_insert_rowid())
}

impl Backend for Db {
    fn query(&self, sql: &str, params: &[Value]) -> Result<Vec<Json>, String> {
        self.pool.with(|conn| query_conn(conn, sql, params))
    }

    fn execute(&self, sql: &str, params: &[Value]) -> Result<usize, String> {
        self.pool.with(|conn| execute_conn(conn, sql, params))
    }

    // SQLite uses the implicit rowid / `last_insert_rowid()`, so the named
    // primary key is not needed here.
    fn insert(&self, table: &str, cols: &[(&str, Value)], _pk: &str) -> Result<i64, String> {
        self.pool.with(|conn| insert_conn(conn, table, cols))
    }

    fn upsert(
        &self,
        table: &str,
        cols: &[(&str, Value)],
        conflict: &str,
        _pk: &str,
    ) -> Result<i64, String> {
        self.pool
            .with(|conn| upsert_conn(conn, table, cols, conflict))
    }

    fn migrate(&self, schema: &TableSchema) -> Result<(), String> {
        self.pool.with(|conn| {
            conn.execute_batch(&create_table_sql(schema))
                .map_err(|e| e.to_string())
        })
    }
}

impl crate::backend::Transactional for Db {
    fn run_in_tx(
        &self,
        f: &mut dyn FnMut(&dyn Backend) -> Result<(), String>,
    ) -> Result<(), String> {
        self.transaction(|tx| f(tx))
    }
}

/// A transaction handle: a [`Backend`] pinned to a single connection for the
/// duration of a [`Db::transaction`] closure. Uses interior mutability because
/// the trait takes `&self` while `rusqlite` wants `&mut` for `execute`.
pub struct Tx<'a> {
    conn: RefCell<&'a mut Connection>,
}

impl Backend for Tx<'_> {
    fn query(&self, sql: &str, params: &[Value]) -> Result<Vec<Json>, String> {
        let conn = self.conn.borrow();
        query_conn(&conn, sql, params)
    }

    fn execute(&self, sql: &str, params: &[Value]) -> Result<usize, String> {
        let conn = self.conn.borrow();
        execute_conn(&conn, sql, params)
    }

    fn insert(&self, table: &str, cols: &[(&str, Value)], _pk: &str) -> Result<i64, String> {
        let conn = self.conn.borrow();
        insert_conn(&conn, table, cols)
    }

    fn upsert(
        &self,
        table: &str,
        cols: &[(&str, Value)],
        conflict: &str,
        _pk: &str,
    ) -> Result<i64, String> {
        let conn = self.conn.borrow();
        upsert_conn(&conn, table, cols, conflict)
    }

    fn migrate(&self, schema: &TableSchema) -> Result<(), String> {
        self.conn
            .borrow()
            .execute_batch(&create_table_sql(schema))
            .map_err(|e| e.to_string())
    }
}

fn to_sql(v: &Value) -> SqlValue {
    match v {
        Value::Null => SqlValue::Null,
        Value::Int(i) => SqlValue::Integer(*i),
        Value::Real(r) => SqlValue::Real(*r),
        Value::Text(s) => SqlValue::Text(s.clone()),
        Value::Bool(b) => SqlValue::Integer(if *b { 1 } else { 0 }),
        // SQLite has no JSON or vector type: store the canonical serialization
        // as TEXT. The typed row extractors parse it back on the way out.
        Value::Json(j) => SqlValue::Text(j.to_string()),
        Value::Vector(vec) => SqlValue::Text(crate::value::vector_to_text(vec)),
    }
}

fn sql_to_json(v: SqlValue) -> Json {
    match v {
        SqlValue::Null => Json::Null,
        SqlValue::Integer(i) => Json::int(i),
        SqlValue::Real(r) => Json::Num(r),
        SqlValue::Text(s) => Json::str(s),
        SqlValue::Blob(b) => Json::str(String::from_utf8_lossy(&b).into_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value::{ColType, Column};
    use crate::{DeleteBuilder, QueryBuilder, UpdateBuilder};

    fn todos_schema() -> TableSchema {
        TableSchema {
            table: "todos",
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

    #[test]
    fn migrate_insert_select_roundtrip() {
        let db = Db::memory().unwrap();
        db.migrate(&todos_schema()).unwrap();
        let id = db
            .insert(
                "todos",
                &[
                    ("title", Value::Text("ship sutegi".into())),
                    ("done", Value::Bool(false)),
                ],
                "id",
            )
            .unwrap();
        assert_eq!(id, 1);

        let rows = db
            .select(&QueryBuilder::table("todos").select(&["id", "title", "done"]))
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].get("title").unwrap(), &Json::str("ship sutegi"));
        assert_eq!(rows[0].get("id").unwrap(), &Json::int(1));
    }

    #[test]
    fn update_delete_and_transaction() {
        let db = Db::memory().unwrap();
        db.migrate(&todos_schema()).unwrap();
        db.insert(
            "todos",
            &[
                ("title", Value::Text("a".into())),
                ("done", Value::Bool(false)),
            ],
            "id",
        )
        .unwrap();

        let (sql, params) = UpdateBuilder::table("todos")
            .set("done", Value::Bool(true))
            .filter("id", "=", Value::Int(1))
            .build();
        assert_eq!(db.execute(&sql, &params).unwrap(), 1);

        // Rollback leaves state untouched. Note the closure uses the Backend
        // API on `&Tx` — the transaction seam is backend-agnostic now.
        let _ = db.transaction(|tx| {
            tx.insert(
                "todos",
                &[
                    ("title", Value::Text("b".into())),
                    ("done", Value::Bool(false)),
                ],
                "id",
            )?;
            Err::<(), String>("boom".into())
        });
        assert_eq!(db.select(&QueryBuilder::table("todos")).unwrap().len(), 1);

        db.transaction(|tx| {
            tx.insert(
                "todos",
                &[
                    ("title", Value::Text("c".into())),
                    ("done", Value::Bool(false)),
                ],
                "id",
            )?;
            Ok(())
        })
        .unwrap();
        assert_eq!(db.select(&QueryBuilder::table("todos")).unwrap().len(), 2);

        let (dsql, dparams) = DeleteBuilder::table("todos")
            .filter("id", "=", Value::Int(1))
            .build();
        assert_eq!(db.execute(&dsql, &dparams).unwrap(), 1);
    }

    #[test]
    fn count_exists_paginate_upsert() {
        let db = Db::memory().unwrap();
        db.migrate(&todos_schema()).unwrap();
        for i in 0..7 {
            db.insert(
                "todos",
                &[
                    ("title", Value::Text(format!("t{i}"))),
                    ("done", Value::Bool(false)),
                ],
                "id",
            )
            .unwrap();
        }
        assert_eq!(db.count(&QueryBuilder::table("todos")).unwrap(), 7);
        assert!(db
            .exists(&QueryBuilder::table("todos").filter("id", "=", Value::Int(3)))
            .unwrap());
        assert!(!db
            .exists(&QueryBuilder::table("todos").filter("id", "=", Value::Int(99)))
            .unwrap());

        let page = db
            .paginate(&QueryBuilder::table("todos").order_by("id", false), 2, 3)
            .unwrap();
        assert_eq!(page.total, 7);
        assert_eq!(page.items.len(), 3);
        assert_eq!(page.total_pages(), 3);
        assert!(page.has_next() && page.has_prev());

        // UPSERT on the primary key: second call updates, doesn't duplicate.
        db.upsert(
            "todos",
            &[
                ("id", Value::Int(1)),
                ("title", Value::Text("upserted".into())),
                ("done", Value::Bool(true)),
            ],
            "id",
            "id",
        )
        .unwrap();
        assert_eq!(db.count(&QueryBuilder::table("todos")).unwrap(), 7);
        let row = db
            .query_one("SELECT title FROM todos WHERE id = 1", &[])
            .unwrap()
            .unwrap();
        assert_eq!(row.get("title").unwrap(), &Json::str("upserted"));
    }

    #[test]
    fn clone_shares_one_database() {
        // A cloned handle shares the same pool, so writes are visible across
        // clones — this is what makes `Db` safe to hand to `App::state`.
        let db = Db::memory().unwrap();
        db.migrate(&todos_schema()).unwrap();
        let db2 = db.clone();
        db2.insert(
            "todos",
            &[
                ("title", Value::Text("shared".into())),
                ("done", Value::Bool(false)),
            ],
            "id",
        )
        .unwrap();
        assert_eq!(db.count(&QueryBuilder::table("todos")).unwrap(), 1);
    }

    #[test]
    fn pooled_file_db_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Db>();
    }
}
