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

use crate::backend::Backend;
use crate::value::{create_table_sql, TableSchema, Value};
use rusqlite::{params_from_iter, types::Value as SqlValue, Connection};
use std::collections::BTreeMap;
use sutegi_json::Json;

/// A single SQLite connection. Cheap to create; not `Clone` (wrap in
/// `Arc<Mutex<Db>>` to share across threads, as the framework does).
pub struct Db {
    conn: Connection,
}

impl Db {
    /// Open an in-memory database (great for tests and demos).
    pub fn memory() -> Result<Db, String> {
        Connection::open_in_memory()
            .map(|conn| Db { conn })
            .map_err(|e| e.to_string())
    }

    /// Open (or create) a database file.
    pub fn open(path: &str) -> Result<Db, String> {
        Connection::open(path)
            .map(|conn| Db { conn })
            .map_err(|e| e.to_string())
    }

    /// Run `f` inside a transaction: BEGIN, then COMMIT on `Ok` or ROLLBACK on
    /// `Err`. The closure receives `&Db`, which is a [`Backend`], so the whole
    /// query builder + `Model` surface works inside the transaction.
    pub fn transaction<T>(&self, f: impl FnOnce(&Db) -> Result<T, String>) -> Result<T, String> {
        self.conn
            .execute_batch("BEGIN")
            .map_err(|e| e.to_string())?;
        match f(self) {
            Ok(value) => {
                self.conn
                    .execute_batch("COMMIT")
                    .map_err(|e| e.to_string())?;
                Ok(value)
            }
            Err(e) => {
                let _ = self.conn.execute_batch("ROLLBACK");
                Err(e)
            }
        }
    }
}

impl Backend for Db {
    fn query(&self, sql: &str, params: &[Value]) -> Result<Vec<Json>, String> {
        let mut stmt = self.conn.prepare(sql).map_err(|e| e.to_string())?;
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

    fn execute(&self, sql: &str, params: &[Value]) -> Result<usize, String> {
        let bound: Vec<SqlValue> = params.iter().map(to_sql).collect();
        self.conn
            .execute(sql, params_from_iter(bound))
            .map_err(|e| e.to_string())
    }

    // SQLite uses the implicit rowid / `last_insert_rowid()`, so the named
    // primary key is not needed here.
    fn insert(&self, table: &str, cols: &[(&str, Value)], _pk: &str) -> Result<i64, String> {
        let names: Vec<&str> = cols.iter().map(|(n, _)| *n).collect();
        let placeholders = vec!["?"; cols.len()].join(", ");
        let sql = format!(
            "INSERT INTO {} ({}) VALUES ({})",
            table,
            names.join(", "),
            placeholders
        );
        let bound: Vec<SqlValue> = cols.iter().map(|(_, v)| to_sql(v)).collect();
        self.conn
            .execute(&sql, params_from_iter(bound))
            .map_err(|e| e.to_string())?;
        Ok(self.conn.last_insert_rowid())
    }

    fn upsert(
        &self,
        table: &str,
        cols: &[(&str, Value)],
        conflict: &str,
        _pk: &str,
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
        self.conn
            .execute(&sql, params_from_iter(bound))
            .map_err(|e| e.to_string())?;
        Ok(self.conn.last_insert_rowid())
    }

    fn migrate(&self, schema: &TableSchema) -> Result<(), String> {
        self.conn
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
        // API on `&Db` — the transaction seam is backend-agnostic now.
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
}
