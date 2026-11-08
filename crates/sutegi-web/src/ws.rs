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

    /// Reject the handshake with `403` unless `f(&Request)` returns `true`.
    /// Runs in the HTTP worker **before** the `101`, so a refused client never
    /// becomes a socket — the place to check tokens, cookies, or a custom
    /// origin policy. (`on_open`, by contrast, runs only after the upgrade is
    /// already committed.)
    pub fn authorize(mut self, f: impl Fn(&Request) -> bool + Send + Sync + 'static) -> Ws {
        self.handlers.authorize = Some(Arc::new(f));
        self
    }

    /// Restrict the handshake to these exact `Origin` values — the built-in
    /// guard against Cross-Site WebSocket Hijacking. A browser always sends
    /// `Origin`; with an allowlist set, a missing or non-matching one is
    /// refused with `403`.
    ///
    /// Without an allowlist the endpoint is **same-origin by default**: an
    /// upgrade whose `Origin` host differs from the request's `Host` is
    /// refused with `403`; a request with no `Origin` (a non-browser client)
    /// is accepted. Set an allowlist when the page is served from another
    /// host, or when a proxy rewrites `Host`.
    pub fn check_origin<I, S>(mut self, origins: I) -> Ws
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.handlers.allowed_origins = Some(origins.into_iter().map(Into::into).collect());
        self
    }

    /// Accept an upgrade from **any** `Origin`, disabling the same-origin
    /// default. Only for public endpoints that do not authenticate by cookie
    /// — otherwise any site the user visits can open the socket as them.
    /// A [`check_origin`](Ws::check_origin) allowlist, if also set, wins.
    pub fn allow_any_origin(mut self) -> Ws {
        self.handlers.allow_any_origin = true;
        self
    }
}

/// Whether an `Origin` header value names the same authority as the
/// request's `Host` header (case-insensitive, default port ignored). Anything
/// unparseable — `null`, a missing `Host` — is not same-origin.
fn same_origin(origin: &str, host: Option<&str>) -> bool {
    let Some(host) = host else {
        return false;
    };
    let Some((scheme, authority)) = origin.trim().split_once("://") else {
        return false;
    };
    let default_port = match scheme.to_ascii_lowercase().as_str() {
        "https" | "wss" => Some(":443"),
        "http" | "ws" => Some(":80"),
        _ => None,
    };
    let normalize = |a: &str| {
        let a = a.trim().trim_end_matches('/').to_ascii_lowercase();
        match default_port.and_then(|p| a.strip_suffix(p)) {
            Some(stripped) => stripped.to_string(),
            None => a,
        }
    };
    let authority = normalize(authority);
    !authority.is_empty() && authority == normalize(host)
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
    // Cross-site protection, before the 101: an allowlisted Origin must be
    // present and match; with no allowlist a browser Origin must be
    // same-origin unless the endpoint opted out. Then the custom authorize
    // gate (if any) must pass.
    if let Some(allowed) = &handlers.allowed_origins {
        let ok = req
            .header("origin")
            .map(|o| allowed.iter().any(|a| a == o))
            .unwrap_or(false);
        if !ok {
            return Response::new(403).with_body(&b"origin not allowed"[..]);
        }
    } else if !handlers.allow_any_origin {
        if let Some(origin) = req.header("origin") {
            if !same_origin(origin, req.header("host")) {
                return Response::new(403).with_body(&b"cross-origin upgrade not allowed"[..]);
            }
        }
    }
    if let Some(authorize) = &handlers.authorize {
        if !authorize(req) {
            return Response::new(403).with_body(&b"forbidden"[..]);
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use sutegi_http::Method;
    use sutegi_ws::Handlers;

    fn ws_request(headers: &[(&str, &str)]) -> Request {
        Request {
            method: Method::Get,
            path: "/ws".into(),
            query: String::new(),
            version: "HTTP/1.1".into(),
            headers: headers
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            body: Vec::new(),
            peer: Some("203.0.113.7:5000".into()),
        }
    }

    const BASE: &[(&str, &str)] = &[
        ("upgrade", "websocket"),
        ("connection", "Upgrade"),
        ("sec-websocket-version", "13"),
        ("sec-websocket-key", "dGhlIHNhbXBsZSBub25jZQ=="),
    ];

    fn with_extra(extra: &[(&str, &str)]) -> Request {
        let mut h = BASE.to_vec();
        h.extend_from_slice(extra);
        ws_request(&h)
    }

    fn runtime() -> Arc<WsRuntime> {
        WsRuntime::start(WsConfig {
            shards: 1,
            raise_nofile: false,
            ..WsConfig::default()
        })
        .unwrap()
    }

    #[test]
    fn origin_allowlist_gates_before_the_101() {
        let rt = runtime();
        let handlers = Arc::new(Handlers {
            allowed_origins: Some(vec!["https://app.example.com".into()]),
            ..Handlers::default()
        });

        // Matching origin → 101 upgrade.
        let ok = upgrade_response(
            &with_extra(&[("origin", "https://app.example.com")]),
            &rt,
            &handlers,
        );
        assert_eq!(ok.status, 101);
        assert!(ok.is_upgrade());

        // Foreign origin (CSWSH attempt) → 403, no upgrade.
        let bad = upgrade_response(
            &with_extra(&[("origin", "https://evil.example")]),
            &rt,
            &handlers,
        );
        assert_eq!(bad.status, 403);
        assert!(!bad.is_upgrade());

        // Missing origin with an allowlist set → 403.
        let none = upgrade_response(&with_extra(&[]), &rt, &handlers);
        assert_eq!(none.status, 403);
    }

    #[test]
    fn cross_origin_refused_by_default_same_origin_accepted() {
        let rt = runtime();
        let handlers = Arc::new(Handlers::default());
        let up = |extra: &[(&str, &str)]| upgrade_response(&with_extra(extra), &rt, &handlers);

        // CSWSH: an attacker's page opening our socket → 403, no upgrade.
        let evil = up(&[
            ("host", "app.example.com"),
            ("origin", "https://evil.example"),
        ]);
        assert_eq!(evil.status, 403);
        assert!(!evil.is_upgrade());
        // Opaque origin (sandboxed iframe, file://) is not same-origin.
        assert_eq!(
            up(&[("host", "app.example.com"), ("origin", "null")]).status,
            403
        );
        // Same host, different port is a different origin.
        assert_eq!(
            up(&[
                ("host", "app.example.com"),
                ("origin", "https://app.example.com:8443")
            ])
            .status,
            403
        );
        // Origin but no Host to compare against → refused.
        assert_eq!(up(&[("origin", "https://app.example.com")]).status, 403);

        // Same origin → 101 (case and default port don't matter).
        assert_eq!(
            up(&[
                ("host", "App.Example.com"),
                ("origin", "https://app.example.com")
            ])
            .status,
            101
        );
        assert_eq!(
            up(&[
                ("host", "app.example.com:443"),
                ("origin", "https://app.example.com")
            ])
            .status,
            101
        );
        assert_eq!(
            up(&[
                ("host", "127.0.0.1:8080"),
                ("origin", "http://127.0.0.1:8080")
            ])
            .status,
            101
        );
        // No Origin at all (non-browser client) → accepted.
        assert_eq!(up(&[("host", "app.example.com")]).status, 101);
    }

    #[test]
    fn allow_any_origin_opts_out_of_the_same_origin_default() {
        let rt = runtime();
        let ws = Ws::new().allow_any_origin();
        let handlers = Arc::new(ws.handlers);
        let req = with_extra(&[
            ("host", "app.example.com"),
            ("origin", "https://evil.example"),
        ]);
        assert_eq!(upgrade_response(&req, &rt, &handlers).status, 101);

        // An explicit allowlist still wins over allow_any_origin.
        let strict = Arc::new(
            Ws::new()
                .allow_any_origin()
                .check_origin(["https://app.example.com"])
                .handlers,
        );
        assert_eq!(upgrade_response(&req, &rt, &strict).status, 403);
    }

    #[test]
    fn authorize_hook_can_reject() {
        let rt = runtime();
        let handlers = Arc::new(Handlers {
            authorize: Some(Arc::new(|req: &Request| {
                req.header("authorization") == Some("Bearer good")
            })),
            ..Handlers::default()
        });
        let ok = upgrade_response(
            &with_extra(&[("authorization", "Bearer good")]),
            &rt,
            &handlers,
        );
        assert_eq!(ok.status, 101);
        let bad = upgrade_response(
            &with_extra(&[("authorization", "Bearer bad")]),
            &rt,
            &handlers,
        );
        assert_eq!(bad.status, 403);
    }

    #[test]
    fn bad_handshakes_still_refused() {
        let rt = runtime();
        let handlers = Arc::new(Handlers::default());
        // Missing upgrade headers.
        assert_eq!(
            upgrade_response(&ws_request(&[]), &rt, &handlers).status,
            400
        );
        // Wrong version (only version 8 present, not 13).
        let wrong_version = ws_request(&[
            ("upgrade", "websocket"),
            ("connection", "Upgrade"),
            ("sec-websocket-version", "8"),
            ("sec-websocket-key", "dGhlIHNhbXBsZSBub25jZQ=="),
        ]);
        assert_eq!(upgrade_response(&wrong_version, &rt, &handlers).status, 426);
    }
}
