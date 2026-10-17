# Logging — one line format for the framework and the app

`sutegi::log` (crate `sutegi-log`, std only) is the log every sutegi crate
writes through and the one your app should use too. One event is one line on
stderr: JSON when stderr is not a terminal, aligned text when it is. Stdout
stays for command output (`migrate:*`, banners).

```rust
use sutegi::log;

log::info!("billing", "invoice sent", invoice = id, cents = total, to = email);
log::warn!("llm", "call failed", provider = "gemini", ms = elapsed, error = %err);
```

```json
{"ts":"2026-10-17T08:01:51.009Z","level":"warn","target":"llm","msg":"call failed","provider":"gemini","ms":812,"error":"429 Too Many Requests","rid":"5f3a91c2000004d1","ws":"acme"}
```

- `key = value` keeps numbers, booleans and `None` typed in JSON (`"ms":812`,
  not `"ms":"812"`). Any `ToValue` works: strings, integers, floats, `bool`,
  `Option<T>`.
- `key = %value` formats anything `Display` (errors, ids, paths).
- Values are clipped at 4 KiB. A field whose name ends in `password`,
  `passwd`, `secret`, `token`, `authorization`, `cookie`, `api_key` or
  `apikey` is written as `[redacted]`, wherever it came from.

## Configuration

| Variable | Default | Meaning |
|---|---|---|
| `SUTEGI_LOG` | `info` | default level, then per-target overrides: `info,queue=debug,http=warn` |
| `SUTEGI_LOG_FORMAT` | `auto` | `json`, `text`, or `auto` (text on a terminal, JSON otherwise) |

A target filter matches the target and its dotted children (`llm` covers
`llm.gemini`); the longest match wins. An app with its own settings calls
`log::init(Config::from_env().level(..).format(..))` once at startup, which
also routes panics through the log as `target=panic` lines.

## Context — the fields every line inherits

```rust
let _scope = log::scope(&[("tenant", log::Value::Str(slug.into()))]);
log::annotate("user", user_id);           // adds to the innermost open scope
log::info!("import", "rows written", n = 412);   // carries tenant + user
```

Context is per thread and is removed when the `Scope` guard drops. Event
fields win over context fields of the same name; inner scopes win over outer.

### Requests

```rust
App::new("shop").access_log(AccessLog::new())
```

opens a scope with a `rid` before anything else runs (ops guard, middleware,
handler, after-hooks), writes one `http request` line at the end with
`method path status ms ip ua`, and returns the id in `x-request-id`. A safe
inbound `x-request-id` (≤ 64 chars of `[A-Za-z0-9._-]`) is reused so an edge
proxy's id carries through; `trust_request_id(false)` turns that off.
`/__health` and `/__ready` log at debug unless they fail; replace that with
`.quiet(|path| …)`. Behind a proxy, pass `.client_ip(|req| …)` so `ip` is the
client and not the proxy. 5xx responses log at error, and the
`request failed` line from an `Error` carries the same `rid`.

### Jobs

`Queue::dispatch` stores the dispatching thread's context in the job row
(`sutegi_jobs.trace`), and the worker reopens it around the handler with
`job`, `job_id` and `attempt` added. A request that enqueues work and the job
that later fails share one `rid`. Every attempt is logged: success at debug
with `ms`, a failure that will retry at warn, the last one at error.

`Queue::migrate` adds the `trace` column to a table created by an older
sutegi. A queue that skipped `migrate` cannot store the trace of a dispatch
made inside a scope.

## Reading the log from an agent

With an `ops_guard` set, `GET /__logs` returns the last 512 events, oldest
first, as a JSON array of the same objects the JSON format writes. Filters:
`level=warn` (minimum), `limit=50`, and any field by name (`rid=…`,
`job=…`, `target=…`). Without an ops guard the endpoint does not exist,
because log lines carry user data.

`/__metrics` gains `sutegi_log_events_total{level="…"}`.

## Tests

```rust
let (result, records) = log::capture(|| svc(request));
assert_eq!(records[0].field("status"), Some(&log::Value::Uint(200)));
```

`capture` collects this thread's events at every level instead of writing
them, so tests can assert on logs without a global sink.
