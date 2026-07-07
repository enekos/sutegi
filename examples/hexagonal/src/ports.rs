//! Ports — the interfaces (traits) the application depends on. Domain-specific
//! on purpose: the outbound port describes exactly what the use cases need from
//! persistence, in domain terms, with no hint of SQL or HTTP.
//!
//! Adapters *implement* these traits; the application *depends on* them. That
//! inversion is what lets us swap an in-memory repo for SQLite without touching
//! a single use case.

use sutegi::hexagon::AppResult;

use crate::domain::Todo;

/// Outbound port: persistence for todos.
pub trait TodoRepository: Send + Sync {
    fn list(&self) -> AppResult<Vec<Todo>>;
    fn find(&self, id: i64) -> AppResult<Option<Todo>>;
    /// Persist a new todo, returning its assigned id.
    fn insert(&self, todo: &Todo) -> AppResult<i64>;
    /// Persist changes to an existing todo.
    fn update(&self, todo: &Todo) -> AppResult<()>;
}
