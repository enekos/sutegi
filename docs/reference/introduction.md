<!-- Generated from landing/src/Docs.svelte. Run: cd landing && pnpm docs:export -->

[Documentation index](../README.md) · [Philosophy & the bet →](philosophy.md)

# Introduction

**sutegi** — Basque for *forge* — is a batteries-included web framework for Rust with a single unusual constraint: it has **zero third-party runtime dependencies**. No tokio, no serde, no hyper, not even a Postgres driver crate. The HTTP/1.1 parser, the JSON codec, the router, the ORM, the Postgres wire protocol, the WebSocket reactor, the crypto primitives and the agent tool layer are all original code, built on the standard library.

If you have used Laravel, the shape will feel familiar: an expressive router, an Eloquent-style ORM with relations and migrations, first-class validation, a durable queue, mail, sessions, storage, and a CLI that scaffolds the conventional pieces. If you have used Phoenix, the realtime half will: channels, presence, and pushed live queries. The difference is what sits underneath — nothing you did not choose to compile in — and one addition neither had to think about: **an AI agent is a first-class user of your app**, able to discover and drive every route, model, and tool over plain JSON with no SDK.

## How to read these docs

They are written to be read in order the first time and grepped after. The path is deliberate:

- **Getting Started** — install, boot a first app, configure it, pick features, and ship it.
- **Architecture Concepts** — what happens to a request, how the crates fit together, hexagonal structure, and non-HTTP listeners.
- **The Basics** — routing, middleware, the request context, responses, templates, sessions, validation and logging: the everyday request loop.
- **Digging Deeper** — the CLI and REPL, collections, event sourcing, mail, the durable queue, actors and file storage.
- **Security** — authentication, the crypto primitives, and an honest security posture.
- **Database** and **ORM** — the one `Backend` trait behind SQLite and Postgres, the query builder, migrations, then the deeper seams: locks, JSON paths, search, embeddings, live queries, KV; models and relations on top.
- **AI Agents** and **Realtime** — the introspection surface and tools, then streaming, WebSockets, pubsub and channels.
- **Testing** and **Operations** — testing in-process, every tuning knob, and the operational endpoints.

> [!NOTE]
> **Start here**
>
> New to sutegi? Read [Philosophy](philosophy.md) and [Is it production-ready?](honesty.md), then jump to [Your first app](first-app.md). The whole “basics + database” arc is one continuous Todo example you can build as you go.

---

[Documentation index](../README.md) · [Philosophy & the bet →](philosophy.md)
