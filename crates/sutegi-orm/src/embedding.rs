//! First-class **embeddings**: a [`Vector`] type, similarity [`Metric`]s, and
//! nearest-neighbour search that runs over any [`Backend`].
//!
//! An embedding is stored in a [`ColType::Vector`](crate::ColType::Vector)
//! column — `vector(dim)` on Postgres (via the pgvector extension), `TEXT` on
//! SQLite — and travels in pgvector's canonical `[1,2,3]` text form, so the
//! same value round-trips identically on either backend.
//!
//! Two search paths, same distance semantics (lower = closer, matching
//! pgvector's operators):
//!
//! - [`nearest`] / [`nearest_typed`] — **portable brute force**: load the
//!   candidate rows and rank them in Rust. Correct on every backend, ideal for
//!   single-node SQLite or modest tables.
//! - [`nearest_pushdown_typed`] — push `ORDER BY col <=> ? LIMIT k` down to the
//!   database. Requires the backend to understand pgvector distance operators
//!   (Postgres + pgvector), where it uses an ANN index for scale.
//!
//! ```ignore
//! // Ten closest documents to a query embedding, brute force:
//! let hits = embedding::nearest_typed::<Doc, _>(
//!     &db, &Doc::query(), "embedding", &query_vec, 10, Metric::Cosine,
//! )?;
//! for (doc, distance) in hits { /* … */ }
//! ```

use crate::backend::{Backend, FromRow};
use crate::builder::{valid_identifier, QueryBuilder};
use crate::value::{vector_from_text, vector_to_text, Value};
use sutegi_json::Json;

/// An embedding vector — a thin newtype over `Vec<f32>` with the vector algebra
/// the similarity metrics need.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Vector(pub Vec<f32>);

impl Vector {
    pub fn new(data: Vec<f32>) -> Vector {
        Vector(data)
    }

    /// The number of dimensions.
    pub fn dim(&self) -> usize {
        self.0.len()
    }

    pub fn as_slice(&self) -> &[f32] {
        &self.0
    }

    pub fn into_inner(self) -> Vec<f32> {
        self.0
    }

    /// Dot product (zips over the shorter length if dimensions differ).
    pub fn dot(&self, other: &[f32]) -> f32 {
        dot(&self.0, other)
    }

    /// Euclidean (L2) distance.
    pub fn l2_distance(&self, other: &[f32]) -> f32 {
        l2_distance(&self.0, other)
    }

    /// Cosine similarity in `[-1, 1]` (0 if either vector has zero norm).
    pub fn cosine_similarity(&self, other: &[f32]) -> f32 {
        cosine_similarity(&self.0, other)
    }

    /// The L2 norm (magnitude).
    pub fn norm(&self) -> f32 {
        self.0.iter().map(|x| x * x).sum::<f32>().sqrt()
    }

    /// A unit-length copy (unchanged if the norm is zero).
    pub fn normalized(&self) -> Vector {
        let n = self.norm();
        if n == 0.0 {
            self.clone()
        } else {
            Vector(self.0.iter().map(|x| x / n).collect())
        }
    }

    /// pgvector's canonical `[1,2,3]` text form.
    pub fn to_text(&self) -> String {
        vector_to_text(&self.0)
    }

    /// Parse from `[1,2,3]` text.
    pub fn from_text(s: &str) -> Result<Vector, String> {
        vector_from_text(s).map(Vector)
    }

    /// A bound query parameter for this vector.
    pub fn to_value(&self) -> Value {
        Value::Vector(self.0.clone())
    }
}

impl From<Vec<f32>> for Vector {
    fn from(v: Vec<f32>) -> Vector {
        Vector(v)
    }
}

impl std::ops::Deref for Vector {
    type Target = [f32];
    fn deref(&self) -> &[f32] {
        &self.0
    }
}

/// A similarity/distance metric. Distances follow pgvector's convention that
/// **lower is closer**, so brute-force and pushed-down search rank identically.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Metric {
    /// Cosine distance: `1 - cosine_similarity`.
    Cosine,
    /// Euclidean (L2) distance.
    L2,
    /// Negative inner product (higher dot product ⇒ smaller distance).
    InnerProduct,
}

impl Metric {
    /// Distance between two vectors — lower means more similar. Vectors of
    /// differing dimension are treated as infinitely far apart (never "near").
    pub fn distance(&self, a: &[f32], b: &[f32]) -> f32 {
        if a.len() != b.len() {
            return f32::INFINITY;
        }
        match self {
            Metric::Cosine => 1.0 - cosine_similarity(a, b),
            Metric::L2 => l2_distance(a, b),
            Metric::InnerProduct => -dot(a, b),
        }
    }

    /// The pgvector operator that computes this distance (`<=>`, `<->`, `<#>`).
    pub fn pg_operator(&self) -> &'static str {
        match self {
            Metric::Cosine => "<=>",
            Metric::L2 => "<->",
            Metric::InnerProduct => "<#>",
        }
    }

    /// The pgvector operator class for building an ANN index over this metric.
    pub fn pg_ops_class(&self) -> &'static str {
        match self {
            Metric::Cosine => "vector_cosine_ops",
            Metric::L2 => "vector_l2_ops",
            Metric::InnerProduct => "vector_ip_ops",
        }
    }
}

/// Dot product over the shorter of the two slices.
pub fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

/// Euclidean distance over the shorter of the two slices.
pub fn l2_distance(a: &[f32], b: &[f32]) -> f32 {
    a.iter()
        .zip(b)
        .map(|(x, y)| (x - y) * (x - y))
        .sum::<f32>()
        .sqrt()
}

/// Cosine similarity; 0 when either vector has zero magnitude.
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    let na = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let nb = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if na == 0.0 || nb == 0.0 {
        0.0
    } else {
        dot(a, b) / (na * nb)
    }
}

/// Ascending-by-distance sort, then keep the first `k`.
fn top_k<T>(mut scored: Vec<(T, f32)>, k: usize) -> Vec<(T, f32)> {
    scored.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
    scored.truncate(k);
    scored
}

/// Brute-force k-nearest search over the rows a query builder selects: load the
/// candidates, score the `column` vector of each against `target`, and return
/// the `k` closest as `(row, distance)`. Rows with a missing/NULL vector are
/// skipped. Portable across every backend.
pub fn nearest<B: Backend>(
    conn: &B,
    query: &QueryBuilder,
    column: &str,
    target: &[f32],
    k: usize,
    metric: Metric,
) -> Result<Vec<(Json, f32)>, String> {
    let rows = conn.select(query)?;
    let mut scored = Vec::with_capacity(rows.len());
    for row in rows {
        if let Some(vec) = crate::backend::row::opt_vector(&row, column)? {
            let d = metric.distance(target, &vec);
            scored.push((row, d));
        }
    }
    Ok(top_k(scored, k))
}

/// Typed variant of [`nearest`]: hydrate each hit into `T`.
pub fn nearest_typed<T: FromRow, B: Backend>(
    conn: &B,
    query: &QueryBuilder,
    column: &str,
    target: &[f32],
    k: usize,
    metric: Metric,
) -> Result<Vec<(T, f32)>, String> {
    nearest(conn, query, column, target, k, metric)?
        .into_iter()
        .map(|(row, d)| T::from_row(&row).map(|t| (t, d)))
        .collect()
}

/// Push k-nearest search down to the database: `ORDER BY col <op> ? LIMIT k`,
/// returning `(row, distance)`. The backend must understand pgvector distance
/// operators — i.e. **Postgres with the pgvector extension**, where an ANN
/// index makes this scale far past brute force. For SQLite (no such operators),
/// use [`nearest`] instead.
pub fn nearest_pushdown_typed<T: FromRow, B: Backend>(
    conn: &B,
    table: &str,
    column: &str,
    target: &[f32],
    k: usize,
    metric: Metric,
) -> Result<Vec<(T, f32)>, String> {
    // `table`/`column` are interpolated directly (identifiers can't be bound),
    // so they must clear the same guard the builder uses or they are an
    // injection vector. `k` is a `usize` and `op` is a fixed allowlist.
    if !valid_identifier(table) {
        return Err(format!("invalid identifier: {table:?}"));
    }
    if !valid_identifier(column) {
        return Err(format!("invalid identifier: {column:?}"));
    }
    let sql = format!(
        "SELECT *, ({column} {op} ?) AS _distance FROM {table} ORDER BY _distance LIMIT {k}",
        op = metric.pg_operator(),
    );
    let rows = conn.query(&sql, &[Value::Vector(target.to_vec())])?;
    rows.iter()
        .map(|row| {
            let d = row.get("_distance").and_then(Json::as_f64).unwrap_or(0.0) as f32;
            T::from_row(row).map(|t| (t, d))
        })
        .collect()
}

/// The SQL to create a pgvector **HNSW** index for `metric` over `table.column`
/// — run it in a migration (Postgres + pgvector only) to accelerate
/// [`nearest_pushdown_typed`].
pub fn create_hnsw_index_sql(table: &str, column: &str, metric: Metric) -> Result<String, String> {
    // Both names are interpolated into DDL, so they must clear the identifier
    // guard before we build the statement (see `nearest_pushdown_typed`).
    if !valid_identifier(table) {
        return Err(format!("invalid identifier: {table:?}"));
    }
    if !valid_identifier(column) {
        return Err(format!("invalid identifier: {column:?}"));
    }
    Ok(format!(
        "CREATE INDEX IF NOT EXISTS {table}_{column}_hnsw ON {table} \
         USING hnsw ({column} {})",
        metric.pg_ops_class()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metrics_rank_lower_is_closer() {
        let a = [1.0, 0.0, 0.0];
        let same = [1.0, 0.0, 0.0];
        let orth = [0.0, 1.0, 0.0];
        // Identical vectors: zero cosine/L2 distance.
        assert!(Metric::Cosine.distance(&a, &same).abs() < 1e-6);
        assert!(Metric::L2.distance(&a, &same).abs() < 1e-6);
        // Orthogonal is farther than identical under cosine.
        assert!(Metric::Cosine.distance(&a, &orth) > Metric::Cosine.distance(&a, &same));
        // Inner product: higher dot ⇒ smaller (more negative) distance.
        assert!(
            Metric::InnerProduct.distance(&a, &same) < Metric::InnerProduct.distance(&a, &orth)
        );
    }

    #[test]
    fn dimension_mismatch_is_infinitely_far() {
        assert_eq!(Metric::L2.distance(&[1.0, 2.0], &[1.0]), f32::INFINITY);
    }

    #[test]
    fn vector_algebra_and_text_roundtrip() {
        let v = Vector::new(vec![3.0, 4.0]);
        assert_eq!(v.dim(), 2);
        assert_eq!(v.norm(), 5.0);
        assert!((v.normalized().norm() - 1.0).abs() < 1e-6);
        assert_eq!(v.dot(&[1.0, 0.0]), 3.0);
        // Text form is pgvector-compatible and round-trips.
        assert_eq!(v.to_text(), "[3,4]");
        assert_eq!(Vector::from_text("[3,4]").unwrap(), v);
        assert_eq!(Vector::from_text("[]").unwrap(), Vector::new(vec![]));
    }

    #[test]
    fn top_k_keeps_closest_in_order() {
        let scored = vec![("far", 0.9_f32), ("near", 0.1), ("mid", 0.5)];
        let top = top_k(scored, 2);
        assert_eq!(
            top.iter().map(|(l, _)| *l).collect::<Vec<_>>(),
            vec!["near", "mid"]
        );
    }

    #[test]
    fn index_sql_uses_ops_class() {
        let sql = create_hnsw_index_sql("docs", "embedding", Metric::Cosine).unwrap();
        assert!(sql.contains("CREATE INDEX IF NOT EXISTS docs_embedding_hnsw ON docs"));
        assert!(sql.contains("USING hnsw (embedding vector_cosine_ops)"));
    }

    #[test]
    fn index_sql_rejects_injection_identifiers() {
        // Payloads that would escape the identifier slots if interpolated
        // unchecked — each must be refused, in either the table or column slot.
        for bad in [
            "1)) UNION SELECT password, 0 FROM users --",
            "docs; DROP TABLE users",
            "col\"; --",
        ] {
            let via_table = create_hnsw_index_sql(bad, "embedding", Metric::Cosine);
            let via_column = create_hnsw_index_sql("docs", bad, Metric::Cosine);
            assert!(via_table.is_err(), "table {bad:?} should be rejected");
            assert!(via_column.is_err(), "column {bad:?} should be rejected");
            assert!(via_table.unwrap_err().contains("invalid identifier"));
            assert!(via_column.unwrap_err().contains("invalid identifier"));
        }
    }

    #[test]
    fn index_sql_accepts_legitimate_identifiers() {
        // The guard is `valid_identifier`, which `nearest_pushdown_typed` shares;
        // ordinary names pass and produce the expected DDL.
        assert!(create_hnsw_index_sql("docs", "embedding", Metric::Cosine).is_ok());
        assert!(create_hnsw_index_sql("user_docs", "vec_col", Metric::L2).is_ok());
    }
}
