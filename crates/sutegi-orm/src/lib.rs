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
    #[cfg(feature = "sqlite")]
    fn migrate(conn: &db::Db) -> Result<(), String> {
        conn.migrate(&Self::schema())
    }

    /// Eloquent-style: fetch every row as a JSON object.
    #[cfg(feature = "sqlite")]
    fn all(conn: &db::Db) -> Result<Vec<sutegi_json::Json>, String> {
        conn.select(&Self::query())
    }

    /// Eloquent-style: find one row by primary key.
    #[cfg(feature = "sqlite")]
    fn find(conn: &db::Db, id: Value) -> Result<Option<sutegi_json::Json>, String> {
        let rows = conn.select(&Self::query().filter(Self::primary_key(), "=", id).limit(1))?;
        Ok(rows.into_iter().next())
    }

    /// Eloquent-style: insert a row, returning its new rowid.
    #[cfg(feature = "sqlite")]
    fn create(conn: &db::Db, values: &[(&str, Value)]) -> Result<i64, String> {
        conn.insert(Self::table(), values)
    }

    /// Typed variant of [`all`](Model::all): hydrate every row into `Self`.
    #[cfg(feature = "sqlite")]
    fn all_typed(conn: &db::Db) -> Result<Vec<Self>, String>
    where
        Self: Sized + FromRow,
    {
        conn.fetch::<Self>(&Self::query())
    }

    /// Typed variant of [`find`](Model::find): hydrate the matching row.
    #[cfg(feature = "sqlite")]
    fn find_typed(conn: &db::Db, id: Value) -> Result<Option<Self>, String>
    where
        Self: Sized + FromRow,
    {
        let rows = conn.fetch::<Self>(&Self::query().filter(Self::primary_key(), "=", id).limit(1))?;
        Ok(rows.into_iter().next())
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
            self.conn.execute_batch("BEGIN").map_err(|e| e.to_string())?;
            match f(self) {
                Ok(value) => {
                    self.conn.execute_batch("COMMIT").map_err(|e| e.to_string())?;
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
            rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())
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
                    Column { name: "id", ty: ColType::Integer, nullable: false, primary: true },
                    Column { name: "title", ty: ColType::Text, nullable: false, primary: false },
                    Column { name: "done", ty: ColType::Boolean, nullable: false, primary: false },
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
                    &[("title", Value::Text("ship sutegi".into())), ("done", Value::Bool(false))],
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
            db.insert("todos", &[("title", Value::Text("a".into())), ("done", Value::Bool(false))]).unwrap();

            // UPDATE via builder.
            let (sql, params) = crate::UpdateBuilder::table("todos")
                .set("done", Value::Bool(true))
                .filter("id", "=", Value::Int(1))
                .build();
            assert_eq!(db.execute(&sql, &params).unwrap(), 1);

            // Rollback leaves state untouched.
            let _ = db.transaction(|tx| {
                tx.insert("todos", &[("title", Value::Text("b".into())), ("done", Value::Bool(false))])?;
                Err::<(), String>("boom".into())
            });
            assert_eq!(db.select(&QueryBuilder::table("todos")).unwrap().len(), 1);

            // Commit persists.
            db.transaction(|tx| {
                tx.insert("todos", &[("title", Value::Text("c".into())), ("done", Value::Bool(false))])?;
                Ok(())
            })
            .unwrap();
            assert_eq!(db.select(&QueryBuilder::table("todos")).unwrap().len(), 2);

            // DELETE via builder.
            let (dsql, dparams) = crate::DeleteBuilder::table("todos").filter("id", "=", Value::Int(1)).build();
            assert_eq!(db.execute(&dsql, &dparams).unwrap(), 1);
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
        row.get(name).ok_or_else(|| format!("missing column '{}'", name))
    }

    fn is_absent(row: &Json, name: &str) -> bool {
        matches!(row.get(name), None | Some(Json::Null))
    }

    pub fn get_i64(row: &Json, name: &str) -> Result<i64, String> {
        match col(row, name)? {
            Json::Num(n) => Ok(*n as i64),
            Json::Bool(b) => Ok(*b as i64),
            Json::Str(s) => s.trim().parse().map_err(|_| format!("column '{}' is not an integer", name)),
            _ => Err(format!("column '{}' is not an integer", name)),
        }
    }

    pub fn get_f64(row: &Json, name: &str) -> Result<f64, String> {
        match col(row, name)? {
            Json::Num(n) => Ok(*n),
            Json::Str(s) => s.trim().parse().map_err(|_| format!("column '{}' is not a number", name)),
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
        if is_absent(row, name) { Ok(None) } else { get_i64(row, name).map(Some) }
    }
    pub fn opt_f64(row: &Json, name: &str) -> Result<Option<f64>, String> {
        if is_absent(row, name) { Ok(None) } else { get_f64(row, name).map(Some) }
    }
    pub fn opt_string(row: &Json, name: &str) -> Result<Option<String>, String> {
        if is_absent(row, name) { Ok(None) } else { get_string(row, name).map(Some) }
    }
    pub fn opt_bool(row: &Json, name: &str) -> Result<Option<bool>, String> {
        if is_absent(row, name) { Ok(None) } else { get_bool(row, name).map(Some) }
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

/// A fluent, parameterized SELECT builder. Emits `?` placeholders and the
/// matching ordered parameter list — driver-agnostic and injection-safe.
#[derive(Clone, Debug)]
pub struct QueryBuilder {
    table: String,
    columns: Vec<String>,
    wheres: Vec<(String, String, Value)>,
    in_clauses: Vec<(String, Vec<Value>)>,
    order: Vec<(String, bool)>,
    limit: Option<i64>,
    offset: Option<i64>,
}

impl QueryBuilder {
    pub fn table(table: &str) -> QueryBuilder {
        QueryBuilder {
            table: table.to_string(),
            columns: Vec::new(),
            wheres: Vec::new(),
            in_clauses: Vec::new(),
            order: Vec::new(),
            limit: None,
            offset: None,
        }
    }

    pub fn select(mut self, cols: &[&str]) -> QueryBuilder {
        self.columns = cols.iter().map(|c| c.to_string()).collect();
        self
    }

    /// Add a `WHERE col <op> ?` clause. `op` is e.g. `=`, `>`, `LIKE`.
    pub fn filter(mut self, col: &str, op: &str, value: Value) -> QueryBuilder {
        self.wheres.push((col.to_string(), op.to_string(), value));
        self
    }

    /// Add a `WHERE col IN (?, ?, …)` clause. An empty list matches nothing.
    pub fn filter_in(mut self, col: &str, values: Vec<Value>) -> QueryBuilder {
        self.in_clauses.push((col.to_string(), values));
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

    /// The `WHERE` clause shared by `build` and `build_count`.
    fn where_clause(&self) -> (String, Vec<Value>) {
        let mut clauses = Vec::new();
        let mut params = Vec::new();
        for (c, op, v) in &self.wheres {
            params.push(v.clone());
            clauses.push(format!("{} {} ?", c, op));
        }
        for (c, values) in &self.in_clauses {
            if values.is_empty() {
                // IN () is invalid SQL; encode "matches nothing".
                clauses.push("0 = 1".to_string());
                continue;
            }
            let marks = vec!["?"; values.len()].join(", ");
            clauses.push(format!("{} IN ({})", c, marks));
            params.extend(values.iter().cloned());
        }
        let sql = if clauses.is_empty() {
            String::new()
        } else {
            format!(" WHERE {}", clauses.join(" AND "))
        };
        (sql, params)
    }

    /// Build the SELECT SQL and ordered bound parameters.
    pub fn build(&self) -> (String, Vec<Value>) {
        let cols = if self.columns.is_empty() {
            "*".to_string()
        } else {
            self.columns.join(", ")
        };
        let (where_sql, params) = self.where_clause();
        let mut sql = format!("SELECT {} FROM {}{}", cols, self.table, where_sql);

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

    /// Build a `SELECT COUNT(*)` over the same table/filters (ignores
    /// columns/order/limit), for pagination totals.
    pub fn build_count(&self) -> (String, Vec<Value>) {
        let (where_sql, params) = self.where_clause();
        (format!("SELECT COUNT(*) AS count FROM {}{}", self.table, where_sql), params)
    }
}

/// A parameterized `UPDATE` builder.
#[derive(Clone, Debug)]
pub struct UpdateBuilder {
    table: String,
    sets: Vec<(String, Value)>,
    wheres: Vec<(String, String, Value)>,
}

impl UpdateBuilder {
    pub fn table(table: &str) -> UpdateBuilder {
        UpdateBuilder { table: table.to_string(), sets: Vec::new(), wheres: Vec::new() }
    }
    pub fn set(mut self, col: &str, value: Value) -> UpdateBuilder {
        self.sets.push((col.to_string(), value));
        self
    }
    pub fn filter(mut self, col: &str, op: &str, value: Value) -> UpdateBuilder {
        self.wheres.push((col.to_string(), op.to_string(), value));
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
        let mut sql = format!("UPDATE {} SET {}", self.table, assignments.join(", "));
        if !self.wheres.is_empty() {
            let clauses: Vec<String> = self
                .wheres
                .iter()
                .map(|(c, op, v)| {
                    params.push(v.clone());
                    format!("{} {} ?", c, op)
                })
                .collect();
            sql.push_str(&format!(" WHERE {}", clauses.join(" AND ")));
        }
        (sql, params)
    }
}

/// A parameterized `DELETE` builder.
#[derive(Clone, Debug)]
pub struct DeleteBuilder {
    table: String,
    wheres: Vec<(String, String, Value)>,
}

impl DeleteBuilder {
    pub fn table(table: &str) -> DeleteBuilder {
        DeleteBuilder { table: table.to_string(), wheres: Vec::new() }
    }
    pub fn filter(mut self, col: &str, op: &str, value: Value) -> DeleteBuilder {
        self.wheres.push((col.to_string(), op.to_string(), value));
        self
    }
    pub fn build(&self) -> (String, Vec<Value>) {
        let mut params = Vec::new();
        let mut sql = format!("DELETE FROM {}", self.table);
        if !self.wheres.is_empty() {
            let clauses: Vec<String> = self
                .wheres
                .iter()
                .map(|(c, op, v)| {
                    params.push(v.clone());
                    format!("{} {} ?", c, op)
                })
                .collect();
            sql.push_str(&format!(" WHERE {}", clauses.join(" AND ")));
        }
        (sql, params)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn todos() -> TableSchema {
        TableSchema {
            table: "todos",
            columns: vec![
                Column { name: "id", ty: ColType::Integer, nullable: false, primary: true },
                Column { name: "title", ty: ColType::Text, nullable: false, primary: false },
                Column { name: "done", ty: ColType::Boolean, nullable: false, primary: false },
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
    fn update_and_delete_builders() {
        let (sql, params) = UpdateBuilder::table("todos")
            .set("title", Value::Text("new".into()))
            .set("done", Value::Bool(true))
            .filter("id", "=", Value::Int(5))
            .build();
        assert_eq!(sql, "UPDATE todos SET title = ?, done = ? WHERE id = ?");
        assert_eq!(params.len(), 3);

        let (dsql, dparams) = DeleteBuilder::table("todos").filter("id", "=", Value::Int(5)).build();
        assert_eq!(dsql, "DELETE FROM todos WHERE id = ?");
        assert_eq!(dparams, vec![Value::Int(5)]);
    }
}
