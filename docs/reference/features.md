<!-- Generated from landing/src/Docs.svelte. Run: cd landing && pnpm docs:export -->

[← Your first app](first-app.md) · [Documentation index](../README.md) · [Configuration →](configuration.md)

# Feature flags

sutegi’s pillars are Cargo features on the facade crate. Only `json`, `http`, `web` and `crypto` are compiled unconditionally; everything else is opt-in, so your binary contains exactly the surface you use. The defaults are `derive`, `orm`, `validate`.

| Feature | Default | Gives you |
| --- | --- | --- |
| `orm` | yes | Schema, query builder, `Backend` trait, migrations, KV, JSON paths, search, embeddings, watchers. |
| `derive` | yes | `#[derive(Model)]` / `#[derive(Validate)]` (build-time only). |
| `validate` | yes | Request + tool validation, `Ctx::validate`/`validated*`. |
| `sqlite` |  | The bundled single-node backend, `Db`. |
| `postgres` |  | The pure-std multi-pod backend, `Pg`. |
| `graceful` |  | SIGTERM/SIGINT draining for rolling deploys. |
| `queue` |  | The durable job queue (SQLite *or* Postgres). |
| `events` |  | Event store, aggregates, projections. |
| `ws` |  | `App::ws` on the kqueue/epoll reactor. |
| `pubsub` / `pubsub-postgres` |  | The in-process broker / cross-pod `PgPubSub`. |
| `channels` / `presence` |  | Phoenix-style channels + `/__channels` / who’s-online tracking. |
| `actors` |  | Actors, supervision trees, `/__actors`. |
| `session` / `auth` / `auth-mail` |  | Cookies + CSRF / the user system / verification & reset flows. |
| `mail` / `template` |  | The mailer (pulls `template`) / the Blade-lite engine. |
| `storage` / `storage-db` |  | Local fs + S3/R2 objects / blobs over the `Backend` seam. |
| `hexagon` |  | Ports & adapters primitives. |
| `repl` |  | The tinker-style shell (data commands light up with `orm`). |

Features compose the obvious way: `auth` implies `session` + `orm`, `channels` implies `ws` + `pubsub`, `mail` implies `template`. Turn everything off with `default-features = false` for a minimal HTTP service, then add back precisely what a given deployment needs.

> [!NOTE]
> **There is no `ai` feature any more**
>
> It was removed in 0.6.0. The tool surface — `App::tool`/`stream_tool`, the `schema` helpers, `ToolCtx`, `/__tools` — lives in `sutegi-web` and is always compiled. Drop `ai` from your feature list; no code change is needed.

---

[← Your first app](first-app.md) · [Documentation index](../README.md) · [Configuration →](configuration.md)
