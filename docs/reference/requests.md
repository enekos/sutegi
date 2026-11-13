<!-- Generated from landing/src/Docs.svelte. Run: cd landing && pnpm docs:export -->

[← Routing](routing.md) · [Documentation index](../README.md) · [Responses & errors →](responses.md)

# Requests & the Ctx

The `&Ctx` is your window into a request and into shared application state. Path parameters, the query string, headers, the peer address and the raw body are all reachable from it, and the body can be parsed as JSON (`c.json()`) or a form (`c.form()`). State registered once with `.state(value)` is retrieved by type with `c.state::<T>()` (or `try_state` if it may be absent) — and, with the ORM feature, the database handle with `c.db::<Db>()`.

```rust
.post("/search", "Search", |c| -> Result<Json, Error> {
    // Path & query
    let id    = c.param("id");                    // Option<&str>
    let query = c.query();                        // BTreeMap<String, String> of ?a=b
    let page  = c.query().get("page").cloned();

    // Headers, the raw body, the peer
    let auth  = c.header("authorization");        // Option<&str>, case-insensitive
    let bytes = &c.req.body;                      // &[u8] — the raw request body
    let peer  = c.req.peer.as_deref();            // Option<&str> — the socket address

    // Bodies, parsed
    let body: Json = c.json()?;                    // application/json
    let form       = c.form();                     // application/x-www-form-urlencoded

    // Shared application state, registered once with .state(...)
    let db  = c.db::<Db>();                        // the pooled DB handle
    let cfg = c.state::<AppConfig>();              // panics if not registered
    let opt = c.try_state::<Mailer>();             // Option<&Mailer>
    Ok(body)
})
```

State is keyed by type, one value per type: registering the same type twice replaces it. Wrap a handle in a newtype when you need two of the same kind.

---

[← Routing](routing.md) · [Documentation index](../README.md) · [Responses & errors →](responses.md)
