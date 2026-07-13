//! `LISTEN`/`NOTIFY`: a dedicated notification connection.
//!
//! PostgreSQL delivers `NotificationResponse` messages asynchronously on any
//! connection that has run `LISTEN`, interleaved with whatever else that
//! connection is doing — so notifications get their own connection, owned by
//! one receive loop, instead of contaminating the pool. That is the same
//! split every serious client makes.
//!
//! ```no_run
//! use sutegi_pg::{Config, Listener};
//!
//! let cfg = Config::from_env().unwrap();
//! let mut listener = Listener::connect(&cfg).unwrap();
//! listener.listen("jobs").unwrap();
//! loop {
//!     let n = listener.recv().unwrap(); // blocks until a NOTIFY arrives
//!     println!("{}: {}", n.channel, n.payload);
//! }
//! ```
//!
//! `recv()` blocks indefinitely (the read timeout from [`Config`] is cleared —
//! a quiet hour is not an error). To stop a blocked listener from another
//! thread, take a [`ListenerShutdown`] handle first and call `shutdown()`.

use std::collections::VecDeque;
use std::net::{Shutdown, TcpStream};

use crate::protocol::{be_i32, parse_error, Client};
use crate::Config;

/// PostgreSQL truncates identifiers to `NAMEDATALEN - 1` bytes. A longer
/// channel name would be silently aliased with every other name sharing its
/// first 63 bytes — a correctness trap, so we refuse it instead.
const MAX_CHANNEL_LEN: usize = 63;

/// One `NOTIFY` received from the server.
#[derive(Clone, Debug, PartialEq)]
pub struct Notification {
    /// Backend PID of the notifying session.
    pub pid: i32,
    /// The channel the `NOTIFY` was sent on.
    pub channel: String,
    /// The notification payload ("" when none was given).
    pub payload: String,
}

/// A connection dedicated to `LISTEN`/`NOTIFY`.
///
/// Single-owner by design (`&mut` methods): register channels, then drive
/// [`Listener::recv`] from one loop — typically a thread you spawn. Use
/// [`Listener::shutdown_handle`] before moving it into the thread if you
/// need to interrupt it later.
pub struct Listener {
    client: Client,
    /// Notifications that arrived interleaved with a `LISTEN`/`UNLISTEN`
    /// round-trip; drained by `recv()` before reading the socket again.
    buffered: VecDeque<Notification>,
}

/// A cheap cross-thread handle that can interrupt a [`Listener`] blocked in
/// [`Listener::recv`] by shutting the socket down. The listener's next read
/// returns an error; the owner decides whether that means "stop" or
/// "reconnect".
#[derive(Clone)]
pub struct ListenerShutdown {
    stream: std::sync::Arc<TcpStream>,
}

impl ListenerShutdown {
    /// Close both directions of the listener's socket. Idempotent.
    pub fn shutdown(&self) {
        let _ = self.stream.shutdown(Shutdown::Both);
    }
}

impl Listener {
    /// Connect and authenticate a fresh notification connection. The
    /// [`Config`] read timeout is cleared on the read side: a listener
    /// legitimately sits idle for hours.
    pub fn connect(cfg: &Config) -> Result<Listener, String> {
        let client = Client::connect(cfg)?;
        client
            .read_stream()
            .set_read_timeout(None)
            .map_err(|e| format!("clear read timeout: {e}"))?;
        Ok(Listener {
            client,
            buffered: VecDeque::new(),
        })
    }

    /// Start listening on `channel`. Names are quoted, so any byte string a
    /// PostgreSQL identifier can hold is fine — but NUL bytes and names over
    /// 63 bytes (which the server would silently truncate and alias) are
    /// refused.
    pub fn listen(&mut self, channel: &str) -> Result<(), String> {
        self.simple_command(&format!("LISTEN {}", quote_ident(channel)?))
    }

    /// Stop listening on `channel`.
    pub fn unlisten(&mut self, channel: &str) -> Result<(), String> {
        self.simple_command(&format!("UNLISTEN {}", quote_ident(channel)?))
    }

    /// Block until the next notification arrives. Returns notifications that
    /// arrived during a `listen`/`unlisten` round-trip first, in order.
    ///
    /// An `Err` means the connection is gone (server restart, network drop,
    /// or a [`ListenerShutdown`]); reconnect with [`Listener::connect`] and
    /// re-`listen` your channels.
    pub fn recv(&mut self) -> Result<Notification, String> {
        loop {
            if let Some(n) = self.buffered.pop_front() {
                return Ok(n);
            }
            let (tag, body) = self.client.recv()?;
            match tag {
                b'A' => match parse_notification(&body) {
                    Some(n) => return Ok(n),
                    None => return Err("malformed NotificationResponse".into()),
                },
                b'E' => return Err(parse_error(&body)),
                // N (Notice), S (ParameterStatus), Z, C from earlier commands.
                _ => continue,
            }
        }
    }

    /// A handle that can interrupt a blocked [`Listener::recv`] from another
    /// thread.
    pub fn shutdown_handle(&self) -> Result<ListenerShutdown, String> {
        let stream = self
            .client
            .read_stream()
            .try_clone()
            .map_err(|e| format!("clone socket: {e}"))?;
        Ok(ListenerShutdown {
            stream: std::sync::Arc::new(stream),
        })
    }

    /// Run one simple-protocol command to `ReadyForQuery`, capturing (not
    /// dropping) any notifications that arrive interleaved. `Client::batch`
    /// would discard them — this is why the listener has its own loop.
    fn simple_command(&mut self, sql: &str) -> Result<(), String> {
        let mut msg = sql.as_bytes().to_vec();
        msg.push(0);
        self.client.send_msg(b'Q', &msg)?;
        let mut err = None;
        loop {
            let (tag, body) = self.client.recv()?;
            match tag {
                b'Z' => break,
                b'E' => err = Some(parse_error(&body)),
                b'A' => {
                    if let Some(n) = parse_notification(&body) {
                        self.buffered.push_back(n);
                    }
                }
                _ => continue,
            }
        }
        match err {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }
}

/// Double-quote a channel name for `LISTEN`/`UNLISTEN`, doubling embedded
/// quotes. Refuses NULs (they would truncate the simple-query message) and
/// names past PostgreSQL's 63-byte identifier limit (silent truncation would
/// alias distinct channels).
fn quote_ident(name: &str) -> Result<String, String> {
    if name.is_empty() {
        return Err("channel name must not be empty".into());
    }
    if name.as_bytes().contains(&0) {
        return Err("channel name must not contain NUL bytes".into());
    }
    if name.len() > MAX_CHANNEL_LEN {
        return Err(format!(
            "channel name exceeds PostgreSQL's {MAX_CHANNEL_LEN}-byte identifier limit \
             (it would be silently truncated): {name:?}"
        ));
    }
    Ok(format!("\"{}\"", name.replace('"', "\"\"")))
}

/// NotificationResponse body: `i32 pid` + channel cstr + payload cstr.
/// Server-controlled bytes, so every read is bounds-checked — a malformed
/// frame yields `None`, never a panic.
fn parse_notification(body: &[u8]) -> Option<Notification> {
    if body.len() < 4 {
        return None;
    }
    let pid = be_i32(body, 0);
    let rest = &body[4..];
    let (channel, rest) = take_cstr(rest)?;
    let (payload, _) = take_cstr(rest)?;
    Some(Notification {
        pid,
        channel,
        payload,
    })
}

/// Split one NUL-terminated string off the front of `buf`.
fn take_cstr(buf: &[u8]) -> Option<(String, &[u8])> {
    let end = buf.iter().position(|&b| b == 0)?;
    let s = String::from_utf8_lossy(&buf[..end]).into_owned();
    Some((s, &buf[end + 1..]))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn notification_body(pid: i32, channel: &[u8], payload: &[u8]) -> Vec<u8> {
        let mut body = pid.to_be_bytes().to_vec();
        body.extend_from_slice(channel);
        body.push(0);
        body.extend_from_slice(payload);
        body.push(0);
        body
    }

    #[test]
    fn parses_a_wellformed_notification() {
        let body = notification_body(4711, b"jobs", b"{\"id\":7}");
        assert_eq!(
            parse_notification(&body),
            Some(Notification {
                pid: 4711,
                channel: "jobs".into(),
                payload: "{\"id\":7}".into(),
            })
        );
    }

    #[test]
    fn empty_payload_is_empty_string() {
        let body = notification_body(1, b"ping", b"");
        assert_eq!(parse_notification(&body).unwrap().payload, "");
    }

    #[test]
    fn truncated_bodies_yield_none_not_panic() {
        // No payload terminator.
        let mut body = 7i32.to_be_bytes().to_vec();
        body.extend_from_slice(b"chan\0payload-without-nul");
        assert_eq!(parse_notification(&body), None);
        // No channel terminator at all.
        assert_eq!(parse_notification(&5i32.to_be_bytes()), None);
        // Shorter than the pid.
        assert_eq!(parse_notification(b"\x00\x01"), None);
        assert_eq!(parse_notification(b""), None);
    }

    #[test]
    fn quote_ident_escapes_and_refuses() {
        assert_eq!(quote_ident("jobs").unwrap(), "\"jobs\"");
        assert_eq!(quote_ident("room:1").unwrap(), "\"room:1\"");
        assert_eq!(quote_ident("we\"ird").unwrap(), "\"we\"\"ird\"");
        assert!(quote_ident("").is_err());
        assert!(quote_ident("nul\0byte").is_err());
        assert!(quote_ident(&"x".repeat(64)).is_err());
        assert!(quote_ident(&"x".repeat(63)).is_ok());
    }

    // -- adversarial: the notification body is server-controlled bytes, and
    // with no TLS a MITM controls it too. Same posture as the protocol
    // parsers: garbage must degrade, never panic.

    fn splitmix(state: &mut u64) -> u64 {
        *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = *state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    #[test]
    fn parse_notification_never_panics_on_garbage() {
        let mut seed = 0x4e4f_5449_4659_0000u64; // "NOTIFY"
        for _ in 0..50_000 {
            let len = (splitmix(&mut seed) as usize) % 64;
            let body: Vec<u8> = (0..len).map(|_| splitmix(&mut seed) as u8).collect();
            let _ = parse_notification(&body);
            let _ = take_cstr(&body);
        }
    }
}
