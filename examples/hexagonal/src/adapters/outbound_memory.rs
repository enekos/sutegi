//! Outbound adapter: an in-memory `TodoRepository`. Zero IO — ideal for unit
//! tests and early development. Interchangeable with the SQLite adapter because
//! both implement the same port.

use std::sync::Mutex;

use sutegi::hex::{AppError, AppResult};

use crate::domain::Todo;
use crate::ports::TodoRepository;

#[derive(Default)]
pub struct InMemoryTodoRepo {
    inner: Mutex<Vec<Todo>>,
}

impl InMemoryTodoRepo {
    pub fn new() -> InMemoryTodoRepo {
        InMemoryTodoRepo::default()
    }
}

impl TodoRepository for InMemoryTodoRepo {
    fn list(&self) -> AppResult<Vec<Todo>> {
        Ok(self.inner.lock().unwrap().clone())
    }

    fn find(&self, id: i64) -> AppResult<Option<Todo>> {
        Ok(self
            .inner
            .lock()
            .unwrap()
            .iter()
            .find(|t| t.id == id)
            .cloned())
    }

    fn insert(&self, todo: &Todo) -> AppResult<i64> {
        let mut store = self.inner.lock().unwrap();
        let id = store.len() as i64 + 1;
        store.push(Todo { id, ..todo.clone() });
        Ok(id)
    }

    fn update(&self, todo: &Todo) -> AppResult<()> {
        let mut store = self.inner.lock().unwrap();
        match store.iter_mut().find(|t| t.id == todo.id) {
            Some(slot) => {
                *slot = todo.clone();
                Ok(())
            }
            None => Err(AppError::not_found(format!("todo {}", todo.id))),
        }
    }
}
