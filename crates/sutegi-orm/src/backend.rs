//! The execution seam. [`Backend`] is the one trait every runnable store
//! implements — bundled SQLite ([`crate::db::Db`], `sqlite` feature), pure-std
//! Postgres ([`crate::pg::Pg`], `postgres` feature), and a Postgres transaction
//! handle ([`crate::pg::Tx`]). [`Model`] is written once against `Backend`, so
//! the same app code runs on any of them — **swap the backend, not the call
//! sites**.
//!
//! The trait is deliberately small: five **required primitives** each backend
//! must provide (they differ by SQL dialect), and a set of **default methods**
//! (`select`/`count`/`exists`/`paginate`/…) implemented once on top of them.
//! That keeps the read/write surface identical across backends with zero
//! per-backend duplication.

use crate::builder::{DeleteBuilder, Page, QueryBuilder, UpdateBuilder};
use crate::value::{TableSchema, Value};
use sutegi_json::Json;

/// A runnable execution backend behind the query builder.
///
/// Implementors provide the five dialect-specific **primitives**
/// (`query`/`execute`/`insert`/`upsert`/`migrate`); everything else is a
/// **default method** built on top, so the full read/write API is available on
/// any backend without re-implementation.
///
/// The query builder emits canonical `?`-placeholder SQL; each backend is
/// responsible for translating to its own placeholder dialect inside `query`
/// and `execute`.
pub trait Backend {
    // --- required primitives (SQL dialect differs per backend) ---

    /// Run an arbitrary parameterized SELECT and return rows as JSON objects.
    fn query(&self, sql: &str, params: &[Value]) -> Result<Vec<Json>, String>;

    /// Execute a parameterized statement; returns rows affected.
    fn execute(&self, sql: &str, params: &[Value]) -> Result<usize, String>;

    /// Insert a row from `(column, value)` pairs; returns the new primary key.
    /// `pk` names the auto-generated key column (e.g. `id`) for backends that
    /// need an explicit `RETURNING`; backends with a native last-insert-id may
    /// ignore it.
    fn insert(&self, table: &str, cols: &[(&str, Value)], pk: &str) -> Result<i64, String>;

    /// Insert, or update on `conflict`-column conflict (`ON CONFLICT … DO
    /// UPDATE`). Non-conflict columns are overwritten. Returns the affected
    /// row's primary key `pk`.
    fn upsert(
        &self,
        table: &str,
        cols: &[(&str, Value)],
        conflict: &str,
        pk: &str,
    ) -> Result<i64, String>;

    /// Create a table from a schema if it does not already exist.
    fn migrate(&self, schema: &TableSchema) -> Result<(), String>;

    // --- default methods (shared, implemented via the primitives) ---

    /// Run a query builder and return rows as JSON objects.
    fn select(&self, qb: &QueryBuilder) -> Result<Vec<Json>, String> {
        let (sql, params) = qb.build();
        self.query(&sql, &params)
    }

    /// Run a SELECT and return only the first row, if any.
    fn query_one(&self, sql: &str, params: &[Value]) -> Result<Option<Json>, String> {
        Ok(self.query(sql, params)?.into_iter().next())
    }

    /// Count rows matching a query builder (uses its `build_count`).
    fn count(&self, qb: &QueryBuilder) -> Result<i64, String> {
        let (sql, params) = qb.build_count();
        Ok(self
            .query_one(&sql, &params)?
            .and_then(|r| r.get("count").and_then(|j| j.as_f64()))
            .map(|f| f as i64)
            .unwrap_or(0))
    }

    /// Whether any row matches.
    fn exists(&self, qb: &QueryBuilder) -> Result<bool, String> {
        Ok(self.count(qb)? > 0)
    }

    /// Run a query builder and hydrate each row into a typed [`FromRow`].
    fn fetch<T: FromRow>(&self, qb: &QueryBuilder) -> Result<Vec<T>, String> {
        self.select(qb)?.iter().map(T::from_row).collect()
    }

    /// Fetch and hydrate the first matching row, if any.
    fn fetch_one<T: FromRow>(&self, qb: &QueryBuilder) -> Result<Option<T>, String> {
        Ok(self.fetch::<T>(qb)?.into_iter().next())
    }

    /// Run a paginated query (1-based `page`): the page's rows plus the total.
    fn paginate(&self, qb: &QueryBuilder, page: i64, per_page: i64) -> Result<Page<Json>, String> {
        let (page, per_page) = (page.max(1), per_page.max(1));
        let total = self.count(qb)?;
        let items = self.select(&qb.clone().limit(per_page).offset((page - 1) * per_page))?;
        Ok(Page {
            items,
            total,
            page,
            per_page,
        })
    }

    /// Typed variant of [`paginate`](Backend::paginate).
    fn paginate_typed<T: FromRow>(
        &self,
        qb: &QueryBuilder,
        page: i64,
        per_page: i64,
    ) -> Result<Page<T>, String> {
        let (page, per_page) = (page.max(1), per_page.max(1));
        let total = self.count(qb)?;
        let items = self.fetch::<T>(&qb.clone().limit(per_page).offset((page - 1) * per_page))?;
        Ok(Page {
            items,
            total,
            page,
            per_page,
        })
    }
}

/// Anything that maps to a table. Implementors describe their schema; the
/// framework derives migrations, query helpers, and introspection from it.
/// Every helper is generic over [`Backend`], so a model runs unchanged on
/// SQLite, Postgres, or inside a transaction.
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

    /// Active-record-style: fetch every row as a JSON object.
    fn all<B: Backend>(conn: &B) -> Result<Vec<Json>, String> {
        conn.select(&Self::query())
    }

    /// Active-record-style: find one row by primary key.
    fn find<B: Backend>(conn: &B, id: Value) -> Result<Option<Json>, String> {
        let rows = conn.select(&Self::query().filter(Self::primary_key(), "=", id).limit(1))?;
        Ok(rows.into_iter().next())
    }

    /// Active-record-style: insert a row, returning its new primary key.
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

/// Hydration from a JSON row (as produced by any [`Backend`]) into a typed
/// struct. Implemented by `#[derive(Model)]`. Strict: every non-nullable column
/// must be present, because a real database row always has them.
pub trait FromRow: Sized {
    fn from_row(row: &Json) -> Result<Self, String>;
}

/// Hydration from a **partial** JSON object — e.g. a request body or an AI
/// tool's arguments — where columns the caller doesn't supply (a
/// database-assigned `id`, a `done` flag with a natural default) are filled with
/// their type's default instead of erroring. Implemented by `#[derive(Model)]`.
///
/// This is the lenient counterpart to [`FromRow`]: use `from_row` for rows that
/// came out of a `Backend`, and `from_input` for data coming in from a client.
/// It backs [`Ctx::validated`](../../sutegi_web/struct.Ctx.html) and is handy in
/// tool closures: `let todo = Todo::from_input(&args)?;`.
pub trait FromInput: Sized {
    fn from_input(row: &Json) -> Result<Self, String>;
}

/// Column extractors used by generated `FromRow` impls. They tolerate the
/// SQLite quirks (booleans stored as `0`/`1`, integers arriving as floats),
/// which is what makes typed round-tripping clean across backends.
pub mod row {
    pub use super::FromRow;
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

    /// A JSON column. Postgres returns structured JSON directly; SQLite returns
    /// the serialized text, which is parsed here — so either way you get a real
    /// [`Json`] value back.
    pub fn get_json(row: &Json, name: &str) -> Result<Json, String> {
        match col(row, name)? {
            Json::Str(s) => {
                Json::parse(s).map_err(|e| format!("column '{}' is not valid JSON: {}", name, e))
            }
            other => Ok(other.clone()),
        }
    }

    /// An embedding vector column, in either backend's representation
    /// (pgvector's `[1,2,3]` text, a SQLite text array, or a JSON array of
    /// numbers) → `Vec<f32>`.
    pub fn get_vector(row: &Json, name: &str) -> Result<Vec<f32>, String> {
        match col(row, name)? {
            Json::Str(s) => crate::value::vector_from_text(s),
            Json::Arr(items) => items
                .iter()
                .map(|v| {
                    v.as_f64().map(|f| f as f32).ok_or_else(|| {
                        format!("column '{}' has a non-numeric vector element", name)
                    })
                })
                .collect(),
            _ => Err(format!("column '{}' is not a vector", name)),
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
    pub fn opt_json(row: &Json, name: &str) -> Result<Option<Json>, String> {
        if is_absent(row, name) {
            Ok(None)
        } else {
            get_json(row, name).map(Some)
        }
    }
    pub fn opt_vector(row: &Json, name: &str) -> Result<Option<Vec<f32>>, String> {
        if is_absent(row, name) {
            Ok(None)
        } else {
            get_vector(row, name).map(Some)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value::{ColType, Column};

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
    fn model_default_primary_key_and_query() {
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
        assert!(row::get_i64(&row, "missing").is_err());
        assert_eq!(
            row::opt_i64(&Json::obj(vec![("x", Json::Null)]), "x").unwrap(),
            None
        );
        assert_eq!(row::opt_string(&row, "name").unwrap().as_deref(), Some("x"));
    }

    /// A hand-rolled in-memory `Backend` proving the default methods
    /// (`select`/`count`/`exists`/`paginate`) work through the primitives
    /// alone — no SQL engine required.
    #[test]
    fn default_methods_ride_on_primitives() {
        use std::cell::RefCell;

        #[derive(Default)]
        struct Mem {
            rows: RefCell<Vec<Json>>,
        }
        impl Backend for Mem {
            fn query(&self, sql: &str, _p: &[Value]) -> Result<Vec<Json>, String> {
                // Only two shapes are issued by the default methods we exercise:
                // a COUNT(*) and a plain select.
                if sql.contains("COUNT(*)") {
                    let n = self.rows.borrow().len() as i64;
                    Ok(vec![Json::obj(vec![("count", Json::int(n))])])
                } else {
                    Ok(self.rows.borrow().clone())
                }
            }
            fn execute(&self, _sql: &str, _p: &[Value]) -> Result<usize, String> {
                Ok(0)
            }
            fn insert(&self, _t: &str, cols: &[(&str, Value)], _pk: &str) -> Result<i64, String> {
                let obj = Json::obj(cols.iter().map(|(k, v)| (*k, v.to_json())).collect());
                self.rows.borrow_mut().push(obj);
                Ok(self.rows.borrow().len() as i64)
            }
            fn upsert(
                &self,
                t: &str,
                cols: &[(&str, Value)],
                _c: &str,
                pk: &str,
            ) -> Result<i64, String> {
                self.insert(t, cols, pk)
            }
            fn migrate(&self, _s: &TableSchema) -> Result<(), String> {
                Ok(())
            }
        }

        let mem = Mem::default();
        assert!(!mem.exists(&QueryBuilder::table("t")).unwrap());
        mem.insert("t", &[("id", Value::Int(1))], "id").unwrap();
        mem.insert("t", &[("id", Value::Int(2))], "id").unwrap();
        assert_eq!(mem.count(&QueryBuilder::table("t")).unwrap(), 2);
        assert!(mem.exists(&QueryBuilder::table("t")).unwrap());
        let page = mem.paginate(&QueryBuilder::table("t"), 1, 10).unwrap();
        assert_eq!(page.total, 2);
        assert_eq!(page.items.len(), 2);
    }
}
