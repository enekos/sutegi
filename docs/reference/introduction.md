<!-- Generated from landing/src/Docs.svelte. Run: cd landing && pnpm docs:export -->

[Documentation index](../README.md) · [Philosophy & the bet →](philosophy.md)

# Introduction

**sutegi** — Basque for *forge* — is a batteries-included web framework for Rust with a single unusual constraint: it has **zero third-party runtime dependencies**. No tokio, no serde, no hyper, not even a Postgres driver crate. The HTTP/1.1 parser, the JSON codec, the router, the ORM, the Postgres wire protocol, the WebSocket reactor, the crypto primitives and the agent tool layer are all original code, built on the standard library.

If you have used Laravel, the shape will feel familiar: an expressive router, an Eloquent-style ORM with relations and migrations, first-class validation, a durable queue, mail, sessions, storage, and a CLI that scaffolds the conventional pieces. If you have used Phoenix, the realtime half will: channels, presence, and pushed live queries. The difference is what sits underneath — nothing you did not choose to compile in — and one addition neither had to think about: **an AI agent is a first-class user of your app**, able to discover and drive every route, model, and tool over plain JSON with no SDK.

## How to read these docs

They are written to be read in order the first time and grepped after. The path is deliberate:

- **Getting started** — install, boot a first app, and learn how features, configuration and the CLI work.
- **The basics** — routing, the request context, responses, middleware, and validation: the everyday request loop.
- **Database** — models and relations, the query builder, the one `Backend` trait behind SQLite and Postgres, then the deeper seams: locks, JSON paths, search, embeddings, live queries, migrations, KV.
- **Agents & realtime** — the introspection surface, tools, streaming, WebSockets, pubsub, channels, the durable queue, and actors.
- **Framework services** — auth, sessions, mail, storage, event sourcing, templates, collections, crypto: opt-in pillars you reach for as needed.
- **Architecture & operations** — hexagonal structure, testing, the REPL, what the server actually does per request, every tuning knob, the operational endpoints, deploying, and an honest security posture.

> [!NOTE]
> **Start here**
>
> New to sutegi? Read [Philosophy](philosophy.md) and [Is it production-ready?](honesty.md), then jump to [Your first app](first-app.md). The whole “basics + database” arc is one continuous Todo example you can build as you go.

---

[Documentation index](../README.md) · [Philosophy & the bet →](philosophy.md)
