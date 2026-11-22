//! PostgreSQL extensions in plain Rust — sutegi's take on
//! [gopgext](https://github.com/jespino/gopgext).
//!
//! A project is an ordinary `staticlib` crate. Functions marked
//! `#[pg_function]` become SQL functions; `sutegi pgext build` generates
//! everything else — the C glue, the `CREATE FUNCTION` script and the control
//! file — and links the result with PGXS into a loadable module.
//!
//! ```ignore
//! use sutegi_pgext::{pg_function, Error, Result, SetOf, SqlState};
//!
//! /// Returns a greeting for name.
//! #[pg_function(immutable, parallel_safe)]
//! fn hello(name: &str) -> String {
//!     format!("Hello, {name}!")
//! }
//!
//! #[pg_function(immutable, parallel_safe)]
//! fn safe_div(a: i64, b: i64) -> Result<i64> {
//!     if b == 0 {
//!         return Err(Error::new(SqlState::DIVISION_BY_ZERO, "division by zero"));
//!     }
//!     a.checked_div(b)
//!         .ok_or_else(|| Error::new(SqlState::NUMERIC_VALUE_OUT_OF_RANGE, "bigint out of range"))
//! }
//!
//! #[pg_function(immutable)]
//! fn series(n: i64) -> SetOf<i64> {
//!     SetOf::new(1..=n)
//! }
//! ```
//!
//! ```sh
//! sutegi pgext new hello && sutegi pgext install hello
//! psql -c 'CREATE EXTENSION hello' -c "SELECT hello('world')"
//! ```
//!
//! # The runtime model
//!
//! A backend is single-threaded: Rust runs on the backend's stack only while
//! PostgreSQL calls in. PostgreSQL reports errors with `siglongjmp`, which
//! must never unwind a Rust frame, so Rust never calls PostgreSQL directly:
//! every call goes through a small C runtime that catches the error, parks
//! it, and returns a Rust `Err` ([`Error::is_postgres_error`]). The parked
//! error is re-thrown once Rust has returned. A Rust `Err` becomes an
//! `ERROR` with its SQLSTATE; a panic is caught and becomes an internal
//! `ERROR` (the backend survives). Results are copied into `palloc` memory
//! before Rust's values are dropped. A failed allocation, which Rust can only
//! abort on, ends just the session with `FATAL` (see the global allocator);
//! deep recursion should call [`check_stack_depth`].
//!
//! Don't call into this crate from threads you spawn: PostgreSQL isn't
//! thread-safe, and every operation here fails outside a SQL function call.

mod alloc;
mod error;
mod ffi;
pub mod spi;
mod value;

#[cfg(feature = "build")]
pub mod build;
#[cfg(test)]
mod fake;

use std::panic::{AssertUnwindSafe, catch_unwind};

pub use error::{Error, Result, SqlState};
pub use sutegi_macros::pg_function;
pub use value::{FromArg, IntoReturn, SetOf, SqlKind, ToSql};

use ffi::{Call, with_current};

fn panic_message(payload: &(dyn std::any::Any + Send)) -> &str {
    if let Some(s) = payload.downcast_ref::<&'static str>() {
        s
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s
    } else {
        "a non-string panic payload"
    }
}

pub(crate) fn run<'a>(
    call: Call<'a>,
    name: &str,
    nargs: i32,
    body: impl FnOnce(Call<'a>) -> Result<()>,
) {
    let outcome = catch_unwind(AssertUnwindSafe(|| {
        if call.nargs() != nargs {
            return Err(Error::internal(format!(
                "{name} takes {nargs} arguments, PostgreSQL passed {}",
                call.nargs()
            ))
            .hint(
                "The installed SQL script is out of date: rebuild and reinstall the extension.",
            ));
        }
        body(call)
    }));
    match outcome {
        Ok(Ok(())) => {}
        Ok(Err(e)) => e.report(call),
        Err(payload) => Error::internal(format!(
            "Rust panic in {name}: {}",
            panic_message(&*payload)
        ))
        .report(call),
    }
}

/// Logs `message` at `NOTICE`. Errors only when PostgreSQL fails to emit
/// it (the call is then poisoned) or outside a SQL function call.
///
/// # Errors
///
/// See above.
pub fn notice(message: &str) -> Result<()> {
    with_current(|c| c.log(3, message))
}

/// Logs `message` at `WARNING`.
///
/// # Errors
///
/// As [`notice`].
pub fn warning(message: &str) -> Result<()> {
    with_current(|c| c.log(4, message))
}

/// Logs `message` at `INFO` (always sent to the client).
///
/// # Errors
///
/// As [`notice`].
pub fn info(message: &str) -> Result<()> {
    with_current(|c| c.log(2, message))
}

/// Logs `message` to the server log only (`LOG`).
///
/// # Errors
///
/// As [`notice`].
pub fn log(message: &str) -> Result<()> {
    with_current(|c| c.log(1, message))
}

/// Logs `message` at `DEBUG1`.
///
/// # Errors
///
/// As [`notice`].
pub fn debug(message: &str) -> Result<()> {
    with_current(|c| c.log(0, message))
}

/// Lets PostgreSQL act on a pending cancel or termination request. Call it
/// in long loops; on cancel it returns the cancel error, to propagate.
///
/// # Errors
///
/// The pending cancel/termination, or outside a SQL function call.
pub fn check_for_interrupts() -> Result<()> {
    with_current(|c| c.check_interrupts())
}

/// Fails when PostgreSQL's `max_stack_depth` is exceeded. Call it at the
/// top of a recursive Rust function: PostgreSQL only checks the stack when it
/// runs, and a Rust stack overflow kills the backend with `SIGSEGV`, which
/// makes the postmaster reset every session.
///
/// # Errors
///
/// `stack depth limit exceeded` (SQLSTATE `54001`), or outside a SQL call.
pub fn check_stack_depth() -> Result<()> {
    if with_current(|call| Ok(call.stack_too_deep()))? {
        return Err(Error::new(
            SqlState::STATEMENT_TOO_COMPLEX,
            "stack depth limit exceeded",
        )
        .hint("Increase max_stack_depth, or recurse less deeply."));
    }
    Ok(())
}

#[cfg(all(panic = "abort", not(feature = "build")))]
compile_error!(
    "sutegi-pgext needs panic = \"unwind\": it turns a panic into a SQL error, and with \"abort\" \
     a panic kills the backend and resets every session. Set panic = \"unwind\" in the profile."
);

#[doc(hidden)]
pub mod __private {
    pub use crate::alloc::sutegi_pgext_on_load;
    pub use crate::ffi::{Call, RawCall};
    pub use crate::value::{FromArg, IntoReturn};

    pub use crate::ffi::invoke;
}

#[cfg(test)]
mod tests {
    use super::fake::{FakeCall, Outcome, Val};
    use super::*;

    #[test]
    fn scalars_round_trip() {
        let out = FakeCall::new(vec![Val::Int(40), Val::Int(2)]).invoke("add", 2, |call| {
            let a = i32::from_arg(call, 0)?;
            let b = i32::from_arg(call, 1)?;
            (a + b).into_return(call)
        });
        assert_eq!(out, Outcome::Value(Val::Int(42)));
    }

    #[test]
    fn text_is_borrowed_and_returned() {
        let out = FakeCall::new(vec![Val::Text("world".into())]).invoke("hello", 1, |call| {
            let name = <&str>::from_arg(call, 0)?;
            format!("Hello, {name}!").into_return(call)
        });
        assert_eq!(out, Outcome::Value(Val::Text("Hello, world!".into())));
    }

    #[test]
    fn null_into_non_option_is_an_error() {
        let out = FakeCall::new(vec![Val::Null])
            .invoke("f", 1, |call| i64::from_arg(call, 0)?.into_return(call));
        assert!(matches!(out, Outcome::Error(s, _) if s == "22004"));
    }

    #[test]
    fn option_maps_null_both_ways() {
        let out = FakeCall::new(vec![Val::Null]).invoke("f", 1, |call| {
            <Option<i64>>::from_arg(call, 0)?
                .map(|v| v * 2)
                .into_return(call)
        });
        assert_eq!(out, Outcome::Value(Val::Null));
    }

    #[test]
    fn invalid_utf8_text_is_rejected() {
        let out = FakeCall::new(vec![Val::Bytes(vec![0xff, 0xfe])])
            .invoke("f", 1, |call| <&str>::from_arg(call, 0)?.into_return(call));
        assert!(matches!(out, Outcome::Error(s, _) if s == "22021"));
    }

    #[test]
    fn err_becomes_sql_error_with_state() {
        let out = FakeCall::new(vec![]).invoke("f", 0, |call| {
            Err::<i32, _>(Error::new(SqlState::DIVISION_BY_ZERO, "division by zero"))
                .into_return(call)
        });
        assert_eq!(
            out,
            Outcome::Error("22012".into(), "division by zero".into())
        );
    }

    #[test]
    fn panic_becomes_internal_error() {
        let out = FakeCall::new(vec![]).invoke("boom", 0, |_| panic!("kaboom"));
        assert!(
            matches!(&out, Outcome::Error(s, m) if s == "XX000" && m.contains("kaboom") && m.contains("boom"))
        );
    }

    #[test]
    fn wrong_arg_count_is_reported() {
        let out = FakeCall::new(vec![Val::Int(1)]).invoke("f", 2, |call| ().into_return(call));
        assert!(matches!(&out, Outcome::Error(s, m) if s == "XX000" && m.contains("takes 2")));
    }

    #[test]
    fn set_rows_are_pushed_in_order() {
        let out = FakeCall::new(vec![]).invoke("s", 0, |call| {
            SetOf::from(vec![Some(1i64), None, Some(3)]).into_return(call)
        });
        assert_eq!(out, Outcome::Set(vec![Val::Int(1), Val::Null, Val::Int(3)]));
    }

    #[test]
    fn lazy_sets_stream_without_collecting() {
        let out = FakeCall::new(vec![]).invoke("s", 0, |call| {
            SetOf::new((1..=3i64).map(|i| (i != 2).then_some(i * 10))).into_return(call)
        });
        assert_eq!(
            out,
            Outcome::Set(vec![Val::Int(10), Val::Null, Val::Int(30)])
        );
    }

    #[test]
    fn foreign_errors_convert_through_into() {
        let out = FakeCall::new(vec![Val::Text("x1".into())]).invoke("p", 1, |call| {
            let s = <&str>::from_arg(call, 0)?;
            s.parse::<i32>().into_return(call)
        });
        assert!(
            matches!(&out, Outcome::Error(st, m) if st == "P0001" && m.contains("invalid digit"))
        );
    }

    #[test]
    fn nul_in_messages_is_shown_not_truncated() {
        let out = FakeCall::new(vec![]).invoke("f", 0, |call| {
            Err::<i32, _>(Error::raise("a\0b")).into_return(call)
        });
        assert_eq!(out, Outcome::Error("P0001".into(), "a\\0b".into()));
    }

    #[test]
    fn stack_depth_is_reported_as_54001() {
        let out = FakeCall::new(vec![]).invoke("depth", 0, |call| {
            check_stack_depth()?;
            fake::set_stack_too_deep(true);
            let deep = check_stack_depth().map_err(|e| e.sqlstate());
            fake::set_stack_too_deep(false);
            assert_eq!(deep, Err(Some(SqlState::STATEMENT_TOO_COMPLEX)));
            ().into_return(call)
        });
        assert_eq!(out, Outcome::Value(Val::Null));
    }

    #[test]
    fn helpers_fail_outside_a_call() {
        assert!(notice("x").is_err());
        assert!(spi::execute("select 1", &[]).is_err());
        assert_eq!(
            check_stack_depth().unwrap_err().sqlstate(),
            Some(SqlState::FEATURE_NOT_SUPPORTED)
        );
    }

    #[test]
    fn current_call_is_restored_after_invoke() {
        FakeCall::new(vec![]).invoke("f", 0, |call| {
            notice("inside")?;
            ().into_return(call)
        });
        assert!(with_current(|_| Ok(())).is_err());
    }
}
