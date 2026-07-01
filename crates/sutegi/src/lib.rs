//! # sutegi — the forge
//!
//! A zero-dependency, agent-native web framework for Rust, built on `std`: the
//! HTTP/1.1 server, JSON codec, and router are always present. Everything else
//! is an opt-in **compile-time feature**, so you ship only what you use:
//!
//! | Feature | Pulls in | Gives you |
//! |---------|----------|-----------|
//! | `orm`      | sutegi-orm      | schema + query builder + migrations + JSON/vector columns + embeddings + KV |
//! | `sqlite`   | + bundled rusqlite | SQLite: the single-node execution layer |
//! | `postgres` | sutegi-pg (pure std) | Postgres: the multi-pod execution layer |
//! | `derive`   | sutegi-macros (build-time only) | `#[derive(Model)]` |
//! | `validate` | sutegi-validate | request / tool validation |
//! | `ai`       | sutegi-ai       | `Tool`/`StreamTool` + `/__tools` |
//! | `queue`    | sutegi-queue (+ sutegi-pg) | durable, cross-pod job queue (Postgres) |
//! | `graceful` | libc            | SIGTERM/SIGINT draining for pods |
//!
//! `default = ["derive", "orm", "validate", "ai"]`. For a minimal
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
#[cfg(feature = "auth")]
pub use sutegi_session as session;
#[cfg(feature = "validate")]
pub use sutegi_validate as validate;

/// The `#[derive(Model)]` and `#[derive(Validate)]` macros (require the
/// `derive` feature; `Validate` additionally needs `validate`).
#[cfg(feature = "derive")]
pub use sutegi_macros::{Model, Validate};

/// Route-model binding: hydrate a typed model straight from a path parameter,
/// or return a ready-made error response. Works over any runnable backend
/// (`sqlite` or `postgres`).
#[cfg(any(feature = "sqlite", feature = "postgres"))]
pub mod binding {
    use sutegi_orm::row::FromRow;
    use sutegi_orm::{Backend, Model, Value};
    use sutegi_web::{json, Params, Response};

    /// Look up `params[key]` as the primary key of model `T`. Returns the
    /// hydrated model, or `Err(Response)` (404 if missing, 500 on db error)
    /// ready to return from the handler.
    pub fn model<T: Model + FromRow, B: Backend>(
        db: &B,
        params: &Params,
        key: &str,
    ) -> Result<T, Response> {
        let raw = params
            .get(key)
            .ok_or_else(|| json(404, &not_found_json()))?;
        let id = match raw.parse::<i64>() {
            Ok(n) => Value::Int(n),
            Err(_) => Value::Text(raw.clone()),
        };
        match T::find_typed(db, id) {
            Ok(Some(m)) => Ok(m),
            Ok(None) => Err(json(404, &not_found_json())),
            Err(e) => Err(json(
                500,
                &sutegi_json::Json::obj(vec![("error", sutegi_json::Json::str(e))]),
            )),
        }
    }

    fn not_found_json() -> sutegi_json::Json {
        sutegi_json::Json::obj(vec![("error", sutegi_json::Json::str("not found"))])
    }
}

/// A clean, std-only configuration layer ([`config::Config`]): typed env
/// access, `.env` loading, required-var validation, and prefix scoping.
pub mod config;

/// Versioned migrations plus a CLI runner for the app binary.
///
/// Define a [`Migrator`](sutegi_orm::migrate::Migrator) in your app, then let
/// [`migrate::dispatch`] intercept the `migrate` / `migrate:rollback` /
/// `migrate:status` subcommands before you start serving:
///
/// ```ignore
/// let db = Db::open("app.db")?;
/// if sutegi::migrate::dispatch(&migrations(), &db) {
///     return Ok(()); // a migrate subcommand ran; don't start the server
/// }
/// app.run("0.0.0.0:8080")
/// ```
///
/// Then `myapp migrate` applies pending migrations, `myapp migrate:rollback [n]`
/// rolls back the last `n` batches (default 1), and `myapp migrate:status`
/// prints the ledger. Same binary serves and migrates — the Rails/Laravel shape.
#[cfg(feature = "orm")]
pub mod migrate {
    pub use sutegi_orm::migrate::{
        status_json, Migration, MigrationOps, MigrationStatus, Migrator,
    };
    use sutegi_orm::Backend;

    /// Inspect `std::env::args()` for a `migrate*` subcommand and run it against
    /// `conn`. Returns `true` if a subcommand was handled (the caller should
    /// stop and exit), `false` if there was no migrate subcommand (carry on and
    /// serve). On a migration error it prints the error and exits the process
    /// with status 1, so CI and deploy scripts see a real failure.
    pub fn dispatch<B: Backend>(migrator: &Migrator, conn: &B) -> bool {
        let args: Vec<String> = std::env::args().collect();
        match args.get(1).map(String::as_str) {
            Some("migrate") => finish("migrate", migrator.run(conn).map(report_applied)),
            Some("migrate:rollback") => {
                let batches = args
                    .get(2)
                    .and_then(|s| s.parse::<usize>().ok())
                    .unwrap_or(1);
                finish(
                    "migrate:rollback",
                    migrator.rollback(conn, batches).map(report_rolled),
                )
            }
            Some("migrate:status") => {
                finish("migrate:status", migrator.status(conn).map(report_status))
            }
            _ => return false,
        }
        true
    }

    fn finish(cmd: &str, result: Result<(), String>) {
        if let Err(e) = result {
            eprintln!("{cmd}: {e}");
            std::process::exit(1);
        }
    }

    fn report_applied(versions: Vec<String>) {
        if versions.is_empty() {
            println!("migrate: already up to date");
        } else {
            println!("migrate: applied {} migration(s):", versions.len());
            for v in versions {
                println!("  ↑ {v}");
            }
        }
    }

    fn report_rolled(versions: Vec<String>) {
        if versions.is_empty() {
            println!("migrate:rollback: nothing to roll back");
        } else {
            println!(
                "migrate:rollback: reverted {} migration(s):",
                versions.len()
            );
            for v in versions {
                println!("  ↓ {v}");
            }
        }
    }

    fn report_status(statuses: Vec<MigrationStatus>) {
        if statuses.is_empty() {
            println!("migrate:status: no migrations defined");
            return;
        }
        for s in statuses {
            let mark = if s.orphan {
                "?"
            } else if s.applied {
                "✓"
            } else {
                " "
            };
            let batch = s.batch.map(|b| format!("batch {b}")).unwrap_or_default();
            let note = if s.orphan {
                "  (orphan: not in code)"
            } else {
                ""
            };
            println!("  [{mark}] {}  {}  {batch}{note}", s.version, s.name);
        }
    }
}

/// A fluent, owned collection type ([`collection::Collection`]) plus the
/// [`collect`] constructor — chainable `map`/`filter`/`group_by`/`chunk`/… over
/// any iterable, with zero third-party deps.
pub mod collection;
pub use collection::{collect, Collection};

/// Read a single env var with a fallback — a shortcut over [`config::Config`]
/// for the common one-off case (`PORT`, `WORKERS`, …).
pub fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

/// The common imports for building an app. Items appear only when their feature
/// is enabled, so the prelude tracks your build.
pub mod prelude {
    pub use crate::collection::{collect, Collection};
    pub use crate::config::Config;
    pub use sutegi_json::Json;
    pub use sutegi_web::{
        basic, bearer, cors, cors_preflight, form_body, html, json, json_body, logger, mw,
        no_content, not_found, query_params, rate_limit, redirect, schema, secure_headers, sse,
        status, stream, text, App, Ctx, Error, Group, IntoResponse, Limits, Method, Mw, Params,
        Request, Response, SseSink, StreamSink, ToolCtx,
    };

    #[cfg(feature = "orm")]
    pub use sutegi_orm::migrate::{Migration, Migrator};
    #[cfg(feature = "orm")]
    pub use sutegi_orm::row::FromRow;
    #[cfg(feature = "orm")]
    pub use sutegi_orm::{
        Backend, ColType, Column, DeleteBuilder, FromInput, Metric, Model, Page, QueryBuilder,
        TableSchema, UpdateBuilder, Value, Vector,
    };
    #[cfg(feature = "queue")]
    pub use sutegi_queue::{Queue, Workers};
    #[cfg(feature = "validate")]
    pub use sutegi_validate::{validate_schema, Rule, Ruleset, Validate, ValidationErrors};

    #[cfg(feature = "derive")]
    pub use sutegi_macros::{Model, Validate};

    #[cfg(feature = "sqlite")]
    pub use sutegi_orm::db::Db;

    #[cfg(feature = "postgres")]
    pub use sutegi_orm::pg::Pg;

    /// The JSON key/value store — available over either backend.
    #[cfg(any(feature = "sqlite", feature = "postgres"))]
    pub use sutegi_orm::kv::Kv;

    #[cfg(feature = "hex")]
    pub use sutegi_hex::{
        respond, respond_created, AppError, AppResult, Command, Event, EventBus, IntoJson, Query,
        UseCase,
    };

    #[cfg(feature = "auth")]
    pub use sutegi_session::{Session, Sessions};
}
