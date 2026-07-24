# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Crypto: two-way encryption. `sutegi-crypto` gains a ChaCha20-Poly1305 AEAD (RFC 8439) — `seal`/`open` encrypt with a fresh random nonce prepended to the output (`nonce ‖ ct ‖ tag`, +28 bytes), so nonce reuse is impossible by construction; `chacha20_poly1305_seal`/`_open` expose explicit nonce + AAD for counter schemes. ChaCha20 over AES deliberately: add-rotate-xor is constant-time in plain software, where AES table lookups leak through cache timing. The raw stream cipher and one-shot Poly1305 stay private. Plus `hkdf_sha256` (RFC 5869) to derive per-purpose subkeys from one master secret (`info` = `b"session-enc"`, `b"kv-enc"`, …) so signing and encryption keys never alias, and `constant_time_eq` now pipes its accumulator through `black_box` so the optimizer can't rewrite it into an early exit. SHA-256 is now incremental too — `Sha256::new()`/`update()`/`finalize()` hashes streams in constant memory (the old one-shot copied the whole input into a `Vec` first; it now wraps the streaming struct, and HMAC's inner/outer concatenations are gone), so S3 bodies and file reads no longer live in memory twice. Known-answer tested against the RFC 8439 §2.4.2/§2.5.2/§2.8.2 and RFC 5869 vectors; deterministic fuzz coverage for round-trip, bit-flip, AAD-mismatch, and truncation rejection. Still zero third-party dependencies.
- Actors (`sutegi-actors`, feature `actors`): actor processes and OTP-style supervision trees. An `Actor` owns its state on its own thread behind a typed, bounded mailbox (`tell` cast with explicit `TellError::Full` backpressure, `ask` call with timeout); a panic in `handle` crashes only the actor and is captured as `ExitReason::Crashed` — "let it crash" over `catch_unwind`, same posture as the ws reactor. `Supervisor` restarts children from their factory (clean state, never poisoned state) with OTP vocabulary: `Restart` policies (`Permanent`/`Transient`/`Temporary`), `Strategy` (`OneForOne`/`RestForOne`/`OneForAll`, dependents stopped in reverse and restarted in start order), restart intensity (default 3 per 5s) that fails the supervisor and stops all children when exceeded, and per-child fixed `backoff`. Stale exits from supervisor-stopped generations are discarded via a generation counter. `SupervisorHandle::child_ref(name)` is the `whereis` analog. Everything is observable through a clone-cheap `Registry` (state, mailbox depth, restart counts, last crash message), mountable at **`GET /__actors`** via `App::actors(registry)` (`ops_guard`-gated) — the Observer-lite half of the agent contract. 12 unit tests + 2 doctests.

## [0.6.0] - 2026-07-18

### Removed

- **`sutegi-ai` crate and the `ai` feature** (breaking). The crate was only a re-export alias (`sutegi::ai`) for the agent tool surface, which already lives in `sutegi-web` and is always compiled. `App::tool`/`stream_tool`, the `schema` helpers, `ToolCtx`, and `/__tools` are now documented as always-on core; the prelude already exported them. Drop `ai` from your feature list (it was in `default`); no code change is needed.
- **zumar fullstack coupling** (breaking). Removed the `sutegi-zumar` live bridge crate and the `zuc`-dependent CLI commands `dev`, `schema:zu`, and `new --fullstack`. The CLI is now `new` / `make:model` / `make:route` / `introspect` / `repl` and no longer requires an external `zuc` binary or a sibling `zumar` checkout.

### Added

- Channels (`sutegi-channels`, feature `channels`): Phoenix-style channels — the realtime identity feature. Topic join/leave with an auth callback, a self-describing JSON envelope (`{topic, event, ref, join_ref, payload}` — an object, not Phoenix's positional array, so `/__channels` alone teaches an agent the protocol), `push`/`broadcast`/`broadcast_from`/replies, per-membership `assigns`, heartbeats, rejoin-replaces semantics, `kick`, and `after_join` deferral. `App::channels(pattern, doc, hub)` mounts the WebSocket endpoint plus the **`/__channels` agent manifest** (patterns, docs, per-event payload schemas; `ops_guard`-gated). Broadcasts ride the pubsub `Broker` seam (one pre-encoded frame fanned via the ws reactor), so the same channel code is single-pod on the in-process broker and **cross-pod on `PgPubSub` with zero changes** — verified by two OS processes chatting through a real PostgreSQL. Ships a ~4 KB dependency-free browser client (`sutegi_channels::JS_CLIENT`: auto-rejoin with backoff, heartbeat liveness, ref-tracked receives). Example: `examples/chat`.
- Presence (feature `presence`): who's-online tracking on channels — `Presence::track/untrack/list`, `presence_state` to the tracked member and `presence_diff {joins, leaves}` to the room (Phoenix's client vocabulary). Cross-pod via per-pod state over the broker: incremental diffs, state-sync on first track, heartbeat re-publish with ~2.5× expiry that reports a silently-dead pod's members as leaves. Deliberately heartbeat-based rather than delta-CRDT; the trade-offs are documented honestly in the module docs. Untrack is automatic on leave/rejoin/disconnect.
- PubSub over PostgreSQL (`sutegi-pubsub` feature `postgres`, facade feature `pubsub-postgres`): `PgPubSub` implements the same `Broker` trait as the in-process `PubSub` over `LISTEN`/`NOTIFY` — one shared PG channel, topics inside a JSON envelope (immune to the 63-byte identifier truncation trap), synchronous local delivery with instance-id echo suppression, lazy publisher with one transparent retry, listener reconnect with capped backoff, and the ~8 KB `NOTIFY` cap surfaced via `try_publish`. `sutegi-pg` grows the underlying primitive: a dedicated `Listener` connection (`listen`/`unlisten`/`recv`, bounds-checked `NotificationResponse` parsing with a garbage fuzz suite, cross-thread `ListenerShutdown`).
- WebSockets (`sutegi-ws`, feature `ws`): `App::ws(pattern, doc, Ws::new().on_open(..).on_message(..).on_close(..))`. The HTTP side stays blocking thread-per-connection; an upgraded socket **detaches** (new `Body::Upgrade` in sutegi-http) into a sharded **kqueue/epoll reactor** — no async runtime, no futures, just `libc` poller syscalls — so an idle connection costs ~340 bytes of user-space RSS and **zero threads/CPU** (measured: 80,000 live sockets on a dev laptop at 0.0% idle CPU; broadcast enqueue of 80k shared-`Arc` frames in ~1.5ms; 5k-fleet delivery p50 15ms / max 30ms end-to-end). Strict RFC 6455 codec (masking required, minimal length encodings, control-frame rules, close-code validation, UTF-8 enforcement, u64-chunk unmasking) with a deterministic fuzz suite; per-connection ordering guaranteed (callbacks run inline on the shard); slow consumers dropped at a buffer cap; ping/idle sweeps; `RLIMIT_NOFILE` raised automatically. `Conn` handles are `Send + Sync + Clone` — broadcast by cloning one encoded frame `Arc` across a million queues. SHA-1 (handshake-only) added to sutegi-crypto with FIPS/RFC vectors. Examples: `ws-chat` (browser room) and `ws-load` (fleet stress harness).
- REPL (`sutegi-repl`, feature `repl`): a tinker-style interactive shell over the surfaces a sutegi app already exposes — routes, introspection, tool invocation (streaming tools print SSE frames live), raw HTTP through the app, and (with an attached `Backend`) raw SQL, a `where`-clause query DSL, KV, the event store, and the job queue. Works in-process (`Repl::new(app).db(db).run()`) or against a running server with no source access (`sutegi repl <addr>` via the CLI — the agent contract, driven by a human).
- Event sourcing (`sutegi-events`, feature `events`): append-only event store with optimistic concurrency (`Expected`), gap-free global log positions, `Aggregate` folding, and checkpointed `Projections` whose read-model writes commit in the same transaction as the checkpoint. Runs on SQLite or Postgres via the `Backend` seam; `append_tx` composes with caller-owned transactions.
- ORM: `Transactional` trait — run a closure inside a transaction on any capable backend (`Db`, `Pg`), receiving `&dyn Backend`. The `Backend` trait is now object-safe (typed `fetch`/`paginate_typed` are `Self: Sized`-gated).

## [0.5.1] - 2026-07-02

### Added

- Storage: unified `Storage` trait with local filesystem and database-blob backends, plus a pure-std S3 SigV4 presigner.
- Auth: full user system — PBKDF2 password hashing, `Users` store over any ORM backend, signed-cookie login sessions, route guards, and hashed API tokens.
- Mail: `sutegi-mail` email builder with RFC 2822/MIME rendering, built-in SMTP/sendmail/log/in-memory transports, and themed messages via the new template engine.
- Template engine: Blade-style templates with `{{ escaped }}` / `{!! raw !!}` interpolation, `@if`/`@else`, `@foreach`, and `@include` partials, rendered over JSON contexts.

## [0.5.0] - 2026-06-??

### Added

- Performance release (see commit `4f2655f`).

[0.5.1]: https://github.com/enekos/sutegi/compare/v0.5.0...v0.5.1
[0.5.0]: https://github.com/enekos/sutegi/releases/tag/v0.5.0
