//! Rust ⇄ SQL value conversion for arguments, results and SPI parameters.
//!
//! | Rust                         | SQL                |
//! |------------------------------|--------------------|
//! | `bool`                       | `boolean`          |
//! | `i16` / `i32` / `i64`        | `smallint` / `integer` / `bigint` |
//! | `f32` / `f64`                | `real` / `double precision` |
//! | `&str` / `String`            | `text`             |
//! | `&[u8]` / `Vec<u8>`          | `bytea`            |
//! | `Option<T>`                  | `T`, `NULL` ⇄ `None` |
//! | `()` (result only)           | `void`             |
//! | `SetOf<T>` (result only)     | `SETOF T`          |
//! | `Result<T>` (result only)    | `T`, `Err` raises  |

use crate::SqlState;
use crate::error::{Error, Result};
use crate::ffi::{Arg, Call, Kind, RawValue};

pub(crate) fn utf8(bytes: &[u8]) -> Result<&str> {
    std::str::from_utf8(bytes).map_err(|e| {
        Error::new(
            SqlState::CHARACTER_NOT_IN_REPERTOIRE,
            format!("text is not valid UTF-8: {e}"),
        )
    })
}

fn null_arg(index: i32) -> Error {
    Error::new(
        SqlState::NULL_VALUE_NOT_ALLOWED,
        format!("argument {} is NULL", index + 1),
    )
    .hint("Declare the parameter as Option<T> to accept NULL.")
}

/// Reads one argument of a SQL function. Implemented for the types in the
/// module table; the `#[pg_function]` wrapper calls it per parameter.
pub trait FromArg<'a>: Sized {
    #[doc(hidden)]
    fn from_arg(call: Call<'a>, index: i32) -> Result<Self>;
}

/// Hands a Rust result back to PostgreSQL.
pub trait IntoReturn {
    #[doc(hidden)]
    fn into_return(self, call: Call<'_>) -> Result<()>;
}

/// One value of a scalar SQL type, used for results, set rows and SPI
/// parameters.
pub trait ToSql {
    #[doc(hidden)]
    fn kind(&self) -> Kind;
    #[doc(hidden)]
    fn to_raw(&self) -> RawValue;
}

macro_rules! int_types {
    ($($t:ty => $kind:ident),*) => {$(
        impl<'a> FromArg<'a> for $t {
            fn from_arg(call: Call<'a>, index: i32) -> Result<Self> {
                <Option<$t>>::from_arg(call, index)?.ok_or_else(|| null_arg(index))
            }
        }
        impl<'a> FromArg<'a> for Option<$t> {
            fn from_arg(call: Call<'a>, index: i32) -> Result<Self> {
                match call.arg(index, Kind::$kind)? {
                    Arg::Null => Ok(None),
                    Arg::Int(i) => <$t>::try_from(i).map(Some).map_err(|_| Error::internal("integer argument out of range")),
                    _ => Err(Error::internal("argument kind mismatch")),
                }
            }
        }
        impl ToSql for $t {
            fn kind(&self) -> Kind { Kind::$kind }
            fn to_raw(&self) -> RawValue { RawValue::int(Kind::$kind, i64::from(*self)) }
        }
    )*};
}

int_types!(i16 => Int2, i32 => Int4, i64 => Int8);

impl<'a> FromArg<'a> for bool {
    fn from_arg(call: Call<'a>, index: i32) -> Result<Self> {
        <Option<bool>>::from_arg(call, index)?.ok_or_else(|| null_arg(index))
    }
}

impl<'a> FromArg<'a> for Option<bool> {
    fn from_arg(call: Call<'a>, index: i32) -> Result<Self> {
        match call.arg(index, Kind::Bool)? {
            Arg::Null => Ok(None),
            Arg::Int(i) => Ok(Some(i != 0)),
            _ => Err(Error::internal("argument kind mismatch")),
        }
    }
}

impl ToSql for bool {
    fn kind(&self) -> Kind {
        Kind::Bool
    }
    fn to_raw(&self) -> RawValue {
        RawValue::int(Kind::Bool, i64::from(*self))
    }
}

macro_rules! float_types {
    ($($t:ty => $kind:ident),*) => {$(
        impl<'a> FromArg<'a> for $t {
            fn from_arg(call: Call<'a>, index: i32) -> Result<Self> {
                <Option<$t>>::from_arg(call, index)?.ok_or_else(|| null_arg(index))
            }
        }
        impl<'a> FromArg<'a> for Option<$t> {
            fn from_arg(call: Call<'a>, index: i32) -> Result<Self> {
                match call.arg(index, Kind::$kind)? {
                    Arg::Null => Ok(None),
                    #[allow(clippy::cast_possible_truncation, reason = "a float4 argument round-trips through f64 exactly")]
                    Arg::Float(f) => Ok(Some(f as $t)),
                    _ => Err(Error::internal("argument kind mismatch")),
                }
            }
        }
        impl ToSql for $t {
            fn kind(&self) -> Kind { Kind::$kind }
            fn to_raw(&self) -> RawValue { RawValue::float(Kind::$kind, f64::from(*self)) }
        }
    )*};
}

float_types!(f32 => Float4, f64 => Float8);

fn bytes_arg<'a>(call: Call<'a>, index: i32, kind: Kind) -> Result<Option<&'a [u8]>> {
    match call.arg(index, kind)? {
        Arg::Null => Ok(None),
        Arg::Bytes(b) => Ok(Some(b)),
        _ => Err(Error::internal("argument kind mismatch")),
    }
}

impl<'a> FromArg<'a> for Option<&'a str> {
    fn from_arg(call: Call<'a>, index: i32) -> Result<Self> {
        bytes_arg(call, index, Kind::Text)?.map(utf8).transpose()
    }
}

impl<'a> FromArg<'a> for &'a str {
    fn from_arg(call: Call<'a>, index: i32) -> Result<Self> {
        <Option<&'a str>>::from_arg(call, index)?.ok_or_else(|| null_arg(index))
    }
}

impl<'a> FromArg<'a> for Option<String> {
    fn from_arg(call: Call<'a>, index: i32) -> Result<Self> {
        Ok(<Option<&'a str>>::from_arg(call, index)?.map(str::to_owned))
    }
}

impl<'a> FromArg<'a> for String {
    fn from_arg(call: Call<'a>, index: i32) -> Result<Self> {
        <&'a str>::from_arg(call, index).map(str::to_owned)
    }
}

impl<'a> FromArg<'a> for Option<&'a [u8]> {
    fn from_arg(call: Call<'a>, index: i32) -> Result<Self> {
        bytes_arg(call, index, Kind::Bytea)
    }
}

impl<'a> FromArg<'a> for &'a [u8] {
    fn from_arg(call: Call<'a>, index: i32) -> Result<Self> {
        <Option<&'a [u8]>>::from_arg(call, index)?.ok_or_else(|| null_arg(index))
    }
}

impl<'a> FromArg<'a> for Option<Vec<u8>> {
    fn from_arg(call: Call<'a>, index: i32) -> Result<Self> {
        Ok(<Option<&'a [u8]>>::from_arg(call, index)?.map(<[u8]>::to_vec))
    }
}

impl<'a> FromArg<'a> for Vec<u8> {
    fn from_arg(call: Call<'a>, index: i32) -> Result<Self> {
        <&'a [u8]>::from_arg(call, index).map(<[u8]>::to_vec)
    }
}

impl ToSql for str {
    fn kind(&self) -> Kind {
        Kind::Text
    }
    fn to_raw(&self) -> RawValue {
        RawValue::bytes(Kind::Text, self.as_bytes())
    }
}

impl ToSql for String {
    fn kind(&self) -> Kind {
        Kind::Text
    }
    fn to_raw(&self) -> RawValue {
        self.as_str().to_raw()
    }
}

impl ToSql for [u8] {
    fn kind(&self) -> Kind {
        Kind::Bytea
    }
    fn to_raw(&self) -> RawValue {
        RawValue::bytes(Kind::Bytea, self)
    }
}

impl ToSql for Vec<u8> {
    fn kind(&self) -> Kind {
        Kind::Bytea
    }
    fn to_raw(&self) -> RawValue {
        self.as_slice().to_raw()
    }
}

impl<T: ToSql + ?Sized> ToSql for &T {
    fn kind(&self) -> Kind {
        (**self).kind()
    }
    fn to_raw(&self) -> RawValue {
        (**self).to_raw()
    }
}

/// The SQL kind of a value type, known without a value (for `NULL`s).
pub trait SqlKind {
    #[doc(hidden)]
    const KIND: Kind;
}

macro_rules! sql_kind {
    ($($t:ty => $kind:ident),*) => {$(
        impl SqlKind for $t { const KIND: Kind = Kind::$kind; }
    )*};
}

sql_kind!(bool => Bool, i16 => Int2, i32 => Int4, i64 => Int8, f32 => Float4, f64 => Float8,
    str => Text, String => Text, [u8] => Bytea, Vec<u8> => Bytea);

impl<T: SqlKind + ?Sized> SqlKind for &T {
    const KIND: Kind = T::KIND;
}

impl<T: SqlKind> SqlKind for Option<T> {
    const KIND: Kind = T::KIND;
}

impl<T: ToSql + SqlKind> ToSql for Option<T> {
    fn kind(&self) -> Kind {
        T::KIND
    }
    fn to_raw(&self) -> RawValue {
        match self {
            Some(v) => v.to_raw(),
            None => RawValue::null(T::KIND),
        }
    }
}

macro_rules! scalar_returns {
    ($($t:ty),*) => {$(
        impl IntoReturn for $t {
            fn into_return(self, call: Call<'_>) -> Result<()> { call.ret(&self.to_raw()) }
        }
        impl IntoReturn for Option<$t> {
            fn into_return(self, call: Call<'_>) -> Result<()> { call.ret(&self.to_raw()) }
        }
    )*};
}

scalar_returns!(bool, i16, i32, i64, f32, f64, String, Vec<u8>, &str, &[u8]);

impl IntoReturn for () {
    fn into_return(self, call: Call<'_>) -> Result<()> {
        call.ret(&RawValue {
            isnull: 0,
            ..RawValue::null(Kind::Void)
        })
    }
}

impl<T: IntoReturn> IntoReturn for Result<T> {
    fn into_return(self, call: Call<'_>) -> Result<()> {
        self?.into_return(call)
    }
}

/// The rows of a set-returning function (`RETURNS SETOF T`), materialized
/// into a tuplestore when the function returns. `T` is a scalar type or an
/// `Option` of one (`NULL` rows).
///
/// ```ignore
/// #[pg_function(immutable)]
/// fn series(n: i64) -> SetOf<i64> {
///     (1..=n).collect()
/// }
/// ```
#[derive(Debug, Clone, PartialEq)]
pub struct SetOf<T>(Vec<T>);

impl<T> SetOf<T> {
    #[must_use]
    pub fn new(rows: Vec<T>) -> SetOf<T> {
        SetOf(rows)
    }

    #[must_use]
    pub fn into_inner(self) -> Vec<T> {
        self.0
    }
}

impl<T> From<Vec<T>> for SetOf<T> {
    fn from(rows: Vec<T>) -> SetOf<T> {
        SetOf(rows)
    }
}

impl<T> FromIterator<T> for SetOf<T> {
    fn from_iter<I: IntoIterator<Item = T>>(iter: I) -> SetOf<T> {
        SetOf(iter.into_iter().collect())
    }
}

const INTERRUPT_CHECK_EVERY: usize = 1024;

impl<T: ToSql + SqlKind> IntoReturn for SetOf<T> {
    fn into_return(self, call: Call<'_>) -> Result<()> {
        call.srf_begin(T::KIND)?;
        for (i, row) in self.0.iter().enumerate() {
            if i % INTERRUPT_CHECK_EVERY == INTERRUPT_CHECK_EVERY - 1 {
                call.check_interrupts()?;
            }
            call.srf_push(&row.to_raw())?;
        }
        Ok(())
    }
}
