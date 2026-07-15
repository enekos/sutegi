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
use std::sync::atomic::{AtomicUsize, Ordering};
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
    guard: Option<Guard>,
    allowed_origins: Option<Vec<String>>,
    identify: Option<Identify>,
    trim_interval: Option<std::time::Duration>,
    trim_max_age: Option<std::time::Duration>,
}

type Factory<M, Ms> = Arc<dyn Fn(&Request) -> Program<M, Ms> + Send + Sync>;
type Guard = Arc<dyn Fn(&Request) -> bool + Send + Sync>;
type Identify = Arc<dyn Fn(&Request) -> Option<String> + Send + Sync>;

/// One live endpoint: `Live::new(factory).ws()`, or [`live`] for defaults.
impl<M: Send + 'static, Ms: Clone + Send + 'static> Live<M, Ms> {
    pub fn new(factory: impl Fn(&Request) -> Program<M, Ms> + Send + Sync + 'static) -> Self {
        Live {
            factory: Arc::new(factory),
            journal: None,
            http_base: None,
            pubsub: None,
            guard: None,
            allowed_origins: None,
            identify: None,
            trim_interval: None,
            trim_max_age: None,
        }
    }

    /// Gate the live socket: `guard(req)` runs on the WS upgrade and a
    /// `false` closes the connection (1008) before any program is mounted.
    /// This is how a live page is made private — pair it with [`sutegi_auth`]
    /// by capturing an `Auth` and returning `auth.user_id(req).is_some()`.
    /// The factory sees the same `Request`, so it can read the user and
    /// render per-session state.
    ///
    /// **A guarded socket also enforces `Origin`.** A WebSocket upgrade is
    /// NOT subject to the same-origin policy, and the browser attaches
    /// first-party cookies to a *cross-site* upgrade — so a cookie-based
    /// guard alone would let `evil.com` open a socket that mounts as a
    /// logged-in victim and drive it (cross-site WebSocket hijacking, CWE-1385).
    /// Therefore, whenever a guard is set you should scope the permitted
    /// origins with [`check_origin`](Self::check_origin); if you don't, the
    /// bridge falls back to a strict **same-origin** check (the `Origin`
    /// header's host must equal the `Host` header, and a missing `Origin` is
    /// rejected). Only with that origin gate in place are live form submits —
    /// ordinary dispatches over the authenticated socket — free of cross-site
    /// request forgery.
    pub fn guard(mut self, guard: impl Fn(&Request) -> bool + Send + Sync + 'static) -> Self {
        self.guard = Some(Arc::new(guard));
        self
    }

    /// Restrict which `Origin`s may open this socket (matched exactly, before
    /// the 101 upgrade). Required in practice for any [`guard`](Self::guard)ed
    /// socket to stop cross-site WebSocket hijacking. If omitted on a guarded
    /// socket, the bridge enforces a strict same-origin fallback instead of
    /// silently accepting cross-origin upgrades.
    pub fn check_origin<I, S>(mut self, origins: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.allowed_origins = Some(origins.into_iter().map(Into::into).collect());
        self
    }

    /// Bind journaled sessions to an authenticated principal. When set, the
    /// per-connection journal stream is namespaced by `identify(req)` (e.g. the
    /// logged-in user id), so a client's `?session=` id can only ever address
    /// its own journals — it cannot load or append to another user's stream
    /// (CWE-639, IDOR). Return `None` to skip journaling for that request. A
    /// guarded socket that journals but has no `identify` disables journaling
    /// per connection (a fresh mount) rather than trust the client-chosen key.
    pub fn identify(
        mut self,
        identify: impl Fn(&Request) -> Option<String> + Send + Sync + 'static,
    ) -> Self {
        self.identify = Some(Arc::new(identify));
        self
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
    /// a fresh mount. For an authenticated endpoint, pair with
    /// [`identify`](Self::identify) so each user's journal is isolated — the
    /// client-supplied `?session=` alone is NOT an authorization boundary.
    pub fn journal(mut self, journal: impl Journal) -> Self {
        self.journal = Some(Arc::new(journal));
        self
    }

    /// Automatically garbage-collect abandoned session journals.
    /// A background thread will run every `interval` and delete sessions
    /// whose most recent event is older than `max_age`.
    pub fn trim_journal(mut self, interval: std::time::Duration, max_age: std::time::Duration) -> Self {
        self.trim_interval = Some(interval);
        self.trim_max_age = Some(max_age);
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
        
        if let (Some(interval), Some(age), Some(ref journal)) = (self.trim_interval, self.trim_max_age, &self.journal) {
            let journal = Arc::clone(journal);
            std::thread::spawn(move || loop {
                std::thread::sleep(interval);
                let cutoff = std::time::SystemTime::now()
                    .checked_sub(age)
                    .unwrap_or(std::time::SystemTime::UNIX_EPOCH)
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_secs() as i64;
                if let Err(e) = journal.trim(cutoff) {
                    eprintln!("sutegi-zumar: journal trim error: {}", e);
                }
            });
        }

        // A guarded socket must validate Origin (a WS upgrade ignores the
        // same-origin policy but still carries first-party cookies). With an
        // explicit allowlist, `check_origin` rejects foreign origins before
        // the 101; without one, `on_open` enforces a strict same-origin
        // fallback so a guarded socket is never silently cross-origin-open.
        let require_same_origin = self.guard.is_some() && self.allowed_origins.is_none();
        let allowed_origins = self.allowed_origins;
        // A guarded, journaling endpoint with no principal binding cannot
        // safely reuse a client-supplied `?session=` key (cross-session
        // replay, CWE-639); `on_open` disables journaling for such connections.
        if self.journal.is_some() && self.guard.is_some() && self.identify.is_none() {
            eprintln!(
                "sutegi-zumar: `Live` has a guard + journal but no `identify(...)`; \
                 journaling is disabled per connection to avoid cross-session replay. \
                 Call `.identify(|req| ...)` to namespace journals per authenticated user."
            );
        }
        let shared = Arc::new(Shared {
            sessions: Mutex::new(HashMap::new()),
            factory: self.factory,
            journal: self.journal,
            http_base,
            pubsub: self.pubsub.unwrap_or_else(|| Arc::new(PubSub::new())),
            guard: self.guard,
            require_same_origin,
            identify: self.identify,
            in_flight_effects: AtomicUsize::new(0),
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
        let ws = Ws::new()
            .on_open(move |conn: &Conn, req: &Request| open.on_open(conn, req))
            .on_message(move |conn: &Conn, msg: Msg| message.on_message(conn, msg))
            .on_close(move |conn: &Conn, _code| close.on_close(conn.id()));
        match allowed_origins {
            Some(origins) => ws.check_origin(origins),
            None => ws,
        }
    }
}

/// A live endpoint with defaults: no journal, effects against the local app.
pub fn live<M: Send + 'static, Ms: Clone + Send + 'static>(
    factory: impl Fn(&Request) -> Program<M, Ms> + Send + Sync + 'static,
) -> Ws {
    Live::new(factory).ws()
}

/// Strict same-origin check for a WS upgrade: the `Origin` header's authority
/// (host[:port]) must equal the `Host` header. A missing or scheme-less
/// `Origin` fails closed — a browser always sends a well-formed `Origin` on a
/// WebSocket handshake.
fn origin_matches_host(req: &Request) -> bool {
    origin_authority_matches(req.header("origin"), req.header("host"))
}

fn origin_authority_matches(origin: Option<&str>, host: Option<&str>) -> bool {
    let (Some(origin), Some(host)) = (origin, host) else {
        return false;
    };
    let authority = match origin.split_once("://") {
        Some((_scheme, rest)) => rest.split('/').next().unwrap_or(""),
        None => return false,
    };
    !authority.is_empty() && authority.eq_ignore_ascii_case(host)
}

#[cfg(test)]
mod origin_tests {
    use super::origin_authority_matches as matches;

    #[test]
    fn same_origin_is_allowed() {
        assert!(matches(Some("https://app.com"), Some("app.com")));
        assert!(matches(Some("http://app.com:8080"), Some("app.com:8080")));
        assert!(matches(Some("https://APP.com"), Some("app.com"))); // host case-insensitive
    }

    #[test]
    fn cross_origin_or_malformed_is_rejected() {
        assert!(!matches(Some("https://evil.com"), Some("app.com")));
        assert!(!matches(None, Some("app.com"))); // missing Origin fails closed
        assert!(!matches(Some("https://app.com"), None));
        assert!(!matches(Some("app.com"), Some("app.com"))); // no scheme => not an Origin
        assert!(!matches(Some("https://app.com:443"), Some("app.com"))); // strict: port differs
    }
}

/// Choose the per-connection journal stream key. When the app supplies a
/// principal (via [`Live::identify`]) the client's `?session=` id is scoped
/// under it, so one client can never load or append to another principal's
/// journal (CWE-639). Anonymous journaling (no guard) keeps the client key —
/// there is no cross-user secret to protect. A guarded socket with no
/// principal returns `None` (no journaling) rather than trust the client key.
fn journal_stream(guarded: bool, principal: Option<&str>, client: Option<&str>) -> Option<String> {
    let client = client?;
    match principal {
        Some(p) => Some(format!("{}:{}", sanitize_principal(p), client)),
        None if !guarded => Some(client.to_string()),
        None => None,
    }
}

/// Keep an app-supplied principal safe to embed in a journal stream name:
/// ASCII alphanumerics, `_` and `-` survive, everything else becomes `_`,
/// capped so a pathological id can't blow up the key.
fn sanitize_principal(p: &str) -> String {
    p.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .take(128)
        .collect()
}

#[cfg(test)]
mod journal_stream_tests {
    use super::{journal_stream, sanitize_principal};

    #[test]
    fn principal_scopes_the_client_key() {
        // Two users sending the SAME client session id get distinct streams.
        let a = journal_stream(true, Some("user-1"), Some("abc"));
        let b = journal_stream(true, Some("user-2"), Some("abc"));
        assert_eq!(a.as_deref(), Some("user-1:abc"));
        assert_ne!(a, b);
    }

    #[test]
    fn guarded_without_principal_refuses_to_journal() {
        assert_eq!(journal_stream(true, None, Some("abc")), None);
    }

    #[test]
    fn anonymous_journaling_keeps_the_client_key() {
        assert_eq!(
            journal_stream(false, None, Some("abc")).as_deref(),
            Some("abc")
        );
    }

    #[test]
    fn no_client_id_means_no_journal() {
        assert_eq!(journal_stream(false, None, None), None);
        assert_eq!(journal_stream(true, Some("u"), None), None);
    }

    #[test]
    fn principal_is_sanitized_for_stream_safety() {
        assert_eq!(sanitize_principal("a/b:c d"), "a_b_c_d");
        assert_eq!(sanitize_principal("ok-1_2"), "ok-1_2");
    }
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
    guard: Option<Guard>,
    require_same_origin: bool,
    identify: Option<Identify>,
    in_flight_effects: AtomicUsize,
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
        // Auth gate: reject the upgrade before mounting anything. 1008 =
        // policy violation (RFC 6455). zumar-live.js won't auto-reconnect a
        // policy close, so a logged-out client stops retrying.
        if let Some(guard) = &self.guard {
            if !guard(req) {
                conn.close(1008, "unauthorized");
                return;
            }
        }

        // Cross-site WebSocket hijacking backstop: a guarded socket with no
        // explicit Origin allowlist requires the upgrade to be same-origin.
        // (With an allowlist, `check_origin` already rejected foreign origins
        // before the 101, so `require_same_origin` is false here.)
        if self.require_same_origin && !origin_matches_host(req) {
            conn.close(1008, "origin not allowed");
            return;
        }

        let mut program = (self.factory)(req);

        // Select the journal stream. Namespaced by the authenticated principal
        // when `identify` is set, so a client's `?session=` can only ever
        // address its own journals (CWE-639); a guarded socket without a
        // principal refuses to journal rather than trust the client-chosen key.
        let stream = self.journal.as_ref().and_then(|_| {
            let client = req
                .query
                .split('&')
                .find_map(|kv| kv.strip_prefix("session="))
                .filter(|s| journal::valid_session(s));
            let principal = self.identify.as_ref().and_then(|id| id(req));
            journal_stream(self.guard.is_some(), principal.as_deref(), client)
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
                    let base = self.http_base.clone();
                    self.spawn_effect(conn, cmd.id, move || http::get(&base, &url));
                }
                CmdSpec::HttpPost { url, body } => {
                    let base = self.http_base.clone();
                    self.spawn_effect(conn, cmd.id, move || http::post(&base, &url, &body));
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

    /// Run a server-side fetch on its own thread, bounded so a dispatch flood
    /// (or an `every` sub firing fetches) can't spawn unbounded threads and
    /// exhaust memory/FDs. Over the cap the effect resolves immediately as a
    /// transport error instead of spawning.
    fn spawn_effect(
        self: &Arc<Self>,
        conn: u64,
        id: u32,
        fetch: impl FnOnce() -> (bool, u16, String) + Send + 'static,
    ) {
        const MAX_INFLIGHT: usize = 64;
        if self.in_flight_effects.fetch_add(1, Ordering::Relaxed) >= MAX_INFLIGHT {
            self.in_flight_effects.fetch_sub(1, Ordering::Relaxed);
            let frame = Frame::Resolve {
                id,
                ok: false,
                status: 0,
                body: "effect rejected: too many concurrent server-side requests".into(),
            };
            self.feed(conn, &frame, &frames::encode(&frame));
            return;
        }
        let shared = Arc::clone(self);
        std::thread::spawn(move || {
            // Decrement the in-flight count even if the fetch or feed panics.
            struct Guard<'a>(&'a AtomicUsize);
            impl Drop for Guard<'_> {
                fn drop(&mut self) {
                    self.0.fetch_sub(1, Ordering::Relaxed);
                }
            }
            let _guard = Guard(&shared.in_flight_effects);
            let (ok, status, body) = fetch();
            let frame = Frame::Resolve {
                id,
                ok,
                status,
                body,
            };
            shared.feed(conn, &frame, &frames::encode(&frame));
        });
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
