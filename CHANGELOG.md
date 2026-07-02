# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

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
