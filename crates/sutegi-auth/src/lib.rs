//! The sutegi **user system** — registration, login, guards, and API tokens,
//! with zero third-party dependencies. The Laravel `auth` scaffolding, over
//! sutegi's own seams:
//!
//! - [`password`] — PBKDF2-HMAC-SHA256 hashing as PHC strings (OWASP work
//!   factor, per-password salts, constant-time verify, rehash detection).
//! - [`Users`] — the user store over any ORM `Backend`: SQLite single-node or
//!   Postgres multi-pod, same calls. Hashes never leave the store.
//! - [`Auth`] — signed-cookie login sessions (via `sutegi-session`) with a
//!   **server-side expiry** stamped into the signed payload, so a stolen
//!   cookie dies on schedule regardless of what the client claims.
//! - [`Tokens`] — hashed bearer tokens for **agents and services**; the
//!   plaintext is returned once and only its SHA-256 is stored.
//! - Route guards: [`require_auth`], [`require_role`], [`require_token`] —
//!   plug into `App::group(prefix, vec![mw(...)], …)`.
//!
//! ```ignore
//! let db = Db::open("app.db")?;
//! let auth = Arc::new(Auth::new(
//!     Users::new(db.clone()),
//!     Sessions::new(config.require("SESSION_SECRET")?.as_bytes()),
//! ));
//! auth.users.migrate()?;
//!
//! App::new("app")
//!     .post("/login", "Log in.", move |c| {
//!         let (email, pw) = …;
//!         match auth.users.authenticate(&email, &pw)? {
//!             Some(user) => Ok(auth.login(c.req, &user, json(200, &user.to_json()))),
//!             None => Err(Error::unauthorized("bad credentials")),
//!         }
//!     })
//!     .group("/admin", vec![mw(require_role(auth.clone(), "admin"))], |g| …)
//! ```

pub mod links;
pub mod password;
pub mod tokens;
pub mod users;

#[cfg(feature = "mail")]
pub mod flows;
#[cfg(feature = "mail")]
pub use flows::AuthMail;

pub use links::Links;
pub use password::{hash_password, verify_password, DEFAULT_ITERATIONS};
pub use tokens::{ApiToken, Tokens, TOKEN_PREFIX};
pub use users::{User, Users, MIN_PASSWORD_LEN};

use std::sync::Arc;
use sutegi_json::Json;
use sutegi_orm::Backend;
use sutegi_session::Sessions;
use sutegi_web::{json, Request, Response};

/// Session keys used inside the signed cookie payload.
const UID_KEY: &str = "uid";
const EXP_KEY: &str = "exp";

/// The session glue: a [`Users`] store plus a [`Sessions`] cookie signer, and
/// the login/logout/current-user operations between them.
pub struct Auth<B: Backend> {
    pub users: Users<B>,
    pub sessions: Sessions,
    ttl: i64,
}

impl<B: Backend> Auth<B> {
    /// Sessions default to a 24 h server-side lifetime; see [`ttl`](Auth::ttl).
    pub fn new(users: Users<B>, sessions: Sessions) -> Auth<B> {
        Auth {
            users,
            sessions,
            ttl: 86_400,
        }
    }

    /// Server-side session lifetime in seconds. This is enforced from the
    /// signed payload at every request — unlike the cookie's `Max-Age`, the
    /// client cannot opt out of it.
    pub fn ttl(mut self, secs: i64) -> Auth<B> {
        self.ttl = secs.max(1);
        self
    }

    /// Stamp `user` into the (signed) session and attach it to `resp`.
    /// Existing session data is preserved.
    pub fn login(&self, req: &Request, user: &User, resp: Response) -> Response {
        let mut s = self.sessions.load(req);
        s.set(UID_KEY, Json::int(user.id));
        s.set(EXP_KEY, Json::int(users::now_secs() + self.ttl));
        self.sessions.save(&s, resp)
    }

    /// Expire the session cookie.
    pub fn logout(&self, resp: Response) -> Response {
        self.sessions.clear(resp)
    }

    /// The logged-in user id, if the request carries a valid, unexpired
    /// session. Pure cookie-HMAC work — no database access.
    pub fn user_id(&self, req: &Request) -> Option<i64> {
        let s = self.sessions.load(req);
        let exp = s.get(EXP_KEY).and_then(Json::as_i64)?;
        if exp < users::now_secs() {
            return None;
        }
        s.get(UID_KEY).and_then(Json::as_i64)
    }

    /// The logged-in [`User`], loaded from the store (one lookup).
    pub fn current(&self, req: &Request) -> Result<Option<User>, String> {
        match self.user_id(req) {
            Some(id) => self.users.find(id),
            None => Ok(None),
        }
    }
}

/// Guard: reject with `401` JSON unless the request carries a valid session.
/// Cookie-signature work only — handlers needing the full user call
/// [`Auth::current`].
pub fn require_auth<B>(
    auth: Arc<Auth<B>>,
) -> impl Fn(&Request) -> Option<Response> + Send + Sync + 'static
where
    B: Backend + Send + Sync + 'static,
{
    move |req| match auth.user_id(req) {
        Some(_) => None,
        None => Some(unauthenticated()),
    }
}

/// Guard: `401` without a valid session, `403` unless the logged-in user
/// carries `role` (one store lookup per request).
pub fn require_role<B>(
    auth: Arc<Auth<B>>,
    role: &str,
) -> impl Fn(&Request) -> Option<Response> + Send + Sync + 'static
where
    B: Backend + Send + Sync + 'static,
{
    let role = role.to_string();
    move |req| match auth.current(req) {
        Ok(Some(user)) if user.is(&role) => None,
        Ok(Some(_)) => Some(json(
            403,
            &Json::obj(vec![("error", Json::str("forbidden"))]),
        )),
        Ok(None) => Some(unauthenticated()),
        Err(e) => Some(json(500, &Json::obj(vec![("error", Json::str(e))]))),
    }
}

/// Guard: reject with `401` unless the request carries a valid
/// `Authorization: Bearer stg_…` API token — the agent/service door.
/// Handlers can identify the caller with [`token_user`].
pub fn require_token<B>(
    tokens: Arc<Tokens<B>>,
) -> impl Fn(&Request) -> Option<Response> + Send + Sync + 'static
where
    B: Backend + Send + Sync + 'static,
{
    move |req| match bearer_of(req).and_then(|t| tokens.verify(t).ok().flatten()) {
        Some(_) => None,
        None => Some(unauthenticated().with_header("www-authenticate", "Bearer")),
    }
}

/// Resolve the request's bearer token to its owning [`User`], for handlers
/// and tools behind [`require_token`].
pub fn token_user<B: Backend>(
    tokens: &Tokens<B>,
    users: &Users<B>,
    req: &Request,
) -> Result<Option<User>, String> {
    match bearer_of(req) {
        Some(t) => match tokens.verify(t)? {
            Some(uid) => users.find(uid),
            None => Ok(None),
        },
        None => Ok(None),
    }
}

fn bearer_of(req: &Request) -> Option<&str> {
    req.header("authorization")?.strip_prefix("Bearer ")
}

fn unauthenticated() -> Response {
    json(
        401,
        &Json::obj(vec![("error", Json::str("unauthenticated"))]),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use sutegi_orm::db::Db;
    use sutegi_web::Method;

    fn rig() -> Arc<Auth<Db>> {
        let db = Db::memory().unwrap();
        let users = Users::new(db).iterations(1_000);
        users.migrate().unwrap();
        Arc::new(Auth::new(users, Sessions::new(b"test-secret").insecure()))
    }

    fn request(headers: Vec<(String, String)>) -> Request {
        Request {
            method: Method::Get,
            path: "/".into(),
            query: String::new(),
            version: "HTTP/1.1".into(),
            headers,
            body: vec![],
            peer: None,
        }
    }

    fn cookie_of(resp: &Response) -> String {
        let header = resp
            .headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("set-cookie"))
            .map(|(_, v)| v.clone())
            .unwrap();
        header.split(';').next().unwrap().to_string()
    }

    #[test]
    fn login_cookie_roundtrips_to_current_user() {
        let auth = rig();
        let user = auth.users.register("a@b.co", "password1").unwrap();

        let resp = auth.login(&request(vec![]), &user, Response::new(200));
        let cookie = cookie_of(&resp);

        let next = request(vec![("Cookie".into(), cookie)]);
        assert_eq!(auth.user_id(&next), Some(user.id));
        assert_eq!(auth.current(&next).unwrap().unwrap().email, "a@b.co");

        // No cookie → anonymous.
        assert_eq!(auth.user_id(&request(vec![])), None);
    }

    #[test]
    fn expired_session_is_anonymous() {
        let auth_rig = rig();
        let auth = Auth {
            users: Users::new(auth_rig.users.backend().clone()).iterations(1_000),
            sessions: Sessions::new(b"test-secret").insecure(),
            ttl: 0, // expires immediately (ttl(0) would clamp to 1)
        };
        let user = auth.users.register("x@y.co", "password1").unwrap();
        let cookie = cookie_of(&auth.login(&request(vec![]), &user, Response::new(200)));
        std::thread::sleep(std::time::Duration::from_millis(1100));
        assert_eq!(
            auth.user_id(&request(vec![("Cookie".into(), cookie)])),
            None
        );
    }

    #[test]
    fn guards_gate_by_session_and_role() {
        let auth = rig();
        let admin = auth
            .users
            .register_with("root@a.co", "password1", "Root", "admin")
            .unwrap();
        let pleb = auth.users.register("user@a.co", "password1").unwrap();

        let admin_req = request(vec![(
            "Cookie".into(),
            cookie_of(&auth.login(&request(vec![]), &admin, Response::new(200))),
        )]);
        let pleb_req = request(vec![(
            "Cookie".into(),
            cookie_of(&auth.login(&request(vec![]), &pleb, Response::new(200))),
        )]);
        let anon_req = request(vec![]);

        let need_auth = require_auth(auth.clone());
        assert!(need_auth(&admin_req).is_none());
        assert!(need_auth(&pleb_req).is_none());
        assert_eq!(need_auth(&anon_req).unwrap().status, 401);

        let need_admin = require_role(auth.clone(), "admin");
        assert!(need_admin(&admin_req).is_none());
        assert_eq!(need_admin(&pleb_req).unwrap().status, 403);
        assert_eq!(need_admin(&anon_req).unwrap().status, 401);
    }

    #[test]
    fn token_guard_and_owner_lookup() {
        let auth = rig();
        let user = auth.users.register("svc@a.co", "password1").unwrap();
        let tokens = Arc::new(Tokens::new(auth.users.backend().clone()));
        tokens.migrate().unwrap();
        let (plain, _) = tokens.issue(user.id, "agent").unwrap();

        let guard = require_token(tokens.clone());
        let good = request(vec![("Authorization".into(), format!("Bearer {plain}"))]);
        let bad = request(vec![("Authorization".into(), "Bearer stg_bogus".into())]);

        assert!(guard(&good).is_none());
        assert_eq!(guard(&bad).unwrap().status, 401);
        assert_eq!(guard(&request(vec![])).unwrap().status, 401);

        let owner = token_user(&tokens, &auth.users, &good).unwrap().unwrap();
        assert_eq!(owner.id, user.id);
    }

    #[test]
    fn tampered_cookie_is_anonymous() {
        let auth = rig();
        let user = auth.users.register("t@a.co", "password1").unwrap();
        let cookie = cookie_of(&auth.login(&request(vec![]), &user, Response::new(200)));
        // Splice two hex chars into the payload: still well-formed, but the
        // signature no longer matches.
        let tampered = cookie.replacen('=', "=61", 1);
        assert_eq!(
            auth.user_id(&request(vec![("Cookie".into(), tampered)])),
            None
        );
    }

    #[test]
    fn logout_clears_cookie() {
        let auth = rig();
        let resp = auth.logout(Response::new(200));
        let header = resp
            .headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("set-cookie"))
            .map(|(_, v)| v.clone())
            .unwrap();
        assert!(header.contains("Max-Age=0"));
    }
}
