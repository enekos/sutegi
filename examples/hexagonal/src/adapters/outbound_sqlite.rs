//! Outbound adapter: a SQLite-backed `TodoRepository`. Note the separate
//! persistence model `TodoRow` (with `#[derive(Model)]`) kept distinct from the
//! domain `Todo` — the database schema is an adapter detail, mapped at the
//! boundary, so it can evolve independently of the domain entity.

use std::sync::{Arc, Mutex};

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
        Todo { id: self.id, title: self.title, done: self.done }
    }
}

pub struct SqliteTodoRepo {
    db: Arc<Mutex<Db>>,
}

impl SqliteTodoRepo {
    /// Wire the adapter and ensure the schema exists.
    pub fn new(db: Arc<Mutex<Db>>) -> AppResult<SqliteTodoRepo> {
        {
            let guard = db.lock().unwrap();
            TodoRow::migrate(&guard).map_err(|e| AppError::internal(e))?;
        }
        Ok(SqliteTodoRepo { db })
    }
}

impl TodoRepository for SqliteTodoRepo {
    fn list(&self) -> AppResult<Vec<Todo>> {
        let db = self.db.lock().unwrap();
        let rows = TodoRow::all_typed(&db).map_err(|e| AppError::internal(e))?;
        Ok(rows.into_iter().map(TodoRow::into_domain).collect())
    }

    fn find(&self, id: i64) -> AppResult<Option<Todo>> {
        let db = self.db.lock().unwrap();
        let row = TodoRow::find_typed(&db, Value::Int(id)).map_err(|e| AppError::internal(e))?;
        Ok(row.map(TodoRow::into_domain))
    }

    fn insert(&self, todo: &Todo) -> AppResult<i64> {
        let db = self.db.lock().unwrap();
        TodoRow::create(
            &db,
            &[("title", Value::Text(todo.title.clone())), ("done", Value::Bool(todo.done))],
        )
        .map_err(|e| AppError::internal(e))
    }

    fn update(&self, todo: &Todo) -> AppResult<()> {
        let db = self.db.lock().unwrap();
        db.execute(
            "UPDATE todos SET title = ?, done = ? WHERE id = ?",
            &[
                Value::Text(todo.title.clone()),
                Value::Bool(todo.done),
                Value::Int(todo.id),
            ],
        )
        .map(|_| ())
        .map_err(|e| AppError::internal(e))
    }
}
