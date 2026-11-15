# Streaming uploads

`App::upload(method, path, doc, UploadLimits, authorize, handler)` registers
an incremental request route. The handler receives `&Ctx` and
`&mut RequestBody`, which implements `Read`. `ctx.req.body` stays empty.
The global middleware chain and the route's required authorization callback
run before the handler reads bytes. Middleware on these routes must inspect
headers, cookies or the path, not a buffered JSON body.

```rust,ignore
app.upload(Method::Post, "/recordings/:id", "Upload a recording",
    UploadLimits::default(), bearer(&token), |ctx, body| {
        // Stream into a temporary file; publish it only after all bytes succeed.
        store_recording(ctx.param("id"), body)
    });
```

Defaults: 200 MiB, five minutes total, 15 seconds idle. The header parser
retains the server's header deadline; upload body time is separate. Ordinary
routes retain `App::max_body` (2 MiB by default). The input is delimited by
Content-Length (required, 411 if absent); oversized bodies get 413 without
allocation. Transfer-Encoding remains unsupported (501), and Expect is
refused (417). Upload responses close the connection, including refusals,
so unread bytes cannot become a following request.

Read incrementally with a fixed buffer or `io::copy`. Feed each chunk to
`sutegi_crypto::Sha256` if a digest is needed. `remaining()` reports bytes
left. Truncation/disconnect is a read error; idle/total timeout gives 408.
Read failures are sticky and incomplete 2xx responses become 400. Treat
storage as provisional until the entire reader and storage commit succeed.
Global draining closes a stalled upload at the shutdown deadline.

`App::service()` preserves the buffered in-process testing API. Streaming
routes require a real HTTP connection; test them as in `tests/uploads.rs`.
The route manifest marks `streaming_upload`, and the app capability map
advertises `streaming_uploads` when such routes are mounted.
