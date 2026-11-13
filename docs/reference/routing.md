<!-- Generated from landing/src/Docs.svelte. Run: cd landing && pnpm docs:export -->

[← The CLI](cli.md) · [Documentation index](../README.md) · [Requests & the Ctx →](requests.md)

# Routing

Register routes with `.get`, `.post`, `.put`, `.delete` — or `.route(method, …)` for anything else, including `PATCH`. Each takes a URL pattern, a short doc string — which shows up in `/__introspect`, so keep it meaningful — and a handler closure. Path parameters use `:name` and are read with `c.param("name")`; a trailing `*name` captures the rest of the path. Related routes can share a prefix and middleware via `.group`.

```rust
App::new("api")
    .get("/", "Health check", |_| "ok")
    .get("/todos/:id", "Show a todo", |c| {
        format!("todo #{}", c.param("id").unwrap_or("?"))
    })
    .post("/todos", "Create a todo", |c| {
        let body = c.json()?;                 // the parsed request body
        Ok::<_, Error>((201, body))
    })
    .put("/todos/:id", "Replace a todo", |_| status(204))
    .delete("/todos/:id", "Delete a todo", |_| status(204))
    // Any other verb goes through the generic .route(...):
    .route(Method::Patch, "/todos/:id", "Patch a todo", |_| status(204))
    // A rest pattern captures the remainder of the path:
    .get("/files/*path", "Serve a file", |c| c.param("path").unwrap_or("").to_string())
    // Group a prefix + shared middleware, then register routes inside it:
    .group("/admin", vec![mw(require_key)], |g| {
        g.get("/stats", "Admin stats", |_| "…")
    })
    .serve()
```

The router splits on segments after trimming leading and trailing slashes, so `/todos/1` and `/todos/1/` are the same route. Patterns are matched in registration order; a path that exists under another method answers `405` rather than `404`.

---

[← The CLI](cli.md) · [Documentation index](../README.md) · [Requests & the Ctx →](requests.md)
