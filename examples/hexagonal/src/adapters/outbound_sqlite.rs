//! Outbound adapter: a SQLite-backed `TodoRepository`. Note the separate
//! persistence model `TodoRow` (with `#[derive(Model)]`) kept distinct from the
//! domain `Todo` — the database schema is an adapter detail, mapped at the
//! boundary, so it can evolve independently of the domain entity.
//!
//! `Db` is a pooled, `Send + Sync + Clone` handle, so the adapter just holds one
//! — no `Arc<Mutex<…>>`.

use sutegi::hex::{AppError, AppResult};
use sutegi::prelude::*;

use crate::domain::Todo;
use crate::ports::TodoRepository;

/// The persistence shape (an adapter concern, not the domain entity).
#[derive(Model)]
#[model(table = "todos")]
struct TodoRow {
    #[model(primary)]
    id: i64,
    title: String,
    done: bool,
}

impl TodoRow {
    fn into_domain(self) -> Todo {
        Todo {
            id: self.id,
            title: self.title,
            done: self.done,
        }
    }
}

pub struct SqliteTodoRepo {
    db: Db,
}

impl SqliteTodoRepo {
    /// Wire the adapter and ensure the schema exists.
    pub fn new(db: Db) -> AppResult<SqliteTodoRepo> {
        TodoRow::migrate(&db).map_err(AppError::internal)?;
        Ok(SqliteTodoRepo { db })
    }
}

impl TodoRepository for SqliteTodoRepo {
    fn list(&self) -> AppResult<Vec<Todo>> {
        let rows = TodoRow::all_typed(&self.db).map_err(AppError::internal)?;
        Ok(rows.into_iter().map(TodoRow::into_domain).collect())
    }

    fn find(&self, id: i64) -> AppResult<Option<Todo>> {
        let row = TodoRow::find_typed(&self.db, Value::Int(id)).map_err(AppError::internal)?;
        Ok(row.map(TodoRow::into_domain))
    }

    fn insert(&self, todo: &Todo) -> AppResult<i64> {
        let row = TodoRow {
            id: 0,
            title: todo.title.clone(),
            done: todo.done,
        };
        row.save(&self.db).map_err(AppError::internal)
    }

    fn update(&self, todo: &Todo) -> AppResult<()> {
        self.db
            .execute(
                "UPDATE todos SET title = ?, done = ? WHERE id = ?",
                &[
                    Value::Text(todo.title.clone()),
                    Value::Bool(todo.done),
                    Value::Int(todo.id),
                ],
            )
            .map(|_| ())
            .map_err(AppError::internal)
    }
}
