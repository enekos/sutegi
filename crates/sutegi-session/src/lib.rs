//! Signed-cookie sessions for sutegi.
//!
//! State lives in an HMAC-SHA256-signed cookie — stateless on the server, and
//! tamper-evident (a modified payload fails verification and is discarded).
//! The signing primitives are the audited RustCrypto `hmac`/`sha2` crates,
//! pulled in only when you enable sutegi's `auth` feature.
//!
//! ```ignore
//! let sessions = Sessions::new(b"a-32+ byte secret from your config");
//!
//! // in a handler:
//! let mut s = sessions.load(req);
//! s.set("user_id", Json::int(42));
//! sessions.save(&s, json(200, &Json::obj(vec![("ok", Json::Bool(true))])))
//! ```

use std::collections::BTreeMap;

use hmac::{Hmac, Mac};
use sha2::Sha256;
use sutegi_json::Json;
use sutegi_web::{Request, Response};

type HmacSha256 = Hmac<Sha256>;

/// Session manager: holds the signing secret and cookie policy.
pub struct Sessions {
    secret: Vec<u8>,
    cookie: String,
    secure: bool,
    max_age: Option<u64>,
}

impl Sessions {
    /// Create a manager from a secret (use a long, random, configured value).
    pub fn new(secret: &[u8]) -> Sessions {
        Sessions {
            secret: secret.to_vec(),
            cookie: "sutegi_session".to_string(),
            secure: true,
            max_age: Some(86_400),
        }
    }

    pub fn cookie_name(mut self, name: &str) -> Sessions {
        self.cookie = name.to_string();
        self
    }

    /// Drop the `Secure` attribute (for local `http://` development only).
    pub fn insecure(mut self) -> Sessions {
        self.secure = false;
        self
    }

    pub fn max_age(mut self, secs: Option<u64>) -> Sessions {
        self.max_age = secs;
        self
    }

    fn sign(&self, msg: &[u8]) -> String {
        let mut mac =
            HmacSha256::new_from_slice(&self.secret).expect("HMAC accepts any key length");
        mac.update(msg);
        to_hex(&mac.finalize().into_bytes())
    }

    /// Sign an arbitrary value into a `<value-hex>.<sig>` token (CSRF tokens,
    /// password-reset links, …). Verify with [`verify_token`](Sessions::verify_token).
    pub fn token(&self, value: &str) -> String {
        let hex = to_hex(value.as_bytes());
        format!("{}.{}", hex, self.sign(value.as_bytes()))
    }

    /// Verify a token from [`token`](Sessions::token); returns the original value.
    pub fn verify_token(&self, token: &str) -> Option<String> {
        let (hex, sig) = token.split_once('.')?;
        let bytes = from_hex(hex)?;
        if constant_time_eq(self.sign(&bytes).as_bytes(), sig.as_bytes()) {
            String::from_utf8(bytes).ok()
        } else {
            None
        }
    }

    /// Load and verify the session from the request cookie; returns an empty
    /// session if absent, tampered, or malformed.
    pub fn load(&self, req: &Request) -> Session {
        if let Some(raw) = req.cookie(&self.cookie) {
            if let Some((payload_hex, sig)) = raw.split_once('.') {
                if let Some(bytes) = from_hex(payload_hex) {
                    if constant_time_eq(self.sign(&bytes).as_bytes(), sig.as_bytes()) {
                        if let Ok(s) = std::str::from_utf8(&bytes) {
                            if let Ok(Json::Obj(map)) = Json::parse(s) {
                                return Session {
                                    data: map,
                                    dirty: false,
                                };
                            }
                        }
                    }
                }
            }
        }
        Session {
            data: BTreeMap::new(),
            dirty: false,
        }
    }

    /// Attach the signed session as a `Set-Cookie` on the response.
    pub fn save(&self, session: &Session, resp: Response) -> Response {
        let payload = Json::Obj(session.data.clone()).to_string();
        let payload_hex = to_hex(payload.as_bytes());
        let sig = self.sign(payload.as_bytes());
        let mut cookie = format!(
            "{}={}.{}; Path=/; HttpOnly; SameSite=Lax",
            self.cookie, payload_hex, sig
        );
        if self.secure {
            cookie.push_str("; Secure");
        }
        if let Some(age) = self.max_age {
            cookie.push_str(&format!("; Max-Age={}", age));
        }
        resp.with_header("set-cookie", &cookie)
    }

    /// Expire the session cookie.
    pub fn clear(&self, resp: Response) -> Response {
        resp.with_header(
            "set-cookie",
            &format!("{}=; Path=/; Max-Age=0", self.cookie),
        )
    }
}

/// The session payload — a small JSON map.
pub struct Session {
    data: BTreeMap<String, Json>,
    dirty: bool,
}

impl Session {
    pub fn get(&self, key: &str) -> Option<&Json> {
        self.data.get(key)
    }
    pub fn get_str(&self, key: &str) -> Option<&str> {
        self.data.get(key).and_then(|j| j.as_str())
    }
    pub fn set(&mut self, key: &str, value: Json) {
        self.data.insert(key.to_string(), value);
        self.dirty = true;
    }
    pub fn remove(&mut self, key: &str) {
        self.data.remove(key);
        self.dirty = true;
    }
    pub fn clear(&mut self) {
        self.data.clear();
        self.dirty = true;
    }
    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }
    /// Whether the session was modified since loading (worth re-saving).
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }
}

fn to_hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{:02x}", b));
    }
    s
}

fn from_hex(s: &str) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok())
        .collect()
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for i in 0..a.len() {
        diff |= a[i] ^ b[i];
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req_with_cookie(name: &str, value: &str) -> Request {
        Request {
            method: sutegi_web::Method::Get,
            path: "/".into(),
            query: String::new(),
            version: "HTTP/1.1".into(),
            headers: vec![("Cookie".into(), format!("{}={}", name, value))],
            body: vec![],
            peer: None,
        }
    }

    /// Pull the cookie value out of a Set-Cookie response header.
    fn cookie_value(resp: &Response, _name: &str) -> String {
        let header = resp
            .headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("set-cookie"))
            .map(|(_, v)| v.clone())
            .unwrap();
        let kv = header.split(';').next().unwrap();
        kv.split_once('=').unwrap().1.to_string()
    }

    #[test]
    fn roundtrip_and_tamper() {
        let s = Sessions::new(b"super-secret-key").insecure();
        let mut sess = s.load(&req_with_cookie("x", "")); // empty
        assert!(sess.is_empty());
        sess.set("user_id", Json::int(42));

        let resp = s.save(&sess, Response::new(200));
        let cookie = cookie_value(&resp, "sutegi_session");

        // Reload from the produced cookie.
        let reloaded = s.load(&req_with_cookie("sutegi_session", &cookie));
        assert_eq!(reloaded.get("user_id").and_then(Json::as_i64), Some(42));

        // Tamper with the payload → signature fails → empty session.
        let tampered = cookie.replacen(|c: char| c.is_ascii_hexdigit(), "0", 1);
        let after = s.load(&req_with_cookie("sutegi_session", &tampered));
        assert!(after.is_empty() || after.get("user_id").and_then(Json::as_i64) != Some(42));
    }

    #[test]
    fn token_sign_verify() {
        let s = Sessions::new(b"k");
        let t = s.token("user:42");
        assert_eq!(s.verify_token(&t).as_deref(), Some("user:42"));
        assert!(s.verify_token("deadbeef.badsig").is_none());
    }

    #[test]
    fn save_sets_cookie_attributes() {
        let s = Sessions::new(b"secret"); // secure by default, 1-day max-age
        let mut sess = s.load(&req_with_cookie("x", ""));
        sess.set("k", Json::int(1));
        let header = s
            .save(&sess, Response::new(200))
            .headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("set-cookie"))
            .map(|(_, v)| v.clone())
            .unwrap();
        assert!(header.contains("HttpOnly"));
        assert!(header.contains("SameSite=Lax"));
        assert!(header.contains("Secure"));
        assert!(header.contains("Max-Age=86400"));
        assert!(header.starts_with("sutegi_session="));
    }

    #[test]
    fn insecure_and_no_max_age_omit_attributes() {
        let s = Sessions::new(b"secret").insecure().max_age(None);
        let header = s
            .save(
                &Session {
                    data: BTreeMap::new(),
                    dirty: false,
                },
                Response::new(200),
            )
            .headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("set-cookie"))
            .map(|(_, v)| v.clone())
            .unwrap();
        assert!(!header.contains("Secure"));
        assert!(!header.contains("Max-Age"));
    }

    #[test]
    fn custom_cookie_name_roundtrips() {
        let s = Sessions::new(b"k").cookie_name("sid").insecure();
        let mut sess = s.load(&req_with_cookie("sid", ""));
        sess.set("user", Json::str("eneko"));
        let cookie = cookie_value(&s.save(&sess, Response::new(200)), "sid");
        let reloaded = s.load(&req_with_cookie("sid", &cookie));
        assert_eq!(reloaded.get_str("user"), Some("eneko"));
        // A session signed under "sid" is invisible under the default name.
        assert!(s
            .load(&req_with_cookie("sutegi_session", &cookie))
            .is_empty());
    }

    #[test]
    fn clear_expires_cookie() {
        let s = Sessions::new(b"k");
        let header = s
            .clear(Response::new(200))
            .headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("set-cookie"))
            .map(|(_, v)| v.clone())
            .unwrap();
        assert!(header.contains("Max-Age=0"));
    }

    #[test]
    fn session_mutation_tracks_dirty() {
        let mut sess = Session {
            data: BTreeMap::new(),
            dirty: false,
        };
        assert!(!sess.is_dirty());
        sess.set("a", Json::int(1));
        assert!(sess.is_dirty());
        assert_eq!(sess.get("a").and_then(Json::as_i64), Some(1));
        sess.remove("a");
        assert!(sess.get("a").is_none());
        sess.set("b", Json::int(2));
        sess.clear();
        assert!(sess.is_empty());
    }

    #[test]
    fn empty_payload_token_verification_fails_cleanly() {
        let s = Sessions::new(b"k");
        // No dot separator → None, not a panic.
        assert!(s.verify_token("nosig").is_none());
        // Odd-length hex → None.
        assert!(s.verify_token("abc.sig").is_none());
    }
}
