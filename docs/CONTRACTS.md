# Contracts: Rust types as the API, a typed TypeScript client for free

tRPC works because the server and the browser are both TypeScript, so the compiler reads one set of types end to end. A sutegi server is Rust, so the same promise needs a different mechanism: **the Rust types are the contract**, the app records them as routes are registered, and the contract renders to a zero-dependency TypeScript client, to JSON Schema for agents, and to a manifest at `/__contract`.

```toml
sutegi = { version = "0.11", features = ["contract"] }            # + "channels" for typed channels
```

| piece | what it is |
|---|---|
| `#[derive(Contract)]` | decode from JSON (every error, keyed by path), encode to JSON, describe the shape |
| `App::typed` / `Group::typed` | a request/response procedure on a normal route |
| `App::typed_stream` | a procedure that streams typed events over SSE, then a typed result |
| `Channel::typed_join` / `typed_on` / `typed_emits` | typed WebSocket channel messages (feature `channels`) |
| `App::contract_ts()` · `GET /__contract.ts` | the generated TypeScript client |
| `GET /__contract` · `/__introspect` | the contract as JSON; every typed route carries its input/output JSON Schema |
| `contract::snapshot::check_file` | the drift test: fails CI when the checked-in client no longer matches |

## Types

```rust
use sutegi::prelude::*;

#[derive(Contract)]
#[contract(doc = "An agent the workspace can run.")]
struct Agent {
    id: AgentId,
    #[contract(min_len = 1, max_len = 80)]
    name: String,
    description: Option<String>,      // optional key, may be null
    approval: Approval,
    #[contract(default, max = 50)]
    max_steps: u32,                   // missing → Default::default()
    #[contract(skip)]
    cache: Option<Vec<u8>>,           // never on the wire
}

#[derive(Contract)]
struct AgentId(String);               // → export type AgentId = string;

#[derive(Contract)]
enum Approval { Always, WritesOnly, #[contract(rename = "off")] Never }
                                      // → "always" | "writes_only" | "off"

#[derive(Contract)]
#[contract(tag = "t")]
enum AgentEvent {
    Step { n: u32, tool: Option<String> },   // { t: "step"; n: number; tool?: string | null }
    Token { text: String },
    Usage(Usage),                             // the struct's fields next to the tag
    Done,
}

#[derive(Contract)]
struct Page<T> { items: Vec<T>, next: Option<String> }   // Page<Agent> → Page_Agent
```

Supported field types: `bool`, every integer (range-checked, and refused beyond ±2⁵³ because JSON numbers are doubles), `f32`/`f64`, `String`, `Option<T>`, `Vec<T>`, `Box<T>`, `BTreeMap<String, T>`, `HashMap<String, T>`, `Json` (rendered as `unknown`), `()`, and any `#[derive(Contract)]` type, recursive ones included.

| attribute | on | effect |
|---|---|---|
| `rename = "x"` | type, field, variant | wire name (a type's TS name must be an identifier) |
| `tag = "t"` | enum | internally tagged union; required for enums with data |
| `doc = "…"` | type, field, variant | description in the TS JSDoc and JSON Schema (`///` doc comments work too) |
| `default` | field | missing → `Default::default()`; optional in TS |
| `skip` | field | not on the wire; filled with `Default::default()` |
| `min_len`, `max_len` | field | characters for strings, items for lists |
| `min`, `max` | field | numeric bounds |
| `crate = "path"` | type | where the runtime lives (default `::sutegi::contract`) |

Enum variants are `snake_case` on the wire. Unknown object keys are ignored. Decoding never stops at the first problem: a `422` lists every field that failed:

```json
{ "error": "validation failed",
  "errors": { "name": ["must have at least 1 characters"], "children[0].label": ["expected a string"] } }
```

That is the same shape `Ctx::validate` and tool calls already return.

## Procedures

```rust
App::new("agents")
    .group("/api/w/:ws", vec![mw(auth)], |g| {
        g.typed(
            Endpoint::get("agents.list", "/agents").doc("Agents in a workspace."),
            |c: &Ctx, q: ListAgents| -> Result<Page<Agent>, Error> { … },
        )
        .typed(
            Endpoint::post("agents.create", "/agents"),
            |c: &Ctx, body: CreateAgent| -> Result<Agent, Error> { … },
        )
        .typed(
            Endpoint::delete("agents.remove", "/agents/:id"),
            |c: &Ctx, r: AgentRef| -> Result<(), Error> { … },
        )
    })
```

- **The URL stays yours.** A typed route is an ordinary route: same patterns, groups, middleware, `curl`, public API. The contract adds a name (`agents.list`), the types, and a decoder. Untyped and typed routes coexist, so a codebase migrates one handler at a time.
- **GET reads the query string, everything else reads the JSON body.** Query values are strings, so a GET decodes leniently: `"10"` is accepted for a `u32`, `"true"` for a `bool`, and a JSON string for a list or a nested struct. A body decodes strictly. A GET input must be a struct (or `()`).
- **Path parameters bind into the input** when the input struct has a field of that name (`/agents/:id` + `id: u64`). They are read leniently and **win over the body**, so a body can't redirect a write to another id. Parameters that aren't fields (`:ws` above) stay on `c.param("ws")`, and the generated client still asks for them.
- **Handlers return `Result<O, Error>`.** `Ok` → `200` with the encoded output (`()` → `null`), `Err` → the usual `{error}` body with the error's status, 5xx messages hidden as everywhere else.
- **Names are dot-separated identifiers** and become the client's nesting: `api.agents.list(...)`. A duplicate name, or a name that is both a leaf and a namespace, panics at registration. So does a GET with a non-struct input, or two Rust types sharing one wire name (rename one).

## Streams

```rust
.typed_stream(
    Endpoint::post("agents.run", "/agents/:id/run"),
    |c: &Ctx, r: RunAgent| -> Result<Stream<AgentEvent, RunSummary>, Error> {
        let agent = load(c, r.id)?;                         // auth + checks: a normal HTTP error
        Ok(Stream::new(move |out| {
            out.emit(&AgentEvent::Step { n: 1, tool: None })?;
            for t in agent.tokens() {
                out.emit(&AgentEvent::Token { text: t })?;  // Err once the client is gone
            }
            Ok(RunSummary { … })
        }))
    },
)
```

Two phases, on purpose. The handler runs with `&Ctx` and can refuse with a normal status (`401`, `404`, `422`) **before** the stream opens. The closure then owns what it moved in and writes the stream. On the wire it is SSE: `event: item` per emitted value, then one `event: done` with the result, or `event: error` with `{status, error, errors?}` if the closure returns `Err`. When the client disconnects, `emit` returns `Err`, `?` ends the closure, and nothing else is written. `out.heartbeat()` sends a comment frame to keep a quiet stream alive through proxies.

**Mind the thread.** sutegi serves HTTP with one thread per connection, so a stream holds a worker for its whole life. Use streams for work that finishes, such as LLM generation, an import's progress, or an export. Anything that lives for minutes or hours belongs on a channel, where an idle socket costs a few hundred bytes on the reactor instead of a thread.

## Channels

```rust
Channel::new("table:*")
    .doc("Live edits on one table.")
    .typed_join("Who is watching.", |socket: &Socket, j: JoinTable| -> Result<Snapshot, String> { … })
    .typed_on("edit", "Edit one cell.", |socket: &Socket, e: EditCell| -> Result<Ack, String> {
        socket.broadcast_typed("change", &TableChange::Edited { … });
        Ok(Ack { version })
    })
    .typed_emits::<TableChange>("change", "Someone changed the table.")
```

The payloads decode like a request body: a bad join or push gets an error reply `{error: "validation failed", errors}`, and `Err(String)` replies `{error}`. `typed_emits` declares what the server pushes (it can't check a `broadcast`, but it puts the type in the client and the `/__channels` schema). `Socket::push_typed`, `broadcast_typed`, `broadcast_from_typed` and `ChannelHub::broadcast_typed` encode for you. Each typed message's JSON Schema also lands in `/__channels`, so agents see it without reading the TypeScript.

## The TypeScript client

```ts
import { createClient, channels, ContractError } from "./gen/api";

const api = createClient({ base: "/api", credentials: "same-origin", headers: () => ({ "x-client-id": id }) });

const page = await api.agents.list({ ws, limit: 20 });          // Page_Agent
try {
  await api.agents.create({ ws, name: "", approval: "always" });
} catch (e) {
  if (e instanceof ContractError && e.status === 422) show(e.errors);   // { name: [...] }
}

const run = api.agents.run({ ws, id, message: "hi" });
for await (const ev of run) {
  if (ev.t === "token") append(ev.text);                      // narrowed by the tag
}
const summary = await run.result;                              // RunSummary
run.abort();                                                   // stop early

const table = channels.table(socket, `${ws}:${id}`, { viewer: me });   // socket = the bundled SutegiSocket
const snapshot = await table.joined;
table.on("change", (c) => apply(c));                           // TableChange
const ack = await table.push("edit", { row: 1, value: "x" });  // Ack
```

One file, no dependencies: the types, a `fetch`-based runtime (works in browsers, Node 18+, Bun, Deno, Workers), and typed channel wrappers that take the bundled `SutegiSocket` (`sutegi::channels::JS_CLIENT`) or anything with the same `channel(topic, params)` shape. `joined` resolves on the first successful join, including the automatic rejoin the socket does once it connects, and rejects only when the server refuses. An input with no required fields is optional (`api.notes.list()`).

## Keeping the client in sync

The app is the source of truth, so the check is a test, not a build step:

```rust
#[test]
fn checked_in_client_matches_the_contract() {
    let ts = my_app::app(test_state()).contract_ts();
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../web/src/lib/gen/api.ts");
    if let Err(e) = sutegi::contract::snapshot::check_file(path, &ts) {
        panic!("{e}");
    }
}
```

It fails with the first differing line when someone changes a type without regenerating. `SUTEGI_CONTRACT_UPDATE=1 cargo test` rewrites the file, which you then commit. A running app also serves the same text at `GET /__contract.ts` (behind `ops_guard`, like all of `/__`).

## What it is not

- **Not a new transport.** Calls are plain HTTP with JSON. Streams are SSE. Channels are the existing channel protocol. There's no request batching and no RPC-over-WebSocket. Every one of these still works from `curl` or a raw socket.
- **Not an IDL.** There's no schema file to keep in step: the derive is the schema.
- **Not serde.** Contract types implement `Contract`, and the derive stays inside the zero-dependency rule (syn/quote run at compile time only).
- **Numbers are JSON numbers.** `i64`/`u64` past 2⁵³ are refused on input and lose precision on output. Send big ids as strings (`struct Id(String)`).

## Agents

Every typed route's entry in `/__introspect` gains a `contract` block: `name`, `kind` (`query` / `mutation` / `stream`), `input_schema`, `output_schema`, and for streams `event_schema`. These are real JSON Schema, with `required`, `enum`, `oneOf` for tagged unions, bounds from the rules, and descriptions from the docs. `GET /__contract` has the full type graph plus every procedure and typed channel, for tools that want to generate their own client.

See `examples/contract` for a whole app: typed CRUD, a stream, a typed channel, the checked-in client, and `client/smoke.ts`, which drives all of it from Node (`cargo run -p contract-example`, then `node examples/contract/client/smoke.ts`).
