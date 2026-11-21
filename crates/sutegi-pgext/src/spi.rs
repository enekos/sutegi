//! Run SQL from inside a SQL function, through PostgreSQL's SPI.
//!
//! Queries are always parameterized (`$1`, `$2`, … bound from typed Rust
//! values) and run read-only when the calling function is `IMMUTABLE` or
//! `STABLE`. Results come back as text, copied into Rust: parse what you need.
//!
//! ```ignore
//! let rows = spi::query("SELECT id, name FROM items WHERE price > $1", &[&10i32])?;
//! for row in rows.iter() {
//!     let id: i64 = row.get(0).unwrap_or("0").parse()?;
//! }
//! ```
//!
//! A failed query returns an [`Error`](crate::Error) that
//! [`is_postgres_error`](crate::Error::is_postgres_error): propagate it.

use crate::error::Result;
use crate::ffi::current;
use crate::value::ToSql;

/// Rows returned by [`query`], every value in its text form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rows {
    columns: Vec<String>,
    cells: Vec<Option<String>>,
    processed: u64,
}

impl Rows {
    #[must_use]
    pub fn columns(&self) -> &[String] {
        &self.columns
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.cells
            .len()
            .checked_div(self.columns.len())
            .unwrap_or(0)
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Rows the statement processed (inserted/updated/deleted/returned).
    #[must_use]
    pub fn processed(&self) -> u64 {
        self.processed
    }

    #[must_use]
    pub fn get(&self, row: usize) -> Option<Row<'_>> {
        let n = self.columns.len();
        let start = row.checked_mul(n)?;
        let cells = self.cells.get(start..start.checked_add(n)?)?;
        Some(Row {
            columns: &self.columns,
            cells,
        })
    }

    #[must_use]
    pub fn first(&self) -> Option<Row<'_>> {
        self.get(0)
    }

    pub fn iter(&self) -> impl Iterator<Item = Row<'_>> {
        (0..self.len()).filter_map(|i| self.get(i))
    }
}

/// One row of [`Rows`].
#[derive(Debug, Clone, Copy)]
pub struct Row<'r> {
    columns: &'r [String],
    cells: &'r [Option<String>],
}

impl<'r> Row<'r> {
    /// Column `index`'s text, or `None` for `NULL` or a missing column.
    #[must_use]
    pub fn get(&self, index: usize) -> Option<&'r str> {
        self.cells.get(index)?.as_deref()
    }

    /// The column named `name`'s text, or `None` for `NULL` or no such column.
    #[must_use]
    pub fn get_by_name(&self, name: &str) -> Option<&'r str> {
        let i = self.columns.iter().position(|c| c == name)?;
        self.get(i)
    }
}

fn exec(sql: &str, params: &[&dyn ToSql], limit: i64) -> Result<Rows> {
    let call = current()?;
    let raw: Vec<_> = params.iter().map(|p| p.to_raw()).collect();
    let res = call.spi_exec(sql, &raw, limit)?;
    Ok(Rows {
        columns: res.columns,
        cells: res.cells,
        processed: res.processed,
    })
}

/// Runs `sql` with `params` bound to `$1…` and returns every row.
///
/// # Errors
///
/// The query's error (a PostgreSQL error — propagate it), or outside a SQL
/// function call.
pub fn query(sql: &str, params: &[&dyn ToSql]) -> Result<Rows> {
    exec(sql, params, 0)
}

/// Like [`query`], but stops after `limit` rows.
///
/// # Errors
///
/// As [`query`].
pub fn query_limit(sql: &str, params: &[&dyn ToSql], limit: u32) -> Result<Rows> {
    exec(sql, params, i64::from(limit.max(1)))
}

/// Runs a statement and returns how many rows it processed.
///
/// # Errors
///
/// As [`query`].
pub fn execute(sql: &str, params: &[&dyn ToSql]) -> Result<u64> {
    exec(sql, params, 0).map(|r| r.processed)
}

/// Runs `sql` and returns the first column of the first row, `None` when
/// there is no row or the value is `NULL`.
///
/// # Errors
///
/// As [`query`].
pub fn query_scalar(sql: &str, params: &[&dyn ToSql]) -> Result<Option<String>> {
    let rows = exec(sql, params, 1)?;
    Ok(rows.first().and_then(|r| r.get(0)).map(str::to_owned))
}

impl From<Rows> for Vec<Vec<Option<String>>> {
    fn from(rows: Rows) -> Self {
        let n = rows.columns.len().max(1);
        rows.cells.chunks(n).map(<[_]>::to_vec).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake::{FakeCall, Outcome, Val};
    use crate::value::IntoReturn;

    #[test]
    fn query_binds_params_and_copies_rows() {
        let fake = FakeCall::new(vec![]).with_spi(
            vec!["id", "name"],
            vec![vec![Some("1"), Some("a")], vec![Some("2"), None]],
        );
        let out = fake.invoke("q", 0, |call| {
            let rows = query("select id, name from t where x > $1", &[&5i32, &"y"])?;
            assert_eq!(rows.len(), 2);
            assert_eq!(rows.columns(), ["id", "name"]);
            assert_eq!(rows.get(1).and_then(|r| r.get_by_name("name")), None);
            assert_eq!(rows.first().and_then(|r| r.get(1)), Some("a"));
            assert_eq!(rows.get(2).map(|r| r.get(0)), None);
            let total: i64 = rows
                .iter()
                .filter_map(|r| r.get(0)?.parse::<i64>().ok())
                .sum();
            total.into_return(call)
        });
        assert_eq!(out, Outcome::Value(Val::Int(3)));
    }

    #[test]
    fn a_failed_query_poisons_the_call() {
        let out = FakeCall::new(vec![])
            .with_spi_error()
            .invoke("q", 0, |call| {
                let err = execute("select boom", &[]).unwrap_err();
                assert!(err.is_postgres_error());
                // Swallowing the error doesn't help: the parked error still wins.
                ().into_return(call)
            });
        assert_eq!(out, Outcome::Pending);
    }
}
