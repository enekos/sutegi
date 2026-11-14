<!-- Generated from landing/src/Docs.svelte. Run: cd landing && pnpm docs:export -->

[← Defining tools](tools.md) · [Documentation index](../README.md) · [WebSockets →](websockets.md)

# Streaming & SSE

Because the server is blocking and thread-per-connection, streaming is trivial and naturally backpressured — there is no executor to fight. `sse(producer)` gives the producer an `SseSink` with `data`, `event`, and `comment`; each frame is flushed immediately. `stream(status, content_type, producer)` is the raw-bytes equivalent for NDJSON and large exports.

```rust
.get("/stream", "SSE demo", |_| sse(|sink| {
    for token in answer().split(' ') {
        sink.data(token)?;          // each frame is flushed immediately
    }
    sink.comment("keep-alive")?;    // a ':'-prefixed no-op frame
    sink.event("done", "{}")        // a named event
}))

// Raw byte streams — NDJSON, large exports — take the same shape:
.get("/export", "Stream rows", |_| stream(200, "application/x-ndjson", |sink| {
    for row in rows() { sink.write_str(&format!("{}\n", row.to_json()))?; }
    Ok(())
}))

// Regular responses use keep-alive; streams are close-framed by design.
```

```mermaid
sequenceDiagram
  participant C as Client
  participant W as pool worker
  participant H as handler
  participant P as SSE producer
  C->>W: request to an sse() route or POST /__tools/name/stream
  W->>H: run the handler
  alt stream tool arguments rejected
    H-->>C: 4xx JSON error, no stream opened
  else accepted
    H-->>W: 200 response wrapping the producer
    W->>W: after-middleware, access log, metrics
    W->>C: head with text/event-stream and connection close
    W->>P: run the producer on the same thread
    loop each chunk
      P->>C: data frame, flushed at once
    end
    P->>C: event done (by convention)
    W->>C: close the socket, worker freed
  end
  Note over P,C: once the head is out the status is fixed, a producer error or panic just closes the socket
```

*A stream fixes its 200 status before the producer runs; frames are then flushed one by one on the same worker until the socket closes.*

It is the same transport that carries live LLM tokens back to a UI and the one `.stream_tool` rides on. Regular responses use keep-alive; streams are close-framed by design, which is valid HTTP/1.1 and needs no chunked encoding. Behind nginx, set `proxy_buffering off` — the `ontzi` config already does.

---

[← Defining tools](tools.md) · [Documentation index](../README.md) · [WebSockets →](websockets.md)
