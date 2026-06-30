//! A small, driver-agnostic data layer: a typed schema, a fluent query
//! builder that emits parameterized SQL, and a migration emitter.
//!
//! There is intentionally **no bundled database driver** — that would pull in
//! a C library and inflate the binary. The builder produces `(sql, params)`
//! that you hand to whatever driver you opt into (e.g. enable a `rusqlite`
//! feature later). What ships here is pure std, and every model can describe
//! itself as JSON so an agent can discover the data model at runtime.

use sutegi_json::Json;

/// A SQL scalar value used as a bound parameter.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Null,
    Int(i64),
    Real(f64),
    Text(String),
    Bool(bool),
}

impl Value {
    /// Render for display/introspection (NOT for SQL interpolation — use
    /// placeholders for that).
    pub fn to_json(&self) -> Json {
        match self {
            Value::Null => Json::Null,
            Value::Int(i) => Json::Num(*i as f64),
            Value::Real(r) => Json::Num(*r),
            Value::Text(s) => Json::str(s.clone()),
            Value::Bool(b) => Json::Bool(*b),
        }
    }
}

/// A column's storage type.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ColType {
    Integer,
    Real,
    Text,
    Boolean,
}

impl ColType {
    /// SQL type keyword (SQLite-flavored, the common denominator).
    pub fn sql(&self) -> &'static str {
        match self {
            ColType::Integer => "INTEGER",
            ColType::Real => "REAL",
            ColType::Text => "TEXT",
            ColType::Boolean => "BOOLEAN",
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            ColType::Integer => "integer",
            ColType::Real => "real",
            ColType::Text => "text",
            ColType::Boolean => "boolean",
        }
    }
}

/// A single column definition.
#[derive(Clone, Debug)]
pub struct Column {
    pub name: &'static str,
    pub ty: ColType,
    pub nullable: bool,
    pub primary: bool,
}

/// A table's full schema.
#[derive(Clone, Debug)]
pub struct TableSchema {
    pub table: &'static str,
    pub columns: Vec<Column>,
}

/// A runnable execution backend behind the query builder. Both the bundled
/// SQLite layer ([`db::Db`], `sqlite` feature) and the pure-std Postgres layer
/// ([`pg::Pg`], `postgres` feature) implement it, so the same [`Model`] code
/// runs against either — swap the backend, not the call sites.
///
/// The query builder emits canonical `?`-placeholder SQL; each backend is
/// responsible for translating to its own placeholder dialect.
pub trait Backend {
    /// Run a query builder and return rows as JSON objects.
    fn select(&self, qb: &QueryBuilder) -> Result<Vec<sutegi_json::Json>, String>;

    /// Execute a parameterized statement; returns rows affected.
    fn execute(&self, sql: &str, params: &[Value]) -> Result<usize, String>;

    /// Insert a row from `(column, value)` pairs; returns the new primary key.
    /// `pk` names the auto-generated key column (e.g. `id`) for backends that
    /// need an explicit `RETURNING`; backends with a native last-insert-id
    /// may ignore it.
    fn insert(&self, table: &str, cols: &[(&str, Value)], pk: &str) -> Result<i64, String>;

    /// Count rows matching a query builder.
    fn count(&self, qb: &QueryBuilder) -> Result<i64, String>;

    /// Create a table from a schema if it does not already exist.
    fn migrate(&self, schema: &TableSchema) -> Result<(), String>;

    /// Run a query builder and hydrate each row into a typed `FromRow`.
    fn fetch<T: FromRow>(&self, qb: &QueryBuilder) -> Result<Vec<T>, String> {
        self.select(qb)?.iter().map(T::from_row).collect()
    }

    /// Fetch and hydrate the first matching row, if any.
    fn fetch_one<T: FromRow>(&self, qb: &QueryBuilder) -> Result<Option<T>, String> {
        Ok(self.fetch::<T>(qb)?.into_iter().next())
    }
}

/// Anything that maps to a table. Implementors describe their schema; the
/// framework derives migrations, query helpers, and introspection from it.
pub trait Model {
    fn schema() -> TableSchema;

    fn table() -> &'static str {
        Self::schema().table
    }

    /// The primary-key column name (falls back to `id`).
    fn primary_key() -> &'static str {
        Self::schema()
            .columns
            .iter()
            .find(|c| c.primary)
            .map(|c| c.name)
            .unwrap_or("id")
    }

    /// Start a query builder scoped to this model's table.
    fn query() -> QueryBuilder {
        QueryBuilder::table(Self::schema().table)
    }

    /// Create this model's table if it does not exist.
    fn migrate<B: Backend>(conn: &B) -> Result<(), String> {
        conn.migrate(&Self::schema())
    }

    /// Eloquent-style: fetch every row as a JSON object.
    fn all<B: Backend>(conn: &B) -> Result<Vec<sutegi_json::Json>, String> {
        conn.select(&Self::query())
    }

    /// Eloquent-style: find one row by primary key.
    fn find<B: Backend>(conn: &B, id: Value) -> Result<Option<sutegi_json::Json>, String> {
        let rows = conn.select(&Self::query().filter(Self::primary_key(), "=", id).limit(1))?;
        Ok(rows.into_iter().next())
    }

    /// Eloquent-style: insert a row, returning its new primary key.
    fn create<B: Backend>(conn: &B, values: &[(&str, Value)]) -> Result<i64, String> {
        conn.insert(Self::table(), values, Self::primary_key())
    }

    /// Typed variant of [`all`](Model::all): hydrate every row into `Self`.
    fn all_typed<B: Backend>(conn: &B) -> Result<Vec<Self>, String>
    where
        Self: Sized + FromRow,
    {
        conn.fetch::<Self>(&Self::query())
    }

    /// Typed variant of [`find`](Model::find): hydrate the matching row.
    fn find_typed<B: Backend>(conn: &B, id: Value) -> Result<Option<Self>, String>
    where
        Self: Sized + FromRow,
    {
        let rows =
            conn.fetch::<Self>(&Self::query().filter(Self::primary_key(), "=", id).limit(1))?;
        Ok(rows.into_iter().next())
    }

    /// Total row count for this model's table.
    fn count<B: Backend>(conn: &B) -> Result<i64, String> {
        conn.count(&Self::query())
    }

    /// Update columns on the row matching the primary key. Returns rows affected.
    fn update<B: Backend>(conn: &B, id: Value, sets: &[(&str, Value)]) -> Result<usize, String> {
        let mut builder = UpdateBuilder::table(Self::table());
        for (col, value) in sets {
            builder = builder.set(col, value.clone());
        }
        let (sql, params) = builder.filter(Self::primary_key(), "=", id).build();
        conn.execute(&sql, &params)
    }

    /// Delete the row matching the primary key. Returns `true` if a row was removed.
    fn delete<B: Backend>(conn: &B, id: Value) -> Result<bool, String> {
        let (sql, params) = DeleteBuilder::table(Self::table())
            .filter(Self::primary_key(), "=", id)
            .build();
        Ok(conn.execute(&sql, &params)? > 0)
    }
}

/// A thin, runnable SQLite execution layer over the query builder. Available
/// only with the `sqlite` feature. Rows come back as JSON objects — consistent
/// with sutegi's "machine-readable everything" stance and zero-boilerplate
/// without a derive macro.
#[cfg(feature = "sqlite")]
pub mod db {
    use super::{create_table_sql, QueryBuilder, TableSchema, Value};
    use rusqlite::{params_from_iter, types::Value as SqlValue, Connection};
    use std::collections::BTreeMap;
    use sutegi_json::Json;

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

        /// Run a `CREATE TABLE IF NOT EXISTS` derived from a schema.
        pub fn migrate(&self, schema: &TableSchema) -> Result<(), String> {
            self.conn
                .execute_batch(&create_table_sql(schema))
                .map_err(|e| e.to_string())
        }

        /// Execute a parameterized statement, returning affected row count.
        pub fn execute(&self, sql: &str, params: &[Value]) -> Result<usize, String> {
            let bound: Vec<SqlValue> = params.iter().map(to_sql).collect();
            self.conn
                .execute(sql, params_from_iter(bound))
                .map_err(|e| e.to_string())
        }

        /// Insert a row from `(column, value)` pairs; returns the new rowid.
        pub fn insert(&self, table: &str, cols: &[(&str, Value)]) -> Result<i64, String> {
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

        /// Run a query builder and return rows as JSON objects.
        pub fn select(&self, qb: &QueryBuilder) -> Result<Vec<Json>, String> {
            let (sql, params) = qb.build();
            self.query(&sql, &params)
        }

        /// Run a query builder and hydrate each row into a typed `FromRow`.
        pub fn fetch<T: crate::FromRow>(&self, qb: &QueryBuilder) -> Result<Vec<T>, String> {
            let rows = self.select(qb)?;
            rows.iter().map(T::from_row).collect()
        }

        /// Fetch the first matching row as a typed value, if any.
        pub fn fetch_one<T: crate::FromRow>(&self, qb: &QueryBuilder) -> Result<Option<T>, String> {
            let rows = self.select(qb)?;
            rows.first().map(T::from_row).transpose()
        }

        /// Count rows matching a query builder (uses its `build_count`).
        pub fn count(&self, qb: &QueryBuilder) -> Result<i64, String> {
            let (sql, params) = qb.build_count();
            let row = self.query_one(&sql, &params)?;
            Ok(row
                .and_then(|r| r.get("count").and_then(|j| j.as_f64()))
                .map(|f| f as i64)
                .unwrap_or(0))
        }

        /// Whether any row matches.
        pub fn exists(&self, qb: &QueryBuilder) -> Result<bool, String> {
            Ok(self.count(qb)? > 0)
        }

        /// Run a paginated query: returns the page's rows (as JSON) plus the
        /// total count. `page` is 1-based.
        pub fn paginate(
            &self,
            qb: &QueryBuilder,
            page: i64,
            per_page: i64,
        ) -> Result<crate::Page<Json>, String> {
            let total = self.count(qb)?;
            let page = page.max(1);
            let per_page = per_page.max(1);
            let items = self.select(&qb.clone().limit(per_page).offset((page - 1) * per_page))?;
            Ok(crate::Page {
                items,
                total,
                page,
                per_page,
            })
        }

        /// Typed variant of [`paginate`](Db::paginate).
        pub fn paginate_typed<T: crate::FromRow>(
            &self,
            qb: &QueryBuilder,
            page: i64,
            per_page: i64,
        ) -> Result<crate::Page<T>, String> {
            let total = self.count(qb)?;
            let page = page.max(1);
            let per_page = per_page.max(1);
            let items =
                self.fetch::<T>(&qb.clone().limit(per_page).offset((page - 1) * per_page))?;
            Ok(crate::Page {
                items,
                total,
                page,
                per_page,
            })
        }

        /// Insert, or update on primary/unique-key conflict (SQLite UPSERT).
        /// Non-conflict columns are overwritten with the new values.
        pub fn upsert(
            &self,
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
            self.conn
                .execute(&sql, params_from_iter(bound))
                .map_err(|e| e.to_string())?;
            Ok(self.conn.last_insert_rowid())
        }

        /// Run a SELECT and return only the first row (as JSON), if any.
        pub fn query_one(&self, sql: &str, params: &[Value]) -> Result<Option<Json>, String> {
            Ok(self.query(sql, params)?.into_iter().next())
        }

        /// Run `f` inside a transaction: BEGIN, then COMMIT on `Ok` or ROLLBACK
        /// on `Err`. Returns whatever `f` returns.
        pub fn transaction<T>(
            &self,
            f: impl FnOnce(&Db) -> Result<T, String>,
        ) -> Result<T, String> {
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

        /// Run an arbitrary SELECT and return rows as JSON objects.
        pub fn query(&self, sql: &str, params: &[Value]) -> Result<Vec<Json>, String> {
            let mut stmt = self.conn.prepare(sql).map_err(|e| e.to_string())?;
            let col_names: Vec<String> =
                stmt.column_names().iter().map(|s| s.to_string()).collect();
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
    }

    impl crate::Backend for Db {
        fn select(&self, qb: &QueryBuilder) -> Result<Vec<Json>, String> {
            Db::select(self, qb)
        }
        fn execute(&self, sql: &str, params: &[Value]) -> Result<usize, String> {
            Db::execute(self, sql, params)
        }
        // SQLite uses the implicit rowid / `last_insert_rowid()`, so the named
        // primary key is not needed here.
        fn insert(&self, table: &str, cols: &[(&str, Value)], _pk: &str) -> Result<i64, String> {
            Db::insert(self, table, cols)
        }
        fn count(&self, qb: &QueryBuilder) -> Result<i64, String> {
            Db::count(self, qb)
        }
        fn migrate(&self, schema: &TableSchema) -> Result<(), String> {
            Db::migrate(self, schema)
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
        use crate::{ColType, Column, TableSchema};

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
            )
            .unwrap();

            // UPDATE via builder.
            let (sql, params) = crate::UpdateBuilder::table("todos")
                .set("done", Value::Bool(true))
                .filter("id", "=", Value::Int(1))
                .build();
            assert_eq!(db.execute(&sql, &params).unwrap(), 1);

            // Rollback leaves state untouched.
            let _ = db.transaction(|tx| {
                tx.insert(
                    "todos",
                    &[
                        ("title", Value::Text("b".into())),
                        ("done", Value::Bool(false)),
                    ],
                )?;
                Err::<(), String>("boom".into())
            });
            assert_eq!(db.select(&QueryBuilder::table("todos")).unwrap().len(), 1);

            // Commit persists.
            db.transaction(|tx| {
                tx.insert(
                    "todos",
                    &[
                        ("title", Value::Text("c".into())),
                        ("done", Value::Bool(false)),
                    ],
                )?;
                Ok(())
            })
            .unwrap();
            assert_eq!(db.select(&QueryBuilder::table("todos")).unwrap().len(), 2);

            // DELETE via builder.
            let (dsql, dparams) = crate::DeleteBuilder::table("todos")
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
}

/// A runnable PostgreSQL execution layer over the same query builder, backed by
/// the pure-std [`sutegi_pg`] driver. Available with the `postgres` feature.
///
/// This is the cross-pod backend: many app replicas share one database and one
/// durable view of the data, where the `sqlite` layer is per-process. The API
/// mirrors [`db::Db`] so a repository written against one can switch to the
/// other by changing the type it holds.
#[cfg(feature = "postgres")]
pub mod pg {
    use super::{QueryBuilder, TableSchema, Value};
    use sutegi_json::Json;
    use sutegi_pg::Config;

    // Re-exported so transaction closures (which receive a `Client`) and direct
    // pool users don't need a separate `sutegi-pg` dependency.
    pub use sutegi_pg::{Client, PgValue, Pool};

    /// Translate the query builder's `?` placeholders into PostgreSQL's
    /// positional `$1, $2, …` form. `?` inside single-quoted string literals is
    /// left untouched.
    pub fn to_pg_placeholders(sql: &str) -> String {
        let mut out = String::with_capacity(sql.len() + 8);
        let mut n = 0;
        let mut in_str = false;
        for c in sql.chars() {
            match c {
                '\'' => {
                    in_str = !in_str;
                    out.push(c);
                }
                '?' if !in_str => {
                    n += 1;
                    out.push('$');
                    out.push_str(&n.to_string());
                }
                _ => out.push(c),
            }
        }
        out
    }

    /// PostgreSQL `CREATE TABLE IF NOT EXISTS` from a schema. Integer primary
    /// keys become identity columns; `Real` maps to `DOUBLE PRECISION`.
    pub fn create_table_pg(schema: &TableSchema) -> String {
        use super::ColType;
        let mut cols = Vec::new();
        for c in &schema.columns {
            if c.primary && c.ty == ColType::Integer {
                // BY DEFAULT (not ALWAYS) mirrors SQLite's `INTEGER PRIMARY KEY`:
                // auto-generated when omitted, but explicit values are allowed
                // (needed for upsert-by-id and seeding).
                cols.push(format!(
                    "  {} BIGINT GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY",
                    c.name
                ));
                continue;
            }
            let ty = match c.ty {
                ColType::Integer => "BIGINT",
                ColType::Real => "DOUBLE PRECISION",
                ColType::Text => "TEXT",
                ColType::Boolean => "BOOLEAN",
            };
            let mut def = format!("  {} {}", c.name, ty);
            if c.primary {
                def.push_str(" PRIMARY KEY");
            }
            if !c.nullable && !c.primary {
                def.push_str(" NOT NULL");
            }
            cols.push(def);
        }
        format!(
            "CREATE TABLE IF NOT EXISTS {} (\n{}\n)",
            schema.table,
            cols.join(",\n")
        )
    }

    fn to_pg_value(v: &Value) -> PgValue {
        match v {
            Value::Null => PgValue::Null,
            Value::Int(i) => PgValue::Int(*i),
            Value::Real(r) => PgValue::Real(*r),
            Value::Text(s) => PgValue::Text(s.clone()),
            Value::Bool(b) => PgValue::Bool(*b),
        }
    }

    /// A PostgreSQL-backed handle, cloneable and shareable across threads (it
    /// holds a connection [`Pool`]).
    #[derive(Clone)]
    pub struct Pg {
        pool: Pool,
    }

    impl Pg {
        /// Wrap an existing connection pool.
        pub fn new(pool: Pool) -> Pg {
            Pg { pool }
        }

        /// Connect using a `postgres://…` URL with a pool of `max_size`.
        pub fn connect(url: &str, max_size: usize) -> Result<Pg, String> {
            Ok(Pg {
                pool: Pool::new(Config::from_url(url)?, max_size),
            })
        }

        /// Build from `DATABASE_URL`/`PG*` environment variables.
        pub fn from_env(max_size: usize) -> Result<Pg, String> {
            Ok(Pg {
                pool: Pool::from_env(max_size)?,
            })
        }

        /// The underlying pool, for queue drivers and advanced use.
        pub fn pool(&self) -> &Pool {
            &self.pool
        }

        /// Run a `CREATE TABLE IF NOT EXISTS` derived from a schema.
        pub fn migrate(&self, schema: &TableSchema) -> Result<(), String> {
            self.pool.batch(&create_table_pg(schema))
        }

        /// Execute a parameterized statement (`?` placeholders), returning the
        /// affected row count.
        pub fn execute(&self, sql: &str, params: &[Value]) -> Result<usize, String> {
            let bound: Vec<PgValue> = params.iter().map(to_pg_value).collect();
            self.pool
                .execute(&to_pg_placeholders(sql), &bound)
                .map(|n| n as usize)
        }

        /// Run an arbitrary SELECT (`?` placeholders) and return JSON rows.
        pub fn query(&self, sql: &str, params: &[Value]) -> Result<Vec<Json>, String> {
            let bound: Vec<PgValue> = params.iter().map(to_pg_value).collect();
            self.pool.query(&to_pg_placeholders(sql), &bound)
        }

        /// Insert a row, returning the generated primary key via `RETURNING`.
        pub fn insert(&self, table: &str, cols: &[(&str, Value)], pk: &str) -> Result<i64, String> {
            let names: Vec<&str> = cols.iter().map(|(n, _)| *n).collect();
            let placeholders: Vec<String> = (1..=cols.len()).map(|i| format!("${i}")).collect();
            let sql = format!(
                "INSERT INTO {} ({}) VALUES ({}) RETURNING {}",
                table,
                names.join(", "),
                placeholders.join(", "),
                pk
            );
            let bound: Vec<PgValue> = cols.iter().map(|(_, v)| to_pg_value(v)).collect();
            let rows = self.pool.query(&sql, &bound)?;
            Ok(rows
                .first()
                .and_then(|r| r.get(pk).and_then(Json::as_i64))
                .unwrap_or(0))
        }

        /// Run a query builder and return rows as JSON objects.
        pub fn select(&self, qb: &QueryBuilder) -> Result<Vec<Json>, String> {
            let (sql, params) = qb.build();
            self.query(&sql, &params)
        }

        /// Run a query builder and hydrate each row into a typed `FromRow`.
        pub fn fetch<T: crate::FromRow>(&self, qb: &QueryBuilder) -> Result<Vec<T>, String> {
            self.select(qb)?.iter().map(T::from_row).collect()
        }

        /// Fetch the first matching row as a typed value, if any.
        pub fn fetch_one<T: crate::FromRow>(&self, qb: &QueryBuilder) -> Result<Option<T>, String> {
            Ok(self.fetch::<T>(qb)?.into_iter().next())
        }

        /// Count rows matching a query builder.
        pub fn count(&self, qb: &QueryBuilder) -> Result<i64, String> {
            let (sql, params) = qb.build_count();
            let row = self.query_one(&sql, &params)?;
            Ok(row
                .and_then(|r| r.get("count").and_then(|j| j.as_f64()))
                .map(|f| f as i64)
                .unwrap_or(0))
        }

        /// Whether any row matches.
        pub fn exists(&self, qb: &QueryBuilder) -> Result<bool, String> {
            Ok(self.count(qb)? > 0)
        }

        /// Run a SELECT and return only the first row, if any.
        pub fn query_one(&self, sql: &str, params: &[Value]) -> Result<Option<Json>, String> {
            Ok(self.query(sql, params)?.into_iter().next())
        }

        /// Run a paginated query (1-based `page`): rows plus the total count.
        pub fn paginate(
            &self,
            qb: &QueryBuilder,
            page: i64,
            per_page: i64,
        ) -> Result<crate::Page<Json>, String> {
            let total = self.count(qb)?;
            let page = page.max(1);
            let per_page = per_page.max(1);
            let items = self.select(&qb.clone().limit(per_page).offset((page - 1) * per_page))?;
            Ok(crate::Page {
                items,
                total,
                page,
                per_page,
            })
        }

        /// Typed variant of [`paginate`](Pg::paginate).
        pub fn paginate_typed<T: crate::FromRow>(
            &self,
            qb: &QueryBuilder,
            page: i64,
            per_page: i64,
        ) -> Result<crate::Page<T>, String> {
            let total = self.count(qb)?;
            let page = page.max(1);
            let per_page = per_page.max(1);
            let items =
                self.fetch::<T>(&qb.clone().limit(per_page).offset((page - 1) * per_page))?;
            Ok(crate::Page {
                items,
                total,
                page,
                per_page,
            })
        }

        /// Insert, or update on primary/unique-key conflict (`ON CONFLICT … DO
        /// UPDATE`). Returns the affected row's primary key.
        pub fn upsert(
            &self,
            table: &str,
            cols: &[(&str, Value)],
            conflict: &str,
            pk: &str,
        ) -> Result<i64, String> {
            let names: Vec<&str> = cols.iter().map(|(n, _)| *n).collect();
            let placeholders: Vec<String> = (1..=cols.len()).map(|i| format!("${i}")).collect();
            let updates: Vec<String> = names
                .iter()
                .filter(|n| **n != conflict)
                .map(|n| format!("{n} = EXCLUDED.{n}"))
                .collect();
            let sql = format!(
                "INSERT INTO {} ({}) VALUES ({}) ON CONFLICT ({}) DO UPDATE SET {} RETURNING {}",
                table,
                names.join(", "),
                placeholders.join(", "),
                conflict,
                updates.join(", "),
                pk
            );
            let bound: Vec<PgValue> = cols.iter().map(|(_, v)| to_pg_value(v)).collect();
            let rows = self.pool.query(&sql, &bound)?;
            Ok(rows
                .first()
                .and_then(|r| r.get(pk).and_then(Json::as_i64))
                .unwrap_or(0))
        }

        /// Run `f` inside a single-connection transaction: `BEGIN`, then
        /// `COMMIT` on `Ok` or `ROLLBACK` on `Err`. The closure receives a
        /// [`Client`] so every statement runs on the same connection.
        pub fn transaction<T>(
            &self,
            f: impl FnOnce(&mut Client) -> Result<T, String>,
        ) -> Result<T, String> {
            self.pool.with(|client| {
                client.batch("BEGIN")?;
                match f(client) {
                    Ok(value) => {
                        client.batch("COMMIT")?;
                        Ok(value)
                    }
                    Err(e) => {
                        let _ = client.batch("ROLLBACK");
                        Err(e)
                    }
                }
            })
        }
    }

    impl crate::Backend for Pg {
        fn select(&self, qb: &QueryBuilder) -> Result<Vec<Json>, String> {
            Pg::select(self, qb)
        }
        fn execute(&self, sql: &str, params: &[Value]) -> Result<usize, String> {
            Pg::execute(self, sql, params)
        }
        fn insert(&self, table: &str, cols: &[(&str, Value)], pk: &str) -> Result<i64, String> {
            Pg::insert(self, table, cols, pk)
        }
        fn count(&self, qb: &QueryBuilder) -> Result<i64, String> {
            Pg::count(self, qb)
        }
        fn migrate(&self, schema: &TableSchema) -> Result<(), String> {
            Pg::migrate(self, schema)
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn placeholder_translation() {
            assert_eq!(
                to_pg_placeholders("SELECT * FROM t WHERE a = ? AND b = ?"),
                "SELECT * FROM t WHERE a = $1 AND b = $2"
            );
            // `?` inside a string literal is preserved.
            assert_eq!(
                to_pg_placeholders("SELECT '? literal' FROM t WHERE a = ?"),
                "SELECT '? literal' FROM t WHERE a = $1"
            );
        }

        #[test]
        fn pg_ddl_uses_identity_and_double() {
            use crate::{ColType, Column, TableSchema};
            let schema = TableSchema {
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
                        name: "score",
                        ty: ColType::Real,
                        nullable: true,
                        primary: false,
                    },
                ],
            };
            let sql = create_table_pg(&schema);
            assert!(sql.contains("id BIGINT GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY"));
            assert!(sql.contains("title TEXT NOT NULL"));
            assert!(sql.contains("score DOUBLE PRECISION"));
            assert!(!sql.contains("score DOUBLE PRECISION NOT NULL"));
        }
    }
}

/// Hydration from a JSON row (as produced by the `sqlite` layer or any JSON
/// source) into a typed struct. Implemented by `#[derive(Model)]`.
pub trait FromRow: Sized {
    fn from_row(row: &sutegi_json::Json) -> Result<Self, String>;
}

/// Column extractors used by generated `FromRow` impls. They tolerate the
/// SQLite quirks (booleans stored as `0`/`1`, integers arriving as floats),
/// which is what makes typed round-tripping clean.
pub mod row {
    pub use crate::FromRow;
    use sutegi_json::Json;

    fn col<'a>(row: &'a Json, name: &str) -> Result<&'a Json, String> {
        row.get(name)
            .ok_or_else(|| format!("missing column '{}'", name))
    }

    fn is_absent(row: &Json, name: &str) -> bool {
        matches!(row.get(name), None | Some(Json::Null))
    }

    pub fn get_i64(row: &Json, name: &str) -> Result<i64, String> {
        match col(row, name)? {
            Json::Num(n) => Ok(*n as i64),
            Json::Bool(b) => Ok(*b as i64),
            Json::Str(s) => s
                .trim()
                .parse()
                .map_err(|_| format!("column '{}' is not an integer", name)),
            _ => Err(format!("column '{}' is not an integer", name)),
        }
    }

    pub fn get_f64(row: &Json, name: &str) -> Result<f64, String> {
        match col(row, name)? {
            Json::Num(n) => Ok(*n),
            Json::Str(s) => s
                .trim()
                .parse()
                .map_err(|_| format!("column '{}' is not a number", name)),
            _ => Err(format!("column '{}' is not a number", name)),
        }
    }

    pub fn get_string(row: &Json, name: &str) -> Result<String, String> {
        match col(row, name)? {
            Json::Str(s) => Ok(s.clone()),
            Json::Num(n) => Ok(n.to_string()),
            Json::Bool(b) => Ok(b.to_string()),
            _ => Err(format!("column '{}' is not text", name)),
        }
    }

    pub fn get_bool(row: &Json, name: &str) -> Result<bool, String> {
        match col(row, name)? {
            Json::Bool(b) => Ok(*b),
            Json::Num(n) => Ok(*n != 0.0),
            Json::Str(s) => Ok(matches!(s.trim(), "1" | "true" | "TRUE" | "yes")),
            _ => Err(format!("column '{}' is not a boolean", name)),
        }
    }

    pub fn opt_i64(row: &Json, name: &str) -> Result<Option<i64>, String> {
        if is_absent(row, name) {
            Ok(None)
        } else {
            get_i64(row, name).map(Some)
        }
    }
    pub fn opt_f64(row: &Json, name: &str) -> Result<Option<f64>, String> {
        if is_absent(row, name) {
            Ok(None)
        } else {
            get_f64(row, name).map(Some)
        }
    }
    pub fn opt_string(row: &Json, name: &str) -> Result<Option<String>, String> {
        if is_absent(row, name) {
            Ok(None)
        } else {
            get_string(row, name).map(Some)
        }
    }
    pub fn opt_bool(row: &Json, name: &str) -> Result<Option<bool>, String> {
        if is_absent(row, name) {
            Ok(None)
        } else {
            get_bool(row, name).map(Some)
        }
    }
}

/// Describe a table schema as JSON, for `/__introspect`.
pub fn schema_json(schema: &TableSchema) -> Json {
    let cols = schema
        .columns
        .iter()
        .map(|c| {
            Json::obj(vec![
                ("name", Json::str(c.name)),
                ("type", Json::str(c.ty.name())),
                ("nullable", Json::Bool(c.nullable)),
                ("primary", Json::Bool(c.primary)),
            ])
        })
        .collect();
    Json::obj(vec![
        ("table", Json::str(schema.table)),
        ("columns", Json::arr(cols)),
    ])
}

/// Emit a `CREATE TABLE IF NOT EXISTS` statement from a schema.
pub fn create_table_sql(schema: &TableSchema) -> String {
    let mut cols = Vec::new();
    for c in &schema.columns {
        let mut def = format!("  {} {}", c.name, c.ty.sql());
        if c.primary {
            def.push_str(" PRIMARY KEY");
        }
        if !c.nullable && !c.primary {
            def.push_str(" NOT NULL");
        }
        cols.push(def);
    }
    format!(
        "CREATE TABLE IF NOT EXISTS {} (\n{}\n);",
        schema.table,
        cols.join(",\n")
    )
}

/// A single `WHERE` predicate, shared by the SELECT/UPDATE/DELETE builders.
/// Predicates are joined with `AND`; use [`Predicate::Or`] for an OR group.
#[derive(Clone, Debug)]
enum Predicate {
    Cmp(String, String, Value),       // col op ?
    In(String, Vec<Value>),           // col IN (?, …)  — empty => "0 = 1"
    IsNull(String, bool),             // true => IS NULL, false => IS NOT NULL
    Or(Vec<(String, String, Value)>), // (a op ? OR b op ? …)
    Raw(String, Vec<Value>),          // an arbitrary parenthesized fragment
}

/// Render a predicate list to a `" WHERE …"` clause (empty string if none) plus
/// the ordered bound parameters. Shared by every builder so AND/OR/NULL/raw
/// behave identically across SELECT, UPDATE, and DELETE.
fn render_predicates(preds: &[Predicate]) -> (String, Vec<Value>) {
    if preds.is_empty() {
        return (String::new(), Vec::new());
    }
    let mut clauses = Vec::new();
    let mut params = Vec::new();
    for p in preds {
        match p {
            Predicate::Cmp(c, op, v) => {
                params.push(v.clone());
                clauses.push(format!("{} {} ?", c, op));
            }
            Predicate::In(c, values) => {
                if values.is_empty() {
                    clauses.push("0 = 1".to_string());
                } else {
                    let marks = vec!["?"; values.len()].join(", ");
                    clauses.push(format!("{} IN ({})", c, marks));
                    params.extend(values.iter().cloned());
                }
            }
            Predicate::IsNull(c, is_null) => {
                clauses.push(format!(
                    "{} IS {}NULL",
                    c,
                    if *is_null { "" } else { "NOT " }
                ));
            }
            Predicate::Or(group) => {
                if group.is_empty() {
                    clauses.push("0 = 1".to_string());
                } else {
                    let parts: Vec<String> = group
                        .iter()
                        .map(|(c, op, v)| {
                            params.push(v.clone());
                            format!("{} {} ?", c, op)
                        })
                        .collect();
                    clauses.push(format!("({})", parts.join(" OR ")));
                }
            }
            Predicate::Raw(frag, ps) => {
                clauses.push(format!("({})", frag));
                params.extend(ps.iter().cloned());
            }
        }
    }
    (format!(" WHERE {}", clauses.join(" AND ")), params)
}

/// A fluent, parameterized SELECT builder with filters, OR groups, joins,
/// grouping, ordering, and paging. Emits `?` placeholders and the matching
/// ordered parameter list — driver-agnostic and injection-safe.
#[derive(Clone, Debug)]
pub struct QueryBuilder {
    table: String,
    distinct: bool,
    columns: Vec<String>,
    joins: Vec<String>,
    preds: Vec<Predicate>,
    group_by: Vec<String>,
    order: Vec<(String, bool)>,
    limit: Option<i64>,
    offset: Option<i64>,
}

impl QueryBuilder {
    pub fn table(table: &str) -> QueryBuilder {
        QueryBuilder {
            table: table.to_string(),
            distinct: false,
            columns: Vec::new(),
            joins: Vec::new(),
            preds: Vec::new(),
            group_by: Vec::new(),
            order: Vec::new(),
            limit: None,
            offset: None,
        }
    }

    pub fn select(mut self, cols: &[&str]) -> QueryBuilder {
        self.columns = cols.iter().map(|c| c.to_string()).collect();
        self
    }

    /// `SELECT DISTINCT …`.
    pub fn distinct(mut self) -> QueryBuilder {
        self.distinct = true;
        self
    }

    /// Add a `WHERE col <op> ?` clause (AND-joined). `op` is e.g. `=`, `>`, `LIKE`.
    pub fn filter(mut self, col: &str, op: &str, value: Value) -> QueryBuilder {
        self.preds
            .push(Predicate::Cmp(col.to_string(), op.to_string(), value));
        self
    }

    /// Add a `WHERE col IN (?, ?, …)` clause. An empty list matches nothing.
    pub fn filter_in(mut self, col: &str, values: Vec<Value>) -> QueryBuilder {
        self.preds.push(Predicate::In(col.to_string(), values));
        self
    }

    /// `WHERE col IS NULL`.
    pub fn where_null(mut self, col: &str) -> QueryBuilder {
        self.preds.push(Predicate::IsNull(col.to_string(), true));
        self
    }

    /// `WHERE col IS NOT NULL`.
    pub fn where_not_null(mut self, col: &str) -> QueryBuilder {
        self.preds.push(Predicate::IsNull(col.to_string(), false));
        self
    }

    /// An OR group, AND-joined with the rest: `AND (a op ? OR b op ? …)`.
    pub fn or_group(mut self, group: &[(&str, &str, Value)]) -> QueryBuilder {
        self.preds.push(Predicate::Or(
            group
                .iter()
                .map(|(c, op, v)| (c.to_string(), op.to_string(), v.clone()))
                .collect(),
        ));
        self
    }

    /// A `col LIKE ?` convenience.
    pub fn like(self, col: &str, pattern: &str) -> QueryBuilder {
        self.filter(col, "LIKE", Value::Text(pattern.to_string()))
    }

    /// An arbitrary parenthesized WHERE fragment with its own bound params —
    /// an escape hatch for SQL the builder doesn't model.
    pub fn where_raw(mut self, fragment: &str, params: Vec<Value>) -> QueryBuilder {
        self.preds
            .push(Predicate::Raw(fragment.to_string(), params));
        self
    }

    /// `INNER JOIN other ON left = right`.
    pub fn join(mut self, other: &str, left: &str, right: &str) -> QueryBuilder {
        self.joins
            .push(format!("JOIN {} ON {} = {}", other, left, right));
        self
    }

    /// `LEFT JOIN other ON left = right`.
    pub fn left_join(mut self, other: &str, left: &str, right: &str) -> QueryBuilder {
        self.joins
            .push(format!("LEFT JOIN {} ON {} = {}", other, left, right));
        self
    }

    /// `GROUP BY …` (call with the grouping columns).
    pub fn group_by(mut self, cols: &[&str]) -> QueryBuilder {
        self.group_by = cols.iter().map(|c| c.to_string()).collect();
        self
    }

    /// Add an `ORDER BY` term. Call multiple times for tie-breaking columns.
    pub fn order_by(mut self, col: &str, descending: bool) -> QueryBuilder {
        self.order.push((col.to_string(), descending));
        self
    }

    pub fn limit(mut self, n: i64) -> QueryBuilder {
        self.limit = Some(n);
        self
    }

    pub fn offset(mut self, n: i64) -> QueryBuilder {
        self.offset = Some(n);
        self
    }

    fn build_from_and_joins(&self) -> String {
        let mut s = self.table.clone();
        for j in &self.joins {
            s.push(' ');
            s.push_str(j);
        }
        s
    }

    /// Build the SELECT SQL and ordered bound parameters.
    pub fn build(&self) -> (String, Vec<Value>) {
        let cols = if self.columns.is_empty() {
            "*".to_string()
        } else {
            self.columns.join(", ")
        };
        let distinct = if self.distinct { "DISTINCT " } else { "" };
        let (where_sql, params) = render_predicates(&self.preds);
        let mut sql = format!(
            "SELECT {}{} FROM {}{}",
            distinct,
            cols,
            self.build_from_and_joins(),
            where_sql
        );

        if !self.group_by.is_empty() {
            sql.push_str(&format!(" GROUP BY {}", self.group_by.join(", ")));
        }
        if !self.order.is_empty() {
            let terms: Vec<String> = self
                .order
                .iter()
                .map(|(c, desc)| format!("{} {}", c, if *desc { "DESC" } else { "ASC" }))
                .collect();
            sql.push_str(&format!(" ORDER BY {}", terms.join(", ")));
        }
        if let Some(n) = self.limit {
            sql.push_str(&format!(" LIMIT {}", n));
        }
        if let Some(n) = self.offset {
            sql.push_str(&format!(" OFFSET {}", n));
        }
        (sql, params)
    }

    /// Build a `SELECT COUNT(*)` over the same table/joins/filters (ignores
    /// columns/order/limit/group), for pagination totals.
    pub fn build_count(&self) -> (String, Vec<Value>) {
        let (where_sql, params) = render_predicates(&self.preds);
        (
            format!(
                "SELECT COUNT(*) AS count FROM {}{}",
                self.build_from_and_joins(),
                where_sql
            ),
            params,
        )
    }
}

/// A parameterized `UPDATE` builder.
#[derive(Clone, Debug)]
pub struct UpdateBuilder {
    table: String,
    sets: Vec<(String, Value)>,
    preds: Vec<Predicate>,
}

impl UpdateBuilder {
    pub fn table(table: &str) -> UpdateBuilder {
        UpdateBuilder {
            table: table.to_string(),
            sets: Vec::new(),
            preds: Vec::new(),
        }
    }
    pub fn set(mut self, col: &str, value: Value) -> UpdateBuilder {
        self.sets.push((col.to_string(), value));
        self
    }
    pub fn filter(mut self, col: &str, op: &str, value: Value) -> UpdateBuilder {
        self.preds
            .push(Predicate::Cmp(col.to_string(), op.to_string(), value));
        self
    }
    pub fn where_null(mut self, col: &str) -> UpdateBuilder {
        self.preds.push(Predicate::IsNull(col.to_string(), true));
        self
    }
    pub fn where_raw(mut self, fragment: &str, params: Vec<Value>) -> UpdateBuilder {
        self.preds
            .push(Predicate::Raw(fragment.to_string(), params));
        self
    }
    /// Returns `(sql, params)`. Params are SET values first, then WHERE values.
    pub fn build(&self) -> (String, Vec<Value>) {
        let mut params = Vec::new();
        let assignments: Vec<String> = self
            .sets
            .iter()
            .map(|(c, v)| {
                params.push(v.clone());
                format!("{} = ?", c)
            })
            .collect();
        let (where_sql, where_params) = render_predicates(&self.preds);
        params.extend(where_params);
        (
            format!(
                "UPDATE {} SET {}{}",
                self.table,
                assignments.join(", "),
                where_sql
            ),
            params,
        )
    }
}

/// A parameterized `DELETE` builder.
#[derive(Clone, Debug)]
pub struct DeleteBuilder {
    table: String,
    preds: Vec<Predicate>,
}

impl DeleteBuilder {
    pub fn table(table: &str) -> DeleteBuilder {
        DeleteBuilder {
            table: table.to_string(),
            preds: Vec::new(),
        }
    }
    pub fn filter(mut self, col: &str, op: &str, value: Value) -> DeleteBuilder {
        self.preds
            .push(Predicate::Cmp(col.to_string(), op.to_string(), value));
        self
    }
    pub fn where_null(mut self, col: &str) -> DeleteBuilder {
        self.preds.push(Predicate::IsNull(col.to_string(), true));
        self
    }
    pub fn where_raw(mut self, fragment: &str, params: Vec<Value>) -> DeleteBuilder {
        self.preds
            .push(Predicate::Raw(fragment.to_string(), params));
        self
    }
    pub fn build(&self) -> (String, Vec<Value>) {
        let (where_sql, params) = render_predicates(&self.preds);
        (format!("DELETE FROM {}{}", self.table, where_sql), params)
    }
}

/// A page of results plus paging metadata — the return of `Db::paginate`.
#[derive(Clone, Debug)]
pub struct Page<T> {
    pub items: Vec<T>,
    pub total: i64,
    pub page: i64,
    pub per_page: i64,
}

impl<T> Page<T> {
    pub fn total_pages(&self) -> i64 {
        if self.per_page <= 0 {
            0
        } else {
            (self.total + self.per_page - 1) / self.per_page
        }
    }
    pub fn has_next(&self) -> bool {
        self.page < self.total_pages()
    }
    pub fn has_prev(&self) -> bool {
        self.page > 1
    }
}

impl Page<sutegi_json::Json> {
    /// `{ items, total, page, per_page, pages }` — ready to return from a handler.
    pub fn to_json(&self) -> sutegi_json::Json {
        sutegi_json::Json::obj(vec![
            ("items", sutegi_json::Json::arr(self.items.clone())),
            ("total", sutegi_json::Json::int(self.total)),
            ("page", sutegi_json::Json::int(self.page)),
            ("per_page", sutegi_json::Json::int(self.per_page)),
            ("pages", sutegi_json::Json::int(self.total_pages())),
        ])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn todos() -> TableSchema {
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
    fn builds_parameterized_select() {
        let (sql, params) = QueryBuilder::table("todos")
            .select(&["id", "title"])
            .filter("done", "=", Value::Bool(false))
            .order_by("id", true)
            .limit(10)
            .build();
        assert_eq!(
            sql,
            "SELECT id, title FROM todos WHERE done = ? ORDER BY id DESC LIMIT 10"
        );
        assert_eq!(params, vec![Value::Bool(false)]);
    }

    #[test]
    fn emits_create_table() {
        let sql = create_table_sql(&todos());
        assert!(sql.contains("CREATE TABLE IF NOT EXISTS todos"));
        assert!(sql.contains("id INTEGER PRIMARY KEY"));
        assert!(sql.contains("title TEXT NOT NULL"));
    }

    #[test]
    fn select_with_in_offset_and_multi_order() {
        let (sql, params) = QueryBuilder::table("todos")
            .filter_in("id", vec![Value::Int(1), Value::Int(2)])
            .order_by("done", false)
            .order_by("id", true)
            .limit(10)
            .offset(20)
            .build();
        assert_eq!(
            sql,
            "SELECT * FROM todos WHERE id IN (?, ?) ORDER BY done ASC, id DESC LIMIT 10 OFFSET 20"
        );
        assert_eq!(params.len(), 2);
    }

    #[test]
    fn empty_in_matches_nothing() {
        let (sql, _) = QueryBuilder::table("t").filter_in("id", vec![]).build();
        assert_eq!(sql, "SELECT * FROM t WHERE 0 = 1");
    }

    #[test]
    fn count_ignores_columns() {
        let (sql, _) = QueryBuilder::table("t")
            .select(&["a", "b"])
            .filter("done", "=", Value::Bool(true))
            .build_count();
        assert_eq!(sql, "SELECT COUNT(*) AS count FROM t WHERE done = ?");
    }

    #[test]
    fn or_group_null_like_and_joins() {
        let (sql, params) = QueryBuilder::table("todos")
            .filter("done", "=", Value::Bool(false))
            .or_group(&[
                ("priority", "=", Value::Text("high".into())),
                ("pinned", "=", Value::Bool(true)),
            ])
            .where_not_null("title")
            .like("title", "%sutegi%")
            .build();
        assert_eq!(
            sql,
            "SELECT * FROM todos WHERE done = ? AND (priority = ? OR pinned = ?) AND title IS NOT NULL AND title LIKE ?"
        );
        assert_eq!(params.len(), 4); // done, priority, pinned, like-pattern

        let (jsql, _) = QueryBuilder::table("todos")
            .select(&["todos.id", "users.name"])
            .join("users", "users.id", "todos.user_id")
            .group_by(&["users.name"])
            .build();
        assert_eq!(
            jsql,
            "SELECT todos.id, users.name FROM todos JOIN users ON users.id = todos.user_id GROUP BY users.name"
        );

        let (dsql, _) = QueryBuilder::table("t").distinct().select(&["a"]).build();
        assert_eq!(dsql, "SELECT DISTINCT a FROM t");
    }

    #[test]
    fn where_raw_fragment() {
        let (sql, params) = QueryBuilder::table("t")
            .where_raw(
                "created_at > ? AND created_at < ?",
                vec![Value::Int(1), Value::Int(9)],
            )
            .build();
        assert_eq!(
            sql,
            "SELECT * FROM t WHERE (created_at > ? AND created_at < ?)"
        );
        assert_eq!(params.len(), 2);
    }

    #[test]
    fn update_and_delete_builders() {
        let (sql, params) = UpdateBuilder::table("todos")
            .set("title", Value::Text("new".into()))
            .set("done", Value::Bool(true))
            .filter("id", "=", Value::Int(5))
            .build();
        assert_eq!(sql, "UPDATE todos SET title = ?, done = ? WHERE id = ?");
        assert_eq!(params.len(), 3);

        let (dsql, dparams) = DeleteBuilder::table("todos")
            .filter("id", "=", Value::Int(5))
            .build();
        assert_eq!(dsql, "DELETE FROM todos WHERE id = ?");
        assert_eq!(dparams, vec![Value::Int(5)]);
    }

    #[test]
    fn update_delete_support_null_and_raw() {
        let (usql, _) = UpdateBuilder::table("t")
            .set("done", Value::Bool(true))
            .where_null("deleted_at")
            .build();
        assert_eq!(usql, "UPDATE t SET done = ? WHERE deleted_at IS NULL");

        let (dsql, dparams) = DeleteBuilder::table("t")
            .where_raw("age > ?", vec![Value::Int(65)])
            .build();
        assert_eq!(dsql, "DELETE FROM t WHERE (age > ?)");
        assert_eq!(dparams, vec![Value::Int(65)]);
    }

    #[test]
    fn build_count_keeps_joins_and_filters() {
        let (sql, params) = QueryBuilder::table("todos")
            .join("users", "users.id", "todos.user_id")
            .filter("users.active", "=", Value::Bool(true))
            .build_count();
        assert_eq!(
            sql,
            "SELECT COUNT(*) AS count FROM todos JOIN users ON users.id = todos.user_id WHERE users.active = ?"
        );
        assert_eq!(params.len(), 1);
    }

    #[test]
    fn coltype_and_value_mappings() {
        assert_eq!(ColType::Integer.sql(), "INTEGER");
        assert_eq!(ColType::Boolean.sql(), "BOOLEAN");
        assert_eq!(ColType::Real.name(), "real");
        assert_eq!(Value::Bool(true).to_json(), Json::Bool(true));
        assert_eq!(Value::Int(5).to_json(), Json::Num(5.0));
        assert_eq!(Value::Null.to_json(), Json::Null);
        assert_eq!(Value::Text("x".into()).to_json(), Json::str("x"));
    }

    #[test]
    fn schema_json_describes_columns() {
        let j = schema_json(&todos());
        assert_eq!(j.get("table").and_then(Json::as_str), Some("todos"));
        let cols = j.get("columns").and_then(Json::as_array).unwrap();
        assert_eq!(cols.len(), 3);
        // The id column is primary and not nullable.
        let id = &cols[0];
        assert_eq!(id.get("name").and_then(Json::as_str), Some("id"));
        assert_eq!(id.get("primary").and_then(Json::as_bool), Some(true));
    }

    #[test]
    fn model_default_primary_key_and_query() {
        // primary_key() finds the primary column; query() scopes to the table.
        struct T;
        impl Model for T {
            fn schema() -> TableSchema {
                todos()
            }
        }
        assert_eq!(T::primary_key(), "id");
        assert_eq!(T::table(), "todos");
        let (sql, _) = T::query().build();
        assert_eq!(sql, "SELECT * FROM todos");
    }

    #[test]
    fn row_extractors_tolerate_sqlite_quirks() {
        // SQLite hands booleans back as 0/1 and ints can arrive as floats/strings.
        let row = Json::obj(vec![
            ("n", Json::Num(7.0)),
            ("done", Json::int(1)),
            ("name", Json::str("x")),
            ("ratio", Json::str("2.5")),
            ("flag", Json::str("true")),
        ]);
        assert_eq!(row::get_i64(&row, "n").unwrap(), 7);
        assert!(row::get_bool(&row, "done").unwrap());
        assert_eq!(row::get_string(&row, "name").unwrap(), "x");
        assert_eq!(row::get_f64(&row, "ratio").unwrap(), 2.5);
        assert!(row::get_bool(&row, "flag").unwrap());
        // Missing column → error; absent optional → None.
        assert!(row::get_i64(&row, "missing").is_err());
        assert_eq!(
            row::opt_i64(&Json::obj(vec![("x", Json::Null)]), "x").unwrap(),
            None
        );
        assert_eq!(row::opt_string(&row, "name").unwrap().as_deref(), Some("x"));
    }

    #[test]
    fn page_pagination_math() {
        let page = Page {
            items: vec![1, 2, 3],
            total: 10,
            page: 2,
            per_page: 3,
        };
        assert_eq!(page.total_pages(), 4); // ceil(10/3)
        assert!(page.has_next());
        assert!(page.has_prev());
        // First page has no previous; a zero per_page degrades to 0 pages.
        let first = Page {
            items: vec![1],
            total: 2,
            page: 1,
            per_page: 3,
        };
        assert!(!first.has_prev());
        let degenerate = Page {
            items: Vec::<i32>::new(),
            total: 5,
            page: 1,
            per_page: 0,
        };
        assert_eq!(degenerate.total_pages(), 0);
        // JSON shape carries the computed page count.
        let j = Page {
            items: vec![Json::int(1)],
            total: 5,
            page: 1,
            per_page: 2,
        }
        .to_json();
        assert_eq!(j.get("pages").and_then(Json::as_i64), Some(3));
        assert_eq!(j.get("total").and_then(Json::as_i64), Some(5));
    }
}
