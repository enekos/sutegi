# Sutegi

Sutegi is a Rust web framework for building HTTP services and exposing application
tools to AI agents. Its HTTP/1.1 server, router, and JSON codec use the standard
library. An application can publish its route and tool descriptions as JSON
through guarded discovery endpoints.

## Quickstart

With Rust 1.100 or newer, add Sutegi to a binary crate.

```toml
[dependencies]
sutegi = "0.16"
```

```rust
use sutegi::prelude::*;

fn main() -> std::io::Result<()> {
    App::new("hello")
        .get("/", "Greet the caller", |_| "Hello from Sutegi!")
        .serve()
}
```

Run `cargo run -- 127.0.0.1:8080` and open `http://127.0.0.1:8080/`.

The default features are `derive`, `orm`, and `validate`. Enable `sqlite` or
`postgres` to run database queries, `queue` for durable background jobs, or `ws`
for WebSockets. For a service that only needs the HTTP core, set
`default-features = false`.

To write a function that runs inside PostgreSQL, use the companion
[sutegi-pgext crate](https://crates.io/crates/sutegi-pgext).

[Tutorial](https://github.com/enekos/sutegi/blob/v0.16.0/docs/TUTORIAL.md) ·
[Feature reference](https://docs.rs/sutegi/0.16.0/sutegi/) ·
[Agent API](https://github.com/enekos/sutegi/blob/v0.16.0/AGENTS.md)

MIT © 2026 Eneko Sarasola
