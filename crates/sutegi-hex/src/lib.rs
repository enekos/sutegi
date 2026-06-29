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
    fn respond_wraps_ok_and_err() {
        let ok: Response = respond::<Json>(Ok(Json::str("hi")));
        assert_eq!(ok.status, 200);
        let err: Response = respond::<Json>(Err(AppError::conflict("dup")));
        assert_eq!(err.status, 409);
    }
}
