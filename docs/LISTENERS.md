# Listeners — non-HTTP socket loops under the app lifecycle

`App::listener` registers a long-running loop — a UDP ingest port, a raw TCP
protocol, a discovery beacon — that runs on its own thread for the life of the
server and shuts down with it. It is a lifecycle seam, not a protocol: sutegi
does not frame your packets. `std::net` already gives you `UdpSocket` and
`TcpListener`; what a hand-spawned thread cannot do is see shared state, drain
gracefully, or show up where an agent can discover it. This closes those three
gaps and nothing more.

## Registering

```rust
use sutegi::prelude::*;
use std::net::UdpSocket;
use std::time::Duration;

let app = App::new("metrics-demo")
    .state(db)
    .listener("statsd", "Ingests statsd counters on udp/8125.", |ctx| {
        let sock = UdpSocket::bind("0.0.0.0:8125").unwrap();
        sock.set_read_timeout(Some(Duration::from_millis(250))).unwrap();
        let mut buf = [0u8; 1500];
        while !ctx.should_stop() {
            if let Ok((n, _from)) = sock.recv_from(&mut buf) {
                ingest(ctx.db::<Db>(), &buf[..n]);
            }
        }
    });
app.serve()?;
```

The closure runs once, on a thread named `sutegi-listener-<name>`, started by
`run` / `run_until` / `run_graceful` / `serve`. `App::service()` never spawns
listeners — the in-process request closure stays socket-free for tests and
benches.

## `ListenerCtx`

- `should_stop()` — true once shutdown has begun (SIGTERM/SIGINT under
  `run_graceful`, or your own flag under `run_until`).
- `state::<T>()` / `try_state::<T>()` — the same typed state registered with
  `App::state`, shared with handlers and tools.
- `db::<B>()` — sugar for `state` pinned to an ORM backend (with the `orm`
  feature).
- `name()` — the registered name.

## The shutdown contract (cooperative)

On shutdown the server stops accepting, drains in-flight HTTP requests, then
**joins every listener thread before returning**. The join waits for your loop
to notice, so never park in an unbounded blocking read: set a read timeout on
the socket and poll `should_stop()` each lap, as above. A listener that never
polls holds the process until the orchestrator's grace period expires and
SIGKILL lands — honest, but avoidable.

## Failure semantics

A panicking listener does not restart and does not take the server down: the
panic is caught, one line lands on stderr, and the thread ends. If the loop
must survive crashes, own that policy — wrap the body in a retry loop, or run
it under a `sutegi-actors` supervisor and keep the listener as the thin socket
shim.

## Discovery

`GET /__introspect` gains a `listeners` block:

```json
{ "listeners": [ { "name": "statsd", "doc": "Ingests statsd counters on udp/8125." } ] }
```

Name and doc only — which port and protocol the loop speaks belongs in the
doc string, where agents read it. The `serve()` banner prints registered
listener names next to the ops routes.
