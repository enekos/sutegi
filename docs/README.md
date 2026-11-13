<!-- Generated from landing/src/{Landing,Docs}.svelte and docs/*.md. Run: cd landing && pnpm docs:export -->

# Sutegi documentation

Read the full website content here on GitHub. Start with the [overview](OVERVIEW.md), follow the [Todo tutorial](TUTORIAL.md), or browse a reference topic below.

[Project README](../README.md) · [Website](https://enekos.github.io/sutegi/) · [Examples](../examples) · [Agent contract](../AGENTS.md)

## Prologue

- [Introduction](reference/introduction.md)
- [Philosophy & the bet](reference/philosophy.md)
- [Is it production-ready?](reference/honesty.md)
- [The workspace map](reference/crates.md)

## Getting started

- [Installation](reference/installation.md)
- [Your first app](reference/first-app.md)
- [Feature flags](reference/features.md)
- [Configuration](reference/configuration.md)
- [Directory & conventions](reference/layout.md)
- [The CLI](reference/cli.md)

## The basics

- [Routing](reference/routing.md)
- [Requests & the Ctx](reference/requests.md)
- [Responses & errors](reference/responses.md)
- [Middleware, CORS & guards](reference/middleware.md)
- [Validation](reference/validation.md)
- [Static files](reference/static.md)

## Database

- [Models](reference/models.md)
- [Relations](reference/relations.md)
- [The query builder](reference/queries.md)
- [Backends & capabilities](reference/backend.md)
- [Locks, isolation, bulk](reference/concurrency.md)
- [Advisory locks](reference/advisory.md)
- [JSON path queries](reference/json.md)
- [Full-text & hybrid search](reference/search.md)
- [Embeddings & vectors](reference/vectors.md)
- [Reactive queries](reference/reactive.md)
- [Migrations](reference/migrations.md)
- [The key/value store](reference/kv.md)

## Agents & realtime

- [The agent surface](reference/agents.md)
- [Defining tools](reference/tools.md)
- [Streaming & SSE](reference/streaming.md)
- [WebSockets](reference/websockets.md)
- [PubSub](reference/pubsub.md)
- [Channels & presence](reference/channels.md)
- [Queues](reference/queues.md)
- [Actors & supervision](reference/actors.md)

## Framework services

- [Authentication](reference/auth.md)
- [Sessions & CSRF](reference/sessions.md)
- [Mail](reference/mail.md)
- [File storage](reference/storage.md)
- [Event sourcing](reference/events.md)
- [Templates](reference/templates.md)
- [Collections](reference/collections.md)
- [Crypto primitives](reference/crypto.md)

## Architecture & operations

- [Hexagonal architecture](reference/hexagonal.md)
- [Testing](reference/testing.md)
- [The REPL](reference/repl.md)
- [Inside the server](reference/internals.md)
- [Listeners](reference/listeners.md)
- [Tuning & limits](reference/options.md)
- [Operational endpoints](reference/ops.md)
- [Deploying](reference/deploying.md)
- [Security posture](reference/security.md)

## In-depth guides

- [Actors & supervision](ACTORS.md)
- [Concurrency & throughput](CONCURRENCY.md)
- [Building clean, hexagonal apps in sutegi](HEXAGONAL.md)
- [JSON path queries](JSON.md)
- [Listeners — non-HTTP socket loops under the app lifecycle](LISTENERS.md)
- [Advisory locks](LOCKS.md)
- [Logging — one line format for the framework and the app](LOGGING.md)
- [Migrations](MIGRATIONS.md)
- [Reactive queries](REACTIVE.md)
- [Realtime: WebSockets, PubSub, Channels, Presence](REALTIME.md)
- [Full-text + hybrid search](SEARCH.md)

## Maintaining these docs

The overview, tutorial, reference pages, and this index are generated from the website source. Edit `landing/src/Landing.svelte` or `landing/src/Docs.svelte`, then run:

```bash
cd landing
pnpm install --frozen-lockfile
pnpm docs:export
pnpm docs:check
```

Commit the regenerated Markdown with the source change. The existing in-depth guides are maintained directly.
