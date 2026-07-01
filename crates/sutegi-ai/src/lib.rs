//! Agent-facing surface for sutegi.
//!
//! Tools are now a **core** concept: register them directly on the app with
//! [`App::tool`](sutegi_web::App::tool) / [`App::stream_tool`], and describe
//! their inputs with the [`schema`] helpers. sutegi validates each call's
//! arguments against the declared schema, mounts `POST /__tools/:name`
//! (and `/__tools/:name/stream` for SSE), and lists everything in the
//! `/__tools` manifest and `/__introspect`.
//!
//! ```ignore
//! use sutegi::prelude::*;
//!
//! App::new("assistant")
//!     .state(db)
//!     .tool("create_todo", "Create a todo",
//!         schema::object(vec![("title", schema::string("the title"))], &["title"]),
//!         |c, args| {
//!             let todo: Todo = Todo::from_row(&args)?;   // args are validated
//!             let id = todo.save(c.db::<Db>())?;
//!             Ok(Todo { id, ..todo }.to_json())
//!         })
//!     .serve()
//! ```
//!
//! This crate re-exports that surface so the `sutegi::ai::…` path keeps working
//! and is the home for future LLM-client helpers.

pub use sutegi_web::schema;
pub use sutegi_web::{Ctx, ToolCtx};
