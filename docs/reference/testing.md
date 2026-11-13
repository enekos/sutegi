<!-- Generated from landing/src/Docs.svelte. Run: cd landing && pnpm docs:export -->

[← Hexagonal architecture](hexagonal.md) · [Documentation index](../README.md) · [The REPL →](repl.md)

# Testing

`App::service()` returns the app as a plain `Fn(Request) -> Response`, so you can exercise the whole routing, state, validation, and tool surface *in process* — no socket, no port, no async harness. Back it with `Db::memory()` for a fresh database per test.

```rust
// App::service() hands you a plain Fn(Request) -> Response — no socket, no port.
#[test]
fn creates_a_todo() {
    let db = Db::memory().unwrap();
    Todo::migrate(&db).unwrap();
    let handle = App::new("test").state(db)
        .post("/todos", "create", |c| {
            let t: Todo = c.validated()?;
            Ok::<_, Error>((201, Todo { id: t.save(c.db::<Db>())?, ..t }.to_json()))
        })
        .service();

    let resp = handle(Request::post("/todos", br#"{"title":"x"}"#));
    assert_eq!(resp.status, 201);
}
// The tool surface is reachable the same way — POST /__tools/:name through
// service(), no server, no agent. For end-to-end coverage the framework's own
// suite boots a real server over a loopback socket: crates/sutegi/tests/server.rs.
```

The pieces that need a server or a real engine are testable too, and the framework’s own suite shows how: `Mailer`’s `memory` driver for mail assertions, the queue’s caller-supplied millisecond timestamps so schedules need no sleeping, an in-process S3 stub over a real socket for the storage wire suite, and `crates/sutegi/tests/server.rs` for the full loopback end-to-end pattern.

---

[← Hexagonal architecture](hexagonal.md) · [Documentation index](../README.md) · [The REPL →](repl.md)
