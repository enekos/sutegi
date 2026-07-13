#![forbid(unsafe_code)]
//! Publish/subscribe: topics with callback fan-out, zero third-party deps.
//!
//! Everything rides the [`Broker`] seam — `subscribe(topic, callback) ->
//! id`, `publish(topic, message)`, `unsubscribe(topic, id)`. [`PubSub`] is
//! the in-process broker (one node); `PgPubSub` (feature `postgres`)
//! speaks PostgreSQL `LISTEN`/`NOTIFY` for cross-pod fan-out behind the
//! same trait, so swapping is a constructor change, not a rewrite.
//! Callbacks are invoked *outside* the registry lock, so a subscriber may
//! publish (or (un)subscribe) from inside its own callback without
//! deadlocking.
//!
//! The design mirror to sutegi's ORM `Backend` trait: one seam, swappable
//! implementations, so `subscribe`/`publish` code is written once.
//!
//! ```
//! use sutegi_pubsub::{Broker, PubSub};
//! use std::sync::{Arc, Mutex};
//!
//! let bus = PubSub::new();
//! let seen = Arc::new(Mutex::new(Vec::new()));
//! let sink = Arc::clone(&seen);
//! let id = bus.on("room:1", move |msg| sink.lock().unwrap().push(msg.to_string()));
//! bus.publish("room:1", "hello");
//! bus.unsubscribe("room:1", id);
//! bus.publish("room:1", "unheard");
//! assert_eq!(&*seen.lock().unwrap(), &["hello".to_string()]);
//! ```

#[cfg(feature = "postgres")]
mod pg;
#[cfg(feature = "postgres")]
pub use pg::PgPubSub;
/// The underlying PostgreSQL client (re-exported so `PgPubSub` users can
/// build a [`sutegi_pg::Config`] without depending on the ORM stack).
#[cfg(feature = "postgres")]
pub use sutegi_pg;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// A callback invoked with each message published to a subscribed topic.
pub type Listener = Arc<dyn Fn(&str) + Send + Sync>;

/// The publish/subscribe seam. One in-process implementation ([`PubSub`])
/// today; a Postgres `LISTEN/NOTIFY` broker can implement it later.
pub trait Broker: Send + Sync {
    /// Register `listener` on `topic`; returns a subscription id for
    /// [`Broker::unsubscribe`]. Ids are unique per broker, never reused.
    fn subscribe(&self, topic: &str, listener: Listener) -> u64;

    /// Remove the subscription `id` from `topic`. Unknown ids are ignored.
    fn unsubscribe(&self, topic: &str, id: u64);

    /// Deliver `message` to every current subscriber of `topic`, in
    /// subscription order. Unsubscribed/absent topics are a no-op.
    fn publish(&self, topic: &str, message: &str);
}

/// A convenience helper for the common `subscribe(topic, closure)` form —
/// takes any `Fn(&str)` and boxes it. Available on any `Broker`.
pub trait BrokerExt: Broker {
    fn on(&self, topic: &str, f: impl Fn(&str) + Send + Sync + 'static) -> u64 {
        self.subscribe(topic, Arc::new(f))
    }
}

impl<B: Broker + ?Sized> BrokerExt for B {}

#[derive(Default)]
struct Registry {
    /// topic -> its (id, listener) subscribers, in subscription order.
    topics: HashMap<String, Vec<(u64, Listener)>>,
    next_id: u64,
}

/// The in-process broker: a locked topic registry. Cheap to [`Clone`]
/// (shares one registry via `Arc`), so hand copies to every connection /
/// handler that needs to publish or subscribe.
#[derive(Clone, Default)]
pub struct PubSub {
    inner: Arc<Mutex<Registry>>,
}

impl PubSub {
    pub fn new() -> PubSub {
        PubSub::default()
    }

    /// Subscribers on a topic (for tests / introspection).
    pub fn subscriber_count(&self, topic: &str) -> usize {
        self.inner
            .lock()
            .unwrap()
            .topics
            .get(topic)
            .map_or(0, Vec::len)
    }

    /// Ergonomic `subscribe` taking a closure directly.
    pub fn on(&self, topic: &str, f: impl Fn(&str) + Send + Sync + 'static) -> u64 {
        self.subscribe(topic, Arc::new(f))
    }
}

impl Broker for PubSub {
    fn subscribe(&self, topic: &str, listener: Listener) -> u64 {
        let mut reg = self.inner.lock().unwrap();
        reg.next_id += 1;
        let id = reg.next_id;
        reg.topics
            .entry(topic.to_string())
            .or_default()
            .push((id, listener));
        id
    }

    fn unsubscribe(&self, topic: &str, id: u64) {
        let mut reg = self.inner.lock().unwrap();
        if let Some(subs) = reg.topics.get_mut(topic) {
            subs.retain(|(sid, _)| *sid != id);
            if subs.is_empty() {
                reg.topics.remove(topic);
            }
        }
    }

    fn publish(&self, topic: &str, message: &str) {
        // Snapshot the listeners under the lock, then release it before
        // calling them: a listener that publishes/subscribes/unsubscribes
        // (a chat message that fans out, say) must not deadlock, and a slow
        // listener must not hold the registry from every other topic.
        let listeners: Vec<Listener> = {
            let reg = self.inner.lock().unwrap();
            match reg.topics.get(topic) {
                Some(subs) => subs.iter().map(|(_, l)| Arc::clone(l)).collect(),
                None => return,
            }
        };
        for listener in listeners {
            listener(message);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn collector() -> (Listener, Arc<Mutex<Vec<String>>>) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&seen);
        let l: Listener = Arc::new(move |m: &str| sink.lock().unwrap().push(m.to_string()));
        (l, seen)
    }

    #[test]
    fn fan_out_in_subscription_order_and_topic_isolation() {
        let bus = PubSub::new();
        let (a, a_seen) = collector();
        let (b, b_seen) = collector();
        let (c, c_seen) = collector();
        bus.subscribe("room", a);
        bus.subscribe("room", b);
        bus.subscribe("other", c);

        bus.publish("room", "hi");
        bus.publish("other", "elsewhere");
        bus.publish("empty", "into the void"); // no subscribers, no panic

        assert_eq!(&*a_seen.lock().unwrap(), &["hi"]);
        assert_eq!(&*b_seen.lock().unwrap(), &["hi"]);
        assert_eq!(&*c_seen.lock().unwrap(), &["elsewhere"]);
    }

    #[test]
    fn unsubscribe_stops_delivery_and_prunes_empty_topics() {
        let bus = PubSub::new();
        let (a, seen) = collector();
        let id = bus.subscribe("t", a);
        bus.publish("t", "one");
        bus.unsubscribe("t", id);
        bus.unsubscribe("t", 9999); // unknown id, ignored
        bus.publish("t", "two");
        assert_eq!(&*seen.lock().unwrap(), &["one"]);
        assert_eq!(bus.subscriber_count("t"), 0);
    }

    #[test]
    fn a_listener_may_publish_without_deadlocking() {
        // room -> log topic: receiving on "room" republishes to "log".
        let bus = PubSub::new();
        let relay = bus.clone();
        bus.on("room", move |m| relay.publish("log", m));
        let (log, seen) = collector();
        bus.subscribe("log", log);

        bus.publish("room", "chained");
        assert_eq!(&*seen.lock().unwrap(), &["chained"]);
    }

    #[test]
    fn broker_trait_object_is_usable() {
        let bus: Arc<dyn Broker> = Arc::new(PubSub::new());
        let (a, seen) = collector();
        let id = bus.subscribe("x", a);
        bus.on("x", |_| {}); // BrokerExt on a trait object
        bus.publish("x", "trait");
        bus.unsubscribe("x", id);
        assert_eq!(&*seen.lock().unwrap(), &["trait"]);
    }
}
