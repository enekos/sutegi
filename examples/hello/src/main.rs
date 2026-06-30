//! The smallest useful sutegi app: routing + the built-in operational endpoints
//! (`/__health`, `/__ready`, `/__metrics`, `/__introspect`), with graceful
//! shutdown. No ORM, AI, queue, or derive compiled in.

use sutegi::prelude::*;

fn main() -> std::io::Result<()> {
    // Config layer: .env (if present) + process env, typed access.
    let cfg = Config::load();

    let app = App::new("hello")
        .workers(cfg.int("WORKERS", 8) as usize)
        .get("/", "Health check.", |_req, _p| text(200, "sutegi up"))
        .get("/hello/:name", "Greet someone.", |_req, p| {
            text(200, &format!("hello, {}", p.get("name").map(String::as_str).unwrap_or("world")))
        });

    let addr = std::env::args()
        .nth(1)
        .unwrap_or_else(|| format!("{}:{}", cfg.string("HOST", "0.0.0.0"), cfg.int("PORT", 8080)));
    println!("hello on http://{addr}");
    app.run_graceful(&addr)
}
