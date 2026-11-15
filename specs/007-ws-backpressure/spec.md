# 007 — Bounded WebSocket sends

Bound shard data queues by commands and bytes, and reserve per-connection
bytes until a frame is written or discarded. Preserve a separately bounded
control path so overload cannot suppress close. Add Queued/Full/Closed send
results and runtime statistics. Existing helpers close on Full. Introspection
advertises ws_backpressure when an app mounts WebSockets.

Acceptance: real-socket tests prove inbox bounds, reservation release, close
under saturation and stale-handle rejection. No new dependencies. Docs and
changelog accompany the change. Gate per constitution.
