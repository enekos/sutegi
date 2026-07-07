//! Application layer — use cases (inbound ports). Each one orchestrates the
//! domain and the outbound ports to fulfill a single intent. It depends on the
//! `TodoRepository` *trait*, never a concrete adapter, and it never touches
//! HTTP/JSON — so it's trivially unit-testable with an in-memory repo.

use std::sync::Arc;

use sutegi::hexagon::{AppError, AppResult, UseCase};

use crate::domain::Todo;
use crate::ports::TodoRepository;

/// Shared handle to whatever persistence adapter was wired at startup.
type Repo = Arc<dyn TodoRepository>;

/// Create a todo from a title.
pub struct CreateTodo {
    pub repo: Repo,
}

impl UseCase for CreateTodo {
    type Input = String; // the title
    type Output = Todo;

    fn execute(&self, title: String) -> AppResult<Todo> {
        // Domain rule enforcement; map the domain error to a transport-agnostic one.
        let todo = Todo::new(title).map_err(AppError::invalid)?;
        let id = self.repo.insert(&todo)?;
        Ok(Todo { id, ..todo })
    }
}

/// List all todos.
pub struct ListTodos {
    pub repo: Repo,
}

impl UseCase for ListTodos {
    type Input = ();
    type Output = Vec<Todo>;

    fn execute(&self, _: ()) -> AppResult<Vec<Todo>> {
        self.repo.list()
    }
}

/// Mark a todo complete.
pub struct CompleteTodo {
    pub repo: Repo,
}

impl UseCase for CompleteTodo {
    type Input = i64;
    type Output = Todo;

    fn execute(&self, id: i64) -> AppResult<Todo> {
        let mut todo = self
            .repo
            .find(id)?
            .ok_or_else(|| AppError::not_found(format!("todo {id} not found")))?;
        todo.complete();
        self.repo.update(&todo)?;
        Ok(todo)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::outbound_memory::InMemoryTodoRepo;

    // Use cases are tested with the in-memory adapter — no HTTP, no SQLite.
    #[test]
    fn create_then_complete() {
        let repo: Repo = Arc::new(InMemoryTodoRepo::new());
        let created = CreateTodo { repo: repo.clone() }
            .execute("ship it".into())
            .unwrap();
        assert_eq!(created.id, 1);
        assert!(!created.done);

        let done = CompleteTodo { repo: repo.clone() }
            .execute(created.id)
            .unwrap();
        assert!(done.done);

        let all = ListTodos { repo }.execute(()).unwrap();
        assert_eq!(all.len(), 1);
        assert!(all[0].done);
    }

    #[test]
    fn create_rejects_empty() {
        let repo: Repo = Arc::new(InMemoryTodoRepo::new());
        let err = CreateTodo { repo }.execute("  ".into()).unwrap_err();
        assert_eq!(err.kind(), "invalid");
    }

    #[test]
    fn complete_missing_is_not_found() {
        let repo: Repo = Arc::new(InMemoryTodoRepo::new());
        let err = CompleteTodo { repo }.execute(99).unwrap_err();
        assert_eq!(err.kind(), "not_found");
    }
}
