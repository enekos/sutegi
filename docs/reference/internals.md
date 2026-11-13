<!-- Generated from landing/src/Docs.svelte. Run: cd landing && pnpm docs:export -->

[← The REPL](repl.md) · [Documentation index](../README.md) · [Listeners →](listeners.md)

# Inside the server

There is no executor and no hidden machinery: a fixed thread pool accepts connections and each worker handles one connection with blocking I/O. That is the whole model, and it is why streaming is trivial, backpressure is free, and a stack trace points at your handler rather than at a runtime.

```text
accept()  ->  a fixed thread pool (WORKERS, default 8)
              |
              |  one connection per worker thread, blocking I/O
              v
   parse request line + headers   (bounded: max_header_bytes, header_timeout)
              |
              v
   ops guard  ->  global middleware  ->  route match  ->  group middleware
              |                                              |
              |                                              v
              |                                        handler(&Ctx)
              |                                              |
              v                                              v
        after-middleware  <---------------------------  IntoResponse
              |
              v
   write response  ->  keep-alive (keep_alive_idle / keep_alive_max) or close
                    ->  or detach: Body::Upgrade hands the socket to the
                        ws reactor and frees the worker immediately

// A handler panic is caught per request and becomes a 500 — one bad request
// never takes a worker down. (This needs panic = "unwind", the Rust default —
// an app that sets panic = "abort" gives that isolation up.)
// /__metrics counts requests total, in-flight, and by status class.
```

The consequences are worth internalising, because they are what you tune:

- **A connection costs a thread while it is open.** Keep-alive is therefore capped twice — `keep_alive_idle` (default 5 s, deliberately much shorter than the socket timeout) and `keep_alive_max` (default 100 requests) — so an idle client cannot pin a worker indefinitely.
- **Reading a request is bounded twice too.** `timeout` bounds a single stalled `recv`; `header_timeout` bounds the *total* time a peer may take to deliver request line, headers and body, so a byte-per-interval dripper cannot hold a worker (slowloris, CWE-400).
- **Panics are isolated per request.** A handler panic becomes a `500` rather than a downed worker. That rests on `catch_unwind`, so keep `panic = "unwind"` (the Rust default) in your release profile.
- **WebSockets escape the model entirely.** An upgrade returns `Body::Upgrade`, the reactor adopts the socket, and the worker is released — which is how one process holds 80k connections on eight threads.
- **The release profile is tuned for size**: `opt-level = "z"`, LTO, one codegen unit, stripped symbols. That is where ~480 KB comes from; the unwind tables that panic isolation needs are about 10% of it.

Ordering is fixed and worth knowing when you place a guard: ops guard → global middleware → `/__metrics` and `/__introspect` → route match → group middleware → handler → after-middleware. The two probes are matched before everything, so an orchestrator never needs a credential. Metrics are recorded on every path, including short-circuits.

---

[← The REPL](repl.md) · [Documentation index](../README.md) · [Listeners →](listeners.md)
