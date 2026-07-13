//! Declaring a channel: which topics it serves, how joins are authorized,
//! which events it handles, and the schemas `/__channels` advertises.

use std::collections::BTreeMap;
use std::sync::Arc;

use sutegi_json::Json;

use crate::hub::Socket;
use crate::protocol::RESERVED_PREFIX;

/// What an event handler tells the client.
pub enum Reply {
    /// Reply `{status:"ok", response}` (when the push carried a `ref`).
    Ok(Json),
    /// Reply `{status:"error", response}`.
    Err(Json),
    /// No reply — fire-and-forget events.
    None,
}

/// Why a member's channel membership ended, passed to the leave callback.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LeaveReason {
    /// The client sent `stg:leave`.
    Leave,
    /// The client joined the same topic again; the old membership is torn
    /// down before the new join callback runs.
    Rejoin,
    /// The WebSocket connection closed (cleanly or not).
    Disconnect,
    /// The server ended the membership ([`Socket::kick`] / hub close).
    Kicked,
}

pub(crate) type JoinFn = Arc<dyn Fn(&Socket, &Json) -> Result<Json, Json> + Send + Sync>;
pub(crate) type EventFn = Arc<dyn Fn(&Socket, &Json) -> Reply + Send + Sync>;
pub(crate) type LeaveFn = Arc<dyn Fn(&Socket, LeaveReason) + Send + Sync>;

/// Documentation + schema for one event, surfaced in `/__channels`.
#[derive(Clone, Default)]
pub(crate) struct EventDoc {
    pub doc: String,
    /// JSON schema of the payload (sutegi-validate style), if declared.
    pub schema: Option<Json>,
}

/// One channel definition: a topic pattern plus its callbacks and schemas.
///
/// ```ignore
/// Channel::new("room:*")
///     .doc("A chat room. Join with {nick}; messages fan out to the room.")
///     .join_schema(json!({"nick": "string"}))
///     .on_join(|socket, payload| {
///         let nick = payload.pointer("/nick").and_then(Json::as_str).ok_or(Json::str("nick required"))?;
///         socket.assign("nick", Json::str(nick));
///         Ok(Json::Null)
///     })
///     .on("new_msg", |socket, payload| {
///         socket.broadcast("new_msg", payload);
///         Reply::None
///     })
///     .on_leave(|socket, _reason| { /* announce departure */ })
/// ```
///
/// Handlers run inline on a reactor shard (the `sutegi-ws` threading
/// contract): keep them CPU-quick and push blocking work to your own threads.
#[derive(Clone)]
pub struct Channel {
    pub(crate) pattern: String,
    pub(crate) doc: String,
    pub(crate) join: Option<JoinFn>,
    pub(crate) join_doc: EventDoc,
    pub(crate) handlers: BTreeMap<String, EventFn>,
    pub(crate) event_docs: BTreeMap<String, EventDoc>,
    /// Server→client events this channel emits, for the manifest only.
    pub(crate) emits: BTreeMap<String, EventDoc>,
    pub(crate) leave: Option<LeaveFn>,
}

impl Channel {
    /// A channel serving `pattern`: an exact topic (`"lobby"`) or a trailing
    /// wildcard (`"room:*"`). The first registered channel whose pattern
    /// matches a join's topic wins.
    pub fn new(pattern: &str) -> Channel {
        assert!(
            !pattern.is_empty() && pattern != "stg" && !pattern.starts_with("stg:"),
            "channel pattern {pattern:?} is empty or collides with the reserved control topic"
        );
        Channel {
            pattern: pattern.to_string(),
            doc: String::new(),
            join: None,
            join_doc: EventDoc::default(),
            handlers: BTreeMap::new(),
            event_docs: BTreeMap::new(),
            emits: BTreeMap::new(),
            leave: None,
        }
    }

    /// One-paragraph description for `/__channels` — write it for an agent
    /// deciding whether to join.
    pub fn doc(mut self, doc: &str) -> Channel {
        self.doc = doc.to_string();
        self
    }

    /// Authorize + set up a join. `Ok(response)` admits the member (the
    /// response rides the ok-reply); `Err(reason)` refuses it. No callback =
    /// open channel, joins always succeed with a null response.
    pub fn on_join(
        mut self,
        f: impl Fn(&Socket, &Json) -> Result<Json, Json> + Send + Sync + 'static,
    ) -> Channel {
        self.join = Some(Arc::new(f));
        self
    }

    /// Document the join payload (`doc` + optional schema) in the manifest.
    pub fn join_schema(mut self, doc: &str, schema: Json) -> Channel {
        self.join_doc = EventDoc {
            doc: doc.to_string(),
            schema: Some(schema),
        };
        self
    }

    /// Handle a client-pushed event. Reserved (`stg:`-prefixed) names are
    /// refused at registration.
    pub fn on(
        mut self,
        event: &str,
        f: impl Fn(&Socket, &Json) -> Reply + Send + Sync + 'static,
    ) -> Channel {
        assert!(
            !event.starts_with(RESERVED_PREFIX) && !event.is_empty(),
            "event name {event:?} is empty or uses the reserved {RESERVED_PREFIX:?} prefix"
        );
        self.handlers.insert(event.to_string(), Arc::new(f));
        self
    }

    /// Document a client→server event (`doc` + optional payload schema).
    pub fn event_schema(mut self, event: &str, doc: &str, schema: Json) -> Channel {
        self.event_docs.insert(
            event.to_string(),
            EventDoc {
                doc: doc.to_string(),
                schema: Some(schema),
            },
        );
        self
    }

    /// Document a server→client event this channel emits (broadcasts and
    /// pushes), so an agent knows what frames to expect after joining.
    pub fn emits(mut self, event: &str, doc: &str, schema: Json) -> Channel {
        self.emits.insert(
            event.to_string(),
            EventDoc {
                doc: doc.to_string(),
                schema: Some(schema),
            },
        );
        self
    }

    /// Called once per membership teardown, with why. The socket is still
    /// addressable (a goodbye broadcast works) unless the reason is
    /// `Disconnect`.
    pub fn on_leave(mut self, f: impl Fn(&Socket, LeaveReason) + Send + Sync + 'static) -> Channel {
        self.leave = Some(Arc::new(f));
        self
    }

    /// This channel's manifest entry.
    pub(crate) fn manifest(&self) -> Json {
        let event_entry = |name: &str, d: &EventDoc| {
            let mut fields = vec![
                ("event", Json::str(name)),
                ("doc", Json::str(d.doc.clone())),
            ];
            if let Some(s) = &d.schema {
                fields.push(("payload_schema", s.clone()));
            }
            Json::obj(fields)
        };
        let mut handles: Vec<Json> = Vec::new();
        for name in self.handlers.keys() {
            let d = self.event_docs.get(name).cloned().unwrap_or_default();
            handles.push(event_entry(name, &d));
        }
        let emits: Vec<Json> = self.emits.iter().map(|(n, d)| event_entry(n, d)).collect();
        let mut join = vec![("doc", Json::str(self.join_doc.doc.clone()))];
        if let Some(s) = &self.join_doc.schema {
            join.push(("payload_schema", s.clone()));
        }
        Json::obj(vec![
            ("pattern", Json::str(self.pattern.clone())),
            ("doc", Json::str(self.doc.clone())),
            ("join", Json::obj(join)),
            ("handles", Json::arr(handles)),
            ("emits", Json::arr(emits)),
        ])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[should_panic(expected = "reserved")]
    fn reserved_event_names_are_refused() {
        let _ = Channel::new("room:*").on("stg:join", |_, _| Reply::None);
    }

    #[test]
    #[should_panic(expected = "reserved")]
    fn control_topic_pattern_is_refused() {
        let _ = Channel::new("stg");
    }

    #[test]
    fn manifest_lists_events_and_schemas() {
        let ch = Channel::new("room:*")
            .doc("A room.")
            .join_schema(
                "Pass a nick.",
                Json::obj(vec![("nick", Json::str("string"))]),
            )
            .on("new_msg", |_, _| Reply::None)
            .event_schema(
                "new_msg",
                "Say something.",
                Json::obj(vec![("body", Json::str("string"))]),
            )
            .emits(
                "new_msg",
                "A message said by anyone in the room.",
                Json::Null,
            );
        let m = ch.manifest();
        assert_eq!(m.pointer("/pattern").and_then(Json::as_str), Some("room:*"));
        assert_eq!(
            m.pointer("/handles/0/event").and_then(Json::as_str),
            Some("new_msg")
        );
        assert_eq!(
            m.pointer("/join/payload_schema/nick")
                .and_then(Json::as_str),
            Some("string")
        );
        assert!(m.pointer("/emits/0/doc").is_some());
    }
}
