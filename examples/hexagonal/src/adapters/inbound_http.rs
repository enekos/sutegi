//! Inbound adapter: HTTP. Translates requests into use-case calls and maps the
//! result back with `respond`/`respond_created`. The presenter (`IntoJson for
//! Todo`) lives here too, so the domain never learns about JSON.

use std::sync::Arc;

use sutegi::hex::{respond, respond_created, AppError, IntoJson, UseCase};
use sutegi::prelude::*;

use crate::application::{CompleteTodo, CreateTodo, ListTodos};
use crate::domain::Todo;

/// Presenter: domain entity → wire JSON. Adapter-layer concern.
impl IntoJson for Todo {
    fn into_json(self) -> Json {
        Json::obj(vec![
            ("id", Json::int(self.id)),
            ("title", Json::str(self.title)),
            ("done", Json::Bool(self.done)),
        ])
    }
}

/// Mount the HTTP inbound adapter under `/api`.
pub fn register(
    app: App,
    create: Arc<CreateTodo>,
    list: Arc<ListTodos>,
    complete: Arc<CompleteTodo>,
) -> App {
    app.group("/api", vec![], move |g| {
        g.get("/todos", "List all todos.", move |_req, _p| {
            respond(list.execute(()))
        })
        .post("/todos", "Create a todo.", move |req, _p| {
            // Inbound translation: HTTP body → use-case input.
            let title = json_body(req)
                .ok()
                .and_then(|b| b.get("title").and_then(|j| j.as_str()).map(str::to_string))
                .unwrap_or_default();
            respond_created(create.execute(title))
        })
        .post(
            "/todos/:id/complete",
            "Mark a todo complete.",
            move |_req, p| match p.get("id").and_then(|s| s.parse::<i64>().ok()) {
                Some(id) => respond(complete.execute(id)),
                None => AppError::invalid("id must be an integer").to_response(),
            },
        )
    })
}
