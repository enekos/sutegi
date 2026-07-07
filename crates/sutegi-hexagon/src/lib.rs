//! Opinionated building blocks for **hexagonal / clean architecture** in sutegi.
//!
//! The shape this crate nudges you toward:
//!
//! ```text
//!            inbound adapters                     outbound adapters
//!         (HTTP handlers, AI tools)            (SQLite repo, HTTP client)
//!                    │  call                          ▲  implement
//!                    ▼                                │
//!            ┌───────────────┐   depends on    ┌──────────────┐
//!            │  application  │ ───────────────▶│    ports     │
//!            │  (use cases)  │                 │   (traits)   │
//!            └───────┬───────┘                 └──────────────┘
//!                    │ uses
//!                    ▼
//!            ┌───────────────┐
//!            │    domain     │  (pure: entities + rules, no framework)
//!            └───────────────┘
//! ```
//!
//! **The dependency rule:** source dependencies point *inward*. Domain knows
//! nothing of the application; the application knows ports but not their
//! concrete adapters; adapters depend on the application/ports — never the
//! reverse. sutegi's HTTP and AI layers are just *inbound adapters*; a database
//! is just an *outbound adapter*. Swap either without touching the core.
//!
//! This crate gives you the three things that make that ergonomic:
//! * [`AppError`] — a transport-agnostic error with a canonical HTTP mapping.
//! * [`UseCase`] — the inbound-port trait every application service implements.
//! * [`respond`] / [`respond_created`] — adapter glue from `AppResult<T>` to a
//!   `Response`, so inbound HTTP adapters are one line.

use sutegi_json::Json;
use sutegi_web::{json, Response};

/// A transport-agnostic application/domain error. Lives in the inner layers;
/// adapters map it to their transport (the HTTP mapping is provided here as the
/// house convention).
#[derive(Debug, Clone)]
pub enum AppError {
    /// The requested resource does not exist (HTTP 404).
    NotFound(String),
    /// The input was rejected by a domain/validation rule (HTTP 422).
    Invalid(String),
    /// The action conflicts with current state (HTTP 409).
    Conflict(String),
    /// The caller is not allowed (HTTP 401).
    Unauthorized(String),
    /// An unexpected failure (HTTP 500).
    Internal(String),
}

impl AppError {
    pub fn not_found(msg: impl Into<String>) -> AppError {
        AppError::NotFound(msg.into())
    }
    pub fn invalid(msg: impl Into<String>) -> AppError {
        AppError::Invalid(msg.into())
    }
    pub fn conflict(msg: impl Into<String>) -> AppError {
        AppError::Conflict(msg.into())
    }
    pub fn unauthorized(msg: impl Into<String>) -> AppError {
        AppError::Unauthorized(msg.into())
    }
    pub fn internal(msg: impl Into<String>) -> AppError {
        AppError::Internal(msg.into())
    }

    pub fn status(&self) -> u16 {
        match self {
            AppError::NotFound(_) => 404,
            AppError::Invalid(_) => 422,
            AppError::Conflict(_) => 409,
            AppError::Unauthorized(_) => 401,
            AppError::Internal(_) => 500,
        }
    }

    /// A stable machine-readable tag (handy for agents and clients).
    pub fn kind(&self) -> &'static str {
        match self {
            AppError::NotFound(_) => "not_found",
            AppError::Invalid(_) => "invalid",
            AppError::Conflict(_) => "conflict",
            AppError::Unauthorized(_) => "unauthorized",
            AppError::Internal(_) => "internal",
        }
    }

    pub fn message(&self) -> &str {
        match self {
            AppError::NotFound(m)
            | AppError::Invalid(m)
            | AppError::Conflict(m)
            | AppError::Unauthorized(m)
            | AppError::Internal(m) => m,
        }
    }

    /// The canonical HTTP response for this error: `{ "error", "kind" }`.
    pub fn to_response(&self) -> Response {
        json(
            self.status(),
            &Json::obj(vec![
                ("error", Json::str(self.message())),
                ("kind", Json::str(self.kind())),
            ]),
        )
    }
}

impl std::fmt::Display for AppError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.kind(), self.message())
    }
}

impl std::error::Error for AppError {}

/// The result type used throughout the application and domain layers.
pub type AppResult<T> = Result<T, AppError>;

/// An **inbound port**: a single application use case. Input and Output are
/// plain domain/DTO types — the use case never sees HTTP or JSON, which is what
/// keeps the core testable and transport-independent.
pub trait UseCase: Send + Sync {
    type Input;
    type Output;
    fn execute(&self, input: Self::Input) -> AppResult<Self::Output>;
}

/// Presentation: how an outbound value becomes JSON at the adapter boundary.
/// Implement this in the *adapter* layer for your domain types so the domain
/// itself stays free of transport concerns.
pub trait IntoJson {
    fn into_json(self) -> Json;
}

impl IntoJson for Json {
    fn into_json(self) -> Json {
        self
    }
}

impl IntoJson for () {
    fn into_json(self) -> Json {
        Json::obj(vec![("ok", Json::Bool(true))])
    }
}

impl<T: IntoJson> IntoJson for Vec<T> {
    fn into_json(self) -> Json {
        Json::arr(self.into_iter().map(IntoJson::into_json).collect())
    }
}

impl<T: IntoJson> IntoJson for Option<T> {
    fn into_json(self) -> Json {
        match self {
            Some(v) => v.into_json(),
            None => Json::Null,
        }
    }
}

/// Adapter glue: map an `AppResult<T>` to a `200` response (or the error's
/// canonical response). This is what makes an inbound HTTP adapter a one-liner.
pub fn respond<T: IntoJson>(result: AppResult<T>) -> Response {
    match result {
        Ok(value) => json(200, &value.into_json()),
        Err(err) => err.to_response(),
    }
}

/// Like [`respond`], but `201 Created` on success.
pub fn respond_created<T: IntoJson>(result: AppResult<T>) -> Response {
    match result {
        Ok(value) => json(201, &value.into_json()),
        Err(err) => err.to_response(),
    }
}

// ---- CQRS ----------------------------------------------------------------

/// A **command** — a write intent. Consumes itself (one-shot) and returns its
/// result. The write side of CQRS; pair with [`Query`] for reads.
pub trait Command: Send + Sync {
    type Output;
    fn execute(self) -> AppResult<Self::Output>;
}

/// A **query** — a read intent. Borrows itself (idempotent, repeatable) and
/// returns data. The read side of CQRS.
pub trait Query: Send + Sync {
    type Output;
    fn execute(&self) -> AppResult<Self::Output>;
}

/// A domain **event** — something that happened. Dispatch via an [`EventBus`]
/// so side effects (read-model updates, notifications) are decoupled from the
/// command that caused them.
pub trait Event: std::any::Any + Send + Sync + 'static {}

/// A single event handler: receives the type-erased event payload.
type EventHandler = Box<dyn Fn(&dyn std::any::Any) + Send + Sync>;

/// A type-keyed event bus. Register handlers with [`EventBus::on`] and fan an
/// event out to all handlers for its type with [`EventBus::dispatch`].
#[derive(Default)]
pub struct EventBus {
    handlers: std::collections::HashMap<std::any::TypeId, Vec<EventHandler>>,
}

impl EventBus {
    pub fn new() -> EventBus {
        EventBus::default()
    }

    /// Register a handler for events of type `E`.
    pub fn on<E: Event, F: Fn(&E) + Send + Sync + 'static>(&mut self, handler: F) {
        let entry = self
            .handlers
            .entry(std::any::TypeId::of::<E>())
            .or_default();
        entry.push(Box::new(move |any| {
            if let Some(event) = any.downcast_ref::<E>() {
                handler(event);
            }
        }));
    }

    /// Dispatch an event to every handler registered for its type.
    pub fn dispatch<E: Event>(&self, event: &E) {
        if let Some(handlers) = self.handlers.get(&std::any::TypeId::of::<E>()) {
            for handler in handlers {
                handler(event);
            }
        }
    }
}

/// An optional generic **outbound-port** convention for persistence. Real ports
/// are usually domain-specific (e.g. `TodoRepository`); this is here for the
/// simple CRUD case. Define your own trait when the domain needs richer queries.
pub trait Repository<T, Id = i64>: Send + Sync {
    fn list(&self) -> AppResult<Vec<T>>;
    fn find(&self, id: &Id) -> AppResult<Option<T>>;
    fn add(&self, entity: T) -> AppResult<T>;
    fn delete(&self, id: &Id) -> AppResult<bool>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_maps_to_http() {
        assert_eq!(AppError::not_found("x").status(), 404);
        assert_eq!(AppError::invalid("x").kind(), "invalid");
    }

    #[test]
    fn cqrs_command_query_and_events() {
        use std::sync::atomic::{AtomicU32, Ordering};
        use std::sync::Arc;

        struct CreateThing {
            name: String,
        }
        impl Command for CreateThing {
            type Output = String;
            fn execute(self) -> AppResult<String> {
                if self.name.is_empty() {
                    return Err(AppError::invalid("empty"));
                }
                Ok(format!("created:{}", self.name))
            }
        }
        assert_eq!(
            CreateThing { name: "x".into() }.execute().unwrap(),
            "created:x"
        );
        assert!(CreateThing {
            name: String::new()
        }
        .execute()
        .is_err());

        struct CountThings;
        impl Query for CountThings {
            type Output = i64;
            fn execute(&self) -> AppResult<i64> {
                Ok(42)
            }
        }
        assert_eq!(CountThings.execute().unwrap(), 42);

        struct ThingCreated;
        impl Event for ThingCreated {}
        let hits = Arc::new(AtomicU32::new(0));
        let h = Arc::clone(&hits);
        let mut bus = EventBus::new();
        bus.on::<ThingCreated, _>(move |_e| {
            h.fetch_add(1, Ordering::Relaxed);
        });
        bus.dispatch(&ThingCreated);
        bus.dispatch(&ThingCreated);
        assert_eq!(hits.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn respond_wraps_ok_and_err() {
        let ok: Response = respond::<Json>(Ok(Json::str("hi")));
        assert_eq!(ok.status, 200);
        let err: Response = respond::<Json>(Err(AppError::conflict("dup")));
        assert_eq!(err.status, 409);
    }

    #[test]
    fn all_error_variants_map_status_kind_message() {
        let cases = [
            (AppError::not_found("a"), 404u16, "not_found"),
            (AppError::invalid("b"), 422, "invalid"),
            (AppError::conflict("c"), 409, "conflict"),
            (AppError::unauthorized("d"), 401, "unauthorized"),
            (AppError::internal("e"), 500, "internal"),
        ];
        for (err, status, kind) in cases {
            assert_eq!(err.status(), status);
            assert_eq!(err.kind(), kind);
            // Display is "<kind>: <message>".
            assert_eq!(format!("{}", err), format!("{}: {}", kind, err.message()));
            // The canonical response carries status + a {error,kind} body.
            assert_eq!(err.to_response().status, status);
        }
    }

    #[test]
    fn respond_created_is_201() {
        let r: Response = respond_created::<Json>(Ok(Json::str("x")));
        assert_eq!(r.status, 201);
        let e: Response = respond_created::<Json>(Err(AppError::invalid("bad")));
        assert_eq!(e.status, 422);
    }

    #[test]
    fn into_json_impls() {
        // () presents as {ok:true}.
        assert_eq!(().into_json(), Json::obj(vec![("ok", Json::Bool(true))]));
        // Option maps None → Null, Some → inner.
        assert_eq!(None::<Json>.into_json(), Json::Null);
        assert_eq!(Some(Json::int(3)).into_json(), Json::int(3));
        // Vec maps element-wise.
        assert_eq!(
            vec![Json::int(1), Json::int(2)].into_json(),
            Json::arr(vec![Json::int(1), Json::int(2)])
        );
        // Json is identity.
        assert_eq!(Json::str("x").into_json(), Json::str("x"));
    }

    #[test]
    fn use_case_executes() {
        struct Doubler;
        impl UseCase for Doubler {
            type Input = i64;
            type Output = i64;
            fn execute(&self, input: i64) -> AppResult<i64> {
                if input < 0 {
                    return Err(AppError::invalid("negative"));
                }
                Ok(input * 2)
            }
        }
        assert_eq!(Doubler.execute(21).unwrap(), 42);
        assert_eq!(Doubler.execute(-1).unwrap_err().status(), 422);
    }

    #[test]
    fn event_bus_only_fires_matching_type() {
        use std::sync::atomic::{AtomicU32, Ordering};
        use std::sync::Arc;
        struct A;
        impl Event for A {}
        struct B;
        impl Event for B {}
        let hits = Arc::new(AtomicU32::new(0));
        let h = Arc::clone(&hits);
        let mut bus = EventBus::new();
        bus.on::<A, _>(move |_| {
            h.fetch_add(1, Ordering::Relaxed);
        });
        bus.dispatch(&A); // matches
        bus.dispatch(&B); // no handler registered → ignored
        assert_eq!(hits.load(Ordering::Relaxed), 1);
    }
}
