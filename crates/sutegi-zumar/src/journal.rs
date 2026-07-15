//! The reconnect journal: a session's ordered stream of program inputs
//! (dispatches and effect completions, as wire frames). Replay a journal
//! through a fresh `Program` and you are back at the exact model — TEA
//! updates are pure and the runtime's id allocation is deterministic, so
//! the journaled completions land on the same ids a fresh run allocates.
//!
//! [`EventJournal`] stores frames in sutegi-events (`zumar-live:<session>`
//! streams, one `frame` event per input) — the LiveView-parity reconnect
//! Phoenix gets from OTP process state, sutegi gets from its event store.
//! [`MemJournal`] is for tests and single-process ephemeral use.

use std::collections::HashMap;
use std::sync::Mutex;

use sutegi_crypto::{from_hex, hex};
use sutegi_events::{event, EventStore, Expected};
use sutegi_json::Json;
use sutegi_orm::Transactional;

pub trait Journal: Send + Sync + 'static {
    fn append(&self, session: &str, frame: &[u8]) -> Result<(), String>;
    fn load(&self, session: &str) -> Result<Vec<Vec<u8>>, String>;
    /// Garbage collect sessions that haven't received an event since `cutoff_epoch_secs`.
    /// Returns the number of sessions deleted. Defaults to a no-op.
    fn trim(&self, cutoff_epoch_secs: i64) -> Result<u64, String> {
        let _ = cutoff_epoch_secs;
        Ok(0)
    }
}

/// In-memory journal — survives reconnects, not restarts.
#[derive(Default)]
pub struct MemJournal {
    streams: Mutex<HashMap<String, Vec<Vec<u8>>>>,
}

impl Journal for MemJournal {
    fn append(&self, session: &str, frame: &[u8]) -> Result<(), String> {
        self.streams
            .lock()
            .unwrap()
            .entry(session.to_string())
            .or_default()
            .push(frame.to_vec());
        Ok(())
    }

    fn load(&self, session: &str) -> Result<Vec<Vec<u8>>, String> {
        Ok(self
            .streams
            .lock()
            .unwrap()
            .get(session)
            .cloned()
            .unwrap_or_default())
    }

    fn trim(&self, cutoff_epoch_secs: i64) -> Result<u64, String> {
        // MemJournal doesn't store timestamps; could be added, but for now
        // we just clear everything for simplicity if it's called, or do nothing.
        // Doing nothing is safer.
        let _ = cutoff_epoch_secs;
        Ok(0)
    }
}

/// Durable journal over sutegi-events: works on SQLite (single node) or
/// Postgres (a session can reconnect to *any* pod). Frames are hex in the
/// event payload — sutegi-json has no bytes type, and frames are tiny.
pub struct EventJournal<B: Transactional> {
    store: EventStore<B>,
}

impl<B: Transactional> EventJournal<B> {
    /// Wrap a backend; creates the event tables if missing.
    pub fn new(backend: B) -> Result<Self, String> {
        let store = EventStore::new(backend);
        store.migrate()?;
        Ok(EventJournal { store })
    }

    fn stream(session: &str) -> String {
        format!("zumar-live:{session}")
    }
}

impl<B: Transactional + Send + Sync + 'static> Journal for EventJournal<B> {
    fn append(&self, session: &str, frame: &[u8]) -> Result<(), String> {
        self.store
            .append(
                &Self::stream(session),
                Expected::Any,
                &[event(
                    "frame",
                    Json::obj(vec![("hex", Json::str(hex(frame)))]),
                )],
            )
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    fn load(&self, session: &str) -> Result<Vec<Vec<u8>>, String> {
        let events = self.store.read_stream(&Self::stream(session), 0)?;
        events
            .iter()
            .map(|e| {
                e.payload
                    .get("hex")
                    .and_then(Json::as_str)
                    .and_then(from_hex)
                    .ok_or_else(|| format!("corrupt frame at {}:{}", e.stream, e.version))
            })
            .collect()
    }

    fn trim(&self, cutoff_epoch_secs: i64) -> Result<u64, String> {
        use sutegi_orm::Value;
        // Delete any zumar-live:* stream whose most recent event is older than the cutoff.
        // The subquery finds exactly those streams.
        let sql = "\
            DELETE FROM sutegi_events \
            WHERE stream LIKE 'zumar-live:%' \
              AND stream IN ( \
                  SELECT stream FROM sutegi_events \
                  WHERE stream LIKE 'zumar-live:%' \
                  GROUP BY stream \
                  HAVING MAX(created_at) < ? \
              )";
        let deleted = self.store.backend().execute(sql, &[Value::Int(cutoff_epoch_secs)])?;
        Ok(deleted as u64)
    }
}

/// Session ids come from the client; only a conservative shape reaches a
/// stream name (and thus SQL text params + journal keys).
pub(crate) fn valid_session(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;
    use sutegi_orm::db::Db;

    #[test]
    fn event_journal_round_trips_frames_in_order() {
        let journal = EventJournal::new(Db::memory().unwrap()).unwrap();
        let frames: Vec<Vec<u8>> = vec![vec![1, 0, 0], vec![1, 1, 7, 1], vec![1, 2, 3, 9]];
        for f in &frames {
            journal.append("s1", f).unwrap();
        }
        journal.append("s2", &[9, 9]).unwrap(); // another session, isolated
        assert_eq!(journal.load("s1").unwrap(), frames);
        assert_eq!(journal.load("s2").unwrap(), vec![vec![9, 9]]);
        assert!(journal.load("nobody").unwrap().is_empty());
    }

    #[test]
    fn session_ids_are_shape_checked() {
        assert!(valid_session("a1B-_x"));
        assert!(!valid_session(""));
        assert!(!valid_session("has space"));
        assert!(!valid_session(&"x".repeat(65)));
        assert!(!valid_session("semi;colon"));
    }
}
