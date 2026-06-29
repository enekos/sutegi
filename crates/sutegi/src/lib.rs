//! # sutegi — the forge
//!
//! A zero-dependency, agent-native web framework for Rust. Everything below is
//! built on `std`: the HTTP/1.1 server, the JSON codec, routing, the ORM query
//! builder, and the LLM tool layer. No tokio, no hyper, no serde. The result
//! is a tiny release binary and a surface small enough to audit in an afternoon.
//!
//! ## Hello, forge
//!
//! ```no_run
//! use sutegi::prelude::*;
//!
//! fn main() -> std::io::Result<()> {
//!     App::new("hello")
//!         .get("/", "Health check", |_req, _params| text(200, "sutegi up"))
//!         .run("127.0.0.1:8080")
//! }
//! ```
//!
//! Three things make it agent-native out of the box:
//! * `GET /__introspect` — the full route/model/tool surface as JSON
//! * `GET /__tools` — an LLM tool-calling manifest
//! * `POST /__tools/:name` — invoke a tool with a JSON argument object

pub use sutegi_ai as ai;
pub use sutegi_http as http;
pub use sutegi_json as json;
pub use sutegi_orm as orm;
pub use sutegi_validate as validate;
pub use sutegi_web as web;

/// The common imports for building an app.
pub mod prelude {
    pub use sutegi_ai::{schema, Tool, ToolRegistry};
    pub use sutegi_json::Json;
    pub use sutegi_orm::{ColType, Column, Model, QueryBuilder, TableSchema, Value};
    pub use sutegi_validate::{validate_schema, Rule, Ruleset, ValidationErrors};
    pub use sutegi_web::{
        json, json_body, not_found, query_params, text, App, Method, Params, Request, Response,
    };

    #[cfg(feature = "sqlite")]
    pub use sutegi_orm::db::Db;
}
