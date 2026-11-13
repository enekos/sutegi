<!-- Generated from landing/src/Docs.svelte. Run: cd landing && pnpm docs:export -->

[← Directory & conventions](layout.md) · [Documentation index](../README.md) · [Routing →](routing.md)

# The CLI

The `sutegi` binary is a scaffolder and a remote control. Scaffolding follows rigid conventions on purpose — one right shape per artifact — so an LLM can extend a sutegi app correctly with minimal context. `introspect` and `repl` need no source access at all: they speak the same agent contract an LLM would.

```bash
sutegi new blog              # scaffold an app in the conventional layout
sutegi make:model Post       # src/models/post.rs   (table: posts)
sutegi make:route health     # src/routes/health.rs with register(app)
sutegi introspect [addr]     # pretty-print a running app's /__introspect
sutegi repl 127.0.0.1:8080   # drive a running app over the agent contract
sutegi version | help
```

Migration verbs are *not* here — they live in your own binary, because they need your model set. See [Migrations](migrations.md).

---

[← Directory & conventions](layout.md) · [Documentation index](../README.md) · [Routing →](routing.md)
