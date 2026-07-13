//! Presence: who is on a topic right now, across pods.
//!
//! ## The design, honestly
//!
//! This is deliberately simpler than Phoenix's delta-CRDT tracker. Each pod
//! keeps its **local** presences (conn → key + meta) as ground truth and a
//! cache of every **remote** pod's entries, synchronized over the pubsub
//! [`Broker`](sutegi_pubsub::Broker) seam:
//!
//! - `track`/`untrack` broadcast an incremental diff.
//! - A pod's first track on a topic requests a state sync; peers answer
//!   with their full local state.
//! - Every pod re-publishes its state on a heartbeat interval; a remote pod
//!   not heard from within ~2.5 intervals is expired and its members are
//!   reported as leaves.
//!
//! Consequences to design for: a **crashed pod's members linger** until
//! expiry (up to ~2.5 × interval, default ~75 s); concurrent track/untrack
//! during a network partition resolves by expiry, not by CRDT merge. For a
//! user-facing "who's online" list this is the right trade; for anything
//! stronger, keep authority in a table.
//!
//! ## Usage
//!
//! ```ignore
//! Channel::new("room:*")
//!     .on_join(|socket, payload| {
//!         Presence::track(socket, user_id, Json::obj(vec![("nick", Json::str(nick))]));
//!         Ok(Json::Null)
//!     })
//! // Members receive:
//! //   presence_state  (to the tracked member: the full {key: {metas}} view)
//! //   presence_diff   (to the topic: {joins: {...}, leaves: {...}})
//! ```
//!
//! Untracking is automatic on leave/rejoin/disconnect.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use sutegi_json::Json;
use sutegi_pubsub::BrokerExt;

use crate::hub::{ChannelHub, HubInner, Socket};

/// Client-facing event names (Phoenix's, on purpose — they are the de facto
/// vocabulary for presence).
pub const EV_STATE: &str = "presence_state";
pub const EV_DIFF: &str = "presence_diff";

/// Broker topic carrying presence traffic for a channel topic.
fn presence_topic(topic: &str) -> String {
    format!("stg:pres:{topic}")
}

/// Per-hub presence state. Lives inside the hub, keyed by channel topic.
pub(crate) struct PresenceState {
    topics: Mutex<HashMap<String, TopicPresence>>,
    /// State re-publish interval; a remote pod is expired after 2.5×.
    pub(crate) interval: Duration,
    /// The housekeeping thread is spawned lazily on the first track.
    housekeeper: Mutex<bool>,
}

impl Default for PresenceState {
    fn default() -> PresenceState {
        PresenceState {
            topics: Mutex::new(HashMap::new()),
            interval: Duration::from_secs(30),
            housekeeper: Mutex::new(false),
        }
    }
}

#[derive(Default)]
struct TopicPresence {
    /// Local ground truth: connection → (key, meta).
    local: HashMap<u64, (String, Json)>,
    /// Remote pods' claimed entries, by instance id.
    remote: HashMap<String, RemotePod>,
    /// Broker subscription for `stg:pres:<topic>`, held while this topic
    /// has any local presence.
    sub_id: u64,
}

struct RemotePod {
    last_seen: Instant,
    /// key → metas (one per membership tracked under that key).
    entries: BTreeMap<String, Vec<Json>>,
}

/// The presence API. All functions are associated (no instance): state
/// lives in the hub the socket belongs to.
pub struct Presence;

impl Presence {
    /// Track this membership under `key` (typically a user id) with `meta`
    /// (anything the UI needs: nick, avatar, device). Replaces this
    /// membership's previous track, if any. The tracked member is pushed
    /// the full [`EV_STATE`]; the topic gets an [`EV_DIFF`].
    pub fn track(socket: &Socket, key: &str, meta: Json) {
        // Inside on_join the member is not admitted yet; defer so the
        // presence_state push lands after the join reply and the join diff
        // reaches the joiner too.
        let key = key.to_string();
        socket.after_join(move |socket| Presence::track_now(socket, &key, meta));
    }

    fn track_now(socket: &Socket, key: &str, meta: Json) {
        let inner = socket.hub_inner();
        let topic = socket.topic().to_string();
        ensure_housekeeper(inner);

        let (replaced, first_local) = {
            let mut topics = inner.presence.topics.lock().unwrap();
            let entry = topics.entry(topic.clone()).or_default();
            let first = entry.local.is_empty() && entry.sub_id == 0;
            if first {
                entry.sub_id = subscribe(inner, &topic);
            }
            let replaced = entry
                .local
                .insert(socket.id(), (key.to_string(), meta.clone()));
            (replaced, first)
        };
        // A replaced track is a leave of the old meta first.
        if let Some((old_key, old_meta)) = replaced {
            publish_diff(inner, &topic, "leave", &old_key, &old_meta);
            fan_diff(inner, &topic, None, Some((&old_key, &old_meta)));
        }
        if first_local {
            // Ask peers for their state — we just started caring.
            publish_action(inner, &topic, "req", |_| {});
        }
        publish_diff(inner, &topic, "join", key, &meta);
        fan_diff(inner, &topic, Some((key, &meta)), None);
        socket.push(EV_STATE, &Presence::list_inner(inner, &topic));
    }

    /// Stop tracking this membership (its key's meta leaves the state).
    /// Called automatically on leave, rejoin, and disconnect.
    pub fn untrack(socket: &Socket) {
        untrack_on_teardown(socket.hub_inner(), socket.id(), socket.topic());
    }

    /// The merged presence view of `topic`: `{key: {"metas": [meta, ...]}}`,
    /// local and remote combined.
    pub fn list(hub: &ChannelHub, topic: &str) -> Json {
        Presence::list_inner(&hub.inner, topic)
    }

    fn list_inner(inner: &HubInner, topic: &str) -> Json {
        let topics = inner.presence.topics.lock().unwrap();
        let Some(entry) = topics.get(topic) else {
            return Json::Obj(BTreeMap::new());
        };
        let mut merged: BTreeMap<String, Vec<Json>> = BTreeMap::new();
        for (key, meta) in entry.local.values() {
            merged.entry(key.clone()).or_default().push(meta.clone());
        }
        for pod in entry.remote.values() {
            for (key, metas) in &pod.entries {
                merged
                    .entry(key.clone())
                    .or_default()
                    .extend(metas.iter().cloned());
            }
        }
        state_json(&merged)
    }
}

/// `{key: {"metas": [...]}}` from a merged map.
fn state_json(entries: &BTreeMap<String, Vec<Json>>) -> Json {
    let mut obj = BTreeMap::new();
    for (key, metas) in entries {
        obj.insert(
            key.clone(),
            Json::obj(vec![("metas", Json::arr(metas.clone()))]),
        );
    }
    Json::Obj(obj)
}

/// A `presence_diff` payload with at most one join and one leave (the
/// incremental case; state-sync diffs use [`diff_json`]).
fn single_diff(join: Option<(&str, &Json)>, leave: Option<(&str, &Json)>) -> Json {
    let side = |entry: Option<(&str, &Json)>| {
        let mut obj = BTreeMap::new();
        if let Some((key, meta)) = entry {
            obj.insert(
                key.to_string(),
                Json::obj(vec![("metas", Json::arr(vec![meta.clone()]))]),
            );
        }
        Json::Obj(obj)
    };
    Json::obj(vec![("joins", side(join)), ("leaves", side(leave))])
}

fn diff_json(joins: &BTreeMap<String, Vec<Json>>, leaves: &BTreeMap<String, Vec<Json>>) -> Json {
    Json::obj(vec![
        ("joins", state_json(joins)),
        ("leaves", state_json(leaves)),
    ])
}

/// Subscribe this hub to a topic's presence traffic.
fn subscribe(inner: &HubInner, topic: &str) -> u64 {
    let weak: Weak<HubInner> = inner.weak_ref();
    let topic_owned = topic.to_string();
    inner.broker.on(&presence_topic(topic), move |msg| {
        if let Some(hub) = weak.upgrade() {
            on_broker_message(&hub, &topic_owned, msg);
        }
    })
}

/// Publish one presence action on the broker. The closure adds
/// action-specific fields.
fn publish_action(
    inner: &HubInner,
    topic: &str,
    action: &str,
    add: impl FnOnce(&mut Vec<(&str, Json)>),
) {
    let mut fields = vec![
        ("o", Json::str(inner.instance.clone())),
        ("a", Json::str(action)),
    ];
    add(&mut fields);
    inner
        .broker
        .publish(&presence_topic(topic), &Json::obj(fields).to_string());
}

fn publish_diff(inner: &HubInner, topic: &str, action: &str, key: &str, meta: &Json) {
    publish_action(inner, topic, action, |fields| {
        fields.push(("k", Json::str(key)));
        fields.push(("m", meta.clone()));
    });
}

/// Publish this pod's full local state for a topic (heartbeat / sync reply).
fn publish_state(inner: &HubInner, topic: &str) {
    let entries: BTreeMap<String, Vec<Json>> = {
        let topics = inner.presence.topics.lock().unwrap();
        let Some(entry) = topics.get(topic) else {
            return;
        };
        let mut map: BTreeMap<String, Vec<Json>> = BTreeMap::new();
        for (key, meta) in entry.local.values() {
            map.entry(key.clone()).or_default().push(meta.clone());
        }
        map
    };
    publish_action(inner, topic, "state", |fields| {
        fields.push(("e", state_json_flat(&entries)));
    });
}

/// Wire form of a state map: `{key: [meta, ...]}` (flat, no "metas" nesting).
fn state_json_flat(entries: &BTreeMap<String, Vec<Json>>) -> Json {
    let mut obj = BTreeMap::new();
    for (key, metas) in entries {
        obj.insert(key.clone(), Json::arr(metas.clone()));
    }
    Json::Obj(obj)
}

fn parse_state_flat(json: &Json) -> BTreeMap<String, Vec<Json>> {
    let mut out = BTreeMap::new();
    if let Some(obj) = json.as_object() {
        for (key, metas) in obj {
            out.insert(key.clone(), metas.as_array().cloned().unwrap_or_default());
        }
    }
    out
}

/// Fan a `presence_diff` frame to this pod's local members of `topic`.
fn fan_diff(
    inner: &HubInner,
    topic: &str,
    join: Option<(&str, &Json)>,
    leave: Option<(&str, &Json)>,
) {
    inner.fan_local(topic, EV_DIFF, &single_diff(join, leave));
}

/// One presence message arrived on the broker (possibly our own echo).
fn on_broker_message(inner: &HubInner, topic: &str, msg: &str) {
    let Ok(parsed) = Json::parse(msg) else { return };
    let (Some(origin), Some(action)) = (
        parsed.get("o").and_then(Json::as_str),
        parsed.get("a").and_then(Json::as_str),
    ) else {
        return;
    };
    if origin == inner.instance {
        return; // local effects were applied at the call site
    }
    match action {
        "join" | "leave" => {
            let (Some(key), Some(meta)) = (parsed.get("k").and_then(Json::as_str), parsed.get("m"))
            else {
                return;
            };
            {
                let mut topics = inner.presence.topics.lock().unwrap();
                let entry = topics.entry(topic.to_string()).or_default();
                let pod = entry
                    .remote
                    .entry(origin.to_string())
                    .or_insert_with(|| RemotePod {
                        last_seen: Instant::now(),
                        entries: BTreeMap::new(),
                    });
                pod.last_seen = Instant::now();
                let metas = pod.entries.entry(key.to_string()).or_default();
                if action == "join" {
                    metas.push(meta.clone());
                } else {
                    remove_one(metas, meta);
                    if metas.is_empty() {
                        pod.entries.remove(key);
                    }
                }
            }
            let pair = (key, meta);
            let (join, leave) = if action == "join" {
                (Some(pair), None)
            } else {
                (None, Some(pair))
            };
            fan_diff(inner, topic, join, leave);
        }
        "state" => {
            let new = parse_state_flat(parsed.get("e").unwrap_or(&Json::Null));
            let (joins, leaves) = {
                let mut topics = inner.presence.topics.lock().unwrap();
                let entry = topics.entry(topic.to_string()).or_default();
                let old = entry
                    .remote
                    .get(origin)
                    .map(|p| p.entries.clone())
                    .unwrap_or_default();
                entry.remote.insert(
                    origin.to_string(),
                    RemotePod {
                        last_seen: Instant::now(),
                        entries: new.clone(),
                    },
                );
                diff_states(&old, &new)
            };
            if !joins.is_empty() || !leaves.is_empty() {
                inner.fan_local(topic, EV_DIFF, &diff_json(&joins, &leaves));
            }
        }
        "req" => publish_state(inner, topic),
        _ => {}
    }
}

/// Remove one meta equal to `meta` (JSON equality) from the vec.
fn remove_one(metas: &mut Vec<Json>, meta: &Json) {
    if let Some(pos) = metas.iter().position(|m| m == meta) {
        metas.remove(pos);
    }
}

/// joins/leaves between two `{key: [metas]}` maps (multiset semantics per
/// key: a meta present twice and now once is one leave).
fn diff_states(
    old: &BTreeMap<String, Vec<Json>>,
    new: &BTreeMap<String, Vec<Json>>,
) -> (BTreeMap<String, Vec<Json>>, BTreeMap<String, Vec<Json>>) {
    let mut joins: BTreeMap<String, Vec<Json>> = BTreeMap::new();
    let mut leaves: BTreeMap<String, Vec<Json>> = BTreeMap::new();
    for (key, new_metas) in new {
        let mut remaining = old.get(key).cloned().unwrap_or_default();
        for meta in new_metas {
            if let Some(pos) = remaining.iter().position(|m| m == meta) {
                remaining.remove(pos);
            } else {
                joins.entry(key.clone()).or_default().push(meta.clone());
            }
        }
        if !remaining.is_empty() {
            leaves.entry(key.clone()).or_default().extend(remaining);
        }
    }
    for (key, old_metas) in old {
        if !new.contains_key(key) {
            leaves
                .entry(key.clone())
                .or_default()
                .extend(old_metas.iter().cloned());
        }
    }
    (joins, leaves)
}

/// Hub teardown hook: drop this connection's presence on `topic`, if any,
/// and tell everyone.
pub(crate) fn untrack_on_teardown(inner: &HubInner, conn_id: u64, topic: &str) {
    let removed = {
        let mut topics = inner.presence.topics.lock().unwrap();
        let Some(entry) = topics.get_mut(topic) else {
            return;
        };
        let removed = entry.local.remove(&conn_id);
        // Keep the broker subscription while remote state is still worth
        // caching; drop everything when the topic is locally dead.
        if removed.is_some() && entry.local.is_empty() {
            let sub_id = entry.sub_id;
            topics.remove(topic);
            drop(topics);
            inner.broker.unsubscribe(&presence_topic(topic), sub_id);
        }
        removed
    };
    if let Some((key, meta)) = removed {
        publish_diff(inner, topic, "leave", &key, &meta);
        fan_diff(inner, topic, None, Some((&key, &meta)));
    }
}

/// Spawn the per-hub housekeeping thread on first use: re-publish local
/// state every interval, expire remote pods not heard from in 2.5×.
fn ensure_housekeeper(inner: &HubInner) {
    let mut started = inner.presence.housekeeper.lock().unwrap();
    if *started {
        return;
    }
    *started = true;
    let weak = inner.weak_ref();
    let interval = inner.presence.interval;
    let spawned = std::thread::Builder::new()
        .name("sutegi-presence".into())
        .spawn(move || loop {
            std::thread::sleep(interval);
            let Some(hub) = weak.upgrade() else { return };
            housekeep(&hub);
        });
    if spawned.is_err() {
        *started = false; // retry on the next track
    }
}

/// One heartbeat tick: publish our state per live topic, expire the dead.
fn housekeep(inner: &Arc<HubInner>) {
    let expiry = inner.presence.interval * 5 / 2;
    let topics: Vec<String> = {
        let map = inner.presence.topics.lock().unwrap();
        map.keys().cloned().collect()
    };
    for topic in topics {
        publish_state(inner, &topic);
        // Expire silent pods and report their members as leaves.
        let expired: Vec<(String, BTreeMap<String, Vec<Json>>)> = {
            let mut map = inner.presence.topics.lock().unwrap();
            let Some(entry) = map.get_mut(&topic) else {
                continue;
            };
            let dead: Vec<String> = entry
                .remote
                .iter()
                .filter(|(_, pod)| pod.last_seen.elapsed() > expiry)
                .map(|(id, _)| id.clone())
                .collect();
            dead.iter()
                .filter_map(|id| entry.remote.remove(id).map(|p| (id.clone(), p.entries)))
                .collect()
        };
        for (_instance, entries) in expired {
            if !entries.is_empty() {
                inner.fan_local(&topic, EV_DIFF, &diff_json(&BTreeMap::new(), &entries));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn metas(vals: &[&str]) -> Vec<Json> {
        vals.iter().map(|v| Json::str(*v)).collect()
    }

    #[test]
    fn state_json_shapes_metas() {
        let mut map = BTreeMap::new();
        map.insert("u1".to_string(), metas(&["a", "b"]));
        let json = state_json(&map);
        assert_eq!(
            json.pointer("/u1/metas/1").and_then(Json::as_str),
            Some("b")
        );
    }

    #[test]
    fn diff_states_computes_joins_and_leaves() {
        let mut old = BTreeMap::new();
        old.insert("u1".to_string(), metas(&["phone"]));
        old.insert("u2".to_string(), metas(&["web"]));
        let mut new = BTreeMap::new();
        new.insert("u1".to_string(), metas(&["phone", "web"])); // +web
        new.insert("u3".to_string(), metas(&["web"])); // new user

        let (joins, leaves) = diff_states(&old, &new);
        assert_eq!(joins.get("u1"), Some(&metas(&["web"])));
        assert_eq!(joins.get("u3"), Some(&metas(&["web"])));
        assert_eq!(leaves.get("u2"), Some(&metas(&["web"])));
        assert!(!joins.contains_key("u2"));
        assert!(!leaves.contains_key("u1"));
    }

    #[test]
    fn diff_states_handles_duplicate_metas_as_multisets() {
        let mut old = BTreeMap::new();
        old.insert("u".to_string(), metas(&["x", "x"]));
        let mut new = BTreeMap::new();
        new.insert("u".to_string(), metas(&["x"]));
        let (joins, leaves) = diff_states(&old, &new);
        assert!(joins.is_empty());
        assert_eq!(leaves.get("u"), Some(&metas(&["x"])));
    }

    #[test]
    fn identical_states_diff_to_nothing() {
        let mut s = BTreeMap::new();
        s.insert("u".to_string(), metas(&["a"]));
        let (joins, leaves) = diff_states(&s, &s.clone());
        assert!(joins.is_empty() && leaves.is_empty());
    }

    #[test]
    fn state_wire_form_round_trips() {
        let mut map = BTreeMap::new();
        map.insert("u1".to_string(), metas(&["a"]));
        map.insert("u2".to_string(), metas(&["b", "c"]));
        let wire = state_json_flat(&map);
        assert_eq!(parse_state_flat(&wire), map);
        // Garbage degrades to empty, never panics.
        assert!(parse_state_flat(&Json::str("junk")).is_empty());
        assert!(parse_state_flat(&Json::Null).is_empty());
    }

    #[test]
    fn remove_one_removes_exactly_one() {
        let mut m = metas(&["a", "b", "a"]);
        remove_one(&mut m, &Json::str("a"));
        assert_eq!(m, metas(&["b", "a"]));
        remove_one(&mut m, &Json::str("zzz")); // absent: no-op
        assert_eq!(m, metas(&["b", "a"]));
    }
}
