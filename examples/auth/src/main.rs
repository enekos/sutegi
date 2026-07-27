//! The sutegi user system, end to end: registration, signed-cookie login with
//! server-side expiry, remember-me revival, login throttling, role-gated
//! admin routes, and API tokens for agents.
//!
//! ```text
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
//! everyone after is a plain `user`. Six failed logins in a minute lock the
//! email+IP pair out (429 with a retry hint).

use std::sync::Arc;
use sutegi::prelude::*;

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

    let secret = std::env::var("SESSION_SECRET")
        .unwrap_or_else(|_| "dev-only-secret-set-SESSION_SECRET".to_string());
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
                let key = format!("login:{email}|{ip}");
                if let Some(retry) = throttle.too_many(&key)? {
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
                        throttle.clear(&key)?;
                        let resp = json(200, &user.to_json());
                        let remember = body.get("remember").and_then(Json::as_bool) == Some(true);
                        Ok::<_, Error>(if remember {
                            a_login.login_remembered(c.req, &user, resp)?
                        } else {
                            a_login.login(c.req, &user, resp)
                        })
                    }
                    None => {
                        throttle.hit(&key)?;
                        Err(Error::unauthorized("bad credentials"))
                    }
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
