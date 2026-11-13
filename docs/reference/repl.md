<!-- Generated from landing/src/Docs.svelte. Run: cd landing && pnpm docs:export -->

[← Testing](testing.md) · [Documentation index](../README.md) · [Inside the server →](internals.md)

# The REPL

The `repl` feature is a tinker-style interactive shell. It is deliberately **not** a Rust evaluator — that would need a compiler toolchain and third-party machinery — but a command shell over the surfaces your app already exposes: the agent contract (introspection, the tool manifest, tool invocation with SSE frames printed live, raw HTTP through your own routes) and, with a `Backend` attached, the data layer (raw SQL, a `where`-clause query DSL, KV, the event store, the job queue).

```text
// In-process: consume the built App; data commands light up via .db(...).
Repl::new(app).db(db).run()?;

// Or against a RUNNING app with no source access — the agent contract,
// driven by a human:
//   sutegi repl 127.0.0.1:8080

sutegi> routes
GET     /api/todos                       List todos
sutegi> tools
create_todo              [unary    ] Create a todo
sutegi> call create_todo {"title":"ship"}
{ "id": 1, "title": "ship", "done": false }
sutegi> q todos where done = false order id desc limit 5
sutegi> sql SELECT count(*) FROM todos
sutegi> kv scan flags
sutegi> events account-42 10
sutegi> jobs

// Line editing is plain stdin (zero deps); wrap with rlwrap for history.
```

Two transports, one command set. In-process it consumes the built `App`; remote mode drives a *running* app over plain HTTP with no source access — exactly the way an LLM does — which is why `sutegi repl <addr>` works against any sutegi app. `Repl::eval(line)` is the programmatic seam the loop is built on, so the same commands are scriptable and testable.

---

[← Testing](testing.md) · [Documentation index](../README.md) · [Inside the server →](internals.md)
