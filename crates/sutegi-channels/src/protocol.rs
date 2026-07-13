//! The channel wire protocol: one JSON envelope per WebSocket text frame.
//!
//! ```json
//! {"topic":"room:1","event":"new_msg","ref":"3","join_ref":"1","payload":{...}}
//! ```
//!
//! Deliberately an *object*, not Phoenix's positional array: the envelope is
//! self-describing, which is what makes `/__channels` enough for an agent to
//! speak the protocol with no client library.
//!
//! Control events live under the `stg:` prefix (reserved — user channels
//! cannot register or send them):
//!
//! | event         | direction | meaning                                          |
//! |---------------|-----------|--------------------------------------------------|
//! | `stg:join`    | c → s     | join `topic`; payload goes to the join callback  |
//! | `stg:leave`   | c → s     | leave `topic`                                    |
//! | `stg:reply`   | s → c     | reply to a `ref`; payload `{status, response}`   |
//! | `stg:error`   | s → c     | protocol-level error not tied to a `ref`         |
//! | `stg:close`   | s → c     | the server ended this channel membership         |
//! | `heartbeat`   | c → s     | on topic `stg`; replied to like any ref'd push   |
//!
//! `ref` is an opaque client-chosen string echoed in the reply. `join_ref`
//! is the `ref` of the join that created the membership; the server stamps
//! it on member-targeted pushes so a client can discard frames from a
//! previous join instance after a rejoin.

use sutegi_json::Json;

/// The control topic (heartbeats live here).
pub const CONTROL_TOPIC: &str = "stg";
/// Reserved event-name prefix.
pub const RESERVED_PREFIX: &str = "stg:";

pub const EV_JOIN: &str = "stg:join";
pub const EV_LEAVE: &str = "stg:leave";
pub const EV_REPLY: &str = "stg:reply";
pub const EV_ERROR: &str = "stg:error";
pub const EV_CLOSE: &str = "stg:close";
pub const EV_HEARTBEAT: &str = "heartbeat";

/// One parsed inbound frame.
#[derive(Clone, Debug, PartialEq)]
pub struct Envelope {
    pub topic: String,
    pub event: String,
    /// Client ref to echo in a reply; absent = fire-and-forget.
    pub reference: Option<String>,
    /// The membership instance this frame belongs to (see module docs).
    pub join_ref: Option<String>,
    pub payload: Json,
}

/// Parse one text frame. The bytes are client-controlled: every failure is
/// an `Err` with a reason (echoed back in an `stg:error`), never a panic.
pub fn parse(text: &str) -> Result<Envelope, String> {
    let json = Json::parse(text).map_err(|e| format!("not JSON: {e}"))?;
    let obj = json.as_object().ok_or("envelope must be a JSON object")?;
    let field = |k: &str| -> Result<String, String> {
        obj.get(k)
            .and_then(Json::as_str)
            .map(str::to_string)
            .ok_or_else(|| format!("envelope is missing string field {k:?}"))
    };
    let opt_field = |k: &str| -> Result<Option<String>, String> {
        match obj.get(k) {
            None => Ok(None),
            Some(v) if v.is_null() => Ok(None),
            Some(v) => v
                .as_str()
                .map(|s| Some(s.to_string()))
                .ok_or_else(|| format!("envelope field {k:?} must be a string")),
        }
    };
    let topic = field("topic")?;
    let event = field("event")?;
    if topic.is_empty() || event.is_empty() {
        return Err("topic and event must be non-empty".into());
    }
    Ok(Envelope {
        topic,
        event,
        reference: opt_field("ref")?,
        join_ref: opt_field("join_ref")?,
        payload: obj.get("payload").cloned().unwrap_or(Json::Null),
    })
}

/// Serialize a server→client frame. `reference`/`join_ref` are omitted when
/// absent (not sent as null) to keep broadcast frames minimal.
pub fn serialize(
    topic: &str,
    event: &str,
    reference: Option<&str>,
    join_ref: Option<&str>,
    payload: &Json,
) -> String {
    let mut fields = vec![("topic", Json::str(topic)), ("event", Json::str(event))];
    if let Some(r) = reference {
        fields.push(("ref", Json::str(r)));
    }
    if let Some(j) = join_ref {
        fields.push(("join_ref", Json::str(j)));
    }
    fields.push(("payload", payload.clone()));
    Json::obj(fields).to_string()
}

/// An `stg:reply` payload: `{"status": "ok"|"error", "response": ...}`.
pub fn reply_payload(ok: bool, response: &Json) -> Json {
    Json::obj(vec![
        ("status", Json::str(if ok { "ok" } else { "error" })),
        ("response", response.clone()),
    ])
}

/// An `stg:error` payload.
pub fn error_payload(reason: &str) -> Json {
    Json::obj(vec![("reason", Json::str(reason))])
}

/// Does `pattern` match `topic`? Exact match, or a trailing `*` prefix
/// wildcard (`room:*` matches `room:1`, `room:`, `room:1:typing` — anything
/// with the prefix). The wildcard is only honored at the end; a `*` anywhere
/// else is literal.
pub fn topic_matches(pattern: &str, topic: &str) -> bool {
    match pattern.strip_suffix('*') {
        Some(prefix) => topic.starts_with(prefix),
        None => pattern == topic,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_full_envelope() {
        let e = parse(r#"{"topic":"room:1","event":"stg:join","ref":"1","join_ref":"1","payload":{"nick":"ada"}}"#).unwrap();
        assert_eq!(e.topic, "room:1");
        assert_eq!(e.event, "stg:join");
        assert_eq!(e.reference.as_deref(), Some("1"));
        assert_eq!(e.join_ref.as_deref(), Some("1"));
        assert_eq!(
            e.payload.pointer("/nick").and_then(Json::as_str),
            Some("ada")
        );
    }

    #[test]
    fn ref_join_ref_and_payload_are_optional() {
        let e = parse(r#"{"topic":"t","event":"e"}"#).unwrap();
        assert_eq!(e.reference, None);
        assert_eq!(e.join_ref, None);
        assert_eq!(e.payload, Json::Null);
        // Explicit nulls are treated as absent, not as the string "null".
        let e = parse(r#"{"topic":"t","event":"e","ref":null,"payload":null}"#).unwrap();
        assert_eq!(e.reference, None);
    }

    #[test]
    fn rejects_malformed_envelopes_with_reasons() {
        for (frame, needle) in [
            ("", "not JSON"),
            ("[1,2]", "must be a JSON object"),
            (r#"{"event":"e"}"#, "topic"),
            (r#"{"topic":"t"}"#, "event"),
            (r#"{"topic":"","event":"e"}"#, "non-empty"),
            (r#"{"topic":"t","event":"e","ref":7}"#, "\"ref\""),
            (r#"{"topic":5,"event":"e"}"#, "topic"),
        ] {
            let err = parse(frame).unwrap_err();
            assert!(err.contains(needle), "frame {frame:?} gave {err:?}");
        }
    }

    #[test]
    fn serialize_round_trips_and_omits_absent_refs() {
        let s = serialize("room:1", "new_msg", None, None, &Json::str("hi"));
        assert!(!s.contains("\"ref\""));
        assert!(!s.contains("join_ref"));
        let e = parse(&s).unwrap();
        assert_eq!(e.topic, "room:1");
        assert_eq!(e.payload, Json::str("hi"));

        let s = serialize(
            "t",
            "stg:reply",
            Some("9"),
            Some("2"),
            &reply_payload(true, &Json::Null),
        );
        let e = parse(&s).unwrap();
        assert_eq!(e.reference.as_deref(), Some("9"));
        assert_eq!(e.join_ref.as_deref(), Some("2"));
        assert_eq!(
            e.payload.pointer("/status").and_then(Json::as_str),
            Some("ok")
        );
    }

    #[test]
    fn topic_matching() {
        assert!(topic_matches("room:1", "room:1"));
        assert!(!topic_matches("room:1", "room:12"));
        assert!(topic_matches("room:*", "room:1"));
        assert!(topic_matches("room:*", "room:"));
        assert!(topic_matches("room:*", "room:1:typing"));
        assert!(!topic_matches("room:*", "lobby"));
        assert!(topic_matches("*", "anything"));
        // A non-trailing star is literal.
        assert!(topic_matches("a*b", "a*b"));
        assert!(!topic_matches("a*b", "axb"));
    }

    // Client-controlled bytes: garbage must degrade to Err, never panic.
    fn splitmix(state: &mut u64) -> u64 {
        *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = *state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    #[test]
    fn parse_never_panics_on_garbage() {
        let mut seed = 0x4348_414e_4e45_4c53u64; // "CHANNELS"
        let alphabet = br#"{}[]":,topicevntrfjpayload0123456789\u"#;
        for _ in 0..50_000 {
            let len = (splitmix(&mut seed) as usize) % 96;
            let s: String = (0..len)
                .map(|_| alphabet[(splitmix(&mut seed) as usize) % alphabet.len()] as char)
                .collect();
            let _ = parse(&s);
        }
    }
}
