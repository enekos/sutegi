<!-- Generated from landing/src/Docs.svelte. Run: cd landing && pnpm docs:export -->

[← Static files](static.md) · [Documentation index](../README.md) · [The CLI →](cli.md)

# Logging

`sutegi::log` is the log every sutegi crate writes through, and the one your app should use too. One event is one line on stderr: JSON when stderr is not a terminal, aligned text when it is. Stdout stays for command output. `key = value` keeps numbers and booleans typed in the JSON; `key = %value` formats anything `Display`. A field whose name ends in `password`, `secret`, `token`, `cookie`, `api_key` and the like is written as `[redacted]`, wherever it came from.

```rust
use sutegi::log;

log::info!("billing", "invoice sent", invoice = id, cents = total, to = email);
log::warn!("llm", "call failed", provider = "gemini", ms = elapsed, error = %err);
// {"ts":"…","level":"warn","target":"llm","msg":"call failed","provider":"gemini",
//  "ms":812,"error":"429 Too Many Requests","rid":"5f3a91c2000004d1"}

// Context every line inside the scope inherits:
let _scope = log::scope(&[("tenant", log::Value::Str(slug.into()))]);
log::annotate("user", user_id);

// One access line per request, a request id on every line it causes:
App::new("shop").access_log(AccessLog::new())

// In tests, collect instead of writing:
let (result, records) = log::capture(|| svc(request));
```

`SUTEGI_LOG` sets the level with per-target overrides (`info,queue=debug,http=warn`) and `SUTEGI_LOG_FORMAT` picks `json`, `text` or `auto`. `AccessLog` opens a scope with a `rid` before the ops guard runs, reuses a safe inbound `x-request-id`, and returns the id in the response. A queued job carries the dispatching request’s context, so the request and the job that later fails share one `rid`. With an `ops_guard` set, `GET /__logs` returns the last 512 events, filterable by level and by any field.

> [!TIP]
> **Deep dive:** [the long-form guide](../LOGGING.md), with every option and edge case.

---

[← Static files](static.md) · [Documentation index](../README.md) · [The CLI →](cli.md)
