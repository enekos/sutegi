#![forbid(unsafe_code)]
//! sutegi-zumar — zumar live mode on sutegi (the LiveView analog).
//!
//! One `zumar_runtime::Program` per WebSocket connection: the model lives
//! in the server, DOM events arrive as binary frames, wire-format patches
//! stream back. Unlike client mode, **effects run here** — `delay`/`every`
//! on the bridge's timer thread, `httpGet` on a worker thread (so a live
//! page's fetches happen next to the data, not across the client's link).
//!
//! Reconnect is replay: every program input (dispatch or effect completion)
//! is journaled per session — see [`Journal`] — and a reconnecting client's
//! fresh `Program` is fast-forwarded through the journal, then sent one
//! full render ([`zumar_runtime::Program::rerender`]). TEA updates are pure
//! and the runtime's effect-id allocation is deterministic, so the replayed
//! model is bit-identical to the one the socket drop interrupted.
//!
//! ```ignore
//! App::new("counter-live")
//!     .ws("/live", "zumar live socket.",
//!         Live::new(|_req| counter::program())
//!             .journal(EventJournal::new(Db::open("live.db")?)?)
//!             .ws())
//!     .serve()
//! ```

mod frames;
mod http;
mod journal;
mod scheduler;

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

pub use frames::Frame;
pub use journal::{EventJournal, Journal, MemJournal};

use scheduler::{Fire, Scheduler};
use sutegi_pubsub::{Broker, PubSub};
use sutegi_web::ws::{Conn, Msg};
use sutegi_web::{Request, Ws};
use zumar_core::EventPayload;
use zumar_runtime::effects::{CmdSpec, FxPayload, SubDelta, SubSpec};
use zumar_runtime::{Program, Update};

/// Builder for a live endpoint. `factory` runs once per connection (and per
/// reconnect replay) with the mount `Request` — query params, cookies and
/// headers are the "session/auth params at mount". It must be deterministic
/// per session for replay to hold: derive state from the request, not from
/// clocks or randomness.
pub struct Live<M, Ms> {
    factory: Factory<M, Ms>,
    journal: Option<Arc<dyn Journal>>,
    http_base: Option<String>,
    pubsub: Option<Arc<dyn Broker>>,
}

type Factory<M, Ms> = Arc<dyn Fn(&Request) -> Program<M, Ms> + Send + Sync>;

/// One live endpoint: `Live::new(factory).ws()`, or [`live`] for defaults.
impl<M: Send + 'static, Ms: Clone + Send + 'static> Live<M, Ms> {
    pub fn new(factory: impl Fn(&Request) -> Program<M, Ms> + Send + Sync + 'static) -> Self {
        Live {
            factory: Arc::new(factory),
            journal: None,
            http_base: None,
            pubsub: None,
        }
    }

    /// Share a pubsub broker across this endpoint's connections (and,
    /// optionally, with the rest of the app — pass a clone of one held in
    /// `App::state`). A `topic(name, Ctor)` sub subscribes the connection;
    /// a `publish(topic, msg)` cmd fans out to every subscriber. Defaults to
    /// a fresh in-process [`PubSub`] scoped to this endpoint.
    pub fn pubsub(mut self, broker: impl Broker + 'static) -> Self {
        self.pubsub = Some(Arc::new(broker));
        self
    }

    /// Journal program inputs per session (`?session=<id>` on the socket
    /// URL) and replay them on reconnect. Without a journal, a reconnect is
    /// a fresh mount.
    pub fn journal(mut self, journal: impl Journal) -> Self {
        self.journal = Some(Arc::new(journal));
        self
    }

    /// Base URL for relative `httpGet` targets (default
    /// `http://127.0.0.1:$PORT` — the app itself).
    pub fn http_base(mut self, base: impl Into<String>) -> Self {
        self.http_base = Some(base.into());
        self
    }

    /// Wire everything into a `Ws` for `App::ws`. Chain `Ws` policies on
    /// the result (origin checks etc.) if the endpoint needs them.
    pub fn ws(self) -> Ws {
        let http_base = self.http_base.unwrap_or_else(|| {
            let port = std::env::var("PORT").unwrap_or_else(|_| "8080".to_string());
            format!("http://127.0.0.1:{port}")
        });
        let shared = Arc::new(Shared {
            sessions: Mutex::new(HashMap::new()),
            factory: self.factory,
            journal: self.journal,
            http_base,
            pubsub: self.pubsub.unwrap_or_else(|| Arc::new(PubSub::new())),
            scheduler: OnceLock::new(),
        });
        // The scheduler thread holds a Weak so a dropped endpoint can wind
        // down; created after the Arc exists, set exactly once.
        let weak = Arc::downgrade(&shared);
        let _ = shared.scheduler.set(Scheduler::new(move |conn, fire| {
            if let Some(shared) = weak.upgrade() {
                shared.on_timer(conn, fire);
            }
        }));

        let open = Arc::clone(&shared);
        let message = Arc::clone(&shared);
        let close = Arc::clone(&shared);
        Ws::new()
            .on_open(move |conn: &Conn, req: &Request| open.on_open(conn, req))
            .on_message(move |conn: &Conn, msg: Msg| message.on_message(conn, msg))
            .on_close(move |conn: &Conn, _code| close.on_close(conn.id()))
    }
}

/// A live endpoint with defaults: no journal, effects against the local app.
pub fn live<M: Send + 'static, Ms: Clone + Send + 'static>(
    factory: impl Fn(&Request) -> Program<M, Ms> + Send + Sync + 'static,
) -> Ws {
    Live::new(factory).ws()
}

struct Session<M, Ms> {
    program: Mutex<Program<M, Ms>>,
    conn: Conn,
    /// Journal stream id (validated client session id), when journaling.
    stream: Option<String>,
    /// Live topic subscriptions: sub id → (topic, broker subscription id),
    /// so a `Stop` delta or a connection close can unsubscribe from pubsub.
    topics: Mutex<HashMap<u32, (String, u64)>>,
}

struct Shared<M, Ms> {
    sessions: Mutex<HashMap<u64, Arc<Session<M, Ms>>>>,
    factory: Factory<M, Ms>,
    journal: Option<Arc<dyn Journal>>,
    http_base: String,
    pubsub: Arc<dyn Broker>,
    scheduler: OnceLock<Scheduler>,
}

impl<M: Send + 'static, Ms: Clone + Send + 'static> Shared<M, Ms> {
    fn scheduler(&self) -> &Scheduler {
        self.scheduler.get().expect("scheduler initialized in ws()")
    }

    fn session(&self, conn: u64) -> Option<Arc<Session<M, Ms>>> {
        self.sessions.lock().unwrap().get(&conn).cloned()
    }

    fn on_open(self: &Arc<Self>, conn: &Conn, req: &Request) {
        let mut program = (self.factory)(req);

        let stream = self.journal.as_ref().and_then(|_| {
            req.query
                .split('&')
                .find_map(|kv| kv.strip_prefix("session="))
                .filter(|s| journal::valid_session(s))
                .map(str::to_string)
        });

        // Reconnect: fast-forward a fresh program through the journal, then
        // one full render. Journaled completions land on the ids the fresh
        // run allocates — same inputs, same order, same ids.
        let journaled = match (&self.journal, &stream) {
            (Some(journal), Some(stream)) => match journal.load(stream) {
                Ok(frames) => frames,
                Err(e) => {
                    eprintln!("sutegi-zumar: journal load failed for {stream}: {e}");
                    Vec::new()
                }
            },
            _ => Vec::new(),
        };

        let first = if journaled.is_empty() {
            program.initial_render()
        } else {
            program.initial_render(); // consumed; its cmds' completions replay
            for bytes in &journaled {
                if let Ok(frame) = frames::decode(bytes) {
                    apply(&mut program, &frame);
                }
            }
            program.rerender()
        };

        let session = Arc::new(Session {
            program: Mutex::new(program),
            conn: conn.clone(),
            stream,
            topics: Mutex::new(HashMap::new()),
        });
        self.sessions
            .lock()
            .unwrap()
            .insert(conn.id(), Arc::clone(&session));

        // Patches/tree go to the client; cmds and sub starts stay here.
        conn.send_binary(
            &zumar_runtime::InitialRender {
                root: first.root,
                events: first.events,
                cmds: Vec::new(),
                subs: Vec::new(),
            }
            .to_bytes(),
        );
        self.run_effects(conn.id(), first.cmds, first.subs);
    }

    fn on_message(self: &Arc<Self>, conn: &Conn, msg: Msg) {
        let Msg::Binary(data) = msg else { return };
        let frame = match frames::decode(&data) {
            Ok(frame) => frame,
            Err(e) => {
                conn.close(1002, &e);
                return;
            }
        };
        self.feed(conn.id(), &frame, &data);
    }

    fn on_close(&self, conn: u64) {
        let session = self.sessions.lock().unwrap().remove(&conn);
        // Drop the connection's pubsub subscriptions so a closed socket
        // stops receiving fan-out (its callback holds only a Weak, but
        // unsubscribing frees the registry slot promptly).
        if let Some(session) = session {
            for (_sub_id, (topic, broker_id)) in session.topics.lock().unwrap().drain() {
                self.pubsub.unsubscribe(&topic, broker_id);
            }
        }
        self.scheduler().drop_conn(conn);
    }

    /// Journal an input, apply it to the session's program, ship the result.
    /// The journal write happens *inside* the program lock: inputs can
    /// arrive from the reactor, the timer thread and HTTP workers at once,
    /// and replay is only sound if journal order equals apply order.
    fn feed(self: &Arc<Self>, conn: u64, frame: &Frame, bytes: &[u8]) {
        let Some(session) = self.session(conn) else {
            return;
        };
        let update = {
            let mut program = session.program.lock().unwrap();
            if let (Some(journal), Some(stream)) = (&self.journal, &session.stream) {
                if let Err(e) = journal.append(stream, bytes) {
                    eprintln!("sutegi-zumar: journal append failed for {stream}: {e}");
                }
            }
            apply(&mut program, frame)
        };
        let Update {
            patches,
            events,
            cmds,
            subs,
        } = update;
        session.conn.send_binary(
            &Update {
                patches,
                events,
                cmds: Vec::new(),
                subs: Vec::new(),
            }
            .to_bytes(),
        );
        self.run_effects(conn, cmds, subs);
    }

    /// Execute a step's effects server-side: timers on the scheduler, HTTP
    /// on a worker thread. Completions come back through [`Shared::feed`],
    /// so they are journaled exactly like client dispatches.
    fn run_effects(
        self: &Arc<Self>,
        conn: u64,
        cmds: Vec<zumar_runtime::effects::CmdOut>,
        subs: Vec<SubDelta>,
    ) {
        for cmd in cmds {
            match cmd.spec {
                CmdSpec::Delay { ms } => self.scheduler().delay(conn, cmd.id, ms),
                CmdSpec::HttpGet { url } => {
                    let shared = Arc::clone(self);
                    let base = self.http_base.clone();
                    std::thread::spawn(move || {
                        let (ok, status, body) = http::get(&base, &url);
                        let frame = Frame::Resolve {
                            id: cmd.id,
                            ok,
                            status,
                            body,
                        };
                        shared.feed(conn, &frame, &frames::encode(&frame));
                    });
                }
                // Fire-and-forget: fan out to every subscriber (this
                // connection's own topic sub included, and every *other*
                // connection's — the multi-client path).
                CmdSpec::Publish { topic, message } => self.pubsub.publish(&topic, &message),
            }
        }
        for delta in subs {
            match delta {
                SubDelta::Start {
                    id,
                    spec: SubSpec::Every { ms },
                } => self.scheduler().start_every(conn, id, ms),
                SubDelta::Start {
                    id,
                    spec: SubSpec::Topic { name },
                } => self.subscribe_topic(conn, id, name),
                SubDelta::Stop { id } => {
                    self.scheduler().stop_every(conn, id);
                    self.unsubscribe_topic(conn, id);
                }
            }
        }
    }

    /// Subscribe a connection's `topic(...)` sub to the pubsub broker. Each
    /// published message becomes a `notify(id, body=message)` fed into that
    /// connection's program — journaled, so it replays on reconnect.
    fn subscribe_topic(self: &Arc<Self>, conn: u64, sub_id: u32, topic: String) {
        let Some(session) = self.session(conn) else {
            return;
        };
        let weak = Arc::downgrade(self);
        let topic_for_cb = topic.clone();
        let broker_id = self.pubsub.subscribe(
            &topic,
            Arc::new(move |message: &str| {
                let Some(shared) = weak.upgrade() else { return };
                let _ = &topic_for_cb; // captured for clarity/debugging
                let frame = Frame::Notify {
                    id: sub_id,
                    now: 0,
                    body: message.to_string(),
                };
                shared.feed(conn, &frame, &frames::encode(&frame));
            }),
        );
        session
            .topics
            .lock()
            .unwrap()
            .insert(sub_id, (topic, broker_id));
    }

    fn unsubscribe_topic(&self, conn: u64, sub_id: u32) {
        if let Some(session) = self.session(conn) {
            if let Some((topic, broker_id)) = session.topics.lock().unwrap().remove(&sub_id) {
                self.pubsub.unsubscribe(&topic, broker_id);
            }
        }
    }

    fn on_timer(self: &Arc<Self>, conn: u64, fire: Fire) {
        let frame = match fire {
            Fire::Delay { id } => Frame::Resolve {
                id,
                ok: true,
                status: 0,
                body: String::new(),
            },
            Fire::Every { id } => Frame::Notify {
                id,
                now: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis() as u64)
                    .unwrap_or(0),
                body: String::new(),
            },
        };
        self.feed(conn, &frame, &frames::encode(&frame));
    }
}

/// One program input, decoded. Shared by the live path and journal replay.
fn apply<M, Ms: Clone>(program: &mut Program<M, Ms>, frame: &Frame) -> Update {
    match frame {
        Frame::Dispatch {
            path,
            name,
            value,
            checked,
            key,
        } => {
            let payload = EventPayload {
                value: value.clone(),
                checked: *checked,
                key: key.clone(),
            };
            program.dispatch(path, name, &payload)
        }
        Frame::Resolve {
            id,
            ok,
            status,
            body,
        } => {
            let payload = FxPayload {
                ok: Some(*ok),
                status: Some(*status),
                body: Some(body.clone()),
                now: None,
            };
            program.resolve(*id, &payload)
        }
        Frame::Notify { id, now, body } => {
            // `every` ticks carry `now`; `topic` fires carry `body`. One
            // notify path serves both — the sub's callback reads whichever
            // field it wants.
            let payload = FxPayload {
                now: Some(*now as f64),
                body: if body.is_empty() {
                    None
                } else {
                    Some(body.clone())
                },
                ..FxPayload::default()
            };
            program.notify(*id, &payload)
        }
    }
}

#[cfg(test)]
mod tests;
