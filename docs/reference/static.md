<!-- Generated from landing/src/Docs.svelte. Run: cd landing && pnpm docs:export -->

[← Validation](validation.md) · [Documentation index](../README.md) · [Models →](models.md)

# Static files

`App::static_dir(prefix, dir)` mounts a directory as a rest route. It is enough to serve a built SPA next to your API: point `/` at `dist` and register it last, since routes match in registration order.

```rust
App::new("site")
    .get("/api/health", "Health check", |_| "ok")   // API routes first…
    .static_dir("/assets", "public/assets")        // …then the file trees
    .static_dir("/", "dist")                        // dist/index.html is the site root
    .serve()

// Routes match in registration order, so register static_dir last.
// A directory (or the bare prefix) serves its index.html; traversal,
// dotfiles and backslashes are 404s, never a read outside the root.
```

Traversal attempts, dotfiles and backslashes are `404`s rather than reads outside the root; a directory (or the bare prefix) serves its `index.html`.

---

[← Validation](validation.md) · [Documentation index](../README.md) · [Models →](models.md)
