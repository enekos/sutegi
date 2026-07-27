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
//!   plaintext is returned once and only its SHA-256 is stored. Optional
//!   expiry, `last_used_at` tracking.
//! - [`Remember`] — "remember me" tokens: selector/validator cookies that
//!   rotate on every use and die on password change;
//!   [`Auth::identify`] revives an expired session from one.
//! - [`Throttle`] — DB-backed login rate limiting (Laravel's
//!   `ThrottlesLogins`), one counter shared by every pod.
//! - Route guards: [`require_auth`], [`require_role`], [`require_verified`],
//!   [`require_token`], [`require_csrf`] — plug into
//!   `App::group(prefix, vec![mw(...)], …)`.
//! - Sessions carry a fingerprint of the password hash: **changing the
//!   password logs out every other device** on the next store-checking
//!   lookup, and logins transparently **re-hash upgraded work factors**.
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
pub mod remember;
pub mod throttle;
pub mod tokens;
pub mod users;

#[cfg(feature = "mail")]
pub mod flows;
#[cfg(feature = "mail")]
pub use flows::AuthMail;

pub use links::Links;
pub use password::{hash_password, needs_rehash, verify_password, DEFAULT_ITERATIONS};
pub use remember::{Remember, REMEMBER_COOKIE, REMEMBER_TTL};
pub use throttle::Throttle;
pub use tokens::{ApiToken, Tokens, TOKEN_PREFIX};
pub use users::{User, Users, MIN_PASSWORD_LEN};

use std::sync::Arc;
use sutegi_json::Json;
use sutegi_orm::Backend;
use sutegi_session::Sessions;
use sutegi_web::{json, Method, Request, Response};

/// Session keys used inside the signed cookie payload.
const UID_KEY: &str = "uid";
const EXP_KEY: &str = "exp";
/// Fingerprint of the password hash at login — sessions die with it.
const PWB_KEY: &str = "pwb";

/// The session glue: a [`Users`] store plus a [`Sessions`] cookie signer, and
/// the login/logout/current-user operations between them. Add a [`Remember`]
/// store to get Laravel's "remember me" on top.
pub struct Auth<B: Backend> {
    pub users: Users<B>,
    pub sessions: Sessions,
    pub remember: Option<Remember<B>>,
    ttl: i64,
}

impl<B: Backend> Auth<B> {
    /// Sessions default to a 24 h server-side lifetime; see [`ttl`](Auth::ttl).
    pub fn new(users: Users<B>, sessions: Sessions) -> Auth<B> {
        Auth {
            users,
            sessions,
            remember: None,
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

    /// Attach a [`Remember`] store: [`login_remembered`](Auth::login_remembered)
    /// mints long-lived tokens and [`identify`](Auth::identify) revives
    /// expired sessions from them.
    pub fn remember(mut self, store: Remember<B>) -> Auth<B> {
        self.remember = Some(store);
        self
    }

    /// Stamp `user` into the (signed) session and attach it to `resp`.
    /// Existing session data is preserved. The session carries a fingerprint
    /// of the current password hash, so a password change invalidates every
    /// other session on the next store-checking lookup.
    pub fn login(&self, req: &Request, user: &User, resp: Response) -> Response {
        let mut s = self.sessions.load(req);
        self.stamp(&mut s, user.id);
        self.sessions.save(&s, resp)
    }

    /// [`login`](Auth::login) plus a rotating remember-me cookie (requires a
    /// [`remember`](Auth::remember) store). The Laravel
    /// `Auth::attempt($creds, remember: true)` shape.
    pub fn login_remembered(
        &self,
        req: &Request,
        user: &User,
        resp: Response,
    ) -> Result<Response, String> {
        let Some(rem) = &self.remember else {
            return Err("no remember store configured — Auth::remember(...)".to_string());
        };
        let bind = self.bind_of(user.id)?.unwrap_or_default();
        let cookie = rem.issue(user.id, &bind)?;
        Ok(self
            .login(req, user, resp)
            .with_header("set-cookie", &rem.cookie_header(&cookie)))
    }

    /// Expire the session cookie. If a remember store is attached, also
    /// revoke the request's remember token and expire that cookie — pass the
    /// request via [`logout_from`](Auth::logout_from) to get that.
    pub fn logout(&self, resp: Response) -> Response {
        self.sessions.clear(resp)
    }

    /// Full logout for this device: expires the session cookie **and**
    /// revokes + expires the remember cookie, when one rode in.
    pub fn logout_from(&self, req: &Request, resp: Response) -> Response {
        let resp = self.sessions.clear(resp);
        match &self.remember {
            Some(rem) => {
                if let Some(presented) = rem.read(req) {
                    let _ = rem.revoke_presented(&presented);
                }
                resp.with_header("set-cookie", &rem.clear_header())
            }
            None => resp,
        }
    }

    /// Kill every remember token a user holds (Laravel's
    /// `logoutOtherDevices` reach). Live session cookies die at their
    /// server-side expiry, or immediately on password change (the session's
    /// hash binding stops matching).
    pub fn logout_everywhere(&self, user_id: i64) -> Result<usize, String> {
        match &self.remember {
            Some(rem) => rem.revoke_all(user_id),
            None => Ok(0),
        }
    }

    /// The logged-in user id, if the request carries a valid, unexpired
    /// session. Pure cookie-HMAC work — no database access (so no password
    /// binding check; [`current`](Auth::current) and [`identify`](Auth::identify)
    /// enforce it).
    pub fn user_id(&self, req: &Request) -> Option<i64> {
        let s = self.sessions.load(req);
        let exp = s.get(EXP_KEY).and_then(Json::as_i64)?;
        if exp < users::now_secs() {
            return None;
        }
        s.get(UID_KEY).and_then(Json::as_i64)
    }

    /// The logged-in [`User`], loaded from the store (one lookup). A session
    /// whose password binding no longer matches the stored hash — the
    /// password changed since login — is treated as anonymous.
    pub fn current(&self, req: &Request) -> Result<Option<User>, String> {
        let Some(id) = self.user_id(req) else {
            return Ok(None);
        };
        let s = self.sessions.load(req);
        if let Some(pwb) = s.get_str(PWB_KEY) {
            if self.bind_of(id)?.as_deref() != Some(pwb) {
                return Ok(None);
            }
        }
        self.users.find(id)
    }

    /// The request's user via session **or** remember-me revival: when the
    /// session is gone but a valid remember cookie rides along, the token is
    /// consumed (rotated) and fresh cookies are minted — call
    /// [`Identified::attach`] on your response to set them. This is the
    /// handler-side revival point (sutegi middleware cannot set cookies on
    /// pass-through), so wire it into whatever endpoint establishes your
    /// client's identity — a `/me`, a page shell, a session probe.
    pub fn identify(&self, req: &Request) -> Result<Option<Identified>, String> {
        if let Some(user) = self.current(req)? {
            return Ok(Some(Identified {
                user,
                via_remember: false,
                cookies: vec![],
            }));
        }
        let Some(rem) = &self.remember else {
            return Ok(None);
        };
        let Some(presented) = rem.read(req) else {
            return Ok(None);
        };
        let Some((uid, rotated)) = rem.consume(&presented, |uid| self.bind_of(uid))? else {
            return Ok(None);
        };
        let Some(user) = self.users.find(uid)? else {
            return Ok(None);
        };
        let mut s = self.sessions.load(req);
        self.stamp(&mut s, user.id);
        Ok(Some(Identified {
            user,
            via_remember: true,
            cookies: vec![
                self.sessions.cookie_for(&s),
                rem.cookie_header(&rotated),
            ],
        }))
    }

    /// Get-or-mint the session's CSRF token and attach the (possibly
    /// re-signed) session to `resp`. Serve it from a small `GET` endpoint;
    /// clients echo it back in `X-CSRF-Token` past [`require_csrf`].
    pub fn csrf(&self, req: &Request, resp: Response) -> Result<(String, Response), String> {
        let mut s = self.sessions.load(req);
        let token = self.sessions.csrf(&mut s)?;
        Ok((token, self.sessions.save(&s, resp)))
    }

    fn stamp(&self, s: &mut sutegi_session::Session, uid: i64) {
        s.set(UID_KEY, Json::int(uid));
        s.set(EXP_KEY, Json::int(users::now_secs() + self.ttl));
        if let Ok(Some(bind)) = self.bind_of(uid) {
            s.set(PWB_KEY, Json::str(bind));
        }
    }

    /// Fingerprint of the user's current password hash (`None` = user gone).
    fn bind_of(&self, uid: i64) -> Result<Option<String>, String> {
        Ok(self
            .users
            .password_hash_of(uid)?
            .map(|h| password::fingerprint(&h)))
    }
}

/// A resolved request identity from [`Auth::identify`] — the user plus any
/// cookies a remember-me revival minted.
pub struct Identified {
    pub user: User,
    /// `true` when the identity came from a remember token rather than a
    /// live session (Laravel's `viaRemember`).
    pub via_remember: bool,
    cookies: Vec<String>,
}

impl Identified {
    /// Set any revival cookies on the response. A no-op for plain session
    /// hits.
    pub fn attach(&self, resp: Response) -> Response {
        self.cookies
            .iter()
            .fold(resp, |r, c| r.with_header("set-cookie", c))
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

/// Guard: `401` without a valid session, `403 {"error":"email unverified"}`
/// until the logged-in user confirms their address — Laravel's `verified`
/// middleware (one store lookup per request).
pub fn require_verified<B>(
    auth: Arc<Auth<B>>,
) -> impl Fn(&Request) -> Option<Response> + Send + Sync + 'static
where
    B: Backend + Send + Sync + 'static,
{
    move |req| match auth.current(req) {
        Ok(Some(user)) if user.is_verified() => None,
        Ok(Some(_)) => Some(json(
            403,
            &Json::obj(vec![("error", Json::str("email unverified"))]),
        )),
        Ok(None) => Some(unauthenticated()),
        Err(e) => Some(json(500, &Json::obj(vec![("error", Json::str(e))]))),
    }
}

/// Guard: on mutating methods, require an `X-CSRF-Token` header matching the
/// session's token (see [`Auth::csrf`]). Reads (`GET`/`HEAD`/`OPTIONS`) pass,
/// and so do requests authenticating with an `Authorization` header — bearer
/// callers carry no ambient cookie credential, which is what CSRF forges.
pub fn require_csrf<B>(
    auth: Arc<Auth<B>>,
) -> impl Fn(&Request) -> Option<Response> + Send + Sync + 'static
where
    B: Backend + Send + Sync + 'static,
{
    move |req| {
        if matches!(req.method, Method::Get | Method::Head | Method::Options) {
            return None;
        }
        if req.header("authorization").is_some() {
            return None;
        }
        let s = auth.sessions.load(req);
        let presented = req.header("x-csrf-token").unwrap_or("");
        if auth.sessions.verify_csrf(&s, presented) {
            None
        } else {
            Some(json(
                419, // Laravel's "Page Expired" — distinct from a plain 403
                &Json::obj(vec![("error", Json::str("csrf token mismatch"))]),
            ))
        }
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
            remember: None,
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

    fn rig_remembered() -> Arc<Auth<Db>> {
        let db = Db::memory().unwrap();
        let users = Users::new(db.clone()).iterations(1_000);
        users.migrate().unwrap();
        let remember = Remember::new(db).insecure();
        remember.migrate().unwrap();
        Arc::new(
            Auth::new(users, Sessions::new(b"test-secret").insecure()).remember(remember),
        )
    }

    fn cookies_of(resp: &Response) -> Vec<(String, String)> {
        resp.headers
            .iter()
            .filter(|(k, _)| k.eq_ignore_ascii_case("set-cookie"))
            .filter_map(|(_, v)| {
                v.split(';')
                    .next()?
                    .split_once('=')
                    .map(|(k, v)| (k.to_string(), v.to_string()))
            })
            .collect()
    }

    #[test]
    fn password_change_invalidates_other_sessions() {
        let auth = rig();
        let user = auth.users.register("p@a.co", "password1").unwrap();
        let cookie = cookie_of(&auth.login(&request(vec![]), &user, Response::new(200)));
        let req = request(vec![("Cookie".into(), cookie)]);
        assert!(auth.current(&req).unwrap().is_some());

        auth.users.set_password(user.id, "newpassword").unwrap();
        // Cookie-only check still passes (documented); the store check dies.
        assert_eq!(auth.user_id(&req), Some(user.id));
        assert!(auth.current(&req).unwrap().is_none());
        assert!(auth.identify(&req).unwrap().is_none());
    }

    #[test]
    fn remember_revives_expired_session_and_rotates() {
        let auth = rig_remembered();
        let user = auth.users.register("r@a.co", "password1").unwrap();
        let resp = auth
            .login_remembered(&request(vec![]), &user, Response::new(200))
            .unwrap();
        let jar = cookies_of(&resp);
        assert_eq!(jar.len(), 2, "session + remember cookies");
        let remember_cookie = jar
            .iter()
            .find(|(k, _)| k == REMEMBER_COOKIE)
            .map(|(_, v)| v.clone())
            .unwrap();

        // Only the remember cookie survives (session expired / new browser).
        let req = request(vec![(
            "Cookie".into(),
            format!("{REMEMBER_COOKIE}={remember_cookie}"),
        )]);
        assert!(auth.current(&req).unwrap().is_none());
        let hit = auth.identify(&req).unwrap().unwrap();
        assert_eq!(hit.user.id, user.id);
        assert!(hit.via_remember);
        let fresh = cookies_of(&hit.attach(Response::new(200)));
        assert_eq!(fresh.len(), 2, "revival mints session + rotated remember");
        let rotated = fresh
            .iter()
            .find(|(k, _)| k == REMEMBER_COOKIE)
            .map(|(_, v)| v.clone())
            .unwrap();
        assert_ne!(rotated, remember_cookie);

        // The revived session cookie works on its own.
        let session_cookie = fresh
            .iter()
            .find(|(k, _)| k == "sutegi_session")
            .map(|(_, v)| v.clone())
            .unwrap();
        let next = request(vec![("Cookie".into(), format!("sutegi_session={session_cookie}"))]);
        assert_eq!(auth.current(&next).unwrap().unwrap().id, user.id);

        // The pre-rotation remember cookie is dead (and burns the row).
        assert!(auth.identify(&req).unwrap().is_none());

        // Password change kills remember tokens too.
        let user2 = auth.users.register("s@a.co", "password1").unwrap();
        let resp2 = auth
            .login_remembered(&request(vec![]), &user2, Response::new(200))
            .unwrap();
        let rc2 = cookies_of(&resp2)
            .iter()
            .find(|(k, _)| k == REMEMBER_COOKIE)
            .map(|(_, v)| v.clone())
            .unwrap();
        auth.users.set_password(user2.id, "changed-pass").unwrap();
        let req2 = request(vec![("Cookie".into(), format!("{REMEMBER_COOKIE}={rc2}"))]);
        assert!(auth.identify(&req2).unwrap().is_none());
    }

    #[test]
    fn logout_from_revokes_and_clears() {
        let auth = rig_remembered();
        let user = auth.users.register("l@a.co", "password1").unwrap();
        let resp = auth
            .login_remembered(&request(vec![]), &user, Response::new(200))
            .unwrap();
        let rc = cookies_of(&resp)
            .iter()
            .find(|(k, _)| k == REMEMBER_COOKIE)
            .map(|(_, v)| v.clone())
            .unwrap();

        let req = request(vec![("Cookie".into(), format!("{REMEMBER_COOKIE}={rc}"))]);
        let out = auth.logout_from(&req, Response::new(200));
        let cleared = cookies_of(&out);
        assert_eq!(cleared.len(), 2, "clears both cookies");
        assert!(cleared.iter().all(|(_, v)| v.is_empty()));
        // The token row is gone: revival is impossible.
        assert!(auth.identify(&req).unwrap().is_none());
    }

    #[test]
    fn logout_everywhere_kills_all_remember_tokens() {
        let auth = rig_remembered();
        let user = auth.users.register("e@a.co", "password1").unwrap();
        for _ in 0..3 {
            auth.login_remembered(&request(vec![]), &user, Response::new(200))
                .unwrap();
        }
        assert_eq!(auth.logout_everywhere(user.id).unwrap(), 3);
    }

    #[test]
    fn csrf_guard_gates_mutations() {
        let auth = rig();
        let (token, resp) = auth
            .csrf(&request(vec![]), Response::new(200))
            .unwrap();
        let cookie = cookie_of(&resp);
        let guard = require_csrf(auth.clone());

        let mut ok = request(vec![
            ("Cookie".into(), cookie.clone()),
            ("X-CSRF-Token".into(), token.clone()),
        ]);
        ok.method = Method::Post;
        assert!(guard(&ok).is_none());

        let mut missing = request(vec![("Cookie".into(), cookie.clone())]);
        missing.method = Method::Post;
        assert_eq!(guard(&missing).unwrap().status, 419);

        let mut wrong = request(vec![
            ("Cookie".into(), cookie),
            ("X-CSRF-Token".into(), "forged".into()),
        ]);
        wrong.method = Method::Delete;
        assert_eq!(guard(&wrong).unwrap().status, 419);

        // Reads pass without a token; bearer callers pass on any method.
        let get = request(vec![]);
        assert!(guard(&get).is_none());
        let mut bearer = request(vec![("Authorization".into(), "Bearer stg_x".into())]);
        bearer.method = Method::Post;
        assert!(guard(&bearer).is_none());
    }

    #[test]
    fn verified_guard() {
        let auth = rig();
        let user = auth.users.register("v@a.co", "password1").unwrap();
        let req = request(vec![(
            "Cookie".into(),
            cookie_of(&auth.login(&request(vec![]), &user, Response::new(200))),
        )]);
        let guard = require_verified(auth.clone());
        assert_eq!(guard(&req).unwrap().status, 403);
        assert_eq!(guard(&request(vec![])).unwrap().status, 401);
        auth.users.mark_verified(user.id).unwrap();
        assert!(guard(&req).is_none());
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
