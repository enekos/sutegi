#![forbid(unsafe_code)]
//! Actor processes: isolated state owned by a thread, driven by a typed
//! mailbox — the GenServer analog, zero third-party deps.
//!
//! An [`Actor`] owns its state outright (no locks, no `Sync` bound on the
//! state itself); other threads hold an [`ActorRef`] and communicate by
//! message: [`ActorRef::tell`] (cast, backpressured by a bounded mailbox) or
//! [`ActorRef::ask`] (call, with a timeout). A panic inside `handle` is
//! caught — it kills the actor, never the caller — and reported through
//! [`ExitReason::Crashed`], which is what a [`Supervisor`] consumes to
//! restart with fresh state ("let it crash").
//!
//! Everything is observable through a [`Registry`]: clone it into your app
//! state (or mount it at `/__actors` via `App::actors` in sutegi-web) for
//! live state, mailbox depth, and restart counts.
//!
//! ```
//! use sutegi_actors::{spawn, Actor, ReplyTo};
//! use std::time::Duration;
//!
//! struct Counter { n: u64 }
//!
//! enum Msg { Bump, Get(ReplyTo<u64>) }
//!
//! impl Actor for Counter {
//!     type Msg = Msg;
//!     fn handle(&mut self, msg: Msg) {
//!         match msg {
//!             Msg::Bump => self.n += 1,
//!             Msg::Get(reply) => reply.reply(self.n),
//!         }
//!     }
//! }
//!
//! let counter = spawn(Counter { n: 0 });
//! counter.tell(Msg::Bump).unwrap();
//! counter.tell(Msg::Bump).unwrap();
//! let n = counter.ask(Msg::Get, Duration::from_secs(1)).unwrap();
//! assert_eq!(n, 2);
//! ```

mod supervisor;

pub use supervisor::{
    ChildSpec, Restart, Strategy, Supervisor, SupervisorHandle, SupervisorState, SupervisorStatus,
};

use std::collections::BTreeMap;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicU64, AtomicU8, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use sutegi_json::Json;

const STATUS_RUNNING: u8 = 0;
const STATUS_STOPPED: u8 = 1;
const STATUS_CRASHED: u8 = 2;

static NEXT_ACTOR_ID: AtomicU64 = AtomicU64::new(1);

/// The unit of isolated, supervised computation. Implementations own their
/// state; the framework drives them one message at a time on their thread.
///
/// `started` runs once per (re)start — after a crash-restart this is a fresh
/// value, so init work belongs here, not in assumptions carried across
/// crashes. `stopped` runs on every exit (clean or crashed) and receives the
/// reason.
pub trait Actor: Send + 'static {
    /// The message type this actor's mailbox accepts.
    type Msg: Send + 'static;

    /// Called once when the actor's thread starts, before the first message.
    fn started(&mut self) {}

    /// Process one message. A panic here crashes the actor (and only the
    /// actor): the panic is captured into [`ExitReason::Crashed`].
    fn handle(&mut self, msg: Self::Msg);

    /// Called once before the thread exits, with the exit reason. Runs after
    /// crashes too — release resources here, not in `Drop` of captured state
    /// that a panic may have left inconsistent.
    fn stopped(&mut self, _reason: &ExitReason) {}
}

/// Why an actor's thread exited.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExitReason {
    /// Told to stop, or every [`ActorRef`] was dropped.
    Stopped,
    /// `handle` (or `started`) panicked. The string is the panic message,
    /// truncated to 240 bytes.
    Crashed(String),
}

impl ExitReason {
    /// True when the actor panicked (as opposed to stopping cleanly).
    pub fn crashed(&self) -> bool {
        matches!(self, ExitReason::Crashed(_))
    }
}

/// Lifecycle state of an actor process.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActorState {
    Running,
    Stopped,
    Crashed,
}

/// A point-in-time snapshot of one actor, from [`Registry::snapshot`] or
/// [`ActorRef::status`].
#[derive(Clone, Debug)]
pub struct ActorStatus {
    pub id: u64,
    pub name: Option<String>,
    pub state: ActorState,
    /// How many times a supervisor has restarted this actor.
    pub restarts: u64,
    /// Messages queued, not yet processed.
    pub mailbox_len: usize,
    /// Mailbox capacity; `tell` against a full mailbox fails fast.
    pub mailbox_cap: usize,
    /// Panic message of the most recent crash, if any.
    pub last_error: Option<String>,
}

pub(crate) struct Shared {
    id: u64,
    name: Option<String>,
    status: AtomicU8,
    restarts: AtomicU64,
    mailbox_len: AtomicUsize,
    mailbox_cap: usize,
    last_error: Mutex<Option<String>>,
}

impl Shared {
    fn new(name: Option<String>, mailbox_cap: usize) -> Shared {
        Shared {
            id: NEXT_ACTOR_ID.fetch_add(1, Ordering::Relaxed),
            name,
            status: AtomicU8::new(STATUS_RUNNING),
            restarts: AtomicU64::new(0),
            mailbox_len: AtomicUsize::new(0),
            mailbox_cap,
            last_error: Mutex::new(None),
        }
    }

    /// Reset for a supervisor restart: fresh mailbox, running state, restart
    /// counter bumped. `last_error` is kept — it describes the crash that
    /// caused this restart.
    fn reset_for_restart(&self) {
        self.mailbox_len.store(0, Ordering::Relaxed);
        self.restarts.fetch_add(1, Ordering::Relaxed);
        self.status.store(STATUS_RUNNING, Ordering::Relaxed);
    }

    pub(crate) fn id(&self) -> u64 {
        self.id
    }

    pub(crate) fn name(&self) -> &str {
        self.name.as_deref().unwrap_or("")
    }

    pub(crate) fn mailbox_cap(&self) -> usize {
        self.mailbox_cap
    }

    fn finish(&self, reason: &ExitReason) {
        match reason {
            ExitReason::Stopped => self.status.store(STATUS_STOPPED, Ordering::Relaxed),
            ExitReason::Crashed(msg) => {
                *self.last_error.lock().unwrap() = Some(msg.clone());
                self.status.store(STATUS_CRASHED, Ordering::Relaxed);
            }
        }
    }

    fn snapshot(&self) -> ActorStatus {
        let state = match self.status.load(Ordering::Relaxed) {
            STATUS_RUNNING => ActorState::Running,
            STATUS_CRASHED => ActorState::Crashed,
            _ => ActorState::Stopped,
        };
        ActorStatus {
            id: self.id,
            name: self.name.clone(),
            state,
            restarts: self.restarts.load(Ordering::Relaxed),
            mailbox_len: self.mailbox_len.load(Ordering::Relaxed),
            mailbox_cap: self.mailbox_cap,
            last_error: self.last_error.lock().unwrap().clone(),
        }
    }
}

pub(crate) enum Envelope<M> {
    Msg(M),
    Stop,
}

pub(crate) struct Exit {
    pub id: u64,
    /// Which start of this actor the exit belongs to — a supervisor stops
    /// dependents itself on RestForOne/OneForAll, and their exits arrive
    /// after the new generation has already started; the generation tells
    /// those stale exits apart from real crashes.
    pub gen: u64,
    pub reason: ExitReason,
}

/// How an actor reports its exit to a supervisor (closure so the supervisor
/// can merge exits onto its own command queue without a forwarder thread).
pub(crate) type ExitSink = Arc<dyn Fn(Exit) + Send + Sync>;

/// Spawn options for [`spawn_opts`]: name, mailbox capacity, registry.
pub struct Opts {
    name: Option<String>,
    mailbox: usize,
    registry: Option<Registry>,
}

impl Default for Opts {
    fn default() -> Opts {
        Opts {
            name: None,
            mailbox: 1024,
            registry: None,
        }
    }
}

impl Opts {
    pub fn new() -> Opts {
        Opts::default()
    }

    /// Name surfaced in [`Registry`] snapshots and `/__actors`.
    pub fn name(mut self, name: &str) -> Opts {
        self.name = Some(name.to_string());
        self
    }

    /// Mailbox capacity (default 1024). `tell` against a full mailbox
    /// returns [`TellError::Full`] immediately — backpressure is explicit.
    pub fn mailbox(mut self, cap: usize) -> Opts {
        self.mailbox = cap;
        self
    }

    /// Register the actor for introspection ([`Registry::snapshot`]).
    pub fn registry(mut self, registry: &Registry) -> Opts {
        self.registry = Some(registry.clone());
        self
    }
}

/// A handle to a running actor. Cheap to clone; every clone may `tell` /
/// `ask`. When the last clone drops, the actor stops (its mailbox closes).
pub struct ActorRef<M> {
    tx: SyncSender<Envelope<M>>,
    shared: Arc<Shared>,
}

impl<M> Clone for ActorRef<M> {
    fn clone(&self) -> ActorRef<M> {
        ActorRef {
            tx: self.tx.clone(),
            shared: Arc::clone(&self.shared),
        }
    }
}

/// Why a [`ActorRef::tell`] failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TellError {
    /// Mailbox at capacity. Retry later or shed load — this is the
    /// backpressure signal.
    Full,
    /// The actor is gone (stopped or crashed, unsupervised).
    Dead,
}

/// Why an [`ActorRef::ask`] failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AskError {
    /// The actor died before (or while) answering.
    Dead,
    /// Mailbox at capacity; the request was never queued.
    Full,
    /// No reply within the timeout. The actor may still answer later; the
    /// reply is discarded.
    Timeout,
}

/// The reply half of an `ask`: hand it to the actor inside the message and
/// call [`ReplyTo::reply`] exactly once.
pub struct ReplyTo<R>(mpsc::Sender<R>);

impl<R> ReplyTo<R> {
    /// Send the answer. If the asker timed out or went away, this is a no-op.
    pub fn reply(self, value: R) {
        let _ = self.0.send(value);
    }
}

impl<M: Send + 'static> ActorRef<M> {
    /// Cast a message (fire-and-forget, in mailbox order).
    pub fn tell(&self, msg: M) -> Result<(), TellError> {
        match self.tx.try_send(Envelope::Msg(msg)) {
            Ok(()) => {
                self.shared.mailbox_len.fetch_add(1, Ordering::Relaxed);
                Ok(())
            }
            Err(TrySendError::Full(_)) => Err(TellError::Full),
            Err(TrySendError::Disconnected(_)) => Err(TellError::Dead),
        }
    }

    /// Call the actor and wait for its reply: `make` builds the message
    /// around a [`ReplyTo`]. Bounded by `timeout`.
    pub fn ask<R: Send + 'static>(
        &self,
        make: impl FnOnce(ReplyTo<R>) -> M,
        timeout: Duration,
    ) -> Result<R, AskError> {
        let (tx, rx) = mpsc::channel();
        self.tell(make(ReplyTo(tx))).map_err(|e| match e {
            TellError::Full => AskError::Full,
            TellError::Dead => AskError::Dead,
        })?;
        rx.recv_timeout(timeout).map_err(|_| AskError::Timeout)
    }

    /// Ask the actor to stop. Queued behind pending messages, so it drains
    /// first; messages told after this may never be processed.
    pub fn stop(&self) {
        let _ = self.tx.send(Envelope::Stop);
    }

    /// True while the actor's thread is alive.
    pub fn is_alive(&self) -> bool {
        self.shared.status.load(Ordering::Relaxed) == STATUS_RUNNING
    }

    /// A point-in-time status snapshot.
    pub fn status(&self) -> ActorStatus {
        self.shared.snapshot()
    }

    /// The actor's process-unique id.
    pub fn id(&self) -> u64 {
        self.shared.id
    }
}

/// Spawn an unsupervised actor (default mailbox, unregistered). For crash
/// recovery use a [`Supervisor`]; for introspection use [`spawn_opts`] with
/// a registry.
pub fn spawn<A: Actor>(actor: A) -> ActorRef<A::Msg> {
    spawn_opts(actor, Opts::default())
}

/// Spawn an unsupervised actor with explicit [`Opts`].
pub fn spawn_opts<A: Actor>(actor: A, opts: Opts) -> ActorRef<A::Msg> {
    let (aref, _join) = spawn_inner(actor, &opts, None);
    if let Some(registry) = &opts.registry {
        registry.register(Arc::clone(&aref.shared));
    }
    aref
}

pub(crate) fn spawn_inner<A: Actor>(
    actor: A,
    opts: &Opts,
    sink: Option<ExitSink>,
) -> (ActorRef<A::Msg>, JoinHandle<()>) {
    let shared = Arc::new(Shared::new(opts.name.clone(), opts.mailbox));
    spawn_on_shared(actor, shared, opts.mailbox, 0, sink)
}

/// Spawn an actor thread on a caller-provided [`Shared`] (supervisor restart
/// path: the Shared outlives generations so restart counts accumulate).
pub(crate) fn spawn_on_shared<A: Actor>(
    mut actor: A,
    shared: Arc<Shared>,
    mailbox: usize,
    gen: u64,
    sink: Option<ExitSink>,
) -> (ActorRef<A::Msg>, JoinHandle<()>) {
    let (tx, rx) = mpsc::sync_channel(mailbox);
    let aref = ActorRef {
        tx,
        shared: Arc::clone(&shared),
    };
    let id = shared.id;
    let join = std::thread::spawn(move || run_actor(id, gen, &mut actor, rx, shared, sink));
    (aref, join)
}

fn run_actor<A: Actor>(
    id: u64,
    gen: u64,
    actor: &mut A,
    rx: Receiver<Envelope<A::Msg>>,
    shared: Arc<Shared>,
    sink: Option<ExitSink>,
) {
    let reason = match catch_unwind(AssertUnwindSafe(|| actor.started())) {
        Err(panic) => ExitReason::Crashed(panic_message(&panic)),
        Ok(()) => loop {
            match rx.recv() {
                Ok(Envelope::Stop) => break ExitReason::Stopped,
                Ok(Envelope::Msg(msg)) => {
                    shared.mailbox_len.fetch_sub(1, Ordering::Relaxed);
                    if let Err(panic) = catch_unwind(AssertUnwindSafe(|| actor.handle(msg))) {
                        break ExitReason::Crashed(panic_message(&panic));
                    }
                }
                // Every ActorRef dropped: close up shop.
                Err(_) => break ExitReason::Stopped,
            }
        },
    };
    shared.finish(&reason);
    let _ = catch_unwind(AssertUnwindSafe(|| actor.stopped(&reason)));
    if let Some(sink) = sink {
        sink(Exit { id, gen, reason });
    }
}

fn panic_message(panic: &Box<dyn std::any::Any + Send>) -> String {
    let msg = if let Some(s) = panic.downcast_ref::<&str>() {
        s.to_string()
    } else if let Some(s) = panic.downcast_ref::<String>() {
        s.clone()
    } else {
        "panic (non-string payload)".to_string()
    };
    let mut msg = msg;
    if msg.len() > 240 {
        msg.truncate(240);
        msg.push('…');
    }
    msg
}

/// A live directory of actors, for introspection. Clone-cheap (shared
/// interior); hand one to [`Opts::registry`] / [`Supervisor::registry`] and
/// keep another for your app's `/__actors` endpoint.
#[derive(Clone, Default)]
pub struct Registry {
    inner: Arc<Mutex<BTreeMap<u64, Arc<Shared>>>>,
}

impl Registry {
    pub fn new() -> Registry {
        Registry::default()
    }

    pub(crate) fn register(&self, shared: Arc<Shared>) {
        self.inner.lock().unwrap().insert(shared.id, shared);
    }

    /// Snapshot every registered actor, ordered by id (spawn order).
    pub fn snapshot(&self) -> Vec<ActorStatus> {
        self.inner
            .lock()
            .unwrap()
            .values()
            .map(|s| s.snapshot())
            .collect()
    }

    /// The snapshot as JSON — the `/__actors` response body.
    pub fn snapshot_json(&self) -> Json {
        Json::arr(
            self.snapshot()
                .iter()
                .map(|a| {
                    Json::obj(vec![
                        ("id", Json::int(a.id as i64)),
                        (
                            "name",
                            a.name.as_deref().map(Json::str).unwrap_or(Json::Null),
                        ),
                        (
                            "state",
                            Json::str(match a.state {
                                ActorState::Running => "running",
                                ActorState::Stopped => "stopped",
                                ActorState::Crashed => "crashed",
                            }),
                        ),
                        ("restarts", Json::int(a.restarts as i64)),
                        ("mailbox_len", Json::int(a.mailbox_len as i64)),
                        ("mailbox_cap", Json::int(a.mailbox_cap as i64)),
                        (
                            "last_error",
                            a.last_error.as_deref().map(Json::str).unwrap_or(Json::Null),
                        ),
                    ])
                })
                .collect(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc as std_mpsc;

    struct Counter {
        n: u64,
    }

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

    #[test]
    fn tell_then_ask_in_order() {
        let c = spawn(Counter { n: 10 });
        c.tell(Msg::Bump).unwrap();
        c.tell(Msg::Bump).unwrap();
        let n = c.ask(Msg::Get, Duration::from_secs(1)).unwrap();
        assert_eq!(n, 12);
    }

    #[test]
    fn ask_times_out_when_actor_never_replies() {
        struct Mute;
        impl Actor for Mute {
            type Msg = ReplyTo<u8>;
            fn handle(&mut self, _msg: ReplyTo<u8>) {}
        }
        let m = spawn(Mute);
        let err = m.ask(|r| r, Duration::from_millis(50)).unwrap_err();
        assert_eq!(err, AskError::Timeout);
        assert!(m.is_alive());
    }

    #[test]
    fn full_mailbox_fails_fast_without_blocking() {
        struct Slow;
        impl Actor for Slow {
            type Msg = ();
            fn handle(&mut self, _msg: ()) {
                std::thread::sleep(Duration::from_millis(200));
            }
        }
        let s = spawn_opts(Slow, Opts::new().mailbox(1));
        s.tell(()).unwrap(); // being processed
                             // Wait until the first message is dequeued so the second fills the box.
        std::thread::sleep(Duration::from_millis(50));
        s.tell(()).unwrap(); // fills the mailbox
        assert_eq!(s.tell(()), Err(TellError::Full));
    }

    #[test]
    fn crash_kills_the_actor_not_the_caller() {
        struct Bomb;
        impl Actor for Bomb {
            type Msg = ();
            fn handle(&mut self, _msg: ()) {
                panic!("boom");
            }
        }
        let b = spawn(Bomb);
        b.tell(()).unwrap();
        for _ in 0..100 {
            if !b.is_alive() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let status = b.status();
        assert_eq!(status.state, ActorState::Crashed);
        assert_eq!(status.last_error.as_deref(), Some("boom"));
        assert_eq!(b.tell(()), Err(TellError::Dead));
    }

    #[test]
    fn stop_drains_then_exits() {
        let (done_tx, done_rx) = std_mpsc::channel();
        struct Drain {
            done: std_mpsc::Sender<u8>,
            n: u8,
        }
        impl Actor for Drain {
            type Msg = u8;
            fn handle(&mut self, msg: u8) {
                self.n += msg;
            }
            fn stopped(&mut self, _reason: &ExitReason) {
                let _ = self.done.send(self.n);
            }
        }
        let d = spawn(Drain {
            done: done_tx,
            n: 0,
        });
        d.tell(1).unwrap();
        d.tell(2).unwrap();
        d.stop();
        assert_eq!(done_rx.recv_timeout(Duration::from_secs(1)).unwrap(), 3);
    }

    #[test]
    fn last_ref_drop_stops_the_actor() {
        let registry = Registry::new();
        let c = spawn_opts(
            Counter { n: 0 },
            Opts::new().name("dropped").registry(&registry),
        );
        let id = c.id();
        drop(c);
        let mut state = ActorState::Running;
        for _ in 0..100 {
            let snap = registry.snapshot();
            match snap.iter().find(|s| s.id == id) {
                Some(s) if s.state == ActorState::Stopped => {
                    state = ActorState::Stopped;
                    break;
                }
                _ => std::thread::sleep(Duration::from_millis(10)),
            }
        }
        assert_eq!(state, ActorState::Stopped);
    }

    #[test]
    fn registry_snapshots_spawned_actors() {
        let registry = Registry::new();
        let a = spawn_opts(
            Counter { n: 0 },
            Opts::new()
                .name("counter-a")
                .mailbox(64)
                .registry(&registry),
        );
        let _b = spawn_opts(
            Counter { n: 0 },
            Opts::new().name("counter-b").registry(&registry),
        );
        let snap = registry.snapshot();
        let a_status = snap
            .iter()
            .find(|s| s.name.as_deref() == Some("counter-a"))
            .unwrap();
        assert_eq!(a_status.mailbox_cap, 64);
        assert_eq!(a_status.state, ActorState::Running);
        assert_eq!(a.id(), a_status.id);
        // JSON shape: the /__actors body.
        let json = registry.snapshot_json().to_string();
        assert!(json.contains("\"counter-a\""));
        assert!(json.contains("\"running\""));
        drop(a);
    }
}
