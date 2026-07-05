//! WebSocket endpoints on the [`App`](crate::App): handshake validation and
//! the handoff from the blocking HTTP worker to the `sutegi-ws` reactor.
//!
//! ```ignore
//! let app = App::new("chat").ws(
//!     "/ws",
//!     "Chat socket: joins the room, broadcasts every message.",
//!     Ws::new()
//!         .on_open(|conn, req| { /* roster.insert(conn.clone()) */ })
//!         .on_message(|conn, msg| { /* fan out */ })
//!         .on_close(|conn, code| { /* roster.remove(conn.id()) */ }),
//! );
//! ```
//!
//! Handlers run inline on a reactor shard (see `sutegi-ws` for the threading
//! contract): keep them CPU-quick, push blocking work to your own threads,
//! and reply via the cloned [`Conn`] handle.

use std::sync::Arc;

use sutegi_http::{Request, Response};
pub use sutegi_ws::{binary_frame, text_frame, Conn, Msg, WsConfig, WsRuntime};

/// Builder for one WebSocket endpoint's callbacks.
#[derive(Clone, Default)]
pub struct Ws {
    pub(crate) handlers: sutegi_ws::Handlers,
}

impl Ws {
    pub fn new() -> Ws {
        Ws::default()
    }

    /// Connection established. Receives the upgrade request (path params,
    /// headers, cookies) for identity/auth decisions; keep a `conn.clone()`
    /// if you want to push to this socket later.
    pub fn on_open(mut self, f: impl Fn(&Conn, &Request) + Send + Sync + 'static) -> Ws {
        self.handlers.on_open = Some(Arc::new(f));
        self
    }

    /// A complete (defragmented, UTF-8-validated) message arrived.
    pub fn on_message(mut self, f: impl Fn(&Conn, Msg) + Send + Sync + 'static) -> Ws {
        self.handlers.on_message = Some(Arc::new(f));
        self
    }

    /// Connection ended: clean close code, or 1006 for a dirty drop.
    pub fn on_close(mut self, f: impl Fn(&Conn, u16) + Send + Sync + 'static) -> Ws {
        self.handlers.on_close = Some(Arc::new(f));
        self
    }
}

/// Validate an RFC 6455 client handshake and produce the `101` upgrade
/// response (or the RFC-mandated refusal).
pub(crate) fn upgrade_response(
    req: &Request,
    runtime: &Arc<WsRuntime>,
    handlers: &Arc<sutegi_ws::Handlers>,
) -> Response {
    let has_token = |header: &str, token: &str| {
        req.header(header)
            .map(|v| v.to_ascii_lowercase().contains(token))
            .unwrap_or(false)
    };
    if !has_token("upgrade", "websocket") || !has_token("connection", "upgrade") {
        return Response::new(400).with_body(&b"expected a WebSocket upgrade request"[..]);
    }
    if req.header("sec-websocket-version").map(str::trim) != Some("13") {
        // RFC 6455 §4.2.2: advertise the version we speak.
        return Response::new(426)
            .with_header("sec-websocket-version", "13")
            .with_body(&b"unsupported WebSocket version"[..]);
    }
    let Some(key) = req
        .header("sec-websocket-key")
        .map(str::trim)
        .filter(|k| !k.is_empty())
    else {
        return Response::new(400).with_body(&b"missing Sec-WebSocket-Key"[..]);
    };
    if !runtime.has_capacity() {
        // Refuse before the 101 so the client sees a clean HTTP error and
        // can back off / be routed elsewhere.
        return Response::new(503).with_body(&b"connection capacity reached"[..]);
    }

    let accept = sutegi_ws::accept_key(key);
    let rt = Arc::clone(runtime);
    let handlers = Arc::clone(handlers);
    let req = req.clone(); // handed to on_open after adoption
    Response::upgrade(move |stream, leftover| {
        let _ = rt.adopt(stream, leftover, handlers, req);
    })
    .with_header("upgrade", "websocket")
    .with_header("connection", "Upgrade")
    .with_header("sec-websocket-accept", &accept)
}
