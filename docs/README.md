<!-- Generated from landing/src/{Landing,Docs}.svelte and docs/*.md. Run: cd landing && pnpm docs:export -->

# Sutegi documentation

Read the full website content here on GitHub. Start with the [overview](OVERVIEW.md), follow the [Todo tutorial](TUTORIAL.md), or browse a reference topic below.

[Project README](../README.md) · [Website](https://enekos.github.io/sutegi/) · [Examples](../examples) · [Agent contract](../AGENTS.md)

## Prologue

- [Introduction](reference/introduction.md)
- [Philosophy & the bet](reference/philosophy.md)
- [Is it production-ready?](reference/honesty.md)

## Getting Started

- [Installation](reference/installation.md)
- [Your first app](reference/first-app.md)
- [Configuration](reference/configuration.md)
- [Feature flags](reference/features.md)
- [Directory structure](reference/layout.md)
- [Deployment](reference/deploying.md)

## Architecture Concepts

- [Request lifecycle](reference/internals.md)
- [The workspace map](reference/crates.md)
- [Hexagonal architecture](reference/hexagonal.md) · [deep dive](HEXAGONAL.md)
- [Listeners](reference/listeners.md) · [deep dive](LISTENERS.md)

## The Basics

- [Routing](reference/routing.md)
- [Middleware, CORS & guards](reference/middleware.md)
- [Requests & the Ctx](reference/requests.md)
- [Responses & errors](reference/responses.md)
- [Templates](reference/templates.md)
- [Sessions & CSRF](reference/sessions.md)
- [Validation](reference/validation.md)
- [Static files](reference/static.md)
- [Logging](reference/logging.md) · [deep dive](LOGGING.md)

## Digging Deeper

- [The CLI](reference/cli.md)
- [The REPL](reference/repl.md)
- [Collections](reference/collections.md)
- [Event sourcing](reference/events.md)
- [Mail](reference/mail.md)
- [Queues](reference/queues.md)
- [Actors & supervision](reference/actors.md) · [deep dive](ACTORS.md)
- [File storage](reference/storage.md)

## Security

- [Authentication](reference/auth.md)
- [Crypto primitives](reference/crypto.md)
- [Security posture](reference/security.md)

## Database

- [Backends & capabilities](reference/backend.md)
- [The query builder](reference/queries.md)
- [Migrations](reference/migrations.md) · [deep dive](MIGRATIONS.md)
- [Locks, isolation, bulk](reference/concurrency.md) · [deep dive](CONCURRENCY.md)
- [Advisory locks](reference/advisory.md) · [deep dive](LOCKS.md)
- [JSON path queries](reference/json.md) · [deep dive](JSON.md)
- [Full-text & hybrid search](reference/search.md) · [deep dive](SEARCH.md)
- [Embeddings & vectors](reference/vectors.md)
- [Reactive queries](reference/reactive.md) · [deep dive](REACTIVE.md)
- [The key/value store](reference/kv.md)

## ORM

- [Models](reference/models.md)
- [Relations](reference/relations.md)

## AI Agents

- [The agent surface](reference/agents.md)
- [Defining tools](reference/tools.md)

## Realtime

- [Calls: signalling, recordings and TURN](CALLS.md)
- [Streaming uploads and recording playback](UPLOADS.md)

- [Streaming & SSE](reference/streaming.md)
- [WebSockets](reference/websockets.md) · [deep dive](REALTIME.md)
- [PubSub](reference/pubsub.md) · [deep dive](REALTIME.md)
- [Channels & presence](reference/channels.md) · [deep dive](REALTIME.md)

## Testing

- [Testing](reference/testing.md)

## Operations

- [Tuning & limits](reference/options.md)
- [Operational endpoints](reference/ops.md)

## Deep dives

Long-form guides, maintained by hand and linked from the reference page they extend.

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

Commit the regenerated Markdown with the source change. The deep dives are maintained directly.

Diagrams are Mermaid. On the website, a `const dName` string in `Docs.svelte` is placed with `{@render diagram(dName, 'caption')}` and rendered in the browser; the export writes it as a ` ```mermaid ` fence, which GitHub renders. In the deep dives, write the fence directly.
