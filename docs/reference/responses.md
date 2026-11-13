<!-- Generated from landing/src/Docs.svelte. Run: cd landing && pnpm docs:export -->

[← Requests & the Ctx](requests.md) · [Documentation index](../README.md) · [Middleware, CORS & guards →](middleware.md)

# Responses & errors

`IntoResponse` is implemented for the shapes you reach for most, so a handler can return a string, a `Json`, an explicit `(status, body)` pair, a bare status code, a `redirect`, or a `Result<T, Error>`. That last one is the important one: it means the `?` operator works throughout your handlers, and any `Error` maps to a correct HTTP response.

`Error` carries a status, a message, and optional per-field detail. The constructors (`Error::bad_request`, `unauthorized`, `forbidden`, `not_found`, `unprocessable`, `internal`, or `Error::new(status, msg)` for anything else) cover the common statuses, and `.with_fields(json)` attaches structured validation detail.

```rust
// A handler returns anything that is IntoResponse:
|_| "a string"                                  // 200 text/plain
|_| Json::obj(vec![("ok", Json::Bool(true))])   // 200 application/json
|_| (201, some_json)                            // an explicit (status, body)
|_| status(204)                                 // a bare status code
|_| no_content()                                // 204, spelled out
|_| html(200, "<h1>hi</h1>")                    // 200 text/html
|_| redirect("/login")                          // 302
|c| c.model::<Todo, Db>("id").map(|t| t.to_json())  // Result — ? just works

// Errors carry a status, a message, and optional per-field detail.
Err(Error::not_found("no such todo"))           // 404
Err(Error::unauthorized("log in first"))        // 401
Err(Error::unprocessable("bad shape")           // 422 with structured fields
    .with_fields(errors.to_json()))

// 4xx messages ARE the API and render verbatim. A 5xx does not:
// its message goes to stderr for the operator and the client gets
// {"error":"internal error"} — a SQL string or a disk path never leaks.
```

> [!NOTE]
> **5xx bodies are redacted**
>
> A `4xx` message is part of your API and renders verbatim. A `5xx` message is an internal detail — a SQL error carrying your schema, a subprocess’s stderr, a path on disk — so it is logged to stderr and the client gets `{"error":"internal error"}`. Anything the caller is meant to act on belongs in a 4xx.

---

[← Requests & the Ctx](requests.md) · [Documentation index](../README.md) · [Middleware, CORS & guards →](middleware.md)
