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

## Recording storage and playback

`Storage::capabilities()` reports `streaming_write` and `seekable_read`.
`FsStorage` supports both. `put_reader(key, reader, length, content_type)`
uses constant memory, private unique temporary files, and publishes complete
bytes by atomic rename. Short/erroring reads leave existing bytes intact.
Use immutable recording keys: object bytes and the content-type sidecar are
separate files, not a transaction. Manage retention and disk quotas in the app.

`Storage::open(key)` returns metadata and a seekable reader over one opened
object. Pass that reader to `web::download(&ctx.req, object.reader,
&object.meta.content_type)` after authorization. The helper derives length
from the open reader, supports GET and HEAD, and advertises `Accept-Ranges`.
Single byte ranges return 206; unsatisfiable ranges return 416 with
`Content-Range: bytes */length`. Invalid/multiple ranges are ignored. HEAD
ignores Range and sends no bytes. If-Range falls back to a full response
because this helper does not supply a validator. These semantics follow
[RFC 9110](https://www.rfc-editor.org/rfc/rfc9110.html#name-range).

`Response::with_reader(reader, length)` is the lower-level primitive. The
server owns Content-Length, copies only that many bytes, and closes on a
short reader or write failure. HEAD does not invoke the reader.

S3 and database storage explicitly return unsupported for `put_reader` and
`open`; they do not silently collect a recording into memory. Existing
`get_reader` can still buffer on these backends. For large S3 recordings,
authorize the recording/key in Sutegi, issue a short-lived `presign_put`,
then verify the stored object's size/type before marking it complete. Use
`presign_get` for playback; the object store handles ranges. Configure bucket
CORS for the app's origin and keep tokens scoped to an immutable object key.
