//! A second sutegi-pgext extension. Its Rust function is also named
//! `hello`, like pgext-demo's, so both define `sutegi_pgext_fn_hello`,
//! `sx_invoke` and a copy of Rust's std: loading both into one backend shows
//! the symbols stay private to each module.

use sutegi_pgext::pg_function;

/// The mini greeting.
#[pg_function(immutable, parallel_safe, name = "mini_hello")]
fn hello(name: &str) -> String {
    format!("mini {name}")
}
