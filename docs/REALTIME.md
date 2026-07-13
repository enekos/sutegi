# Realtime: WebSockets, PubSub, Channels, Presence

The realtime stack is four layers, each a cargo feature, each usable alone:

| layer | crate | feature | what it gives you |
|---|---|---|---|
| Transport | `sutegi-ws` | `ws` | RFC 6455 sockets on a sharded kqueue/epoll reactor — ~340 B/idle conn, no async runtime |
| PubSub | `sutegi-pubsub` | `pubsub` (+`pubsub-postgres`) | topic fan-out behind one `Broker` trait: in-process, or cross-pod over PG `LISTEN/NOTIFY` |
| Channels | `sutegi-channels` | `channels` | topics/joins/replies/broadcasts multiplexed over one socket + the `/__channels` agent manifest |
| Presence | `sutegi-channels` | `presence` | who's-online per topic, synced across pods, heartbeat-expired |

Phoenix is the design reference throughout; the deltas are deliberate and
documented (object envelope instead of positional array, heartbeat presence
instead of delta-CRDT, agent introspection as a first-class surface).

## Channels in five minutes

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
                Ok(Json::Null)                       // rides the ok reply
            })
            .on("new_msg", |socket, payload| {
                socket.broadcast("new_msg", payload); // all members, all pods
                Reply::None
            })
            .on_leave(|socket, _reason| {
                socket.broadcast_from("left", &Json::obj(vec![]));
            }),
    )
    // .broker(PgPubSub::connect(&pg_cfg)?)   // ← cross-pod; omit = single pod
    // .check_origin(["https://app.example.com"])  // MUST set if cookies auth the socket
    .build();

App::new("chat")
    .channels("/channels", "The chat socket.", hub.clone())
    .serve()?;

// From anywhere — HTTP handlers, background threads, the REPL:
hub.broadcast("room:1", "announcement", &Json::str("maintenance at noon"));
```

Handlers run inline on reactor shards (the `sutegi-ws` contract): keep them
CPU-quick, push blocking work to your own threads, answer later via the
cloneable `Socket`/`ChannelHub` handles.

### The wire protocol

One JSON object per text frame:

```json
{"topic":"room:1","event":"new_msg","ref":"3","join_ref":"1","payload":{"body":"hi"}}
```

Control events are `stg:`-prefixed and reserved: `stg:join`, `stg:leave`,
`stg:reply` (payload `{status: "ok"|"error", response}`), `stg:error`,
`stg:close`. Heartbeats are a ref'd push on topic `stg`, event `heartbeat`.
`GET /__channels` returns the full manifest — envelope shape, control
events, every channel's pattern/docs/schemas — which is enough for an agent
to join and speak over a raw WebSocket with no client library. That
manifest is part of the agent contract, next to `/__introspect` and
`/__tools`.

### The browser client

`sutegi_channels::JS_CLIENT` is a bundled ~4 KB dependency-free client;
serve it as a static asset. Auto-reconnect with capped backoff, automatic
rejoin (fresh `join_ref`, stale frames discarded), heartbeat liveness,
ref-tracked `receive("ok"|"error"|"timeout")`.

```js
const socket = new SutegiSocket("/channels");
socket.connect();
const room = socket.channel("room:1", {nick: "ada"});
room.on("new_msg", p => render(p));
room.join().receive("ok", () => {});
room.push("new_msg", {body: "hello"});
```

### Join lifecycle notes

- Inside `on_join` the member is **not admitted yet**: a broadcast there
  reaches the room but not the joiner, and a push lands before the join
  reply. Use `socket.after_join(|socket| …)` for welcome pushes — it runs
  after the ok reply is on the wire (and never runs if the join is refused).
- A join on an already-joined topic **replaces** the membership: the old one
  gets the leave callback with `LeaveReason::Rejoin`, assigns start fresh.
- `assigns` are per-membership JSON state (`socket.assign` /
  `socket.assign_get`), gone on leave/disconnect.

## Cross-pod fan-out (PgPubSub)

`Channels::broker(PgPubSub::connect(&cfg)?)` is the only change between one
pod and a fleet. Every broker `LISTEN`s on one PG channel; topics travel
inside the payload envelope; a pod recognizes its own instance id and skips
the echo (local delivery already happened synchronously at publish).

Honest limits, by design:

- **At-most-once, fire-and-forget** — Phoenix's contract too. A pod that is
  reconnecting misses messages sent meanwhile. If a message must survive,
  put it in a table (or `sutegi-events`) and broadcast *that there is news*.
- `NOTIFY` payloads cap at ~8 KB. Oversized messages still deliver locally;
  `try_publish` returns the error. Ship ids, not blobs.
- One PG connection per pod for listening plus one lazy publisher — fan-out
  cost on the database is per-*message*, not per-subscriber.

## Presence

```rust
.on_join(|socket, payload| {
    Presence::track(socket, user_id, Json::obj(vec![("nick", Json::str(nick))]));
    Ok(Json::Null)
})
```

The tracked member receives `presence_state` (the full
`{key: {"metas": [...]}}` view, after its join reply); the room receives
`presence_diff {joins, leaves}` on every change. Untrack is automatic on
leave/rejoin/disconnect; `Presence::list(&hub, topic)` is the server-side
view. Multiple memberships may track the same key (one user, many tabs) —
each contributes a meta.

Cross-pod sync is **heartbeat-based, not CRDT**: each pod re-publishes its
local state every `presence_heartbeat` interval (default 30 s) and expires
pods silent for ~2.5×, reporting their members as leaves. So a crashed
pod's users can linger in the list up to ~75 s, and partition conflicts
resolve by expiry. That is the right trade for a "who's online" sidebar;
keep anything stronger in a table.

## Scaling notes

- Broadcast to a large room encodes the frame **once** (`Arc`-shared) and
  takes each reactor shard's lock once — the 80k-socket transport numbers
  in the ws docs apply to channel broadcasts unchanged.
- Per-connection ordering is guaranteed (callbacks inline on the shard).
  Cross-connection ordering within a topic follows broker order: in-process
  is synchronous; PG delivers `NOTIFY` in commit order.
- The `/__channels` endpoint, like the whole `/__` surface, is gated by
  `App::ops_guard`. A cookie-authenticated socket endpoint MUST set
  `check_origin` (CSWSH) — same rule as `App::ws`.

## Try it

```sh
cargo run -p chat-example                    # single pod on :8080
# cross-pod: two processes, one database
DATABASE_URL=postgres://… cargo run -p chat-example -- 127.0.0.1:8080 &
DATABASE_URL=postgres://… cargo run -p chat-example -- 127.0.0.1:8081 &
# open a room on each port; messages and the online list cross pods
```
