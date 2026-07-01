//! Ergonomic response conversion.
//!
//! Handlers return anything that is [`IntoResponse`] — a string, a [`Json`],
//! a `(status, body)` tuple, `()`, or a `Result` whose `Err` is an [`Error`].
//! That is what lets a handler use `?`:
//!
//! ```ignore
//! app.get("/todos/:id", "show", |c| {
//!     let todo = c.model::<Todo, Db>("id")?;   // 404 Error propagates
//!     Ok(todo.to_json())                        // 200 application/json
//! });
//! ```

use crate::{json, no_content, text, Response};
use sutegi_json::Json;

/// A transport-agnostic error that converts into an HTTP [`Response`].
///
/// Build one with a named constructor ([`Error::not_found`],
/// [`Error::bad_request`], …) or let `?` convert a `String`/`io::Error` into a
/// `500`. When rendered it becomes `{"error": <message>}` (plus `"errors"` when
/// [`with_fields`](Error::with_fields) carries structured validation detail).
#[derive(Debug, Clone)]
pub struct Error {
    /// The HTTP status code to send.
    pub status: u16,
    /// A human/agent-readable message.
    pub message: String,
    /// Optional structured detail (e.g. per-field validation errors).
    pub fields: Option<Json>,
}

impl Error {
    /// An error with an explicit status code.
    pub fn new(status: u16, message: impl Into<String>) -> Error {
        Error {
            status,
            message: message.into(),
            fields: None,
        }
    }

    /// `400 Bad Request`.
    pub fn bad_request(message: impl Into<String>) -> Error {
        Error::new(400, message)
    }
    /// `401 Unauthorized`.
    pub fn unauthorized(message: impl Into<String>) -> Error {
        Error::new(401, message)
    }
    /// `403 Forbidden`.
    pub fn forbidden(message: impl Into<String>) -> Error {
        Error::new(403, message)
    }
    /// `404 Not Found`.
    pub fn not_found(message: impl Into<String>) -> Error {
        Error::new(404, message)
    }
    /// `422 Unprocessable Entity` — the usual validation failure.
    pub fn unprocessable(message: impl Into<String>) -> Error {
        Error::new(422, message)
    }
    /// `500 Internal Server Error`.
    pub fn internal(message: impl Into<String>) -> Error {
        Error::new(500, message)
    }

    /// Attach structured detail, rendered under `"errors"` in the JSON body.
    pub fn with_fields(mut self, fields: Json) -> Error {
        self.fields = Some(fields);
        self
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} ({})", self.message, self.status)
    }
}

impl std::error::Error for Error {}

impl From<String> for Error {
    fn from(message: String) -> Error {
        Error::internal(message)
    }
}

impl From<&str> for Error {
    fn from(message: &str) -> Error {
        Error::internal(message)
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Error {
        Error::internal(e.to_string())
    }
}

/// Anything a handler may return. Converts into a concrete [`Response`].
pub trait IntoResponse {
    /// Consume `self` and produce the response to send.
    fn into_response(self) -> Response;
}

impl IntoResponse for Response {
    fn into_response(self) -> Response {
        self
    }
}

impl IntoResponse for Error {
    fn into_response(self) -> Response {
        let mut obj = vec![("error", Json::str(self.message))];
        if let Some(fields) = self.fields {
            obj.push(("errors", fields));
        }
        json(self.status, &Json::obj(obj))
    }
}

impl IntoResponse for Json {
    fn into_response(self) -> Response {
        json(200, &self)
    }
}

impl IntoResponse for &str {
    fn into_response(self) -> Response {
        text(200, self)
    }
}

impl IntoResponse for String {
    fn into_response(self) -> Response {
        text(200, &self)
    }
}

impl IntoResponse for () {
    fn into_response(self) -> Response {
        no_content()
    }
}

impl IntoResponse for (u16, Json) {
    fn into_response(self) -> Response {
        json(self.0, &self.1)
    }
}

impl IntoResponse for (u16, &str) {
    fn into_response(self) -> Response {
        text(self.0, self.1)
    }
}

impl IntoResponse for (u16, String) {
    fn into_response(self) -> Response {
        text(self.0, &self.1)
    }
}

impl<T: IntoResponse, E: IntoResponse> IntoResponse for Result<T, E> {
    fn into_response(self) -> Response {
        match self {
            Ok(t) => t.into_response(),
            Err(e) => e.into_response(),
        }
    }
}
