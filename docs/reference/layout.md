<!-- Generated from landing/src/Docs.svelte. Run: cd landing && pnpm docs:export -->

[← Configuration](configuration.md) · [Documentation index](../README.md) · [The CLI →](cli.md)

# Directory & conventions

A minimal app is a single `main.rs`. As it grows, the conventional layout separates your domain from the transport edges — and the [Backend trait](backend.md) plus the [hexagonal toolkit](hexagonal.md) keep business logic free of any HTTP or database detail. The CLI’s `make:*` generators follow the same conventions, which is also what lets an LLM extend the codebase correctly with minimal context.

> [!TIP]
> **Learn from the examples**
>
> The repo’s `examples/` directory is the fastest way in: `hello` (minimal), `todo` (every pillar in ~60 lines), `auth`, `events`, `kv`, `storage`, `hexagonal`, `redactor` (an agent tool service), `chat` (channels + presence, single-pod or cross-pod), `ws-chat`, `ws-load` and `http-load` (the stress harnesses behind the numbers quoted here).

---

[← Configuration](configuration.md) · [Documentation index](../README.md) · [The CLI →](cli.md)
