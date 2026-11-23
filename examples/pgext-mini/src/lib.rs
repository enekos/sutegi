//! A second sutegi-pgext extension. Its Rust function is also named
//! `hello`, like pgext-demo's, so both define `sutegi_pgext_fn_hello`,
//! `sx_invoke` and a copy of Rust's std: loading both into one backend shows
//! the symbols stay private to each module.

use sutegi_pgext::{SetOf, pg_function};

/// The mini greeting.
#[pg_function(immutable, parallel_safe, name = "mini_hello")]
fn hello(name: &str) -> String {
    format!("mini {name}")
}

// Exercises the SQL signature emitted for escaped names and planner literals.
#[pg_function(immutable, name = "mini\x5fseries", rows = 1_000, cost = 0x10)]
fn series(arg2: i32, _: i32) -> SetOf<i32> {
    SetOf::new(1..=arg2)
}
