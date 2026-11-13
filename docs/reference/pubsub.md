<!-- Generated from landing/src/Docs.svelte. Run: cd landing && pnpm docs:export -->

[← WebSockets](websockets.md) · [Documentation index](../README.md) · [Channels & presence →](channels.md)

# PubSub

Topic fan-out behind one `Broker` trait, so the layer above it — channels, presence, your own code — does not know whether it is talking to one pod or a fleet. `PubSub` is the in-process broker; `PgPubSub` (`pubsub-postgres`) is the same trait over Postgres `LISTEN`/`NOTIFY`.

```rust
use sutegi::prelude::*;      // Broker, BrokerExt, PubSub (+ PgPubSub)

// Single pod: an in-process broker. Clone-cheap — it shares one registry.
let broker = PubSub::new();

// Many pods: the same Broker trait over PG LISTEN/NOTIFY. Nothing else changes.
// let broker = PgPubSub::connect(&pg_config)?;   // sutegi_pg::Config

// Subscribe with a closure (BrokerExt::on); the id is for unsubscribe.
let id = broker.on("orders", |msg: &str| { handle(msg); });
broker.publish("orders", &Json::obj(vec![("id", Json::Int(42))]).to_string());
broker.unsubscribe("orders", id);

// Local delivery is synchronous and in subscription order. Cross-pod,
// PgPubSub echoes back its own instance id and skips it — so a publish is
// delivered exactly once locally and once per other pod.
// NOTIFY payloads cap at ~8 KB: try_publish returns the error instead of
// dropping it silently. Ship ids, not blobs; delivery is at-most-once.
```

`PgPubSub` uses one shared PG channel with topics inside a JSON envelope — immune to the 63-byte identifier truncation trap — plus a lazy publisher with one transparent retry and a listener that reconnects with capped backoff. Fan-out cost on the database is per *message*, not per subscriber: one connection per pod for listening, one for publishing.

> [!WARNING]
> **At-most-once, by design**
>
> Delivery is fire-and-forget (Phoenix’s contract too): a pod that is reconnecting misses messages sent meanwhile, and `NOTIFY` payloads cap at ~8 KB. If a message must survive, put it in a table (or the [event store](events.md)) and broadcast *that there is news*.

---

[← WebSockets](websockets.md) · [Documentation index](../README.md) · [Channels & presence →](channels.md)
