<!-- Generated from landing/src/Docs.svelte. Run: cd landing && pnpm docs:export -->

[← Listeners](listeners.md) · [Documentation index](../README.md) · [Operational endpoints →](ops.md)

# Tuning & limits

Every knob is a builder method with a documented default; nothing here is required to run. The HTTP defaults are conservative on purpose — raise `max_body` before you accept large uploads, and remember that in a thread-per-connection server the keep-alive settings are a capacity decision, not a cosmetic one.

```rust
use std::time::Duration;

App::new("api")
    .workers(16)                              // HTTP threads (WORKERS env wins)
    .max_body(8 * 1024 * 1024)                // 413 above this (default 2 MiB)
    .request_timeout(Some(Duration::from_secs(60)))   // per-socket read/write
    .limits(Limits {                          // …or replace the whole set
        max_body: 2 * 1024 * 1024,            // default 2 MiB
        max_header_bytes: 64 * 1024,          // default 64 KiB
        timeout: Some(Duration::from_secs(30)),
        header_timeout: Duration::from_secs(15),      // whole-request deadline
        keep_alive_idle: Duration::from_secs(5),      // idle keep-alive pins a thread
        keep_alive_max: 100,                          // requests per connection
    })
    .ws_config(WsConfig {                     // the realtime engine, if enabled
        shards: 0,                            // 0 = one per core
        max_frame: 1 << 20,
        max_message: 1 << 20,
        ping_interval: Duration::from_secs(30),
        idle_timeout: Duration::from_secs(75),        // must exceed ping_interval
        max_buffered: 1 << 20,                        // slow consumers get dropped
        max_connections: 1 << 20,
        max_connections_per_ip: 1024,
        raise_nofile: true,                           // lift RLIMIT_NOFILE at start
    })
    .serve()
```

| Setting | Default | What it bounds |
| --- | --- | --- |
| `workers` | 8 | HTTP threads; `WORKERS` in the environment overrides it. |
| `max_body` | 2 MiB | Request body size; `413` above it. |
| `max_header_bytes` | 64 KiB | Total header bytes; `413` above it. |
| `timeout` | 30 s | One stalled socket read or write. |
| `header_timeout` | 15 s | The whole request delivery, start to finish. |
| `keep_alive_idle` | 5 s | Idle time between requests on one connection. |
| `keep_alive_max` | 100 | Requests served per connection. |
| `ws.shards` | 0 (per core) | Reactor threads. |
| `ws.max_frame` / `max_message` | 1 MiB | One frame / an assembled message (close 1009 above). |
| `ws.ping_interval` / `idle_timeout` | 30 s / 75 s | Server ping cadence / drop a silent socket. |
| `ws.max_buffered` | 1 MiB | Per-connection outbound queue; a slower consumer is dropped. |
| `ws.max_connections` / `per_ip` | 1,048,576 / 1024 | Process-wide cap / single-source exhaustion. |
| `queue.visibility_timeout` | — | How long a claimed job may run before it becomes visible again. |
| `actor mailbox` | 1024 | Queued messages before `tell` fails with `Full`. |

Pool sizes are constructor arguments rather than builder methods: `Db::open_pool(path, n)` and `Pg::from_env(n)`. Size the Postgres pool with the advisory-lock note in mind — a held lock uses its own dedicated connection, outside the pool.

---

[← Listeners](listeners.md) · [Documentation index](../README.md) · [Operational endpoints →](ops.md)
