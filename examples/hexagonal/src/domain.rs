//! Domain layer — pure business types and rules. No sutegi, no JSON, no SQL.
//! Nothing in here may `use` an adapter or the framework. This is the center of
//! the hexagon; everything else depends on it, it depends on nothing.

/// A todo entity. Invariants live in the constructor and methods, not in the
/// HTTP handler or the database.
#[derive(Clone, Debug, PartialEq)]
pub struct Todo {
    pub id: i64,
    pub title: String,
    pub done: bool,
}

impl Todo {
    /// Create a new (unsaved) todo, enforcing the domain rule that a title is
    /// non-empty and bounded. `id == 0` means "not yet persisted".
    pub fn new(title: impl Into<String>) -> Result<Todo, String> {
        let title = title.into();
        let trimmed = title.trim();
        if trimmed.is_empty() {
            return Err("title must not be empty".into());
        }
        if trimmed.chars().count() > 200 {
            return Err("title must be at most 200 characters".into());
        }
        Ok(Todo {
            id: 0,
            title: trimmed.to_string(),
            done: false,
        })
    }

    /// Mark complete. A domain operation, not a database UPDATE — adapters
    /// persist the result, but the rule (idempotent completion) lives here.
    pub fn complete(&mut self) {
        self.done = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_empty_title() {
        assert!(Todo::new("   ").is_err());
    }

    #[test]
    fn completes() {
        let mut t = Todo::new("write tests").unwrap();
        assert!(!t.done);
        t.complete();
        assert!(t.done);
    }
}
