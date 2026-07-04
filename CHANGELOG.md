# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

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
