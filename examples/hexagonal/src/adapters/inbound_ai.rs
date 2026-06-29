//! Inbound adapter: an AI tool. The point of hexagonal architecture in one
//! file — this drives the *same* `CreateTodo` use case the HTTP adapter uses.
//! Two transports, one application core, zero duplicated business logic.

use std::sync::Arc;

use sutegi::hex::{IntoJson, UseCase};
use sutegi::prelude::*;

use crate::application::CreateTodo;

pub struct CreateTodoTool {
    pub use_case: Arc<CreateTodo>,
}

impl Tool for CreateTodoTool {
    fn name(&self) -> &str {
        "create_todo"
    }
    fn description(&self) -> &str {
        "Create a todo item (drives the same use case as the HTTP API)."
    }
    fn parameters(&self) -> Json {
        schema::object(vec![("title", schema::string("the todo's title"))], &["title"])
    }
    fn call(&self, args: Json) -> Result<Json, String> {
        let title = args.get("title").and_then(|j| j.as_str()).unwrap_or("").to_string();
        // Run the use case; map the domain error onto the tool's String error.
        self.use_case
            .execute(title)
            .map(IntoJson::into_json)
            .map_err(|e| e.to_string())
    }
}
