//! Cross-pod publish/subscribe over PostgreSQL `LISTEN`/`NOTIFY`.
//!
//! [`PgPubSub`] implements the same [`Broker`](crate::Broker) seam as the
//! in-process [`PubSub`](crate::PubSub), so swapping a single-node app to
//! cross-pod fan-out is a constructor change, not a rewrite. The design is
//! the one Phoenix.PubSub ships for Postgres:
//!
//! - Every broker `LISTEN`s on **one** PostgreSQL channel (default
//!   `sutegi_pubsub`). Topics travel *inside* the payload envelope, so topic
//!   names are unconstrained by PostgreSQL's 63-byte identifier limit and
//!   subscribing never touches the database.
//! - `publish` delivers to **local** subscribers synchronously (same
//!   semantics and latency as the in-process broker), then hands the
//!   envelope to a dedicated publisher thread that `NOTIFY`s the channel.
//!   The caller — often a WebSocket reactor thread — never waits on the
//!   network: the hand-off is a bounded queue, and a full queue drops the
//!   message (counted in [`PgPubSub::dropped`]). Remote pods dispatch it to their local subscribers; the
//!   originating pod recognizes its own instance id in the envelope and
//!   skips the echo.
//! - Delivery is **at-most-once, fire-and-forget** — exactly the in-process
//!   broker's contract. A pod that is reconnecting misses messages sent
//!   meanwhile; durable delivery is what `sutegi-queue`/`sutegi-events` are
//!   for.
//!
//! `NOTIFY` payloads are capped by the server at ~8000 bytes. A message
//! whose envelope exceeds that is still delivered locally but not sent
//! cross-pod (an error is returned from [`PgPubSub::try_publish`] and logged
//! from `publish`). Ship large payloads through a table and publish the id.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex, Weak};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use sutegi_json::Json;
use sutegi_pg::{Client, Config, Listener, ListenerShutdown, PgValue};

use crate::{Broker, Listener as Callback, PubSub};

/// The server rejects `NOTIFY` payloads of 8000 bytes or more.
const MAX_NOTIFY_PAYLOAD: usize = 7999;

/// Reconnect backoff bounds for the listener thread.
const BACKOFF_FLOOR: Duration = Duration::from_millis(250);
const BACKOFF_CEIL: Duration = Duration::from_secs(5);

/// Envelopes waiting for the publisher thread. Past this, publishes are
/// dropped (delivery is at-most-once anyway) instead of blocking the caller.
const PUBLISH_QUEUE: usize = 4096;

/// A cross-pod [`Broker`] over PostgreSQL `LISTEN`/`NOTIFY`. Cheap to
/// [`Clone`]; all clones share one listener connection, one publisher
/// thread (with its lazily-opened connection), and one local subscriber
/// registry. Dropping the last clone stops and joins the publisher thread
/// and interrupts the listener thread.
#[derive(Clone)]
pub struct PgPubSub {
    inner: Arc<Inner>,
}

struct Inner {
    /// Local subscriber registry — remote messages land here too.
    local: PubSub,
    cfg: Config,
    /// The single PostgreSQL channel every pod listens on.
    channel: String,
    /// Random per-broker id, stamped into every envelope so the listener
    /// can drop the echo of its own publishes (already delivered locally).
    instance: String,
    /// Bounded hand-off to the publisher thread, which owns the connection.
    publisher: Publisher,
    /// Interrupts the listener thread's current socket on Drop.
    listener_shutdown: Mutex<Option<ListenerShutdown>>,
    shutdown: AtomicBool,
}

impl Drop for Inner {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Release);
        if let Some(h) = self.listener_shutdown.lock().unwrap().take() {
            h.shutdown();
        }
    }
}

impl PgPubSub {
    /// Connect on the default PostgreSQL channel (`sutegi_pubsub`). Fails
    /// fast if the listener connection cannot be established; the publisher
    /// connection opens lazily on first publish.
    pub fn connect(cfg: &Config) -> Result<PgPubSub, String> {
        PgPubSub::connect_on(cfg, "sutegi_pubsub")
    }

    /// Connect with a custom channel name — isolate multiple independent
    /// buses (or test suites) sharing one database.
    pub fn connect_on(cfg: &Config, channel: &str) -> Result<PgPubSub, String> {
        let mut listener = Listener::connect(cfg)?;
        listener.listen(channel)?;
        let publisher =
            Publisher::start(PUBLISH_QUEUE, notify_sink(cfg.clone(), channel.to_string()))?;
        let inner = Arc::new(Inner {
            local: PubSub::new(),
            cfg: cfg.clone(),
            channel: channel.to_string(),
            instance: instance_id(),
            publisher,
            listener_shutdown: Mutex::new(listener.shutdown_handle().ok()),
            shutdown: AtomicBool::new(false),
        });
        // The listener holds only a Weak: a strong ref would keep `Inner`
        // alive forever and its Drop (which stops both threads) would never run.
        let for_thread = Arc::downgrade(&inner);
        thread::Builder::new()
            .name("sutegi-pubsub-pg".into())
            .spawn(move || listener_loop(for_thread, listener))
            .map_err(|e| format!("spawn listener thread: {e}"))?;
        Ok(PgPubSub { inner })
    }

    /// Like [`Broker::publish`], but surfaces why a message will not go
    /// cross-pod instead of logging it: over the `NOTIFY` size cap, or the
    /// publisher queue is full. `Ok` means *queued* — the `NOTIFY` itself
    /// happens on the publisher thread, which logs network failures. Local
    /// subscribers have always already been delivered to when this
    /// returns, even on `Err`. Never blocks on the network.
    pub fn try_publish(&self, topic: &str, message: &str) -> Result<(), String> {
        self.inner.local.publish(topic, message);
        self.inner.notify(topic, message)
    }

    /// Messages that never went cross-pod because the publisher queue was
    /// full or the `NOTIFY` failed (cumulative, for metrics).
    pub fn dropped(&self) -> u64 {
        self.inner.publisher.dropped()
    }

    /// Local subscribers on a topic (tests / introspection). Remote pods'
    /// subscribers are invisible by design — there is no global registry.
    pub fn subscriber_count(&self, topic: &str) -> usize {
        self.inner.local.subscriber_count(topic)
    }

    /// This broker's instance id (stamped into envelopes; useful in tests).
    pub fn instance_id(&self) -> &str {
        &self.inner.instance
    }
}

impl Broker for PgPubSub {
    fn subscribe(&self, topic: &str, listener: Callback) -> u64 {
        self.inner.local.subscribe(topic, listener)
    }

    fn unsubscribe(&self, topic: &str, id: u64) {
        self.inner.local.unsubscribe(topic, id);
    }

    fn publish(&self, topic: &str, message: &str) {
        if let Err(e) = self.try_publish(topic, message) {
            sutegi_log::error!(
                "pubsub",
                "cross-pod publish failed",
                topic = topic,
                error = e
            );
        }
    }
}

impl Inner {
    /// Envelope a message and queue it for the publisher thread.
    fn notify(&self, topic: &str, message: &str) -> Result<(), String> {
        let envelope = encode_envelope(&self.instance, topic, message);
        if envelope.len() > MAX_NOTIFY_PAYLOAD {
            return Err(format!(
                "message on {topic:?} is {} bytes enveloped, over the ~8 KB NOTIFY cap; \
                 store it in a table and publish the id instead",
                envelope.len()
            ));
        }
        self.publisher.enqueue(envelope)
    }

    /// Deliver one received notification to local subscribers, dropping our
    /// own echoes and anything that isn't a well-formed envelope.
    fn dispatch(&self, payload: &str) {
        let Some((instance, topic, message)) = decode_envelope(payload) else {
            sutegi_log::warn!(
                "pubsub",
                "ignoring malformed envelope",
                channel = self.channel
            );
            return;
        };
        if instance == self.instance {
            return; // our own publish, already delivered locally
        }
        self.local.publish(&topic, &message);
    }
}

/// The publisher half: a bounded queue drained by one owned thread that
/// performs every `NOTIFY` (and every reconnect), so publishing callers
/// never block on the network. Dropping it stops and joins the thread.
struct Publisher {
    tx: Option<SyncSender<String>>,
    stop: Arc<AtomicBool>,
    dropped: Arc<AtomicU64>,
    handle: Option<JoinHandle<()>>,
}

impl Publisher {
    /// Spawn the publisher thread. `sink` sends one envelope; it runs only
    /// on that thread.
    fn start(
        capacity: usize,
        mut sink: impl FnMut(&str) -> Result<(), String> + Send + 'static,
    ) -> Result<Publisher, String> {
        let (tx, rx): (SyncSender<String>, Receiver<String>) = mpsc::sync_channel(capacity);
        let stop = Arc::new(AtomicBool::new(false));
        let dropped = Arc::new(AtomicU64::new(0));
        let (thread_stop, thread_dropped) = (Arc::clone(&stop), Arc::clone(&dropped));
        let handle = thread::Builder::new()
            .name("sutegi-pubsub-pg-pub".into())
            .spawn(move || {
                // Log the first failure of an outage, not every message.
                let mut failing = false;
                while let Ok(envelope) = rx.recv() {
                    if thread_stop.load(Ordering::Acquire) {
                        return;
                    }
                    match sink(&envelope) {
                        Ok(()) => failing = false,
                        Err(e) => {
                            thread_dropped.fetch_add(1, Ordering::Relaxed);
                            if !failing {
                                sutegi_log::error!(
                                    "pubsub",
                                    "cross-pod publish failed; dropping until the publisher recovers",
                                    error = e
                                );
                            }
                            failing = true;
                        }
                    }
                }
            })
            .map_err(|e| format!("spawn publisher thread: {e}"))?;
        Ok(Publisher {
            tx: Some(tx),
            stop,
            dropped,
            handle: Some(handle),
        })
    }

    /// Queue one envelope without blocking; a full queue drops it.
    fn enqueue(&self, envelope: String) -> Result<(), String> {
        let Some(tx) = &self.tx else {
            return Err("publisher stopped".into());
        };
        match tx.try_send(envelope) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(_)) => {
                self.dropped.fetch_add(1, Ordering::Relaxed);
                Err("cross-pod publish queue full; message dropped".into())
            }
            Err(TrySendError::Disconnected(_)) => {
                self.dropped.fetch_add(1, Ordering::Relaxed);
                Err("publisher thread gone; message dropped".into())
            }
        }
    }

    fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }
}

impl Drop for Publisher {
    fn drop(&mut self) {
        // Abandon whatever is still queued, then close the queue so the
        // thread's recv returns. The wait is bounded by one in-flight
        // NOTIFY (the client's own I/O timeouts).
        self.stop.store(true, Ordering::Release);
        drop(self.tx.take());
        if let Some(handle) = self.handle.take() {
            if handle.join().is_err() {
                sutegi_log::error!("pubsub", "publisher thread panicked");
            }
        }
    }
}

/// The real sink: `NOTIFY` over a connection owned by the publisher thread,
/// opened lazily, retried once when the kept-forever connection turns out
/// to be stale. After a failed connect, messages are dropped for a backoff
/// window instead of paying a connect timeout each.
fn notify_sink(cfg: Config, channel: String) -> impl FnMut(&str) -> Result<(), String> + Send {
    let mut client: Option<Client> = None;
    let mut retry_at: Option<Instant> = None;
    move |envelope: &str| {
        if retry_at.is_some_and(|t| Instant::now() < t) {
            return Err("publisher reconnecting; message dropped".into());
        }
        let params = [
            PgValue::Text(channel.clone()),
            PgValue::Text(envelope.to_string()),
        ];
        let mut last_err = String::new();
        for _attempt in 0..2 {
            let conn = match client.as_mut() {
                Some(c) => c,
                None => match Client::connect(&cfg) {
                    Ok(c) => {
                        retry_at = None;
                        client.insert(c)
                    }
                    Err(e) => {
                        retry_at = Some(Instant::now() + BACKOFF_CEIL);
                        return Err(e);
                    }
                },
            };
            match conn.execute("SELECT pg_notify($1, $2)", &params) {
                Ok(_) => return Ok(()),
                Err(e) => {
                    client = None; // drop the broken connection
                    last_err = e;
                }
            }
        }
        Err(last_err)
    }
}

/// The listener thread: dispatch notifications forever, reconnecting with
/// exponential backoff when the connection drops. Exits when the broker is
/// dropped (its shutdown handle errors the blocked read, and `inner` no
/// longer upgrades).
fn listener_loop(weak: Weak<Inner>, mut listener: Listener) {
    let mut backoff = BACKOFF_FLOOR;
    loop {
        let received = listener.recv();
        let Some(inner) = weak.upgrade() else { return };
        match received {
            Ok(n) => {
                backoff = BACKOFF_FLOOR;
                inner.dispatch(&n.payload);
            }
            Err(_) if inner.shutdown.load(Ordering::Acquire) => return,
            Err(e) => {
                sutegi_log::warn!(
                    "pubsub",
                    "listener connection lost; reconnecting, messages published meanwhile are missed",
                    channel = inner.channel,
                    error = %e
                );
                drop(inner);
                listener = loop {
                    thread::sleep(backoff);
                    let Some(inner) = weak.upgrade() else { return };
                    if inner.shutdown.load(Ordering::Acquire) {
                        return;
                    }
                    backoff = (backoff * 2).min(BACKOFF_CEIL);
                    match reconnect(&inner) {
                        Ok(l) => break l,
                        Err(e) => sutegi_log::warn!(
                            "pubsub",
                            "reconnect failed",
                            channel = inner.channel,
                            retry_ms = backoff.as_millis(),
                            error = %e
                        ),
                    }
                };
            }
        }
    }
}

fn reconnect(inner: &Inner) -> Result<Listener, String> {
    let mut listener = Listener::connect(&inner.cfg)?;
    listener.listen(&inner.channel)?;
    // Swap in the new socket's shutdown handle so a Drop after reconnect
    // still interrupts the right socket.
    *inner.listener_shutdown.lock().unwrap() = listener.shutdown_handle().ok();
    Ok(listener)
}

/// `{"i": instance, "t": topic, "m": message}` — instance first so echo
/// suppression can bail before looking at the rest.
fn encode_envelope(instance: &str, topic: &str, message: &str) -> String {
    Json::obj(vec![
        ("i", Json::str(instance)),
        ("t", Json::str(topic)),
        ("m", Json::str(message)),
    ])
    .to_string()
}

fn decode_envelope(payload: &str) -> Option<(String, String, String)> {
    let json = Json::parse(payload).ok()?;
    Some((
        json.get("i")?.as_str()?.to_string(),
        json.get("t")?.as_str()?.to_string(),
        json.get("m")?.as_str()?.to_string(),
    ))
}

/// A random 16-hex-char instance id from the OS CSPRNG, falling back to a
/// time/address mix. Uniqueness across pods is all that matters here.
fn instance_id() -> String {
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
        let addr = &buf as *const _ as u64;
        buf = (t ^ addr.rotate_left(32) ^ std::process::id() as u64).to_le_bytes();
    }
    sutegi_pg::crypto::hex(&buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn envelope_round_trips() {
        let e = encode_envelope("abc123", "room:1", "hello \"world\"");
        assert_eq!(
            decode_envelope(&e),
            Some((
                "abc123".to_string(),
                "room:1".to_string(),
                "hello \"world\"".to_string()
            ))
        );
    }

    #[test]
    fn envelope_survives_json_payloads_and_unicode() {
        let msg = r#"{"event":"msg","payload":{"body":"ñ→🔥"}}"#;
        let e = encode_envelope("i", "topic/with spaces:and:colons", msg);
        let (_, topic, message) = decode_envelope(&e).unwrap();
        assert_eq!(topic, "topic/with spaces:and:colons");
        assert_eq!(message, msg);
    }

    #[test]
    fn malformed_envelopes_decode_to_none() {
        for bad in [
            "",
            "not json",
            "42",
            "[]",
            r#"{"i":"x","t":"y"}"#,       // missing m
            r#"{"i":1,"t":"y","m":"z"}"#, // non-string instance
            r#"{"t":"y","m":"z"}"#,       // missing i
        ] {
            assert_eq!(decode_envelope(bad), None, "payload: {bad:?}");
        }
    }

    /// A sink that signals when it is entered and then blocks until
    /// released — a stand-in for a NOTIFY stuck on a slow network.
    fn blocking_sink(
        entered: mpsc::Sender<String>,
        release: Arc<(Mutex<bool>, std::sync::Condvar)>,
    ) -> impl FnMut(&str) -> Result<(), String> + Send {
        move |envelope: &str| {
            let _ = entered.send(envelope.to_string());
            let (lock, cvar) = &*release;
            let mut open = lock.lock().unwrap();
            while !*open {
                open = cvar.wait(open).unwrap();
            }
            Ok(())
        }
    }

    #[test]
    fn publish_never_blocks_on_a_stuck_notify_and_drops_when_full() {
        let (entered_tx, entered_rx) = mpsc::channel();
        let release = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
        let publisher =
            Publisher::start(2, blocking_sink(entered_tx, Arc::clone(&release))).unwrap();

        // The first envelope is taken by the thread, which then blocks.
        publisher.enqueue("first".into()).unwrap();
        assert_eq!(
            entered_rx.recv_timeout(Duration::from_secs(5)).unwrap(),
            "first"
        );
        // The queue absorbs `capacity` more, without the caller waiting...
        let start = Instant::now();
        publisher.enqueue("q1".into()).unwrap();
        publisher.enqueue("q2".into()).unwrap();
        // ...and past that drops instead of blocking.
        let err = publisher.enqueue("over".into()).unwrap_err();
        assert!(err.contains("queue full"), "got: {err}");
        assert!(start.elapsed() < Duration::from_secs(1));
        assert_eq!(publisher.dropped(), 1);

        // Unblock: the queued envelopes go out in order.
        {
            let (lock, cvar) = &*release;
            *lock.lock().unwrap() = true;
            cvar.notify_all();
        }
        assert_eq!(
            entered_rx.recv_timeout(Duration::from_secs(5)).unwrap(),
            "q1"
        );
        assert_eq!(
            entered_rx.recv_timeout(Duration::from_secs(5)).unwrap(),
            "q2"
        );
    }

    #[test]
    fn sink_failures_are_counted_and_drop_joins_the_thread() {
        let calls = Arc::new(AtomicU64::new(0));
        let seen = Arc::clone(&calls);
        let publisher = Publisher::start(8, move |_: &str| {
            seen.fetch_add(1, Ordering::SeqCst);
            Err("connection refused".to_string())
        })
        .unwrap();
        publisher.enqueue("a".into()).unwrap();
        publisher.enqueue("b".into()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while publisher.dropped() < 2 {
            assert!(Instant::now() < deadline, "sink never ran");
            thread::sleep(Duration::from_millis(5));
        }
        // Drop stops and joins: the sink is never called again.
        drop(publisher);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn instance_ids_are_unique_and_hex() {
        let a = instance_id();
        let b = instance_id();
        assert_eq!(a.len(), 16);
        assert!(a.bytes().all(|b| b.is_ascii_hexdigit()));
        assert_ne!(a, b);
    }
}
