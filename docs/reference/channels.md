<!-- Generated from landing/src/Docs.svelte. Run: cd landing && pnpm docs:export -->

[← PubSub](pubsub.md) · [Documentation index](../README.md) · [Queues →](queues.md)

# Channels & presence

Channels (`channels`) are the realtime identity feature: many topics multiplexed over one socket, with joins, replies, broadcasts, per-membership state — and an agent manifest. Phoenix is the design reference; the deltas are deliberate.

```rust
use sutegi::prelude::*;

let hub = Channels::new()
    .channel(
        Channel::new("room:*")
            .doc("A chat room. Join with a nick; messages fan out to the room.")
            .join_schema("A display name.", schema::object(
                vec![("nick", schema::string("Display name"))], &["nick"]))
            .on_join(|socket, payload| {
                let nick = payload.pointer("/nick").and_then(Json::as_str)
                    .ok_or_else(|| Json::str("nick required"))?;
                socket.assign("nick", Json::str(nick));
                Presence::track(socket, nick, Json::obj(vec![]));   // feature "presence"
                Ok(Json::Null)                        // rides the ok reply
            })
            .on("new_msg", |socket, payload| {
                socket.broadcast("new_msg", payload);  // all members, all pods
                Reply::None
            })
            .on_leave(|socket, _reason| {
                socket.broadcast_from("left", &Json::obj(vec![]));
            }),
    )
    // .broker(PgPubSub::connect(&pg_cfg)?)          // <- the only cross-pod change
    .check_origin(["https://app.example.com"])       // MUST set if cookies auth the socket
    .build();

App::new("chat")
    .channels("/channels", "The chat socket.", hub.clone())   // + the /__channels manifest
    .serve()?;

// From anywhere — an HTTP handler, a background thread, the REPL:
hub.broadcast("room:1", "announcement", &Json::str("maintenance at noon"));
```

Broadcasts ride the pubsub `Broker` seam, so **the same channel code is single-pod on the in-process broker and cross-pod on `PgPubSub` with zero changes** — verified by two OS processes chatting through a real PostgreSQL. A broadcast to a large room encodes the frame once and takes each reactor shard’s lock once, so the 80k-socket transport numbers apply unchanged.

## The wire protocol

```jsonc
// One JSON object per text frame — an object, not a positional array, so
// /__channels alone teaches an agent the protocol:
{"topic":"room:1","event":"new_msg","ref":"3","join_ref":"1","payload":{"body":"hi"}}

// Control events are stg:-prefixed and reserved:
//   stg:join  stg:leave  stg:reply {status:"ok"|"error", response}  stg:error  stg:close
// Heartbeats are a ref'd push on topic "stg", event "heartbeat".
```

`GET /__channels` returns the full manifest — envelope shape, control events, every channel’s pattern, docs and join/event schemas — which is enough for an agent to join and speak over a raw WebSocket with no client library. That manifest is part of the agent contract, next to `/__introspect` and `/__tools`.

```javascript
// sutegi_channels::JS_CLIENT is a bundled ~4 KB dependency-free client:
// auto-reconnect with capped backoff, automatic rejoin, heartbeat liveness.
const socket = new SutegiSocket("/channels");
socket.connect();
const room = socket.channel("room:1", {nick: "ada"});
room.on("new_msg", p => render(p));
room.join().receive("ok", () => {});
room.push("new_msg", {body: "hello"});
```

## Join lifecycle notes

- Inside `on_join` the member is **not admitted yet**: a broadcast there reaches the room but not the joiner, and a push lands before the join reply. Use `socket.after_join(…)` for welcome pushes — it runs after the ok reply is on the wire, and never runs if the join is refused.
- A join on an already-joined topic **replaces** the membership: the old one gets the leave callback with `LeaveReason::Rejoin`, and `assigns` start fresh.
- `assigns` are per-membership JSON state (`socket.assign` / `assign_get`), gone on leave or disconnect.

## Presence

The `presence` feature adds who’s-online tracking: `Presence::track` / `untrack` / `list`. The tracked member receives `presence_state` (the full view, after its join reply); the room receives `presence_diff {joins, leaves}` on every change. Untrack is automatic on leave, rejoin and disconnect, and several memberships may track the same key — one user, many tabs, each contributing a meta.

> [!WARNING]
> **Presence is heartbeat-based, not a CRDT**
>
> Each pod re-publishes its local state every `presence_heartbeat` (default 30 s) and expires pods silent for ~2.5×, reporting their members as leaves. So a crashed pod’s users can linger up to ~75 s, and partition conflicts resolve by expiry. That is the right trade for a “who’s online” sidebar; keep anything stronger in a table.

---

[← PubSub](pubsub.md) · [Documentation index](../README.md) · [Queues →](queues.md)
