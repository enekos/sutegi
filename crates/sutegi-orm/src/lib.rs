//! A small, driver-agnostic data layer: a typed schema, a fluent query builder
//! that emits parameterized SQL, one [`Backend`] trait, and — behind it — two
//! opinionated stores plus a key/value layer.
//!
//! ## The opinionated split
//!
//! - **[`db::Db`] — SQLite, the single-node store** (`sqlite` feature). Embedded,
//!   zero-ops, one writer. For a one-instance app, or as the backing store for
//!   the [`kv`] layer (config/cache/sessions/flags).
//! - **[`pg::Pg`] — Postgres, the multi-pod server store** (`postgres` feature).
//!   A shared, durable source of truth many replicas talk to. Pure `std`, no
//!   async runtime, no C library.
//! - **[`kv::Kv`] — a JSON key/value store over *either* backend.**
//!
//! Both stores implement the same [`Backend`] trait, and [`Model`] is written
//! once against it — so app code moves from SQLite to Postgres by changing the
//! type it holds, not the call sites. The query builder emits canonical `?`
//! placeholders; each backend translates to its own dialect.
//!
//! The pure-schema core (this module's [`QueryBuilder`], [`TableSchema`], …) is
//! `std`-only; a *runnable* backend is opt-in via the `sqlite` / `postgres`
//! features.

mod backend;
mod builder;
mod value;

pub use backend::{row, Backend, FromInput, FromRow, Model, Transactional};
pub use builder::{DeleteBuilder, Page, QueryBuilder, UpdateBuilder};
pub use value::{
    create_table_sql, default_sql, parse_default_literal, schema_json, ColType, Column, Dialect,
    FkAction, ForeignKey, Index, TableSchema, Value,
};

/// Versioned, up/down migrations with a `_sutegi_migrations` history table,
/// over any [`Backend`].
pub mod migrate;

/// First-class embeddings: the [`embedding::Vector`] type, similarity
/// [`embedding::Metric`]s, and nearest-neighbour search over any [`Backend`].
pub mod embedding;

pub use embedding::{Metric, Vector};

/// The bundled SQLite execution layer (single-node). Requires `sqlite`.
#[cfg(feature = "sqlite")]
pub mod db;

/// The pure-std PostgreSQL execution layer (multi-pod). Requires `postgres`.
#[cfg(feature = "postgres")]
pub mod pg;

/// A namespaced JSON key/value store over any [`Backend`]. Available whenever a
/// runnable backend (`sqlite` or `postgres`) is enabled.
#[cfg(any(feature = "sqlite", feature = "postgres"))]
pub mod kv;
