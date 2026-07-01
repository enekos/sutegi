//! Inbound adapter: an AI tool. The point of hexagonal architecture in one
//! file — this drives the *same* `CreateTodo` use case the HTTP adapter uses.
//! Two transports, one application core, zero duplicated business logic.

use std::sync::Arc;

use sutegi::hex::{IntoJson, UseCase};
use sutegi::prelude::*;

use crate::application::CreateTodo;

/// Mount the AI inbound adapter: a `create_todo` tool over the same use case.
pub fn register(app: App, create: Arc<CreateTodo>) -> App {
    app.tool(
        "create_todo",
        "Create a todo item (drives the same use case as the HTTP API).",
        schema::object(
            vec![("title", schema::string("the todo's title"))],
            &["title"],
        ),
        move |_c, args| {
            let title = args
                .get("title")
                .and_then(Json::as_str)
                .unwrap_or("")
                .to_string();
            // Run the use case; map the domain error onto the tool's Error.
            create
                .execute(title)
                .map(IntoJson::into_json)
                .map_err(|e| Error::new(e.status(), e.message()))
        },
    )
}
