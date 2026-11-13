<!-- Generated from landing/src/Docs.svelte. Run: cd landing && pnpm docs:export -->

[← Channels & presence](channels.md) · [Documentation index](../README.md) · [Actors & supervision →](actors.md)

# Queues

Some work should not block the response. The durable queue (`queue`) runs over the `Backend` seam, so one jobs table and one set of SQL work on bundled SQLite (a single box) and on Postgres (many pods). Jobs survive a crash: the claim stamps a lease instead of deleting the row, so a dead worker’s job becomes visible again after the visibility timeout — at-least-once. Retries, delays, priorities and dedupe keys are columns, so a restart forgets nothing.

```rust
use std::sync::Arc;
use sutegi::queue::Queue;

let mut queue = Queue::new(db.clone());        // ANY Backend: Db or Pg
queue.migrate()?;                              // creates sutegi_jobs
queue.register("notify", |job| {
    let to = job.payload().get("to").and_then(Json::as_str).unwrap_or("");
    if job.is_last_attempt() { /* write a user-visible failure */ }
    job.heartbeat()?;                          // push the visibility window forward
    Ok(())                                     // Err -> retried with backoff
});

// Enqueue from a handler and return immediately:
queue.dispatch("notify", Json::obj(vec![("to", Json::str("a@b.com"))]))?;

// …or shape the dispatch: its own pool, one in flight per key, 3 tries.
queue.job("video.ingest", Json::obj(vec![("id", Json::str("abc"))]))
    .queue("video")
    .unique("yt:abc")          // hands back the live row's id instead of a duplicate
    .priority(10)              // runs ahead of older work in the same queue
    .max_attempts(3)
    .delay(Duration::from_secs(30))
    .dispatch()?;

let queue = Arc::new(queue);
let fast = Arc::clone(&queue).start(4);             // 4 workers on "default"
let slow = Arc::clone(&queue).start_on("video", 1); // 1 on the slow queue
// … later: fast.stop(); slow.stop();

// Ops:
queue.failed(20)?;                  // the dead letters
queue.retry(job_id, 3)?;            // revive one
queue.purge_failed(Duration::ZERO)?;
queue.stats()?; queue.stats_for("video")?;
queue.cross_pod();                  // true on Postgres — the honest answer
```

Claims are exclusive on both backends: `FOR UPDATE SKIP LOCKED` wherever `capabilities().skip_locked` says it exists, and on SQLite the serialized writer already provides it, since the second `UPDATE … RETURNING` no longer sees the claimed row. Postgres additionally makes that exclusivity *cross-pod*; `queue.cross_pod()` tells you which guarantee you actually have instead of letting the docs imply the stronger one.

- **Named queues with their own pools** — `.queue("video")` plus `start_on("video", 1)`, so a slow job class cannot starve a fast one.
- **Dedupe keys** — `.unique("yt:abc")` hands back the live row’s id instead of enqueueing a second copy, backed by a partial unique index that deliberately excludes dead letters, so a failure never owns a key forever.
- **A panicking handler is a failed job, not a lost worker** — the panic is caught, the row retries or dead-letters like any other, and the pool keeps going.
- **Dispatch wakes an idle worker** over a condvar instead of making it wait out the poll interval; the interval stays as the safety net for delayed jobs and work enqueued by another pod.
- `JobCtx` gives a handler what it needs to behave well: `is_last_attempt()` to tell a retryable blip from a terminal failure *before* writing a user-visible error, `heartbeat()` to outlive the visibility timeout, and `should_stop()` for loops.

Timestamps are epoch milliseconds supplied by the caller rather than SQL `now()` — which is what lets one statement work in both dialects, and makes schedules testable without sleeping. Tuning: `visibility_timeout`, `poll_interval`, `retry_backoff`.

---

[← Channels & presence](channels.md) · [Documentation index](../README.md) · [Actors & supervision →](actors.md)
