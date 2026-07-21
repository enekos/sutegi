# Actors & supervision

`sutegi-actors` (facade feature `actors`) is the OTP half of the stack:
isolated actor processes with typed mailboxes, and supervision trees that
restart them when they crash. Zero third-party deps — `std::thread` +
`std::sync::mpsc` only.

## The actor primitive (GenServer analog)

An `Actor` owns its state outright — no locks, no `Sync` bound on the state.
Other threads hold an `ActorRef` and communicate by message:

```rust
use sutegi::prelude::*;

struct Counter { n: u64 }

enum Msg {
    Bump,
    Get(ReplyTo<u64>),
}

impl Actor for Counter {
    type Msg = Msg;
    fn handle(&mut self, msg: Msg) {
        match msg {
            Msg::Bump => self.n += 1,
            Msg::Get(reply) => reply.reply(self.n),
        }
    }
}

let counter = spawn(Counter { n: 0 });
counter.tell(Msg::Bump)?;                                  // cast
let n = counter.ask(Msg::Get, Duration::from_secs(1))?;    // call
```

- **Bounded mailbox** (default 1024, `ActorOpts::mailbox`). A full mailbox
  fails `tell` fast with `TellError::Full` — backpressure is explicit, never
  a hidden unbounded queue.
- **Let it crash.** A panic inside `handle` (or `started`) is caught with
  `catch_unwind` — the same posture as the ws reactor — kills only that
  actor, and is reported as `ExitReason::Crashed(msg)`. Unsupervised, that's
  final; supervised, it's a restart signal.
- **Lifecycle hooks**: `started()` on every (re)start, `stopped(&reason)` on
  every exit — including after crashes.
- `stop()` is queued *behind* pending messages, so the mailbox drains first.

## Supervision trees

```rust
let sup = Supervisor::new("pipeline")
    .strategy(Strategy::OneForOne)          // default
    .intensity(3, Duration::from_secs(5))   // OTP defaults
    .child(ChildSpec::new("worker-1", || Worker::new()))
    .child(
        ChildSpec::new("flaky-api", || ApiClient::new())
            .restart(Restart::Transient)
            .backoff(Duration::from_millis(250)),
    )
    .start();
```

- **Restart policies** (`Restart`): `Permanent` (always), `Transient` (only
  on crash), `Temporary` (never).
- **Strategies** (`Strategy`): `OneForOne`, `RestForOne` (the crashed child +
  everything started after it), `OneForAll`. Dependents are stopped in
  reverse order and restarted in start order.
- **Clean state on restart.** The child factory re-runs on every restart, so
  a crashed child comes back as a fresh value — never with the poisoned
  state it died holding.
- **Intensity cap.** More than `max` restart events inside `window` → the
  supervisor stops all children and enters `SupervisorState::Failed`. A
  crash-looping child can't burn a thread forever.
- **Backoff** per child: fixed delay before a restart, blocking the
  supervisor loop (deliberately simple).
- `SupervisorHandle::child_ref::<M>(name)` — the `whereis` analog. Re-fetch
  after a crash: an old ref still points at the dead generation's queue.
- `Supervisor::start()` returns only after every child's first generation is
  up, so `child_ref` works immediately.

Stopping is cooperative (OTP's infinite shutdown timeout): `sup.stop()`
stops children in reverse order and waits for their threads.

## Observability (`/__actors`)

Every actor can register into a clone-cheap `Registry` — supervised children
do so automatically:

```rust
let sup = Supervisor::new("pipeline").child(/* … */).start();
let registry = sup.registry();

App::new("myapp")
    .actors(registry)   // GET /__actors
    // …
    .serve()
```

```json
[
  {
    "id": 1,
    "name": "worker-1",
    "state": "running",
    "restarts": 2,
    "mailbox_len": 0,
    "mailbox_cap": 1024,
    "last_error": "connection reset"
  }
]
```

`/__actors` is gated by `App::ops_guard` like the rest of the `/__` surface.
Unsupervised actors opt in with `spawn_opts(actor, ActorOpts::new().name(..).registry(&reg))`.

## Honest limits

- One OS thread per actor. This is supervision and fault isolation, not
  BEAM-scale lightweight processes — thousands of actors, not millions.
- No process links/monitors beyond the supervisor relationship, no
  distribution (PG is the bus, per the cross-pod posture), no hot code
  reload.
- In `release` the workspace builds with `panic = "abort"`, so
  `catch_unwind` never runs there — a panic aborts the process. Dev/test
  builds (unwind) get the full crash-restart semantics; production posture
  is "the pod supervisor restarts the process", same as the ws reactor.
