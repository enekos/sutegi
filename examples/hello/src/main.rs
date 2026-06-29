//! The smallest useful sutegi app: routing + the built-in operational endpoints
//! (`/__health`, `/__ready`, `/__metrics`, `/__introspect`), with graceful
//! shutdown. No ORM, AI, queue, or derive compiled in.

use sutegi::prelude::*;

fn main() -> std::io::Result<()> {
    let app = App::new("hello")
        .workers(sutegi::env_or("WORKERS", "8").parse().unwrap_or(8))
        .get("/", "Health check.", |_req, _p| text(200, "sutegi up"))
        .get("/hello/:name", "Greet someone.", |_req, p| {
            text(200, &format!("hello, {}", p.get("name").map(String::as_str).unwrap_or("world")))
        });

    let addr = std::env::args()
        .nth(1)
        .unwrap_or_else(|| format!("{}:{}", sutegi::env_or("HOST", "0.0.0.0"), sutegi::env_or("PORT", "8080")));
    println!("hello on http://{addr}");
    app.run_graceful(&addr)
}
