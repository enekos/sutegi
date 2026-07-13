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
//!   semantics and latency as the in-process broker), then `NOTIFY`s the
//!   channel. Remote pods dispatch it to their local subscribers; the
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

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use sutegi_json::Json;
use sutegi_pg::{Client, Config, Listener, ListenerShutdown, PgValue};

use crate::{Broker, Listener as Callback, PubSub};

/// The server rejects `NOTIFY` payloads of 8000 bytes or more.
const MAX_NOTIFY_PAYLOAD: usize = 7999;

/// Reconnect backoff bounds for the listener thread.
const BACKOFF_FLOOR: Duration = Duration::from_millis(250);
const BACKOFF_CEIL: Duration = Duration::from_secs(5);

/// A cross-pod [`Broker`] over PostgreSQL `LISTEN`/`NOTIFY`. Cheap to
/// [`Clone`]; all clones share one listener connection, one lazily-opened
/// publisher connection, and one local subscriber registry.
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
    /// Publisher connection, opened on first publish, dropped on error and
    /// reopened on the next.
    publisher: Mutex<Option<Client>>,
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
        let inner = Arc::new(Inner {
            local: PubSub::new(),
            cfg: cfg.clone(),
            channel: channel.to_string(),
            instance: instance_id(),
            publisher: Mutex::new(None),
            listener_shutdown: Mutex::new(listener.shutdown_handle().ok()),
            shutdown: AtomicBool::new(false),
        });
        let for_thread = Arc::clone(&inner);
        thread::Builder::new()
            .name("sutegi-pubsub-pg".into())
            .spawn(move || listener_loop(for_thread, listener))
            .map_err(|e| format!("spawn listener thread: {e}"))?;
        Ok(PgPubSub { inner })
    }

    /// Like [`Broker::publish`], but surfaces the cross-pod send error
    /// instead of logging it. Local subscribers have always already been
    /// delivered to when this returns, even on `Err`.
    pub fn try_publish(&self, topic: &str, message: &str) -> Result<(), String> {
        self.inner.local.publish(topic, message);
        self.inner.notify(topic, message)
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
            eprintln!("sutegi-pubsub: cross-pod publish to {topic:?} failed: {e}");
        }
    }
}

impl Inner {
    /// `NOTIFY` the shared channel with an enveloped message, reconnecting
    /// the publisher once on a broken connection.
    fn notify(&self, topic: &str, message: &str) -> Result<(), String> {
        let envelope = encode_envelope(&self.instance, topic, message);
        if envelope.len() > MAX_NOTIFY_PAYLOAD {
            return Err(format!(
                "message on {topic:?} is {} bytes enveloped, over the ~8 KB NOTIFY cap; \
                 store it in a table and publish the id instead",
                envelope.len()
            ));
        }
        let params = [PgValue::Text(self.channel.clone()), PgValue::Text(envelope)];
        let mut guard = self.publisher.lock().unwrap();
        // One transparent retry: the pooled-forever connection may have been
        // idled out by the server since the last publish.
        for attempt in 0..2 {
            if guard.is_none() {
                *guard = Some(Client::connect(&self.cfg)?);
            }
            let client = guard.as_mut().expect("publisher just ensured");
            match client.execute("SELECT pg_notify($1, $2)", &params) {
                Ok(_) => return Ok(()),
                Err(e) => {
                    *guard = None; // drop the broken connection
                    if attempt == 1 {
                        return Err(e);
                    }
                }
            }
        }
        unreachable!("loop returns on success or second failure");
    }

    /// Deliver one received notification to local subscribers, dropping our
    /// own echoes and anything that isn't a well-formed envelope.
    fn dispatch(&self, payload: &str) {
        let Some((instance, topic, message)) = decode_envelope(payload) else {
            eprintln!(
                "sutegi-pubsub: ignoring malformed envelope on {:?}",
                self.channel
            );
            return;
        };
        if instance == self.instance {
            return; // our own publish, already delivered locally
        }
        self.local.publish(&topic, &message);
    }
}

/// The listener thread: dispatch notifications forever, reconnecting with
/// exponential backoff when the connection drops. Exits when the broker is
/// dropped (its shutdown handle errors the blocked read).
fn listener_loop(inner: Arc<Inner>, mut listener: Listener) {
    let mut backoff = BACKOFF_FLOOR;
    loop {
        match listener.recv() {
            Ok(n) => {
                backoff = BACKOFF_FLOOR;
                inner.dispatch(&n.payload);
            }
            Err(_) if inner.shutdown.load(Ordering::Acquire) => return,
            Err(e) => {
                eprintln!(
                    "sutegi-pubsub: listener connection lost ({e}); reconnecting \
                     (messages published meanwhile are missed)"
                );
                listener = loop {
                    thread::sleep(backoff);
                    if inner.shutdown.load(Ordering::Acquire) {
                        return;
                    }
                    backoff = (backoff * 2).min(BACKOFF_CEIL);
                    match reconnect(&inner) {
                        Ok(l) => break l,
                        Err(e) => eprintln!("sutegi-pubsub: reconnect failed: {e}"),
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

    #[test]
    fn instance_ids_are_unique_and_hex() {
        let a = instance_id();
        let b = instance_id();
        assert_eq!(a.len(), 16);
        assert!(a.bytes().all(|b| b.is_ascii_hexdigit()));
        assert_ne!(a, b);
    }
}
