<!-- Generated from landing/src/Docs.svelte. Run: cd landing && pnpm docs:export -->

[← The workspace map](crates.md) · [Documentation index](../README.md) · [Your first app →](first-app.md)

# Installation

Every sutegi app is an ordinary Rust binary — `cargo new`, add the crate, write a `main`. There is no separate runtime to install and nothing trailing behind the binary at run time. Choose the feature pillars you want; the HTTP core (`json` + `http` + `web` + `crypto`) is always present, and so is the agent tool surface.

```text
# The defaults — derive + orm + validate — suit a typical app.
cargo add sutegi

# In Cargo.toml, pick the pillars you want. The HTTP core and the agent
# tool surface are always present; there is no "ai" feature to enable.
[dependencies]
# Single-node app: bundled SQLite + graceful shutdown.
sutegi = { version = "0.13", features = ["sqlite", "graceful"] }

# Multi-pod app: Postgres, the durable queue, cross-pod realtime.
# sutegi = { version = "0.13", features = [
#     "postgres", "queue", "channels", "pubsub-postgres", "graceful" ] }

# Nothing but the HTTP core — ~480 KB, no ORM, no derives:
# sutegi = { version = "0.13", default-features = false }
```

> [!TIP]
> **Scaffolding**
>
> In a hurry? `sutegi new todo-api` lays down the conventional project, and `sutegi make:model Todo` / `sutegi make:route todos` add pieces afterwards.

---

[← The workspace map](crates.md) · [Documentation index](../README.md) · [Your first app →](first-app.md)
