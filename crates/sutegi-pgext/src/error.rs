use std::fmt;

/// A five-character SQLSTATE code.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct SqlState([u8; 5]);

impl SqlState {
    pub const RAISE_EXCEPTION: SqlState = SqlState::new("P0001");
    pub const DATA_EXCEPTION: SqlState = SqlState::new("22000");
    pub const NUMERIC_VALUE_OUT_OF_RANGE: SqlState = SqlState::new("22003");
    pub const NULL_VALUE_NOT_ALLOWED: SqlState = SqlState::new("22004");
    pub const DIVISION_BY_ZERO: SqlState = SqlState::new("22012");
    pub const INVALID_PARAMETER_VALUE: SqlState = SqlState::new("22023");
    pub const INVALID_TEXT_REPRESENTATION: SqlState = SqlState::new("22P02");
    pub const CHARACTER_NOT_IN_REPERTOIRE: SqlState = SqlState::new("22021");
    pub const CHECK_VIOLATION: SqlState = SqlState::new("23514");
    pub const UNIQUE_VIOLATION: SqlState = SqlState::new("23505");
    pub const INSUFFICIENT_PRIVILEGE: SqlState = SqlState::new("42501");
    pub const FEATURE_NOT_SUPPORTED: SqlState = SqlState::new("0A000");
    pub const PROGRAM_LIMIT_EXCEEDED: SqlState = SqlState::new("54000");
    pub const INTERNAL_ERROR: SqlState = SqlState::new("XX000");

    /// Parses a code such as `"22023"`.
    ///
    /// # Panics
    ///
    /// When `code` is not five ASCII digits or uppercase letters; in a
    /// `const` that is a compile error.
    #[must_use]
    pub const fn new(code: &str) -> SqlState {
        match SqlState::parse(code) {
            Some(s) => s,
            None => panic!("a SQLSTATE is five ASCII digits or uppercase letters"),
        }
    }

    /// Parses a code such as `"22023"`, or `None` when it is malformed.
    #[must_use]
    pub const fn parse(code: &str) -> Option<SqlState> {
        let b = code.as_bytes();
        if b.len() != 5 {
            return None;
        }
        let mut i = 0;
        while i < 5 {
            if !(b[i].is_ascii_digit() || b[i].is_ascii_uppercase()) {
                return None;
            }
            i += 1;
        }
        Some(SqlState([b[0], b[1], b[2], b[3], b[4]]))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        std::str::from_utf8(&self.0).unwrap_or("XX000")
    }

    pub(crate) fn bytes(&self) -> &[u8; 5] {
        &self.0
    }
}

impl fmt::Debug for SqlState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SqlState({})", self.as_str())
    }
}

impl fmt::Display for SqlState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// An error a SQL function raises: PostgreSQL reports it as an `ERROR` with
/// its SQLSTATE, message, detail and hint once the Rust function returns.
///
/// Any `std::error::Error` converts into one (SQLSTATE `P0001`, like
/// PL/pgSQL's `RAISE`), so `?` works on parse and I/O errors. `Error` itself
/// deliberately doesn't implement `std::error::Error`, which keeps that
/// conversion coherent.
pub struct Error {
    repr: Repr,
}

enum Repr {
    Sql {
        state: SqlState,
        message: String,
        detail: Option<String>,
        hint: Option<String>,
    },
    Raised,
}

/// `Result<T, Error>`.
pub type Result<T, E = Error> = std::result::Result<T, E>;

impl Error {
    #[must_use]
    pub fn new(state: SqlState, message: impl Into<String>) -> Error {
        Error {
            repr: Repr::Sql {
                state,
                message: message.into(),
                detail: None,
                hint: None,
            },
        }
    }

    /// An error with SQLSTATE `P0001` (`raise_exception`).
    #[must_use]
    pub fn raise(message: impl Into<String>) -> Error {
        Error::new(SqlState::RAISE_EXCEPTION, message)
    }

    #[must_use]
    pub fn detail(mut self, detail: impl Into<String>) -> Error {
        if let Repr::Sql { detail: d, .. } = &mut self.repr {
            *d = Some(detail.into());
        }
        self
    }

    #[must_use]
    pub fn hint(mut self, hint: impl Into<String>) -> Error {
        if let Repr::Sql { hint: h, .. } = &mut self.repr {
            *h = Some(hint.into());
        }
        self
    }

    /// The SQLSTATE, or `None` for an error PostgreSQL itself raised (a
    /// failed query, a cancel), which is re-thrown unchanged.
    #[must_use]
    pub fn sqlstate(&self) -> Option<SqlState> {
        match &self.repr {
            Repr::Sql { state, .. } => Some(*state),
            Repr::Raised => None,
        }
    }

    /// Whether PostgreSQL raised this error. The call is then poisoned: every
    /// later PostgreSQL operation fails, and the original error is reported
    /// whatever the function returns.
    #[must_use]
    pub fn is_postgres_error(&self) -> bool {
        matches!(self.repr, Repr::Raised)
    }

    pub(crate) fn raised() -> Error {
        Error { repr: Repr::Raised }
    }

    pub(crate) fn internal(message: impl Into<String>) -> Error {
        Error::new(SqlState::INTERNAL_ERROR, message)
    }

    pub(crate) fn report(&self, call: crate::ffi::Call<'_>) {
        if let Repr::Sql {
            state,
            message,
            detail,
            hint,
        } = &self.repr
        {
            call.set_error(state.bytes(), message, detail.as_deref(), hint.as_deref());
        }
    }
}

impl<E: std::error::Error> From<E> for Error {
    fn from(e: E) -> Error {
        Error::raise(e.to_string())
    }
}

impl fmt::Debug for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.repr {
            Repr::Sql {
                state,
                message,
                detail,
                hint,
            } => f
                .debug_struct("Error")
                .field("state", state)
                .field("message", message)
                .field("detail", detail)
                .field("hint", hint)
                .finish(),
            Repr::Raised => f.write_str("Error(raised by PostgreSQL)"),
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.repr {
            Repr::Sql { state, message, .. } => write!(f, "{message} (SQLSTATE {state})"),
            Repr::Raised => f.write_str("error raised by PostgreSQL"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sqlstate_parse_rejects_malformed_codes() {
        assert_eq!(
            SqlState::parse("22023").map(|s| s.to_string()).as_deref(),
            Some("22023")
        );
        assert!(SqlState::parse("2202").is_none());
        assert!(SqlState::parse("2202a").is_none());
        assert!(SqlState::parse("220234").is_none());
        assert!(SqlState::parse("22 23").is_none());
    }

    #[test]
    fn std_errors_convert_to_raise_exception() {
        let e: Error = "x".parse::<i32>().unwrap_err().into();
        assert_eq!(e.sqlstate(), Some(SqlState::RAISE_EXCEPTION));
        assert!(e.to_string().starts_with("invalid digit"));
    }
}
