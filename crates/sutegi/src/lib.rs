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
//! | `queue`    | sutegi-queue (+ sutegi-pg) | durable, cross-pod job queue (Postgres) |
//! | `events`   | sutegi-events (+ orm) | event sourcing: append-only event store, aggregates, projections |
//! | `session`  | sutegi-session  | signed-cookie sessions (HMAC-SHA256) |
//! | `auth`     | sutegi-auth (+ session/orm) | the user system: passwords, Users, guards, API tokens |
//! | `template` | sutegi-template | Blade-style template engine (`{{ }}`, `@if`, `@foreach`, `@include`) |
//! | `mail`     | sutegi-mail (+ template) | Email builder, themed messages, Transport seam, smtp/sendmail/log drivers |
//! | `auth-mail` | + sutegi-auth/mail | email-verification + password-reset flows |
//! | `storage`  | sutegi-storage (pure std) | file storage: local fs + S3 presigned URLs |
//! | `storage-db` | + sutegi-orm  | blobs in SQLite/Postgres over the `Backend` seam |
//! | `ws`       | sutegi-ws       | WebSockets: `App::ws` on the sharded kqueue/epoll reactor |
//! | `pubsub`   | sutegi-pubsub   | in-process topic fan-out behind the `Broker` seam |
//! | `pubsub-postgres` | + sutegi-pg | cross-pod pubsub over PG `LISTEN`/`NOTIFY` (`PgPubSub`) |
//! | `channels` | sutegi-channels (+ ws/pubsub) | Phoenix-style channels: `App::channels` + `/__channels` manifest |
//! | `presence` | + sutegi-channels/presence | who's-online per topic, cross-pod, heartbeat-expired |
//! | `graceful` | libc            | SIGTERM/SIGINT draining for pods |
//!
//! The agent tool surface (`App::tool`/`stream_tool`, `schema` helpers,
//! `ToolCtx`, `/__tools`) is part of always-on core — no feature needed.
//!
//! `default = ["derive", "orm", "validate"]`. For a minimal
//! HTTP service: `sutegi = { version = "*", default-features = false }`.
//!
//! ## Built-in operational endpoints (always on)
//! `GET /__health` (liveness), `GET /__ready` (readiness probe),
//! `GET /__metrics` (Prometheus), `GET /__introspect` (full app surface).

// --- core, always available ---
pub use sutegi_crypto as crypto;
pub use sutegi_http as http;
pub use sutegi_json as json;
pub use sutegi_web as web;

// --- optional pillars ---
#[cfg(feature = "actors")]
pub use sutegi_actors as actors;
#[cfg(feature = "auth")]
pub use sutegi_auth as auth;
#[cfg(feature = "channels")]
pub use sutegi_channels as channels;
#[cfg(feature = "events")]
pub use sutegi_events as events;
#[cfg(feature = "hexagon")]
pub use sutegi_hexagon as hexagon;
#[cfg(feature = "mail")]
pub use sutegi_mail as mail;
#[cfg(feature = "orm")]
pub use sutegi_orm as orm;
#[cfg(feature = "pubsub")]
pub use sutegi_pubsub as pubsub;
#[cfg(feature = "queue")]
pub use sutegi_queue as queue;
#[cfg(feature = "repl")]
pub use sutegi_repl as repl;
#[cfg(feature = "session")]
pub use sutegi_session as session;
#[cfg(feature = "storage")]
pub use sutegi_storage as storage;
#[cfg(feature = "template")]
pub use sutegi_template as template;
#[cfg(feature = "validate")]
pub use sutegi_validate as validate;
#[cfg(feature = "ws")]
pub use sutegi_ws as ws;

/// The `#[derive(Model)]` and `#[derive(Validate)]` macros (require the
/// `derive` feature; `Validate` additionally needs `validate`).
#[cfg(feature = "derive")]
pub use sutegi_macros::{Model, Validate};

/// Collect a set of models' schemas into a `Vec<TableSchema>` — the desired
/// state passed to `migrate:gen` / `migrate:drift` / [`migrate::report_json`].
///
/// ```ignore
/// let models = sutegi::schemas![Todo, User, Post];
/// ```
#[cfg(feature = "orm")]
#[macro_export]
macro_rules! schemas {
    ($($model:ty),* $(,)?) => {
        ::std::vec![ $( <$model as $crate::orm::Model>::schema() ),* ]
    };
}

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
    use sutegi_json::Json;
    pub use sutegi_orm::migrate::{
        drift, generate, generate_via, status_json, write_migration_file, DriftReport, Migration,
        MigrationOps, MigrationStatus, Migrator,
    };
    pub use sutegi_orm::schema_diff::{self, Plan, SchemaOp};
    use sutegi_orm::{Backend, TableSchema};

    /// The conventional directory for generated migration files.
    pub const MIGRATIONS_DIR: &str = "migrations";

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

    /// The full CLI runner: everything [`dispatch`] handles plus the diff-driven
    /// verbs that need the model set and a migrations directory —
    /// `migrate:gen <name>` (write a migration from the model↔shadow diff),
    /// `migrate:plan` (show it without writing), `migrate:drift` (three-way
    /// report), and `migrate:fresh` (roll everything back and re-run; dev only).
    ///
    /// ```ignore
    /// let models = vec![Todo::schema(), User::schema()];
    /// if sutegi::migrate::dispatch_full(&migrations(), &db, &models, sutegi::migrate::MIGRATIONS_DIR) {
    ///     return Ok(());
    /// }
    /// ```
    pub fn dispatch_full<B: Backend>(
        migrator: &Migrator,
        conn: &B,
        models: &[TableSchema],
        dir: &str,
    ) -> bool {
        let args: Vec<String> = std::env::args().collect();
        match args.get(1).map(String::as_str) {
            Some("migrate:gen") => {
                let name = args
                    .get(2)
                    .cloned()
                    .unwrap_or_else(|| "changes".to_string());
                finish("migrate:gen", run_gen(migrator, conn, models, dir, &name));
            }
            Some("migrate:plan") => finish("migrate:plan", run_plan(migrator, conn, models)),
            Some("migrate:drift") => finish("migrate:drift", run_drift(migrator, conn, models)),
            Some("migrate:fresh") => finish(
                "migrate:fresh",
                run_fresh(migrator, conn).map(report_applied),
            ),
            // Fall through to the base verbs (migrate / :rollback / :status).
            _ => return dispatch(migrator, conn),
        }
        true
    }

    fn run_gen<B: Backend>(
        migrator: &Migrator,
        conn: &B,
        models: &[TableSchema],
        dir: &str,
        name: &str,
    ) -> Result<(), String> {
        let plan = generate(migrator, models, conn.dialect())?;
        if plan.is_empty() {
            println!("migrate:gen: no changes — models match the migration history");
            return Ok(());
        }
        let migration = Migration::ops(timestamp_version(), name, plan.ops.clone());
        let path = write_migration_file(dir, &migration)?;
        println!("migrate:gen: wrote {path}");
        for op in &plan.ops {
            println!("  + {}", op.summary());
        }
        print_warnings(&plan);
        Ok(())
    }

    fn run_plan<B: Backend>(
        migrator: &Migrator,
        conn: &B,
        models: &[TableSchema],
    ) -> Result<(), String> {
        let plan = generate(migrator, models, conn.dialect())?;
        if plan.is_empty() {
            println!("migrate:plan: no pending changes");
        } else {
            println!("migrate:plan: {} op(s) would be generated:", plan.ops.len());
            for op in &plan.ops {
                println!("  + {}", op.summary());
            }
            print_warnings(&plan);
        }
        Ok(())
    }

    fn run_drift<B: Backend>(
        migrator: &Migrator,
        conn: &B,
        models: &[TableSchema],
    ) -> Result<(), String> {
        let report = drift(conn, migrator, models)?;
        if report.is_clean() {
            println!("migrate:drift: clean — database, migrations, and models agree");
            return Ok(());
        }
        if !report.db_vs_migrations.is_empty() {
            println!("database has drifted from the migration history:");
            for op in &report.db_vs_migrations.ops {
                println!("  ! {}", op.summary());
            }
        }
        if !report.models_vs_migrations.is_empty() {
            println!("models have changes not yet captured in a migration (run migrate:gen):");
            for op in &report.models_vs_migrations.ops {
                println!("  + {}", op.summary());
            }
        }
        Ok(())
    }

    /// Roll back every batch, then re-run — a clean rebuild for local dev.
    fn run_fresh<B: Backend>(migrator: &Migrator, conn: &B) -> Result<Vec<String>, String> {
        // Roll back until nothing remains (each call undoes one batch).
        while !migrator.rollback(conn, 1)?.is_empty() {}
        migrator.run(conn)
    }

    fn print_warnings(plan: &Plan) {
        for w in &plan.warnings {
            println!("  ⚠ {w}");
        }
    }

    /// A machine-readable migration report for a read-only `/__migrations`
    /// endpoint: applied/pending status, the pending model diff, and drift.
    /// Mount it as `app.get("/__migrations", move |_| json(200, &report))`.
    pub fn report_json<B: Backend>(migrator: &Migrator, conn: &B, models: &[TableSchema]) -> Json {
        let status = migrator
            .status(conn)
            .map(|s| status_json(&s))
            .unwrap_or(Json::Null);
        let pending = generate(migrator, models, conn.dialect())
            .map(|p| Json::arr(p.ops.iter().map(|o| Json::str(o.summary())).collect()))
            .unwrap_or(Json::Null);
        let drift_report = drift(conn, migrator, models)
            .map(|d| d.to_json())
            .unwrap_or(Json::Null);
        Json::obj(vec![
            ("migrations", status),
            ("pending", pending),
            ("drift", drift_report),
        ])
    }

    /// A sortable `YYYYMMDD_HHMMSS` version id from the current UTC time, for a
    /// freshly generated migration.
    fn timestamp_version() -> String {
        let secs = sutegi_crypto::now_secs().max(0);
        let (days, rem) = (secs / 86_400, secs % 86_400);
        let (hh, mm, ss) = (rem / 3600, (rem % 3600) / 60, rem % 60);
        // Civil date from days-since-epoch (Howard Hinnant's algorithm).
        let z = days + 719_468;
        let era = z / 146_097;
        let doe = z - era * 146_097;
        let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
        let y = yoe + era * 400;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let d = doy - (153 * mp + 2) / 5 + 1;
        let m = if mp < 10 { mp + 3 } else { mp - 9 };
        let y = if m <= 2 { y + 1 } else { y };
        format!("{y:04}{m:02}{d:02}_{hh:02}{mm:02}{ss:02}")
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

    #[cfg(feature = "events")]
    pub use sutegi_events::{
        event, Aggregate, EventError, EventStore, Expected, NewEvent, ProjectionWorkers,
        Projections, StoredEvent,
    };
    #[cfg(feature = "orm")]
    pub use sutegi_orm::migrate::{Migration, Migrator};
    #[cfg(feature = "orm")]
    pub use sutegi_orm::row::FromRow;
    #[cfg(feature = "orm")]
    pub use sutegi_orm::{
        Backend, ColType, Column, DeleteBuilder, Dialect, FkAction, ForeignKey, FromInput, Index,
        Metric, Model, Page, QueryBuilder, TableSchema, Transactional, UpdateBuilder, Value,
        Vector,
    };
    #[cfg(feature = "queue")]
    pub use sutegi_queue::{Queue, Workers};
    #[cfg(feature = "repl")]
    pub use sutegi_repl::Repl;
    #[cfg(feature = "validate")]
    pub use sutegi_validate::{validate_schema, Rule, Ruleset, Validate, ValidationErrors};

    #[cfg(feature = "derive")]
    pub use sutegi_macros::{Model, Validate};

    #[cfg(feature = "actors")]
    pub use sutegi_actors::{
        spawn, spawn_opts, Actor, ActorRef, ActorState, ActorStatus, AskError, ChildSpec,
        ExitReason, Opts as ActorOpts, Registry as ActorRegistry, ReplyTo, Restart, Strategy,
        Supervisor, SupervisorHandle, SupervisorState, SupervisorStatus, TellError,
    };

    #[cfg(feature = "presence")]
    pub use sutegi_channels::Presence;
    #[cfg(feature = "channels")]
    pub use sutegi_channels::{Channel, ChannelHub, Channels, LeaveReason, Reply, Socket};
    #[cfg(feature = "pubsub-postgres")]
    pub use sutegi_pubsub::PgPubSub;
    #[cfg(feature = "pubsub")]
    pub use sutegi_pubsub::{Broker, BrokerExt, PubSub};
    #[cfg(feature = "session")]
    pub use sutegi_session::{Session, Sessions};
    #[cfg(feature = "storage-db")]
    pub use sutegi_storage::DbStorage;
    #[cfg(feature = "ws")]
    pub use sutegi_web::ws::{binary_frame, text_frame, Conn, Msg, WsConfig};
    #[cfg(feature = "ws")]
    pub use sutegi_web::Ws;

    #[cfg(feature = "auth")]
    pub use sutegi_auth::{
        hash_password, require_auth, require_role, require_token, token_user, verify_password,
        ApiToken, Auth, Tokens, User, Users,
    };

    #[cfg(feature = "template")]
    pub use sutegi_template::{Template, Templates};

    #[cfg(feature = "mail")]
    pub use sutegi_mail::{Email, MailMessage, Mailer, Theme, Transport};

    #[cfg(feature = "auth-mail")]
    pub use sutegi_auth::AuthMail;

    #[cfg(feature = "storage")]
    pub use sutegi_storage::{FsStorage, ObjectMeta, S3Store, Storage};

    #[cfg(feature = "sqlite")]
    pub use sutegi_orm::db::Db;

    #[cfg(feature = "postgres")]
    pub use sutegi_orm::pg::Pg;

    /// The JSON key/value store — available over either backend.
    #[cfg(any(feature = "sqlite", feature = "postgres"))]
    pub use sutegi_orm::kv::Kv;

    #[cfg(feature = "hexagon")]
    pub use sutegi_hexagon::{
        respond, respond_created, AppError, AppResult, Command, Event, EventBus, IntoJson, Query,
        UseCase,
    };
}
