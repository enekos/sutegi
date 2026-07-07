//! The pure-std **PostgreSQL** execution layer — sutegi's *multi-pod server*
//! store, backed by the [`sutegi_pg`] driver. Available with the `postgres`
//! feature.
//!
//! This is the cross-pod backend: many app replicas share one database and one
//! durable view of the data, where the [`crate::db::Db`] (SQLite) layer is
//! per-process. It implements the same [`Backend`] trait, so a repository
//! written against one runs on the other by changing only the type it holds.
//!
//! Reach for Postgres when you run more than one instance, need a shared
//! source of truth, or use the durable job queue; reach for SQLite when a
//! single node (or a local KV store) is enough.

use crate::backend::Backend;
use crate::value::{create_table_sql, Dialect, TableSchema, Value};
use std::cell::RefCell;
use sutegi_json::Json;
use sutegi_pg::Config;

// Re-exported so transaction users and direct pool users don't need a separate
// `sutegi-pg` dependency.
pub use sutegi_pg::{Client, PgValue, Pool};

/// Translate the query builder's `?` placeholders into PostgreSQL's positional
/// `$1, $2, …` form. `?` inside single-quoted string literals is left untouched.
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

fn to_pg_value(v: &Value) -> PgValue {
    match v {
        Value::Null => PgValue::Null,
        Value::Int(i) => PgValue::Int(*i),
        Value::Real(r) => PgValue::Real(*r),
        Value::Text(s) => PgValue::Text(s.clone()),
        Value::Bool(b) => PgValue::Bool(*b),
        Value::Json(j) => PgValue::Json(j.to_string()),
        Value::Vector(vec) => PgValue::Vector(crate::value::vector_to_text(vec)),
    }
}

/// `INSERT … RETURNING pk` with `$n` placeholders — shared by [`Pg`] and [`Tx`].
fn insert_sql(
    table: &str,
    cols: &[(&str, Value)],
    pk: &str,
) -> Result<(String, Vec<PgValue>), String> {
    crate::builder::validate_write_idents(table, cols, &[pk])?;
    let names: Vec<&str> = cols.iter().map(|(n, _)| *n).collect();
    let placeholders: Vec<String> = (1..=cols.len()).map(|i| format!("${i}")).collect();
    let sql = format!(
        "INSERT INTO {} ({}) VALUES ({}) RETURNING {}",
        table,
        names.join(", "),
        placeholders.join(", "),
        pk
    );
    Ok((sql, cols.iter().map(|(_, v)| to_pg_value(v)).collect()))
}

/// `INSERT … ON CONFLICT … DO UPDATE … RETURNING pk` — shared by [`Pg`] / [`Tx`].
fn upsert_sql(
    table: &str,
    cols: &[(&str, Value)],
    conflict: &str,
    pk: &str,
) -> Result<(String, Vec<PgValue>), String> {
    crate::builder::validate_write_idents(table, cols, &[conflict, pk])?;
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
    Ok((sql, cols.iter().map(|(_, v)| to_pg_value(v)).collect()))
}

/// Extract the `pk` column from a single-row `RETURNING` result.
fn pk_from_rows(rows: &[Json], pk: &str) -> i64 {
    rows.first()
        .and_then(|r| r.get(pk).and_then(Json::as_i64))
        .unwrap_or(0)
}

/// Map a Postgres `information_schema` type back to a [`ColType`].
fn pg_coltype(data_type: &str, udt_name: &str) -> crate::value::ColType {
    use crate::value::ColType;
    match data_type {
        "bigint" | "integer" | "smallint" => ColType::Integer,
        "double precision" | "real" | "numeric" => ColType::Real,
        "boolean" => ColType::Boolean,
        "jsonb" | "json" => ColType::Json,
        // pgvector surfaces as USER-DEFINED with udt_name `vector`. The declared
        // dimension lives in `atttypmod`, not information_schema — reflect it
        // back dimensionless (the diff compares storage, and a dim'd vs
        // dimensionless vector both render `vector*` — documented lossiness).
        _ if udt_name == "vector" => ColType::Vector { dim: None },
        _ => ColType::Text,
    }
}

/// Reflect every public user table out of Postgres over any [`Backend`]
/// (a [`Pg`] pool or a [`Tx`]). Uses `information_schema` + `pg_index`; the
/// framework's `_sutegi_migrations` ledger is excluded.
fn introspect_pg(exec: &dyn Backend) -> Result<Vec<TableSchema>, String> {
    use crate::value::{parse_default_literal, Column, FkAction, ForeignKey, Index};

    let s = |j: &Json, k: &str| j.get(k).and_then(Json::as_str).unwrap_or("").to_string();
    let b = |j: &Json, k: &str| j.get(k).and_then(Json::as_bool).unwrap_or(false);

    let tables = exec.query(
        "SELECT table_name FROM information_schema.tables \
         WHERE table_schema = 'public' AND table_type = 'BASE TABLE' \
         AND table_name NOT LIKE '\\_sutegi\\_%' ORDER BY table_name",
        &[],
    )?;

    let mut out = Vec::new();
    for t in &tables {
        let table = s(t, "table_name");
        let mut schema = TableSchema::new(table.clone());

        // Primary-key columns for this table.
        let pk_rows = exec.query(
            "SELECT kcu.column_name AS col FROM information_schema.table_constraints tc \
             JOIN information_schema.key_column_usage kcu \
               ON tc.constraint_name = kcu.constraint_name AND tc.table_schema = kcu.table_schema \
             WHERE tc.table_schema = 'public' AND tc.table_name = ? \
               AND tc.constraint_type = 'PRIMARY KEY'",
            &[Value::Text(table.clone())],
        )?;
        let pks: Vec<String> = pk_rows.iter().map(|r| s(r, "col")).collect();

        let cols = exec.query(
            "SELECT column_name, data_type, udt_name, is_nullable, column_default \
             FROM information_schema.columns \
             WHERE table_schema = 'public' AND table_name = ? ORDER BY ordinal_position",
            &[Value::Text(table.clone())],
        )?;
        for c in &cols {
            let name = s(c, "column_name");
            let mut col = Column::new(
                name.clone(),
                pg_coltype(&s(c, "data_type"), &s(c, "udt_name")),
            );
            let is_pk = pks.contains(&name);
            if s(c, "is_nullable") == "YES" && !is_pk {
                col = col.nullable();
            }
            if is_pk {
                col = col.primary();
            }
            if let Some(d) = c.get("column_default").and_then(Json::as_str) {
                if let Some(v) = parse_default_literal(d) {
                    col = col.default(v);
                }
            }
            schema.columns.push(col);
        }

        // Foreign keys with their delete rule.
        let fks = exec.query(
            "SELECT kcu.column_name AS col, ccu.table_name AS ref_table, \
                    ccu.column_name AS ref_col, rc.delete_rule AS del \
             FROM information_schema.table_constraints tc \
             JOIN information_schema.key_column_usage kcu \
               ON tc.constraint_name = kcu.constraint_name AND tc.table_schema = kcu.table_schema \
             JOIN information_schema.constraint_column_usage ccu \
               ON tc.constraint_name = ccu.constraint_name AND tc.table_schema = ccu.table_schema \
             JOIN information_schema.referential_constraints rc \
               ON tc.constraint_name = rc.constraint_name \
             WHERE tc.table_schema = 'public' AND tc.table_name = ? \
               AND tc.constraint_type = 'FOREIGN KEY'",
            &[Value::Text(table.clone())],
        )?;
        for fk in &fks {
            schema.foreign_keys.push(ForeignKey {
                column: s(fk, "col"),
                ref_table: s(fk, "ref_table"),
                ref_column: s(fk, "ref_col"),
                on_delete: FkAction::from_sql(&s(fk, "del")),
            });
        }

        // Indexes + uniques. Group multi-column indexes by name; a unique
        // single-column index that isn't ours (`idx_*`) is a column-level
        // UNIQUE (Postgres names it `<table>_<col>_key`) — fold it back.
        let idx_rows = exec.query(
            "SELECT i.relname AS index_name, ix.indisunique AS is_unique, \
                    ix.indisprimary AS is_primary, a.attname AS column_name \
             FROM pg_index ix \
             JOIN pg_class i ON i.oid = ix.indexrelid \
             JOIN pg_class t ON t.oid = ix.indrelid \
             JOIN pg_namespace n ON n.oid = t.relnamespace \
             JOIN unnest(ix.indkey) WITH ORDINALITY AS k(attnum, ord) ON true \
             JOIN pg_attribute a ON a.attrelid = t.oid AND a.attnum = k.attnum \
             WHERE t.relname = ? AND n.nspname = 'public' ORDER BY i.relname, k.ord",
            &[Value::Text(table.clone())],
        )?;
        // Preserve first-seen order of index names.
        let mut order: Vec<String> = Vec::new();
        let mut grouped: std::collections::HashMap<String, (bool, bool, Vec<String>)> =
            std::collections::HashMap::new();
        for r in &idx_rows {
            let name = s(r, "index_name");
            let entry = grouped
                .entry(name.clone())
                .or_insert_with(|| (b(r, "is_unique"), b(r, "is_primary"), Vec::new()));
            entry.2.push(s(r, "column_name"));
            if !order.contains(&name) {
                order.push(name);
            }
        }
        for name in order {
            let (unique, primary, columns) = &grouped[&name];
            if *primary {
                continue;
            }
            if *unique && columns.len() == 1 && !name.starts_with("idx_") {
                if let Some(col) = schema.columns.iter_mut().find(|c| c.name == columns[0]) {
                    col.unique = true;
                }
                continue;
            }
            schema.indexes.push(Index {
                name,
                columns: columns.clone(),
                unique: *unique,
            });
        }

        out.push(schema.normalized());
    }
    Ok(out)
}

/// A PostgreSQL-backed handle, cloneable and shareable across threads (it holds
/// a connection [`Pool`]).
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

    /// Run `f` inside a single-connection transaction: `BEGIN`, then `COMMIT`
    /// on `Ok` or `ROLLBACK` on `Err`. The closure receives a [`Tx`], which is
    /// a [`Backend`] pinned to one connection — so the query builder + `Model`
    /// surface all run inside the transaction (unlike a raw `Client`).
    pub fn transaction<T>(&self, f: impl FnOnce(&Tx) -> Result<T, String>) -> Result<T, String> {
        self.pool.with(|client| {
            client.batch("BEGIN")?;
            let tx = Tx {
                client: RefCell::new(client),
            };
            match f(&tx) {
                Ok(value) => {
                    tx.client.borrow_mut().batch("COMMIT")?;
                    Ok(value)
                }
                Err(e) => {
                    let _ = tx.client.borrow_mut().batch("ROLLBACK");
                    Err(e)
                }
            }
        })
    }
}

impl Backend for Pg {
    fn query(&self, sql: &str, params: &[Value]) -> Result<Vec<Json>, String> {
        let bound: Vec<PgValue> = params.iter().map(to_pg_value).collect();
        self.pool.query(&to_pg_placeholders(sql), &bound)
    }

    fn execute(&self, sql: &str, params: &[Value]) -> Result<usize, String> {
        let bound: Vec<PgValue> = params.iter().map(to_pg_value).collect();
        self.pool
            .execute(&to_pg_placeholders(sql), &bound)
            .map(|n| n as usize)
    }

    fn insert(&self, table: &str, cols: &[(&str, Value)], pk: &str) -> Result<i64, String> {
        let (sql, bound) = insert_sql(table, cols, pk)?;
        Ok(pk_from_rows(&self.pool.query(&sql, &bound)?, pk))
    }

    fn upsert(
        &self,
        table: &str,
        cols: &[(&str, Value)],
        conflict: &str,
        pk: &str,
    ) -> Result<i64, String> {
        let (sql, bound) = upsert_sql(table, cols, conflict, pk)?;
        Ok(pk_from_rows(&self.pool.query(&sql, &bound)?, pk))
    }

    fn migrate(&self, schema: &TableSchema) -> Result<(), String> {
        self.pool
            .batch(&create_table_sql(schema, Dialect::Postgres))
    }

    fn introspect(&self) -> Result<Vec<TableSchema>, String> {
        introspect_pg(self)
    }

    fn dialect(&self) -> Dialect {
        Dialect::Postgres
    }
}

impl crate::backend::Transactional for Pg {
    fn run_in_tx(
        &self,
        f: &mut dyn FnMut(&dyn Backend) -> Result<(), String>,
    ) -> Result<(), String> {
        self.transaction(|tx| f(tx))
    }
}

/// A transaction handle: a [`Backend`] pinned to a single pooled connection for
/// the duration of a [`Pg::transaction`] closure. Uses interior mutability
/// because the trait takes `&self` while the underlying [`Client`] needs
/// `&mut`.
pub struct Tx<'a> {
    client: RefCell<&'a mut Client>,
}

impl Backend for Tx<'_> {
    fn query(&self, sql: &str, params: &[Value]) -> Result<Vec<Json>, String> {
        let bound: Vec<PgValue> = params.iter().map(to_pg_value).collect();
        self.client
            .borrow_mut()
            .query(&to_pg_placeholders(sql), &bound)
    }

    fn execute(&self, sql: &str, params: &[Value]) -> Result<usize, String> {
        let bound: Vec<PgValue> = params.iter().map(to_pg_value).collect();
        self.client
            .borrow_mut()
            .execute(&to_pg_placeholders(sql), &bound)
            .map(|n| n as usize)
    }

    fn insert(&self, table: &str, cols: &[(&str, Value)], pk: &str) -> Result<i64, String> {
        let (sql, bound) = insert_sql(table, cols, pk)?;
        Ok(pk_from_rows(
            &self.client.borrow_mut().query(&sql, &bound)?,
            pk,
        ))
    }

    fn upsert(
        &self,
        table: &str,
        cols: &[(&str, Value)],
        conflict: &str,
        pk: &str,
    ) -> Result<i64, String> {
        let (sql, bound) = upsert_sql(table, cols, conflict, pk)?;
        Ok(pk_from_rows(
            &self.client.borrow_mut().query(&sql, &bound)?,
            pk,
        ))
    }

    fn migrate(&self, schema: &TableSchema) -> Result<(), String> {
        self.client
            .borrow_mut()
            .batch(&create_table_sql(schema, Dialect::Postgres))
    }

    fn introspect(&self) -> Result<Vec<TableSchema>, String> {
        introspect_pg(self)
    }

    fn dialect(&self) -> Dialect {
        Dialect::Postgres
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value::{ColType, Column};

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
        let schema = TableSchema::new("todos")
            .column(Column::new("id", ColType::Integer).primary())
            .column(Column::new("title", ColType::Text))
            .column(Column::new("score", ColType::Real).nullable());
        let sql = create_table_sql(&schema, Dialect::Postgres);
        assert!(sql.contains("id BIGINT GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY"));
        assert!(sql.contains("title TEXT NOT NULL"));
        assert!(sql.contains("score DOUBLE PRECISION"));
        assert!(!sql.contains("score DOUBLE PRECISION NOT NULL"));
    }

    #[test]
    fn pg_ddl_maps_json_and_vector() {
        let schema = TableSchema::new("docs")
            .column(Column::new("meta", ColType::Json))
            .column(Column::new("embedding", ColType::Vector { dim: Some(384) }).nullable())
            .column(Column::new("loose", ColType::Vector { dim: None }).nullable());
        let sql = create_table_sql(&schema, Dialect::Postgres);
        assert!(sql.contains("meta JSONB NOT NULL"));
        assert!(sql.contains("embedding vector(384)"));
        assert!(sql.contains("loose vector"));
        assert!(!sql.contains("loose vector("));
    }

    #[test]
    fn insert_and_upsert_sql_shapes() {
        let (isql, ib) = insert_sql(
            "t",
            &[("a", Value::Int(1)), ("b", Value::Text("x".into()))],
            "id",
        )
        .unwrap();
        assert_eq!(isql, "INSERT INTO t (a, b) VALUES ($1, $2) RETURNING id");
        assert_eq!(ib.len(), 2);

        let (usql, _) = upsert_sql(
            "t",
            &[("id", Value::Int(1)), ("b", Value::Text("x".into()))],
            "id",
            "id",
        )
        .unwrap();
        assert_eq!(
            usql,
            "INSERT INTO t (id, b) VALUES ($1, $2) ON CONFLICT (id) DO UPDATE SET b = EXCLUDED.b RETURNING id"
        );
    }
}
