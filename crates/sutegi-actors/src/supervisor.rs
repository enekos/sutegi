//! OTP-style supervision: restart policies, strategies, and intensity caps.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::mpsc::{self, Sender, SyncSender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::{
    spawn_on_shared, Actor, ActorRef, ActorStatus, Envelope, Exit, ExitReason, ExitSink, Registry,
    Shared,
};

/// When a child is (re)started after exiting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Restart {
    /// Always restart, on any exit. The default — for children that must run
    /// for the life of the app.
    Permanent,
    /// Restart only on crash; a clean stop is final.
    Transient,
    /// Never restart.
    Temporary,
}

/// How siblings are involved when one child needs a restart.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Strategy {
    /// Restart only the child that exited. The default.
    OneForOne,
    /// Restart the exited child and every child started after it, in start
    /// order — for children that depend on earlier siblings.
    RestForOne,
    /// Restart every child, in start order.
    OneForAll,
}

type ErasedTx = Arc<dyn std::any::Any + Send + Sync>;
type StopFn = Box<dyn Fn() + Send + 'static>;
type StartFn = Box<
    dyn FnMut(Arc<Shared>, u64, ExitSink) -> (ErasedTx, StopFn, JoinHandle<()>) + Send + 'static,
>;

/// A child template: how to build it, when to restart it. Built via
/// [`ChildSpec::new`] + the `restart` / `backoff` / `mailbox` modifiers.
pub struct ChildSpec {
    name: String,
    restart: Restart,
    backoff: Duration,
    mailbox: usize,
    start: StartFn,
}

impl ChildSpec {
    /// A child named `name`, built fresh by `factory` on every (re)start —
    /// so a crashed child comes back with clean state, never the poisoned
    /// state it died with.
    pub fn new<A: Actor>(name: &str, factory: impl FnMut() -> A + Send + 'static) -> ChildSpec {
        let mut factory = factory;
        ChildSpec {
            name: name.to_string(),
            restart: Restart::Permanent,
            backoff: Duration::ZERO,
            mailbox: 1024,
            start: Box::new(move |shared, gen, sink| {
                let mailbox = shared.mailbox_cap();
                let actor = factory();
                let (aref, join) = spawn_on_shared(actor, shared, mailbox, gen, Some(sink));
                let stop_tx = aref.tx.clone();
                let stop: StopFn = Box::new(move || {
                    let _ = stop_tx.send(Envelope::Stop);
                });
                let erased: ErasedTx = Arc::new(aref.tx);
                (erased, stop, join)
            }),
        }
    }

    /// Restart policy (default [`Restart::Permanent`]).
    pub fn restart(mut self, restart: Restart) -> ChildSpec {
        self.restart = restart;
        self
    }

    /// Fixed delay before restarting this child (default none) — a cheap
    /// circuit breaker for crash-looping children. Blocks the supervisor
    /// loop for its duration.
    pub fn backoff(mut self, backoff: Duration) -> ChildSpec {
        self.backoff = backoff;
        self
    }

    /// Mailbox capacity for this child (default 1024).
    pub fn mailbox(mut self, cap: usize) -> ChildSpec {
        self.mailbox = cap;
        self
    }
}

/// The stable identity of one child: shared state survives restarts, the
/// mailbox sender is swapped on every generation.
struct ChildEntry {
    shared: Arc<Shared>,
    tx: Mutex<Option<ErasedTx>>,
}

struct Child {
    spec: ChildSpec,
    entry: Arc<ChildEntry>,
    stop: Option<StopFn>,
    join: Option<JoinHandle<()>>,
    /// Incremented on every start; matched against [`Exit::gen`].
    gen: u64,
}

impl Child {
    /// (Re)start this child: fresh actor from the factory, same Shared (so
    /// restart counts accumulate in the registry).
    fn start(&mut self, sink: &ExitSink) {
        if self.gen > 0 {
            self.entry.shared.reset_for_restart();
        }
        self.gen += 1;
        let (tx, stop, join) =
            (self.spec.start)(Arc::clone(&self.entry.shared), self.gen, sink.clone());
        *self.entry.tx.lock().unwrap() = Some(tx);
        self.stop = Some(stop);
        self.join = Some(join);
    }

    /// Ask the child to stop and wait for its thread. Stop is cooperative
    /// (like OTP with an infinite shutdown timeout): an actor stuck in
    /// `handle` blocks this.
    fn stop_and_join(&mut self) {
        if let Some(stop) = self.stop.take() {
            stop();
        }
        *self.entry.tx.lock().unwrap() = None;
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

/// Lifecycle state of a [`Supervisor`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SupervisorState {
    Running,
    Stopped,
    /// Restart intensity exceeded — every child was shut down and the
    /// supervisor gave up.
    Failed,
}

/// A point-in-time snapshot of a supervisor.
#[derive(Clone, Debug)]
pub struct SupervisorStatus {
    pub name: String,
    pub state: SupervisorState,
    /// Restarts within the current intensity window.
    pub recent_restarts: usize,
    pub max_restarts: usize,
    pub children: Vec<ActorStatus>,
}

const SUP_RUNNING: u8 = 0;
const SUP_STOPPED: u8 = 1;
const SUP_FAILED: u8 = 2;

struct SupShared {
    name: String,
    state: AtomicU8,
    recent_restarts: Mutex<VecDeque<Instant>>,
    max_restarts: usize,
    children: Mutex<Vec<Arc<ChildEntry>>>,
}

enum SupMsg {
    Exit(Exit),
    Shutdown,
}

/// OTP-style supervisor: owns a set of children, restarts them per their
/// [`Restart`] policy and the supervisor's [`Strategy`], and gives up
/// (stopping every child) when restarts exceed the intensity cap.
///
/// ```
/// use sutegi_actors::{Actor, ChildSpec, Supervisor, SupervisorState};
///
/// struct Worker;
/// impl Actor for Worker {
///     type Msg = ();
///     fn handle(&mut self, _msg: ()) {}
/// }
///
/// let sup = Supervisor::new("pipeline")
///     .child(ChildSpec::new("worker-1", || Worker))
///     .start();
/// assert_eq!(sup.status().state, SupervisorState::Running);
/// assert_eq!(sup.status().children.len(), 1);
/// sup.stop();
/// ```
pub struct Supervisor {
    name: String,
    strategy: Strategy,
    max_restarts: usize,
    window: Duration,
    children: Vec<ChildSpec>,
    registry: Registry,
}

impl Supervisor {
    /// A supervisor named `name` with [`Strategy::OneForOne`] and an
    /// intensity of 3 restarts per 5 seconds (OTP's defaults).
    pub fn new(name: &str) -> Supervisor {
        Supervisor {
            name: name.to_string(),
            strategy: Strategy::OneForOne,
            max_restarts: 3,
            window: Duration::from_secs(5),
            children: Vec::new(),
            registry: Registry::new(),
        }
    }

    pub fn strategy(mut self, strategy: Strategy) -> Supervisor {
        self.strategy = strategy;
        self
    }

    /// Give up after `max_restarts` within `window`: every child is stopped
    /// and the supervisor enters [`SupervisorState::Failed`]. Without this a
    /// crash-looping child would burn a thread forever.
    pub fn intensity(mut self, max_restarts: usize, window: Duration) -> Supervisor {
        self.max_restarts = max_restarts;
        self.window = window;
        self
    }

    pub fn child(mut self, spec: ChildSpec) -> Supervisor {
        self.children.push(spec);
        self
    }

    /// The registry every child is visible in. Clone it before [`start`] to
    /// hand to your app (`App::actors` / `/__actors`).
    ///
    /// [`start`]: Supervisor::start
    pub fn registry(&self) -> Registry {
        self.registry.clone()
    }

    /// Spawn the supervisor thread and start every child in order.
    pub fn start(self) -> SupervisorHandle {
        // One queue for child exits and shutdown commands alike: the exit
        // sink clones this sender, so no select/forwarder is needed.
        let (tx, rx) = mpsc::channel::<SupMsg>();
        let entries: Vec<Arc<ChildEntry>> = self
            .children
            .iter()
            .map(|spec| {
                Arc::new(ChildEntry {
                    shared: Arc::new(Shared::new(Some(spec.name.clone()), spec.mailbox)),
                    tx: Mutex::new(None),
                })
            })
            .collect();
        let shared = Arc::new(SupShared {
            name: self.name.clone(),
            state: AtomicU8::new(SUP_RUNNING),
            recent_restarts: Mutex::new(VecDeque::new()),
            max_restarts: self.max_restarts,
            children: Mutex::new(entries),
        });
        let registry = self.registry.clone();
        let exit_sink: ExitSink = {
            let tx = tx.clone();
            Arc::new(move |exit| {
                let _ = tx.send(SupMsg::Exit(exit));
            })
        };
        let thread_shared = Arc::clone(&shared);
        let (ready_tx, ready_rx) = mpsc::channel::<()>();
        let join = std::thread::spawn(move || {
            run_supervisor(self, rx, exit_sink, thread_shared, ready_tx)
        });
        // Don't hand back the handle until every child's first generation is
        // started, so `child_ref` works immediately after `start()`.
        let _ = ready_rx.recv();
        SupervisorHandle {
            cmd: tx,
            shared,
            registry,
            join: Mutex::new(Some(join)),
        }
    }
}

/// Handle to a running supervisor.
pub struct SupervisorHandle {
    cmd: Sender<SupMsg>,
    shared: Arc<SupShared>,
    registry: Registry,
    join: Mutex<Option<JoinHandle<()>>>,
}

impl SupervisorHandle {
    /// Snapshot of the supervisor and its children.
    pub fn status(&self) -> SupervisorStatus {
        let state = match self.shared.state.load(Ordering::Relaxed) {
            SUP_RUNNING => SupervisorState::Running,
            SUP_FAILED => SupervisorState::Failed,
            _ => SupervisorState::Stopped,
        };
        SupervisorStatus {
            name: self.shared.name.clone(),
            state,
            recent_restarts: self.shared.recent_restarts.lock().unwrap().len(),
            max_restarts: self.shared.max_restarts,
            children: self
                .shared
                .children
                .lock()
                .unwrap()
                .iter()
                .map(|e| e.shared.snapshot())
                .collect(),
        }
    }

    /// A reference to a supervised child by name — the `whereis` analog.
    /// Returns `None` for unknown names or a type mismatch on `M`.
    ///
    /// The ref survives restarts only in the sense that it addresses the
    /// *current* generation's mailbox; a ref obtained before a restart still
    /// sends into the dead generation's queue and will get
    /// [`TellError::Dead`](crate::TellError) — re-fetch after a crash.
    pub fn child_ref<M: Send + 'static>(&self, name: &str) -> Option<ActorRef<M>> {
        let children = self.shared.children.lock().unwrap();
        let entry = children.iter().find(|e| e.shared.name() == name)?;
        let tx = entry.tx.lock().unwrap();
        let erased = tx.as_ref()?;
        let sender = erased.downcast_ref::<SyncSender<Envelope<M>>>()?;
        Some(ActorRef {
            tx: sender.clone(),
            shared: Arc::clone(&entry.shared),
        })
    }

    /// The child registry (same one [`Supervisor::registry`] returned).
    pub fn registry(&self) -> Registry {
        self.registry.clone()
    }

    /// Ask the supervisor to shut down (stops every child, then exits)
    /// without waiting.
    pub fn shutdown(&self) {
        let _ = self.cmd.send(SupMsg::Shutdown);
    }

    /// Shut down and wait for the supervisor thread to finish.
    pub fn stop(&self) {
        self.shutdown();
        if let Some(join) = self.join.lock().unwrap().take() {
            let _ = join.join();
        }
    }
}

impl Drop for SupervisorHandle {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn run_supervisor(
    sup: Supervisor,
    rx: mpsc::Receiver<SupMsg>,
    exit_sink: ExitSink,
    shared: Arc<SupShared>,
    ready: Sender<()>,
) {
    let entries = shared.children.lock().unwrap().clone();
    // Register every child up front; registry entries survive restarts.
    for entry in &entries {
        sup.registry.register(Arc::clone(&entry.shared));
    }

    let mut children: Vec<Child> = sup
        .children
        .into_iter()
        .zip(entries)
        .map(|(spec, entry)| Child {
            spec,
            entry,
            stop: None,
            join: None,
            gen: 0,
        })
        .collect();
    for child in &mut children {
        child.start(&exit_sink);
    }
    let _ = ready.send(());

    let mut restarts: VecDeque<Instant> = VecDeque::new();
    while let Ok(msg) = rx.recv() {
        match msg {
            SupMsg::Shutdown => break,
            SupMsg::Exit(exit) => {
                let Some(idx) = children
                    .iter()
                    .position(|c| c.entry.shared.id() == exit.id && c.gen == exit.gen)
                else {
                    continue; // stale exit from a generation we already replaced
                };
                // Reap the dead thread.
                if let Some(join) = children[idx].join.take() {
                    let _ = join.join();
                }
                children[idx].stop = None;
                *children[idx].entry.tx.lock().unwrap() = None;
                if !should_restart(children[idx].spec.restart, &exit.reason) {
                    continue;
                }

                // Intensity: one restart *event* per exit handled.
                let now = Instant::now();
                restarts.push_back(now);
                while restarts
                    .front()
                    .is_some_and(|t| now.duration_since(*t) > sup.window)
                {
                    restarts.pop_front();
                }
                *shared.recent_restarts.lock().unwrap() = restarts.clone();
                if restarts.len() > sup.max_restarts {
                    stop_all(&mut children);
                    shared.state.store(SUP_FAILED, Ordering::Relaxed);
                    return;
                }

                let backoff = children[idx].spec.backoff;
                if !backoff.is_zero() {
                    std::thread::sleep(backoff);
                }

                let last = children.len() - 1;
                let range = match sup.strategy {
                    Strategy::OneForOne => idx..=idx,
                    Strategy::RestForOne => idx..=last,
                    Strategy::OneForAll => 0..=last,
                };
                // Stop dependents (reverse order), then restart in start order.
                for child in children.iter_mut().skip(*range.start()).rev() {
                    if child.join.is_some() {
                        child.stop_and_join();
                    }
                }
                for child in children.iter_mut().skip(*range.start()) {
                    child.start(&exit_sink);
                }
            }
        }
    }

    stop_all(&mut children);
    shared.state.store(SUP_STOPPED, Ordering::Relaxed);
}

fn should_restart(policy: Restart, reason: &ExitReason) -> bool {
    match policy {
        Restart::Permanent => true,
        Restart::Transient => reason.crashed(),
        Restart::Temporary => false,
    }
}

fn stop_all(children: &mut [Child]) {
    for child in children.iter_mut().rev() {
        child.stop_and_join();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ActorState, ReplyTo};
    use std::sync::atomic::AtomicUsize;

    fn wait_for(cond: impl Fn() -> bool) {
        for _ in 0..200 {
            if cond() {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("condition not met within 2s");
    }

    /// Crashes on `Boom` as long as `bombs` is armed; `starts` counts factory
    /// invocations (= generations).
    struct Flaky {
        bombs: Arc<AtomicUsize>,
    }

    enum FlakyMsg {
        Boom,
        Ping(ReplyTo<()>),
    }

    impl Actor for Flaky {
        type Msg = FlakyMsg;
        fn handle(&mut self, msg: FlakyMsg) {
            match msg {
                FlakyMsg::Boom => {
                    if self.bombs.load(Ordering::SeqCst) > 0 {
                        panic!("flaky boom");
                    }
                }
                FlakyMsg::Ping(reply) => reply.reply(()),
            }
        }
    }

    #[test]
    fn permanent_child_restarts_after_crash_with_clean_state() {
        let bombs = Arc::new(AtomicUsize::new(1));
        let starts = Arc::new(AtomicUsize::new(0));
        let (b2, s2) = (Arc::clone(&bombs), Arc::clone(&starts));
        let sup = Supervisor::new("s")
            .child(ChildSpec::new("flaky", move || {
                s2.fetch_add(1, Ordering::SeqCst);
                Flaky {
                    bombs: Arc::clone(&b2),
                }
            }))
            .start();

        let flaky: ActorRef<FlakyMsg> = sup.child_ref("flaky").unwrap();
        flaky.tell(FlakyMsg::Boom).unwrap();

        // Restarted (factory ran again) and back to serving.
        wait_for(|| starts.load(Ordering::SeqCst) == 2);
        let flaky: ActorRef<FlakyMsg> = sup.child_ref("flaky").unwrap();
        flaky
            .ask(FlakyMsg::Ping, Duration::from_secs(1))
            .expect("restarted child answers");

        let status = sup.status();
        let child = &status.children[0];
        assert_eq!(child.restarts, 1);
        assert_eq!(child.state, ActorState::Running);
        assert_eq!(child.last_error.as_deref(), Some("flaky boom"));
        sup.stop();
    }

    #[test]
    fn transient_child_stays_down_after_clean_stop() {
        let starts = Arc::new(AtomicUsize::new(0));
        let s2 = Arc::clone(&starts);
        struct Steady;
        impl Actor for Steady {
            type Msg = ();
            fn handle(&mut self, _msg: ()) {}
        }
        let sup = Supervisor::new("t")
            .child(
                ChildSpec::new("steady", move || {
                    s2.fetch_add(1, Ordering::SeqCst);
                    Steady
                })
                .restart(Restart::Transient),
            )
            .start();
        wait_for(|| starts.load(Ordering::SeqCst) == 1);

        let steady: ActorRef<()> = sup.child_ref("steady").unwrap();
        steady.stop();
        wait_for(|| sup.status().children[0].state == ActorState::Stopped);
        // Clean stop under Transient: no restart.
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(starts.load(Ordering::SeqCst), 1);
        assert_eq!(sup.status().children[0].restarts, 0);
        sup.stop();
    }

    #[test]
    fn one_for_all_restarts_siblings_too() {
        struct Quiet;
        impl Actor for Quiet {
            type Msg = ();
            fn handle(&mut self, _msg: ()) {}
        }
        let starts_a = Arc::new(AtomicUsize::new(0));
        let starts_b = Arc::new(AtomicUsize::new(0));
        let (a2, b2) = (Arc::clone(&starts_a), Arc::clone(&starts_b));
        let bombs = Arc::new(AtomicUsize::new(1));
        let b3 = Arc::clone(&bombs);
        let sup = Supervisor::new("all")
            .strategy(Strategy::OneForAll)
            .child(ChildSpec::new("a", move || {
                a2.fetch_add(1, Ordering::SeqCst);
                Flaky {
                    bombs: Arc::clone(&b3),
                }
            }))
            .child(ChildSpec::new("b", move || {
                b2.fetch_add(1, Ordering::SeqCst);
                Quiet
            }))
            .start();
        wait_for(|| starts_a.load(Ordering::SeqCst) == 1 && starts_b.load(Ordering::SeqCst) == 1);

        let a: ActorRef<FlakyMsg> = sup.child_ref("a").unwrap();
        a.tell(FlakyMsg::Boom).unwrap();

        wait_for(|| starts_a.load(Ordering::SeqCst) == 2 && starts_b.load(Ordering::SeqCst) == 2);
        sup.stop();
    }

    #[test]
    fn intensity_exceeded_fails_supervisor_and_stops_children() {
        struct AlwaysBombs;
        impl Actor for AlwaysBombs {
            type Msg = ();
            fn started(&mut self) {
                panic!("die on start");
            }
            fn handle(&mut self, _msg: ()) {}
        }
        let sup = Supervisor::new("doomed")
            .intensity(2, Duration::from_secs(60))
            .child(ChildSpec::new("bomber", || AlwaysBombs))
            .start();
        wait_for(|| sup.status().state == SupervisorState::Failed);
        assert!(sup
            .status()
            .children
            .iter()
            .all(|c| c.state != ActorState::Running));
    }

    #[test]
    fn backoff_delays_the_restart() {
        let bombs = Arc::new(AtomicUsize::new(1));
        let starts = Arc::new(AtomicUsize::new(0));
        let (b2, s2) = (Arc::clone(&bombs), Arc::clone(&starts));
        let sup = Supervisor::new("slow")
            .child(
                ChildSpec::new("flaky", move || {
                    s2.fetch_add(1, Ordering::SeqCst);
                    Flaky {
                        bombs: Arc::clone(&b2),
                    }
                })
                .backoff(Duration::from_millis(300)),
            )
            .start();
        wait_for(|| starts.load(Ordering::SeqCst) == 1);
        let flaky: ActorRef<FlakyMsg> = sup.child_ref("flaky").unwrap();
        flaky.tell(FlakyMsg::Boom).unwrap();
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(starts.load(Ordering::SeqCst), 1, "still inside backoff");
        wait_for(|| starts.load(Ordering::SeqCst) == 2);
        sup.stop();
    }
}
