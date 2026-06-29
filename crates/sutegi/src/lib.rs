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
pub use sutegi_queue as queue;
pub use sutegi_validate as validate;
pub use sutegi_web as web;

/// The `#[derive(Model)]` macro (with the default `derive` feature).
#[cfg(feature = "derive")]
pub use sutegi_macros::Model;

/// Route-model binding: hydrate a typed model straight from a path parameter,
/// or return a ready-made error response. The `sqlite` analogue of Laravel's
/// implicit route-model binding.
#[cfg(feature = "sqlite")]
pub mod binding {
    use sutegi_orm::db::Db;
    use sutegi_orm::row::FromRow;
    use sutegi_orm::{Model, Value};
    use sutegi_web::{json, Params, Response};

    /// Look up `params[key]` as the primary key of model `T`. Returns the
    /// hydrated model, or `Err(Response)` (404 if missing, 500 on db error)
    /// ready to return from the handler.
    pub fn model<T: Model + FromRow>(db: &Db, params: &Params, key: &str) -> Result<T, Response> {
        let raw = params
            .get(key)
            .ok_or_else(|| json(404, &not_found_json()))?;
        // Prefer an integer key, fall back to text.
        let id = match raw.parse::<i64>() {
            Ok(n) => Value::Int(n),
            Err(_) => Value::Text(raw.clone()),
        };
        match T::find_typed(db, id) {
            Ok(Some(m)) => Ok(m),
            Ok(None) => Err(json(404, &not_found_json())),
            Err(e) => Err(json(500, &sutegi_json::Json::obj(vec![("error", sutegi_json::Json::str(e))]))),
        }
    }

    fn not_found_json() -> sutegi_json::Json {
        sutegi_json::Json::obj(vec![("error", sutegi_json::Json::str("not found"))])
    }
}

/// The common imports for building an app.
pub mod prelude {
    pub use sutegi_ai::{schema, Tool, ToolRegistry};
    pub use sutegi_json::Json;
    pub use sutegi_orm::row::FromRow;
    pub use sutegi_orm::{ColType, Column, Model, QueryBuilder, TableSchema, Value};
    pub use sutegi_queue::{Job, Queue};
    pub use sutegi_validate::{validate_schema, Rule, Ruleset, ValidationErrors};
    pub use sutegi_web::{
        json, json_body, mw, not_found, query_params, text, App, Group, Method, Mw, Params,
        Request, Response,
    };

    #[cfg(feature = "derive")]
    pub use sutegi_macros::Model;

    #[cfg(feature = "sqlite")]
    pub use sutegi_orm::db::Db;
}
