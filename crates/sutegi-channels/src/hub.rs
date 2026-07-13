//! The channel runtime: membership, dispatch, and broker-backed fan-out.
//!
//! One [`ChannelHub`] serves one WebSocket endpoint. Every broadcast rides
//! the [`Broker`] seam on the pubsub topic `stg:chan:<topic>` — with the
//! in-process broker that is a function call; with `PgPubSub` the same
//! broadcast reaches every pod. Local members are fanned out with one
//! shared pre-encoded frame (`sutegi_ws::broadcast`), so a large room costs
//! one encode.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, Weak};

use sutegi_http::Request;
use sutegi_json::Json;
use sutegi_pubsub::{Broker, BrokerExt, PubSub};
use sutegi_ws::{text_frame, Conn, Msg};

use crate::channel::{Channel, LeaveReason, Reply};
use crate::protocol::{
    self, error_payload, reply_payload, topic_matches, CONTROL_TOPIC, EV_CLOSE, EV_ERROR,
    EV_HEARTBEAT, EV_JOIN, EV_LEAVE, EV_REPLY, RESERVED_PREFIX,
};

/// The pre-101 handshake gate (see `Ws::authorize`).
pub type AuthorizeFn = Arc<dyn Fn(&Request) -> bool + Send + Sync>;

/// Namespace prefix for hub traffic on the pubsub broker, so channel
/// broadcasts never collide with an app's own broker topics.
fn pubsub_topic(topic: &str) -> String {
    format!("stg:chan:{topic}")
}

/// Builder for a [`ChannelHub`].
///
/// ```ignore
/// let hub = Channels::new()
///     .channel(Channel::new("room:*").on_join(...).on("new_msg", ...))
///     .broker(PgPubSub::connect(&pg_cfg)?)      // omit for single-pod
///     .check_origin(["https://app.example.com"])
///     .build();
/// ```
#[derive(Default)]
pub struct Channels {
    channels: Vec<Channel>,
    broker: Option<Arc<dyn Broker>>,
    allowed_origins: Option<Vec<String>>,
    authorize: Option<AuthorizeFn>,
    #[cfg(feature = "presence")]
    presence_interval: Option<std::time::Duration>,
}

impl Channels {
    pub fn new() -> Channels {
        Channels::default()
    }

    /// Register a channel. First matching pattern wins at join time.
    pub fn channel(mut self, channel: Channel) -> Channels {
        self.channels.push(channel);
        self
    }

    /// The pubsub broker broadcasts ride. Default: a private in-process
    /// [`PubSub`] (single pod). Pass a `PgPubSub` for cross-pod fan-out.
    pub fn broker(mut self, broker: impl Broker + 'static) -> Channels {
        self.broker = Some(Arc::new(broker));
        self
    }

    /// Origin allowlist for the WebSocket handshake — the CSWSH guard. Any
    /// endpoint that authenticates by cookie MUST set this (see `Ws::check_origin`).
    pub fn check_origin<I, S>(mut self, origins: I) -> Channels
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.allowed_origins = Some(origins.into_iter().map(Into::into).collect());
        self
    }

    /// Token/cookie gate run before the `101` (see `Ws::authorize`).
    pub fn authorize(mut self, f: impl Fn(&Request) -> bool + Send + Sync + 'static) -> Channels {
        self.authorize = Some(Arc::new(f));
        self
    }

    /// How often this pod re-publishes its presence state (remote pods are
    /// expired after ~2.5×). Default 30 s. Shorter = faster crash detection,
    /// more broker chatter.
    #[cfg(feature = "presence")]
    pub fn presence_heartbeat(mut self, interval: std::time::Duration) -> Channels {
        self.presence_interval = Some(interval);
        self
    }

    pub fn build(self) -> ChannelHub {
        let inner = Arc::new_cyclic(|weak: &Weak<HubInner>| HubInner {
            channels: self.channels,
            broker: self.broker.unwrap_or_else(|| Arc::new(PubSub::new())),
            instance: unique_id(),
            conns: Mutex::new(HashMap::new()),
            topics: Mutex::new(HashMap::new()),
            weak: weak.clone(),
            allowed_origins: self.allowed_origins,
            authorize: self.authorize,
            #[cfg(feature = "presence")]
            presence: {
                let mut p = crate::presence::PresenceState::default();
                if let Some(i) = self.presence_interval {
                    p.interval = i;
                }
                p
            },
        });
        ChannelHub { inner }
    }
}

/// The runtime handle: cheap to clone, share it with any thread that needs
/// to broadcast. Wire it to a WebSocket endpoint via `App::channels` (the
/// `sutegi` facade) or manually with [`ChannelHub::on_open`] /
/// [`ChannelHub::on_message`] / [`ChannelHub::on_close`].
#[derive(Clone)]
pub struct ChannelHub {
    pub(crate) inner: Arc<HubInner>,
}

pub(crate) struct HubInner {
    pub(crate) channels: Vec<Channel>,
    pub(crate) broker: Arc<dyn Broker>,
    /// Unique per hub instance (= per pod), for cross-pod sender exclusion.
    pub(crate) instance: String,
    conns: Mutex<HashMap<u64, ConnEntry>>,
    topics: Mutex<HashMap<String, TopicEntry>>,
    weak: Weak<HubInner>,
    allowed_origins: Option<Vec<String>>,
    authorize: Option<AuthorizeFn>,
    #[cfg(feature = "presence")]
    pub(crate) presence: crate::presence::PresenceState,
}

struct ConnEntry {
    req: Arc<Request>,
    /// topic → this connection's membership.
    joins: HashMap<String, JoinEntry>,
}

#[derive(Clone)]
struct JoinEntry {
    join_ref: Option<String>,
    assigns: Arc<Mutex<Json>>,
}

struct TopicEntry {
    /// Broker subscription backing this topic, held while any local member
    /// remains.
    sub_id: u64,
    members: HashMap<u64, Conn>,
}

/// One member's view of one channel membership — what every channel
/// callback receives. Addressing the member, the room, and per-membership
/// state (`assigns`) all happen through it.
type AfterJoinFn = Box<dyn FnOnce(&Socket) + Send>;

#[derive(Clone)]
pub struct Socket {
    hub: ChannelHub,
    conn: Conn,
    req: Arc<Request>,
    topic: String,
    join_ref: Option<String>,
    assigns: Arc<Mutex<Json>>,
    /// Armed only while the join callback runs: work queued here executes
    /// after the member is admitted and the ok reply is on the wire.
    pending: Option<Arc<Mutex<Vec<AfterJoinFn>>>>,
}

impl Socket {
    #[cfg(feature = "presence")]
    pub(crate) fn hub_inner(&self) -> &Arc<HubInner> {
        &self.hub.inner
    }

    /// The underlying connection id (stable for the connection's life).
    pub fn id(&self) -> u64 {
        self.conn.id()
    }

    /// Run `f` **after** this join completes — after the membership exists
    /// and the ok reply is on the wire. Inside `on_join` the member is not
    /// admitted yet, so a push/broadcast there reaches the client *before*
    /// its join reply (or misses it entirely); defer such work here. If the
    /// join is refused the closure never runs. Outside a join callback `f`
    /// runs immediately.
    pub fn after_join(&self, f: impl FnOnce(&Socket) + Send + 'static) {
        match &self.pending {
            Some(queue) => queue.lock().unwrap().push(Box::new(f)),
            None => f(self),
        }
    }

    /// The topic this membership is on (the concrete one, e.g. `room:7`,
    /// not the channel's pattern).
    pub fn topic(&self) -> &str {
        &self.topic
    }

    /// The HTTP upgrade request that opened this connection — headers,
    /// cookies, path, query — for identity decisions in `on_join`.
    pub fn request(&self) -> &Request {
        &self.req
    }

    /// Set one key of this membership's assigns (per-join state that lives
    /// until leave/disconnect).
    pub fn assign(&self, key: &str, value: Json) {
        let mut guard = self.assigns.lock().unwrap();
        if let Json::Obj(map) = &mut *guard {
            map.insert(key.to_string(), value);
        }
    }

    /// Read one key of this membership's assigns.
    pub fn assign_get(&self, key: &str) -> Option<Json> {
        self.assigns.lock().unwrap().get(key).cloned()
    }

    /// Push an event to **this member only** (stamped with its `join_ref`).
    pub fn push(&self, event: &str, payload: &Json) {
        self.conn.send_text(&protocol::serialize(
            &self.topic,
            event,
            None,
            self.join_ref.as_deref(),
            payload,
        ));
    }

    /// Broadcast to every member of this topic — all pods, sender included.
    pub fn broadcast(&self, event: &str, payload: &Json) {
        self.hub.broadcast(&self.topic, event, payload);
    }

    /// Broadcast to every member of this topic **except this one**.
    pub fn broadcast_from(&self, event: &str, payload: &Json) {
        self.hub
            .inner
            .publish(&self.topic, event, payload, Some(self.conn.id()));
    }

    /// End this membership from the server side: the member gets an
    /// `stg:close`, the leave callback runs with [`LeaveReason::Kicked`].
    pub fn kick(&self, reason: &str) {
        self.conn.send_text(&protocol::serialize(
            &self.topic,
            EV_CLOSE,
            None,
            self.join_ref.as_deref(),
            &error_payload(reason),
        ));
        self.hub
            .inner
            .teardown(&self.conn, &self.topic, LeaveReason::Kicked);
    }
}

impl ChannelHub {
    /// Broadcast an event to every member of `topic`, across pods. Callable
    /// from anywhere — HTTP handlers, background threads, the REPL.
    pub fn broadcast(&self, topic: &str, event: &str, payload: &Json) {
        assert!(
            !event.starts_with(RESERVED_PREFIX),
            "cannot broadcast reserved event {event:?}"
        );
        self.inner.publish(topic, event, payload, None);
    }

    /// Members of `topic` connected to **this pod** (there is no global
    /// registry by design; presence is the cross-pod view).
    pub fn local_members(&self, topic: &str) -> usize {
        self.inner
            .topics
            .lock()
            .unwrap()
            .get(topic)
            .map_or(0, |t| t.members.len())
    }

    /// The `/__channels` manifest: the envelope protocol plus every
    /// registered channel's patterns, docs, and schemas — enough for an
    /// agent to join and speak with no client library.
    pub fn manifest(&self, mount_path: &str) -> Json {
        let channels: Vec<Json> = self.inner.channels.iter().map(Channel::manifest).collect();
        Json::obj(vec![
            ("path", Json::str(mount_path)),
            ("transport", Json::str("websocket")),
            (
                "protocol",
                Json::obj(vec![
                    (
                        "envelope",
                        Json::obj(vec![
                            ("topic", Json::str("string — the concrete topic, e.g. room:7")),
                            ("event", Json::str("string")),
                            ("ref", Json::str("string, optional — echoed in the stg:reply")),
                            (
                                "join_ref",
                                Json::str("string, optional — the ref of the join that created the membership"),
                            ),
                            ("payload", Json::str("any JSON")),
                        ]),
                    ),
                    (
                        "control_events",
                        Json::obj(vec![
                            ("join", Json::str(EV_JOIN)),
                            ("leave", Json::str(EV_LEAVE)),
                            ("reply", Json::str(EV_REPLY)),
                            ("error", Json::str(EV_ERROR)),
                            ("close", Json::str(EV_CLOSE)),
                        ]),
                    ),
                    (
                        "heartbeat",
                        Json::obj(vec![
                            ("topic", Json::str(CONTROL_TOPIC)),
                            ("event", Json::str(EV_HEARTBEAT)),
                            ("doc", Json::str("send periodically with a ref; an ok reply proves liveness")),
                        ]),
                    ),
                    (
                        "reply_payload",
                        Json::obj(vec![
                            ("status", Json::str("\"ok\" | \"error\"")),
                            ("response", Json::str("any JSON — the handler's reply value")),
                        ]),
                    ),
                ]),
            ),
            ("channels", Json::arr(channels)),
        ])
    }

    // --- WebSocket endpoint wiring ------------------------------------------
    // Map these 1:1 onto a `Ws` endpoint (App::channels does it for you).

    /// Wire to `Ws::on_open`.
    pub fn on_open(&self, conn: &Conn, req: &Request) {
        self.inner.conns.lock().unwrap().insert(
            conn.id(),
            ConnEntry {
                req: Arc::new(req.clone()),
                joins: HashMap::new(),
            },
        );
    }

    /// Wire to `Ws::on_message`.
    pub fn on_message(&self, conn: &Conn, msg: Msg) {
        let text = match msg {
            Msg::Text(t) => t,
            // The protocol is JSON text; binary frames are a client bug.
            Msg::Binary(_) => {
                conn.send_text(&protocol::serialize(
                    CONTROL_TOPIC,
                    EV_ERROR,
                    None,
                    None,
                    &error_payload("binary frames are not part of the channel protocol"),
                ));
                return;
            }
        };
        let envelope = match protocol::parse(&text) {
            Ok(e) => e,
            Err(reason) => {
                conn.send_text(&protocol::serialize(
                    CONTROL_TOPIC,
                    EV_ERROR,
                    None,
                    None,
                    &error_payload(&reason),
                ));
                return;
            }
        };
        self.inner.dispatch(conn, envelope);
    }

    /// Wire to `Ws::on_close`.
    pub fn on_close(&self, conn: &Conn, _code: u16) {
        self.inner.close_conn(conn);
    }

    /// Handshake gates set on the builder, applied by `App::channels`.
    pub fn upgrade_gates(&self) -> (Option<Vec<String>>, Option<AuthorizeFn>) {
        (
            self.inner.allowed_origins.clone(),
            self.inner.authorize.clone(),
        )
    }
}

impl HubInner {
    /// Route one parsed envelope.
    fn dispatch(&self, conn: &Conn, env: crate::protocol::Envelope) {
        // Heartbeat: any ref'd push on the control topic.
        if env.topic == CONTROL_TOPIC {
            if env.event == EV_HEARTBEAT {
                self.reply(conn, &env, None, true, &Json::obj(vec![]));
            } else {
                self.error_reply(conn, &env, "unknown control event");
            }
            return;
        }
        match env.event.as_str() {
            EV_JOIN => self.join(conn, &env),
            EV_LEAVE => {
                self.teardown(conn, &env.topic, LeaveReason::Leave);
                self.reply(
                    conn,
                    &env,
                    env.join_ref.as_deref(),
                    true,
                    &Json::obj(vec![]),
                );
            }
            ev if ev.starts_with(RESERVED_PREFIX) => {
                self.error_reply(conn, &env, "clients cannot send reserved events");
            }
            _ => self.member_event(conn, &env),
        }
    }

    /// `stg:join`: match a channel, tear down a previous membership on the
    /// same topic (rejoin), run the join callback, admit on Ok.
    fn join(&self, conn: &Conn, env: &crate::protocol::Envelope) {
        let Some(channel) = self.channel_for(&env.topic) else {
            self.error_reply(conn, env, "no channel serves this topic");
            return;
        };
        // A fresh join on an already-joined topic replaces the membership
        // (the client rejoined after losing state); tell the old one why.
        if self.membership(conn.id(), &env.topic).is_some() {
            self.teardown(conn, &env.topic, LeaveReason::Rejoin);
        }

        let join_ref = env.join_ref.clone().or_else(|| env.reference.clone());
        let pending: Arc<Mutex<Vec<AfterJoinFn>>> = Arc::new(Mutex::new(Vec::new()));
        let socket = Socket {
            hub: ChannelHub {
                inner: self.strong(),
            },
            conn: conn.clone(),
            req: self.request_of(conn.id()),
            topic: env.topic.clone(),
            join_ref: join_ref.clone(),
            assigns: Arc::new(Mutex::new(Json::obj(vec![]))),
            pending: Some(pending.clone()),
        };
        // The callback runs outside every hub lock: it may broadcast, push,
        // or assign freely. It is not a member yet — a broadcast from inside
        // on_join reaches the room but not the joiner (push to the socket
        // directly for a welcome message).
        let verdict = match &channel.join {
            Some(f) => f(&socket, &env.payload),
            None => Ok(Json::Null),
        };
        match verdict {
            Ok(response) => {
                self.admit(conn, &env.topic, &socket);
                self.reply(conn, env, join_ref.as_deref(), true, &response);
                // Deferred join work runs against the admitted, disarmed
                // socket — pushes now land after the ok reply.
                let admitted = Socket {
                    pending: None,
                    ..socket
                };
                for f in pending.lock().unwrap().drain(..) {
                    f(&admitted);
                }
            }
            Err(reason) => {
                self.reply(conn, env, join_ref.as_deref(), false, &reason);
            }
        }
    }

    /// Insert the membership maps entry + broker subscription for a
    /// successful join.
    fn admit(&self, conn: &Conn, topic: &str, socket: &Socket) {
        {
            let mut conns = self.conns.lock().unwrap();
            if let Some(entry) = conns.get_mut(&conn.id()) {
                entry.joins.insert(
                    topic.to_string(),
                    JoinEntry {
                        join_ref: socket.join_ref.clone(),
                        assigns: Arc::clone(&socket.assigns),
                    },
                );
            }
        }
        let mut topics = self.topics.lock().unwrap();
        let entry = topics.entry(topic.to_string()).or_insert_with(|| {
            let weak = self.weak.clone();
            let topic = topic.to_string();
            let sub_id = self.broker.on(&pubsub_topic(&topic), move |msg| {
                if let Some(hub) = weak.upgrade() {
                    hub.deliver(&topic, msg);
                }
            });
            TopicEntry {
                sub_id,
                members: HashMap::new(),
            }
        });
        entry.members.insert(conn.id(), conn.clone());
    }

    /// A member-pushed custom event: must be joined; dispatch to the
    /// channel's handler.
    fn member_event(&self, conn: &Conn, env: &crate::protocol::Envelope) {
        let Some(join) = self.membership(conn.id(), &env.topic) else {
            self.error_reply(conn, env, "not joined to this topic");
            return;
        };
        let Some(channel) = self.channel_for(&env.topic) else {
            self.error_reply(conn, env, "no channel serves this topic");
            return;
        };
        let Some(handler) = channel.handlers.get(&env.event) else {
            self.error_reply(conn, env, "unhandled event");
            return;
        };
        let socket = Socket {
            hub: ChannelHub {
                inner: self.strong(),
            },
            conn: conn.clone(),
            req: self.request_of(conn.id()),
            topic: env.topic.clone(),
            join_ref: join.join_ref.clone(),
            assigns: join.assigns,
            pending: None,
        };
        match handler(&socket, &env.payload) {
            Reply::Ok(resp) => self.reply(conn, env, socket.join_ref.as_deref(), true, &resp),
            Reply::Err(resp) => self.reply(conn, env, socket.join_ref.as_deref(), false, &resp),
            Reply::None => {}
        }
    }

    /// Publish one broadcast onto the broker (local delivery is synchronous
    /// inside `publish` for both bundled brokers).
    pub(crate) fn publish(&self, topic: &str, event: &str, payload: &Json, exclude: Option<u64>) {
        let mut fields = vec![
            ("o", Json::str(self.instance.clone())),
            ("e", Json::str(event)),
            ("p", payload.clone()),
        ];
        if let Some(id) = exclude {
            fields.push(("x", Json::str(id.to_string())));
        }
        self.broker
            .publish(&pubsub_topic(topic), &Json::obj(fields).to_string());
    }

    /// A broker message for `topic` arrived (from this pod or another):
    /// fan it out to local members with one shared frame.
    fn deliver(&self, topic: &str, msg: &str) {
        let Ok(parsed) = Json::parse(msg) else { return };
        let (Some(origin), Some(event)) = (
            parsed.get("o").and_then(Json::as_str),
            parsed.get("e").and_then(Json::as_str),
        ) else {
            return;
        };
        let payload = parsed.get("p").cloned().unwrap_or(Json::Null);
        // Sender exclusion is only meaningful on the pod the sender lives on.
        let exclude: Option<u64> = if origin == self.instance {
            parsed
                .get("x")
                .and_then(Json::as_str)
                .and_then(|s| s.parse().ok())
        } else {
            None
        };
        let members: Vec<Conn> = {
            let topics = self.topics.lock().unwrap();
            match topics.get(topic) {
                Some(t) => t
                    .members
                    .iter()
                    .filter(|(id, _)| Some(**id) != exclude)
                    .map(|(_, c)| c.clone())
                    .collect(),
                None => return,
            }
        };
        let frame = text_frame(&protocol::serialize(topic, event, None, None, &payload));
        sutegi_ws::broadcast(members.iter(), &frame);
    }

    /// Remove one membership (if present) and run the leave callback.
    pub(crate) fn teardown(&self, conn: &Conn, topic: &str, reason: LeaveReason) {
        let removed = {
            let mut conns = self.conns.lock().unwrap();
            conns
                .get_mut(&conn.id())
                .and_then(|e| e.joins.remove(topic))
        };
        let Some(join) = removed else { return };
        self.forget_member(conn.id(), topic);
        #[cfg(feature = "presence")]
        crate::presence::untrack_on_teardown(self, conn.id(), topic);
        if let Some(channel) = self.channel_for(topic) {
            if let Some(leave) = &channel.leave {
                let socket = Socket {
                    hub: ChannelHub {
                        inner: self.strong(),
                    },
                    conn: conn.clone(),
                    req: self.request_of(conn.id()),
                    topic: topic.to_string(),
                    join_ref: join.join_ref.clone(),
                    assigns: join.assigns,
                    pending: None,
                };
                leave(&socket, reason);
            }
        }
    }

    /// The connection is gone: tear down every membership it held.
    fn close_conn(&self, conn: &Conn) {
        let topics: Vec<String> = {
            let conns = self.conns.lock().unwrap();
            conns
                .get(&conn.id())
                .map(|e| e.joins.keys().cloned().collect())
                .unwrap_or_default()
        };
        for topic in topics {
            self.teardown(conn, &topic, LeaveReason::Disconnect);
        }
        self.conns.lock().unwrap().remove(&conn.id());
    }

    /// Drop a member from the topic map, unsubscribing from the broker when
    /// the last local member leaves.
    fn forget_member(&self, conn_id: u64, topic: &str) {
        let mut topics = self.topics.lock().unwrap();
        if let Some(entry) = topics.get_mut(topic) {
            entry.members.remove(&conn_id);
            if entry.members.is_empty() {
                let sub_id = entry.sub_id;
                topics.remove(topic);
                drop(topics);
                self.broker.unsubscribe(&pubsub_topic(topic), sub_id);
            }
        }
    }

    // --- small lookups -------------------------------------------------------

    fn channel_for(&self, topic: &str) -> Option<&Channel> {
        self.channels
            .iter()
            .find(|c| topic_matches(&c.pattern, topic))
    }

    fn membership(&self, conn_id: u64, topic: &str) -> Option<JoinEntry> {
        self.conns
            .lock()
            .unwrap()
            .get(&conn_id)
            .and_then(|e| e.joins.get(topic).cloned())
    }

    fn request_of(&self, conn_id: u64) -> Arc<Request> {
        self.conns
            .lock()
            .unwrap()
            .get(&conn_id)
            .map(|e| Arc::clone(&e.req))
            .unwrap_or_else(|| Arc::new(placeholder_request()))
    }

    fn strong(&self) -> Arc<HubInner> {
        self.weak.upgrade().expect("hub alive while dispatching")
    }

    #[cfg(feature = "presence")]
    pub(crate) fn weak_ref(&self) -> Weak<HubInner> {
        self.weak.clone()
    }

    /// Fan one server→client frame to every local member of `topic` (no
    /// broker round-trip, no exclusion) — presence diffs ride this.
    #[cfg(feature = "presence")]
    pub(crate) fn fan_local(&self, topic: &str, event: &str, payload: &Json) {
        let members: Vec<Conn> = {
            let topics = self.topics.lock().unwrap();
            match topics.get(topic) {
                Some(t) => t.members.values().cloned().collect(),
                None => return,
            }
        };
        if members.is_empty() {
            return;
        }
        let frame = text_frame(&protocol::serialize(topic, event, None, None, payload));
        sutegi_ws::broadcast(members.iter(), &frame);
    }

    /// Reply to a ref'd envelope; silently skip when it carried no ref.
    fn reply(
        &self,
        conn: &Conn,
        env: &crate::protocol::Envelope,
        join_ref: Option<&str>,
        ok: bool,
        response: &Json,
    ) {
        let Some(reference) = env.reference.as_deref() else {
            return;
        };
        conn.send_text(&protocol::serialize(
            &env.topic,
            EV_REPLY,
            Some(reference),
            join_ref,
            &reply_payload(ok, response),
        ));
    }

    /// Errors reply to the ref when there is one, and fall back to a
    /// ref-less `stg:error` frame when there is not.
    fn error_reply(&self, conn: &Conn, env: &crate::protocol::Envelope, reason: &str) {
        if env.reference.is_some() {
            self.reply(conn, env, None, false, &error_payload(reason));
        } else {
            conn.send_text(&protocol::serialize(
                &env.topic,
                EV_ERROR,
                None,
                None,
                &error_payload(reason),
            ));
        }
    }
}

/// A Request stand-in for the (unreachable in practice) case of a callback
/// firing for a connection that was never registered.
fn placeholder_request() -> Request {
    Request {
        method: sutegi_http::Method::Get,
        path: String::new(),
        query: String::new(),
        version: "HTTP/1.1".into(),
        headers: Vec::new(),
        body: Vec::new(),
        peer: None,
    }
}

/// A process-unique, cross-pod-unique id (16 hex chars) from the OS CSPRNG,
/// with a time/pid fallback. Used for cross-pod sender exclusion.
fn unique_id() -> String {
    use std::io::Read;
    let mut buf = [0u8; 8];
    let ok = std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut buf))
        .is_ok();
    if !ok {
        let t = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);
        buf = (t ^ (std::process::id() as u64).rotate_left(32)).to_le_bytes();
    }
    let mut out = String::with_capacity(16);
    for b in buf {
        out.push_str(&format!("{b:02x}"));
    }
    out
}
