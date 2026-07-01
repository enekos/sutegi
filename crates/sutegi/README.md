# sutegi

A **zero-dependency, agent-native web framework for Rust** — batteries-included
ergonomics, built from `std` up. No tokio, no serde, no hyper.

```rust
use sutegi::prelude::*;

fn main() -> std::io::Result<()> {
    App::new("hello")
        .get("/", "Health check", |_req, _p| text(200, "sutegi up"))
        .run_graceful("0.0.0.0:8080")
}
```

- **Zero runtime dependencies.** Hand-built HTTP/1.1 server, JSON, and router on
  `std`. Core binary ~362 KB.
- **Agent-native.** `GET /__introspect` (full app surface), `GET /__tools`
  (LLM tool-calling manifest), `POST /__tools/:name[/stream]` (invoke / SSE).
- **Composable.** Every pillar is an opt-in feature: `orm`, `derive`,
  `validate`, `ai`, `queue` (default) + `sqlite`, `graceful`, `hex`.

```toml
# minimal HTTP service:
sutegi = { version = "0.1", default-features = false }
# with the runnable SQLite ORM + graceful shutdown:
sutegi = { version = "0.1", features = ["sqlite", "graceful"] }
```

Pillars: routing & middleware · ORM + query builder + `#[derive(Model)]` ·
validation · streaming/SSE · background jobs · hexagonal toolkit · built-in
health/readiness/metrics + graceful shutdown.

Full docs, examples, and the agent contract:
<https://github.com/enekos/sutegi>

MIT © 2026 Eneko Sarasola
