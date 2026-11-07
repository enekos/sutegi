//! The sutegi user system, end to end: registration, signed-cookie login with
//! server-side expiry, remember-me revival, login throttling, role-gated
//! admin routes, and API tokens for agents.
//!
//! ```text
//! SESSION_SECRET=$(openssl rand -hex 32) cargo run -p auth-example   # or SUTEGI_ENV=dev
//! curl -c /tmp/cj -X POST localhost:8080/register -d '{"email":"root@example.com","password":"password1","name":"Root"}'
//! curl -c /tmp/cj -X POST localhost:8080/login    -d '{"email":"root@example.com","password":"password1","remember":true}'
//! curl -b /tmp/cj -c /tmp/cj localhost:8080/me      # revives from the remember cookie if the session lapsed
//! curl -b /tmp/cj localhost:8080/admin/users        # first user is admin
//! curl -b /tmp/cj -X POST localhost:8080/tokens -d '{"name":"my-agent"}'
//! curl -H "Authorization: Bearer stg_…" localhost:8080/api/whoami
//! curl -b /tmp/cj -X POST localhost:8080/logout     # kills session + remember token
//! curl "localhost:8080/verify-email?token=…"        # from the emailed link
//! curl -X POST localhost:8080/forgot-password -d '{"email":"root@example.com"}'
//! curl -X POST localhost:8080/reset-password -d '{"token":"…","password":"newpass99"}'
//! ```
//!
//! The **first registered user becomes `admin`** (bootstrap convention);
//! everyone after is a plain `user`. Login attempts are throttled per
//! email+IP pair (5 a minute) and per account across all IPs (20 a minute);
//! past either limit the answer is 429 with a retry hint.

use std::sync::Arc;
use sutegi::auth::login_key;
use sutegi::crypto::{hex, random_bytes};
use sutegi::prelude::*;
use sutegi::session::MIN_SECRET_LEN;

fn main() -> std::io::Result<()> {
    let db = Db::open(&std::env::var("AUTH_DB").unwrap_or_else(|_| "auth.db".to_string()))
        .expect("open db");
    let ready = db.clone();

    // Demo work factor: PBKDF2 at 10k iterations so debug builds stay snappy.
    // Production default is 600k (OWASP) — just drop the `.iterations(…)`.
    let users = Users::new(db.clone()).iterations(10_000);
    users.migrate().expect("migrate users");
    let tokens = Arc::new(Tokens::new(db.clone()));
    tokens.migrate().expect("migrate tokens");
    let remember = Remember::new(db.clone()).insecure();
    remember.migrate().expect("migrate remember tokens");
    let throttle = Arc::new(Throttle::new(db.clone())); // 5 attempts / 60 s
    throttle.migrate().expect("migrate throttle");
    // Per-account limit, IP-independent: looser so one attacker can't
    // trivially lock a real user out, but rotating IPs buys no extra guesses.
    let account_throttle = Arc::new(Throttle::new(db.clone()).max(20));

    let secret = session_secret(|k| std::env::var(k).ok())
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;
    // `.insecure()` drops the cookies' `Secure` flag for local http:// dev.
    let auth =
        Arc::new(Auth::new(users, Sessions::new(secret.as_bytes()).insecure()).remember(remember));

    let (a_reg, a_login, a_logout, a_me, a_tok) = (
        auth.clone(),
        auth.clone(),
        auth.clone(),
        auth.clone(),
        auth.clone(),
    );
    let (admin_guard, api_guard) = (
        require_role(auth.clone(), "admin"),
        require_token(tokens.clone()),
    );
    let (tok_issue, tok_who, who_auth) = (tokens.clone(), tokens.clone(), auth.clone());

    // Mail: MAIL_* env if set, else the log driver (links print to stderr).
    if std::env::var("MAIL_FROM").is_err() {
        std::env::set_var("MAIL_FROM", "Auth Demo <auth@example.com>");
    }
    let mailer = Arc::new(Mailer::from_env().expect("configure mailer"));
    let base_url = std::env::var("APP_URL").unwrap_or_else(|_| "http://localhost:8080".to_string());
    let mail = Arc::new(AuthMail::new(
        mailer,
        secret.as_bytes(),
        &base_url,
        "Auth Demo",
    ));
    let (m_reg, m_confirm, m_forgot, m_reset) = (mail.clone(), mail.clone(), mail.clone(), mail);
    let (a_confirm, a_forgot, a_reset) = (auth.clone(), auth.clone(), auth.clone());

    App::new("auth-demo")
        .state(auth.clone())
        .readiness(move || ready.query("SELECT 1", &[]).is_ok())
        .get("/", "Health check.", |_| "sutegi auth up")
        .post(
            "/register",
            "Create an account (first account becomes admin) and log it in.",
            move |c| {
                let body = c.json()?;
                let (email, password, name) = credentials(&body)?;
                let role = if a_reg.users.count()? == 0 {
                    "admin"
                } else {
                    "user"
                };
                let user = a_reg
                    .users
                    .register_with(email, password, name, role)
                    .map_err(Error::unprocessable)?;
                m_reg.send_verification(&user)?; // log driver prints the link
                Ok::<_, Error>(a_reg.login(c.req, &user, json(201, &user.to_json())))
            },
        )
        .post(
            "/login",
            "Log in with email + password (\"remember\": true for a 30-day cookie). Throttled.",
            move |c| {
                let body = c.json()?;
                let (email, password, _) = credentials(&body)?;
                let ip = c
                    .req
                    .peer
                    .as_deref()
                    .map(|p| p.rsplit_once(':').map(|(host, _)| host).unwrap_or(p))
                    .unwrap_or_default()
                    .to_string();
                // Count-then-decide in one atomic step per key (no check-
                // then-hit race), keyed on the normalized email so case or
                // whitespace variants share the counter.
                let (client_key, account_key) =
                    (login_key(email, Some(&ip)), login_key(email, None));
                let limited = match throttle.attempt(&client_key)? {
                    Some(retry) => Some(retry),
                    None => account_throttle.attempt(&account_key)?,
                };
                if let Some(retry) = limited {
                    return Ok(json(
                        429,
                        &Json::obj(vec![
                            ("error", Json::str("too many attempts")),
                            ("retry_after", Json::int(retry)),
                        ]),
                    ));
                }
                match a_login.users.authenticate(email, password)? {
                    Some(user) => {
                        throttle.clear(&client_key)?;
                        account_throttle.clear(&account_key)?;
                        let resp = json(200, &user.to_json());
                        let remember = body.get("remember").and_then(Json::as_bool) == Some(true);
                        Ok::<_, Error>(if remember {
                            a_login.login_remembered(c.req, &user, resp)?
                        } else {
                            a_login.login(c.req, &user, resp)
                        })
                    }
                    // The failed attempt is already counted by `attempt`.
                    None => Err(Error::unauthorized("bad credentials")),
                }
            },
        )
        .post(
            "/logout",
            "Log out: expires the session cookie and revokes the remember token.",
            move |c| {
                Ok::<_, Error>(
                    a_logout
                        .logout_from(c.req, json(200, &Json::obj(vec![("ok", Json::Bool(true))]))),
                )
            },
        )
        .get(
            "/me",
            "The logged-in user (revives a lapsed session from the remember cookie).",
            move |c| match a_me.identify(c.req)? {
                Some(hit) => {
                    let resp = json(200, &hit.user.to_json());
                    Ok::<_, Error>(hit.attach(resp))
                }
                None => Err(Error::unauthorized("unauthenticated")),
            },
        )
        .post(
            "/tokens",
            "Mint an API token for the logged-in user (plaintext shown once).",
            move |c| {
                let Some(user) = a_tok.current(c.req)? else {
                    return Err(Error::unauthorized("unauthenticated"));
                };
                let name = c
                    .json()?
                    .get("name")
                    .and_then(Json::as_str)
                    .unwrap_or("api")
                    .to_string();
                let (plaintext, rec) = tok_issue.issue(user.id, &name)?;
                Ok::<_, Error>(json(
                    201,
                    &Json::obj(vec![
                        ("token", Json::str(plaintext)),
                        ("meta", rec.to_json()),
                    ]),
                ))
            },
        )
        .get(
            "/verify-email",
            "Confirm an email-verification link (?token=…).",
            move |c| {
                let token = query_params(c.req)
                    .get("token")
                    .cloned()
                    .unwrap_or_default();
                match m_confirm.confirm_email(&a_confirm.users, &token)? {
                    Some(user) => Ok::<_, Error>(json(200, &user.to_json())),
                    None => Err(Error::unprocessable("invalid or expired link")),
                }
            },
        )
        .post(
            "/forgot-password",
            "Send a password-reset link if the account exists.",
            move |c| {
                let email = c
                    .json()?
                    .get("email")
                    .and_then(Json::as_str)
                    .unwrap_or("")
                    .to_string();
                m_forgot.send_password_reset(&a_forgot.users, &email)?;
                // Always 200: no account enumeration.
                Ok::<_, Error>(json(200, &Json::obj(vec![("ok", Json::Bool(true))])))
            },
        )
        .post(
            "/reset-password",
            "Set a new password with a reset token.",
            move |c| {
                let body = c.json()?;
                let token = body.get("token").and_then(Json::as_str).unwrap_or("");
                let password = body.get("password").and_then(Json::as_str).unwrap_or("");
                match m_reset.reset_password(&a_reset.users, token, password)? {
                    Some(user) => Ok::<_, Error>(json(200, &user.to_json())),
                    None => Err(Error::unprocessable("invalid or expired link")),
                }
            },
        )
        .group("/admin", vec![mw(admin_guard)], |g| {
            g.get("/users", "Every registered user (admin only).", |c| {
                let auth = c.state::<Arc<Auth<Db>>>();
                let all = auth.users.list()?;
                Ok::<_, Error>(json(
                    200,
                    &Json::arr(all.iter().map(User::to_json).collect()),
                ))
            })
        })
        .group("/api", vec![mw(api_guard)], move |g| {
            g.get(
                "/whoami",
                "The token's owning user (agents call this).",
                move |c| match token_user(&tok_who, &who_auth.users, c.req)? {
                    Some(user) => Ok::<_, Error>(json(200, &user.to_json())),
                    None => Err(Error::unauthorized("unauthenticated")),
                },
            )
        })
        .serve()
}

/// `SESSION_SECRET` (≥ 32 bytes) signs sessions and email links. There is
/// no baked-in fallback — a known default would let anyone forge sessions
/// on a deployment that forgot to set it. For local hacking, opt in with
/// `SUTEGI_ENV=dev` to get a random per-process secret (sessions don't
/// survive a restart).
fn session_secret(env: impl Fn(&str) -> Option<String>) -> Result<String, String> {
    match env("SESSION_SECRET") {
        Some(s) if s.len() >= MIN_SECRET_LEN => Ok(s),
        Some(s) => Err(format!(
            "SESSION_SECRET is {} bytes; need at least {MIN_SECRET_LEN} \
             (generate one with `openssl rand -hex 32`)",
            s.len()
        )),
        None if env("SUTEGI_ENV").as_deref() == Some("dev") => {
            eprintln!("SUTEGI_ENV=dev: using a random per-process SESSION_SECRET");
            Ok(hex(&random_bytes(32)?))
        }
        None => Err("SESSION_SECRET is not set (or set SUTEGI_ENV=dev for a \
                     throwaway local secret)"
            .to_string()),
    }
}

fn credentials(body: &Json) -> Result<(&str, &str, &str), Error> {
    let email = body
        .get("email")
        .and_then(Json::as_str)
        .ok_or_else(|| Error::bad_request("email is required"))?;
    let password = body
        .get("password")
        .and_then(Json::as_str)
        .ok_or_else(|| Error::bad_request("password is required"))?;
    let name = body.get("name").and_then(Json::as_str).unwrap_or("");
    Ok((email, password, name))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env<'a>(vars: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |k| {
            vars.iter()
                .find(|(n, _)| *n == k)
                .map(|(_, v)| v.to_string())
        }
    }

    #[test]
    fn missing_secret_refuses_to_start() {
        assert!(session_secret(env(&[])).is_err());
        assert!(session_secret(env(&[("SUTEGI_ENV", "production")])).is_err());
    }

    #[test]
    fn short_secret_refuses_to_start() {
        assert!(session_secret(env(&[("SESSION_SECRET", "hunter2")])).is_err());
    }

    #[test]
    fn configured_or_explicit_dev_secret_starts() {
        let good = "a".repeat(MIN_SECRET_LEN);
        assert_eq!(
            session_secret(env(&[("SESSION_SECRET", &good)])).unwrap(),
            good
        );
        let a = session_secret(env(&[("SUTEGI_ENV", "dev")])).unwrap();
        let b = session_secret(env(&[("SUTEGI_ENV", "dev")])).unwrap();
        assert!(a.len() >= MIN_SECRET_LEN);
        assert_ne!(a, b, "dev secret is random, never a baked-in constant");
    }
}
