//! # sutegi — the forge
//!
//! A zero-dependency, agent-native web framework for Rust, built on `std`: the
//! HTTP/1.1 server, JSON codec, and router are always present. Everything else
//! is an opt-in **compile-time feature**, so you ship only what you use:
//!
//! | Feature | Pulls in | Gives you |
//! |---------|----------|-----------|
//! | `orm`      | sutegi-orm      | schema + query builder + migrations |
//! | `sqlite`   | + bundled rusqlite | a runnable SQLite execution layer |
//! | `derive`   | sutegi-macros (build-time only) | `#[derive(Model)]` |
//! | `validate` | sutegi-validate | request / tool validation |
//! | `ai`       | sutegi-ai       | `Tool`/`StreamTool` + `/__tools` |
//! | `queue`    | sutegi-queue    | background jobs |
//! | `graceful` | libc            | SIGTERM/SIGINT draining for pods |
//!
//! `default = ["derive", "orm", "validate", "ai", "queue"]`. For a minimal
//! HTTP service: `sutegi = { version = "*", default-features = false }`.
//!
//! ## Built-in operational endpoints (always on)
//! `GET /__health` (liveness), `GET /__ready` (readiness probe),
//! `GET /__metrics` (Prometheus), `GET /__introspect` (full app surface).

// --- core, always available ---
pub use sutegi_http as http;
pub use sutegi_json as json;
pub use sutegi_web as web;

// --- optional pillars ---
#[cfg(feature = "ai")]
pub use sutegi_ai as ai;
#[cfg(feature = "hex")]
pub use sutegi_hex as hex;
#[cfg(feature = "orm")]
pub use sutegi_orm as orm;
#[cfg(feature = "queue")]
pub use sutegi_queue as queue;
#[cfg(feature = "validate")]
pub use sutegi_validate as validate;

/// The `#[derive(Model)]` macro (requires the `derive` feature).
#[cfg(feature = "derive")]
pub use sutegi_macros::Model;

/// Route-model binding: hydrate a typed model straight from a path parameter,
/// or return a ready-made error response. Requires `sqlite` (needs a live DB).
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
        let raw = params.get(key).ok_or_else(|| json(404, &not_found_json()))?;
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

/// Read a config value from the environment with a fallback — small 12-factor
/// helper so apps configure cleanly across pods (`PORT`, `WORKERS`, etc.).
pub fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

/// The common imports for building an app. Items appear only when their feature
/// is enabled, so the prelude tracks your build.
pub mod prelude {
    pub use sutegi_json::Json;
    pub use sutegi_web::{
        json, json_body, mw, not_found, query_params, sse, stream, text, App, Group, Method, Mw,
        Params, Request, Response, SseSink, StreamSink,
    };

    #[cfg(feature = "ai")]
    pub use sutegi_ai::{schema, StreamTool, Tool, ToolRegistry};
    #[cfg(feature = "orm")]
    pub use sutegi_orm::row::FromRow;
    #[cfg(feature = "orm")]
    pub use sutegi_orm::{ColType, Column, Model, QueryBuilder, TableSchema, Value};
    #[cfg(feature = "queue")]
    pub use sutegi_queue::{Job, Queue};
    #[cfg(feature = "validate")]
    pub use sutegi_validate::{validate_schema, Rule, Ruleset, ValidationErrors};

    #[cfg(feature = "derive")]
    pub use sutegi_macros::Model;

    #[cfg(feature = "sqlite")]
    pub use sutegi_orm::db::Db;

    #[cfg(feature = "hex")]
    pub use sutegi_hex::{respond, respond_created, AppError, AppResult, IntoJson, UseCase};
}
