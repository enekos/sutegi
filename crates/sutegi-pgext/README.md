# sutegi-pgext

Write PostgreSQL extension functions in Rust. The `#[pg_function]` attribute
exports a function to SQL. Use `Option<T>` for nullable values, `SetOf<T>` for
a set of rows, and `spi` to run parameterized SQL in the caller's transaction.

```rust
use sutegi_pgext::pg_function;

#[pg_function(immutable, parallel_safe)]
fn greet(name: &str) -> String {
    format!("Hello, {name}!")
}
```

The Sutegi CLI generates the extension's SQL and control files and links the
module against PostgreSQL's build system. Install the CLI with
`cargo install sutegi-cli --version 0.16.0 --locked`. Building an extension
requires Rust 1.100 or newer, PostgreSQL 14 or newer with server development
files, a C compiler, and Make.

This crate is experimental. Test an extension in a disposable database before
installing it on a production server. [Build, install, and SQL examples](https://github.com/enekos/sutegi/blob/v0.16.0/docs/PGEXT.md)
