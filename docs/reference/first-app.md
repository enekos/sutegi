<!-- Generated from landing/src/Docs.svelte. Run: cd landing && pnpm docs:export -->

[← Installation](installation.md) · [Documentation index](../README.md) · [Feature flags →](features.md)

# Your first app

A handler is a closure that takes a single `&Ctx` and returns anything that implements `IntoResponse`. Register routes fluently on the `App`, then call `.serve()` — that one call reads `HOST`/`PORT`/`WORKERS` from the environment (or `argv[1]`) and, with the `graceful` feature, drains in-flight requests on `SIGTERM`.

```rust
use sutegi::prelude::*;

fn main() -> std::io::Result<()> {
    let ops_token = std::env::var("OPS_TOKEN").expect("set OPS_TOKEN");
    App::new("hello")
        .ops_guard(bearer(&ops_token))
        .get("/", "Health check", |_| "sutegi up")
        .get("/hello/:name", "Greet someone", |c| {
            format!("hi, {}", c.param("name").unwrap_or("world"))
        })
        .serve()   // reads HOST/PORT/WORKERS (or argv[1]); drains on SIGTERM
}
```

```bash
OPS_TOKEN=dev-token cargo run
# sutegi · hello on http://0.0.0.0:8080
#   ops: /__health /__ready /__metrics /__introspect

# From another terminal:
curl localhost:8080/hello/ada        # -> hi, ada
curl -H 'Authorization: Bearer dev-token' localhost:8080/__introspect
```

> [!NOTE]
> **You already have an agent surface**
>
> Notice you never wrote `/__introspect`. Every route you register is reflected there automatically — the foundation of the [agent surface](agents.md).

---

[← Installation](installation.md) · [Documentation index](../README.md) · [Feature flags →](features.md)
