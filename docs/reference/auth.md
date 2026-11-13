<!-- Generated from landing/src/Docs.svelte. Run: cd landing && pnpm docs:export -->

[← Actors & supervision](actors.md) · [Documentation index](../README.md) · [Sessions & CSRF →](sessions.md)

# Authentication

The `auth` feature is a complete user system over either backend — everything Laravel’s auth scaffolding does, with no third-party dependency. `Users` handles registration and authentication with PBKDF2-HMAC-SHA256 passwords (600k iterations by default, per OWASP; the hash never leaves the store). `Auth` issues signed-cookie logins. `Tokens` mints `stg_` bearer tokens for agents. The guards are just middleware you attach to a group.

```rust
use std::sync::Arc;

let users = Users::new(db.clone());       // PBKDF2-HMAC-SHA256, 600k iters (OWASP)
users.migrate()?;
let tokens = Arc::new(Tokens::new(db.clone()));
tokens.migrate()?;

let auth = Arc::new(
    Auth::new(users, Sessions::new(secret.as_bytes()))   // .insecure() for local http://
        .remember(Remember::new(db.clone())),            // selector/validator cookies
);
let throttle = Throttle::new(db.clone());                // 5 attempts / 60s by default

App::new("app")
    .state(auth.clone())
    .post("/login", "Log in", move |c| {
        let key = format!("login:{}|{}", email, c.req.peer.as_deref().unwrap_or(""));
        if let Some(retry_after) = throttle.too_many(&key)? {
            return Err(Error::new(429, "too many attempts").with_fields(
                Json::obj(vec![("retry_after", Json::Int(retry_after))])));
        }
        match auth.users.authenticate(email, password)? {
            Some(u) => { throttle.clear(&key)?;
                         Ok::<_, Error>(auth.login_remembered(c.req, &u, remember, resp)) }
            None => { throttle.hit(&key)?; Err(Error::unauthorized("bad credentials")) }
        }
    })
    // identify() = session-or-remember revival; attach() sets the fresh cookies.
    .get("/me", "Current user", move |c| match auth_me.identify(c.req)? {
        Some(id) => Ok::<_, Error>(id.attach(json(200, &id.user.to_json()))),
        None => Err(Error::unauthorized("unauthenticated")),
    })
    // Guards are just middleware:
    .group("/admin", vec![mw(require_role(auth.clone(), "admin")),
                          mw(require_verified(auth.clone())),
                          mw(require_csrf(auth.clone()))], |g| { /* … */ })
    .group("/api", vec![mw(require_token(tokens.clone()))], |g| { /* stg_ bearer tokens */ })
    .serve()
```

## What the production pieces do

- **Remember me** — selector/validator cookies where only the validator’s SHA-256 is stored, the validator *rotates on every use* (a stolen-then-replayed copy revokes the row, surfacing the theft), tokens are bound to the password hash at mint, and server-side expiry (default 30 days) ignores whatever the client claims.
- **Sessions bound to the password hash** — `login` stamps a fingerprint of the current PHC string into the signed session, and a stale binding reads as anonymous. Changing a password logs out every other device on its next store-checking request.
- **Login throttling** — a DB-backed fixed window (default 5 attempts / 60 s, keyed however you like; the convention is `login:<email>|<ip>`), so every pod counts the same attempts.
- **CSRF** — a token minted inside the signed session and compared in constant time; the `require_csrf` guard enforces `X-CSRF-Token` on mutating methods (419, Laravel’s “Page Expired”) while passing reads and `Authorization`-header callers.
- **Auto-rehash at login** — a verified password whose stored iteration count is below the store’s is transparently re-hashed (best-effort; a rehash failure never fails a valid login). Raise the work factor once and the fleet upgrades itself credential-by-credential.
- **Guards** — `require_auth`, `require_role`, `require_verified`, `require_csrf`, `require_token`. `Auth::identify` is the revival point (session-or-remember) and its `Identified::attach` sets the fresh cookies, because sutegi middleware cannot set cookies on pass-through.
- **API tokens** — the agent door. Plaintext shown once, only the SHA-256 stored; `issue_expiring` adds a deadline, every successful verify stamps `last_used_at`, and tokens survive logout.
- **Profile operations** — `change_password` (verifies the current one first), `set_name`, and `set_email` (normalizes, checks uniqueness, and resets `verified_at` so the new address must re-verify).

Unknown emails burn the same PBKDF2 time as wrong passwords, so the endpoint does not become a user enumeration oracle.

> [!TIP]
> **Verification & reset**
>
> Add the `auth-mail` feature and `AuthMail` for email verification and password-reset flows: built-in text+HTML templates, signed expiring links (24 h / 1 h), no state tables. Reset tokens are enumeration-safe (an unknown email is a silent `Ok`) and bound to the current password hash — the moment the password changes, every outstanding link dies, which makes them single-use without storing anything.

---

[← Actors & supervision](actors.md) · [Documentation index](../README.md) · [Sessions & CSRF →](sessions.md)
