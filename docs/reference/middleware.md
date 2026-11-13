<!-- Generated from landing/src/Docs.svelte. Run: cd landing && pnpm docs:export -->

[← Responses & errors](responses.md) · [Documentation index](../README.md) · [Validation →](validation.md)

# Middleware, CORS & guards

Middleware comes in two flavours. **Before-middleware** (`Fn(&Request) -> Option<Response>`) runs ahead of the handler and can short-circuit by returning `Some(response)` — this is how auth guards and rate limits work. **After-middleware** (`Fn(&Request, Response) -> Response`, registered with `.after`) rewrites the outgoing response — this is how CORS and secure headers work. Wrap a before-middleware function with `mw(…)` to attach it to a group.

```rust
// Before-middleware returns Some(Response) to short-circuit, None to continue.
fn require_key(req: &Request) -> Option<Response> {
    match req.header("x-api-key") {
        Some("secret") => None,                       // allow
        _ => Some(status(401)),                       // block
    }
}

App::new("api")
    // App-wide middleware, in registration order:
    .middleware(logger())                             // log every request
    .middleware(rate_limit(100, Duration::from_secs(60)))
    // After-middleware rewrites the outgoing response:
    .after(secure_headers())
    .after(cors("https://app.example.com"))
    // A browser frontend on another origin that must send its session cookie
    // needs the credentialed pair — plain cors() is not enough:
    .after(cors_credentialed("https://app.example.com"))
    .middleware(cors_preflight_credentialed("https://app.example.com", "GET, POST", "content-type"))
    // Gate the whole /__ agent + ops surface (probes stay open):
    .ops_guard(|req| match req.header("authorization") {
        Some(t) if t == "Bearer ops-token" => None,
        _ => Some(status(401)),
    })
    // Or scope middleware to a group:
    .group("/admin", vec![mw(require_key)], |g| {
        g.get("/users", "List users", |_| "…")
    })
    .serve()
```

The batteries are built in: `logger()`, `rate_limit(max, per)`, `bearer`, `basic`, `cors`, `cors_preflight`, `cors_credentialed`, `cors_preflight_credentialed`, and `secure_headers`.

> [!WARNING]
> **A cookie-authenticated frontend needs the credentialed pair**
>
> Plain `cors` stamps `Access-Control-Allow-Origin` and nothing else — correct for a public API, quietly useless for a browser app on another origin that must send a session cookie. Three things have to be right and each fails silently alone: `Allow-Credentials` must be on the *real* response and not only the preflight; `Allow-Headers: *` stops being a wildcard once credentials are in play, so the first JSON `POST` is refused by a server that believes it allows everything; and a preflight without `Max-Age` is paid on every mutating call. The credentialed pair handles all three and is idempotent, so composing both halves cannot emit a duplicate `Allow-Origin` — which a browser rejects outright while `curl -i` shows a perfectly good `204`.

> [!NOTE]
> **`ops_guard` is the one that protects `/__`**
>
> Introspection and tool invocation are **closed by default**: without an `ops_guard`, they return `403`. Configure the guard and send the credential it expects. It runs ahead of the global middleware chain for every `/__`-mounted route (`/__introspect`, `/__metrics`, `/__tools*`, `/__channels`, `/__actors`, your own), while `/__health` and `/__ready` stay open so orchestrator probes need no credential. It is gated on the segments the router actually matches, not on the raw path string — see [Security posture](security.md) for why that distinction was a CVE-shaped bug.

---

[← Responses & errors](responses.md) · [Documentation index](../README.md) · [Validation →](validation.md)
