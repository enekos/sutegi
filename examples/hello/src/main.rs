//! The smallest useful sutegi app: routing + the built-in operational endpoints
//! (`/__health`, `/__ready`, `/__metrics`, `/__introspect`), with graceful
//! shutdown. No ORM, AI, queue, or derive compiled in.
//!
//! Handlers take a single [`Ctx`] and return anything that is `IntoResponse`
//! (here, a plain `String`). `serve()` reads `HOST`/`PORT`/`WORKERS` from the
//! environment (or `argv[1]`) and drains gracefully on SIGTERM.

use sutegi::prelude::*;

fn main() -> std::io::Result<()> {
    App::new("hello")
        .get("/", "Health check.", |_| "sutegi up")
        .get("/hello/:name", "Greet someone.", |c| {
            format!("hello, {}", c.param("name").unwrap_or("world"))
        })
        .serve()
}
