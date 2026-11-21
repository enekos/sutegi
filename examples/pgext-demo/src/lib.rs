//! A PostgreSQL extension written with sutegi-pgext. Build and install it
//! with `sutegi pgext install examples/pgext-demo`, then
//! `CREATE EXTENSION pgext_demo`.

use sutegi_pgext::{Error, Result, SetOf, SqlState, pg_function, spi};

/// Returns a greeting for name.
#[pg_function(immutable, parallel_safe)]
fn hello(name: &str) -> String {
    format!("Hello, {name}!")
}

/// Divides a by b, raising division_by_zero instead of panicking.
#[pg_function(immutable, parallel_safe)]
fn safe_div(a: i64, b: i64) -> Result<i64> {
    a.checked_div(b).ok_or_else(|| {
        Error::new(SqlState::DIVISION_BY_ZERO, "division by zero")
            .detail(format!("{a} / {b}"))
            .hint("Pass a non-zero divisor.")
    })
}

/// Counts from 1 to n.
#[pg_function(immutable, parallel_safe, rows = 10)]
fn count_to(n: i64) -> SetOf<i64> {
    (1..=n).collect()
}

/// Splits s on sep; empty pieces come back as NULL.
#[pg_function(immutable, parallel_safe)]
fn split_nonempty(s: &str, sep: &str) -> SetOf<Option<String>> {
    s.split(sep)
        .map(|p| (!p.is_empty()).then(|| p.to_owned()))
        .collect()
}

/// Coalesces two integers, preferring a; NULL only when both are.
#[pg_function(immutable, parallel_safe, name = "first_of")]
fn first_of(a: Option<i32>, b: Option<i32>) -> Option<i32> {
    a.or(b)
}

/// Mean of a float8 and a float4, in float8.
#[pg_function(immutable)]
fn mean(a: f64, b: f32) -> f64 {
    (a + f64::from(b)) / 2.0
}

/// A smallint flipped, and a bool negated, to cover the narrow types.
#[pg_function(immutable)]
fn flip(x: i16, neg: bool) -> i16 {
    if neg { x.wrapping_neg() } else { x }
}

/// Reverses a bytea.
#[pg_function(immutable, parallel_safe)]
fn reverse_bytes(b: &[u8]) -> Vec<u8> {
    b.iter().rev().copied().collect()
}

/// Parses an integer, surfacing Rust's parse error as a SQL error.
#[pg_function(immutable)]
fn parse_int(s: &str) -> Result<i64> {
    Ok(s.trim().parse::<i64>()?)
}

/// Counts the rows of table t whose column v exceeds min, through SPI.
#[pg_function(stable)]
fn count_above(t: &str, min: i32) -> Result<i64> {
    if !t.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return Err(Error::new(
            SqlState::INVALID_PARAMETER_VALUE,
            "bad table name",
        ));
    }
    let n = spi::query_scalar(&format!("SELECT count(*) FROM {t} WHERE v > $1"), &[&min])?;
    Ok(n.as_deref().unwrap_or("0").parse()?)
}

/// Records a message in demo_log through SPI and returns the row count.
#[pg_function(volatile)]
fn log_message(msg: &str) -> Result<i64> {
    spi::execute("INSERT INTO demo_log(msg) VALUES ($1)", &[&msg])?;
    sutegi_pgext::notice(&format!("logged: {msg}"))?;
    let rows = spi::query("SELECT msg FROM demo_log ORDER BY id", &[])?;
    Ok(i64::try_from(rows.len())?)
}

/// Calls hello() through SPI: a nested call into this extension.
#[pg_function(stable)]
fn hello_via_spi(name: &str) -> Result<Option<String>> {
    spi::query_scalar("SELECT hello($1)", &[&name])
}

/// Runs a query that fails: the PostgreSQL error comes back unchanged.
#[pg_function(volatile)]
fn bad_query() -> Result<()> {
    spi::execute("SELECT * FROM no_such_table", &[])?;
    Ok(())
}

/// Swallows a failed query's error and returns normally: the original error
/// is still raised.
#[pg_function(volatile)]
fn swallow_error() -> i32 {
    let _ignored = spi::execute("SELECT 1/0", &[]);
    42
}

/// Panics: the backend survives and reports an internal error.
#[pg_function(volatile)]
fn boom() -> i32 {
    panic!("kaboom")
}

/// Spins until cancelled, checking for interrupts.
#[pg_function(volatile)]
fn spin() -> Result<()> {
    loop {
        sutegi_pgext::check_for_interrupts()?;
        std::hint::spin_loop();
    }
}
