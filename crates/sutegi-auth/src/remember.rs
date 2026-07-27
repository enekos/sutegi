//! **Remember me** — the long-lived login that survives the session cookie,
//! done the selector/validator way rather than Laravel's single
//! `remember_token` column:
//!
//! - the cookie is `<selector>.<validator>`; only the validator's SHA-256 is
//!   stored, so a leaked table recalls nobody;
//! - the validator **rotates on every use** — a stolen-then-used cookie
//!   invalidates the victim's copy, which surfaces the theft as a logout;
//! - each token is **bound to the password hash** at mint time, so changing
//!   the password silently kills every outstanding remember cookie;
//! - tokens expire server-side (default 30 days) regardless of the cookie's
//!   `Max-Age`.
//!
//! Pair it with [`crate::Auth`]: `Auth::remember(store)` makes
//! `login_remembered` mint the cookie and `identify` revive expired sessions
//! from it.

use crate::users::now_secs;
use sutegi_crypto::{constant_time_eq, hex, random_bytes, sha256};
use sutegi_json::Json;
use sutegi_orm::{Backend, ColType, Column, TableSchema, Value};
use sutegi_web::Request;

/// Default cookie name.
pub const REMEMBER_COOKIE: &str = "sutegi_remember";

/// Default server-side lifetime: 30 days.
pub const REMEMBER_TTL: i64 = 30 * 86_400;

/// The remember-token store, over any ORM [`Backend`].
pub struct Remember<B: Backend> {
    backend: B,
    ttl: i64,
    cookie: String,
    secure: bool,
}

impl<B: Backend> Remember<B> {
    pub fn new(backend: B) -> Remember<B> {
        Remember {
            backend,
            ttl: REMEMBER_TTL,
            cookie: REMEMBER_COOKIE.to_string(),
            secure: true,
        }
    }

    /// Server-side token lifetime in seconds (default 30 days).
    pub fn ttl(mut self, secs: i64) -> Remember<B> {
        self.ttl = secs.max(1);
        self
    }

    pub fn cookie_name(mut self, name: &str) -> Remember<B> {
        self.cookie = name.to_string();
        self
    }

    /// Drop the cookie's `Secure` attribute (local `http://` dev only).
    pub fn insecure(mut self) -> Remember<B> {
        self.secure = false;
        self
    }

    /// Create the `remember_tokens` table and its selector index if absent.
    pub fn migrate(&self) -> Result<(), String> {
        self.backend.migrate(
            &TableSchema::new("remember_tokens")
                .column(Column::new("id", ColType::Integer).primary())
                .column(Column::new("user_id", ColType::Integer))
                .column(Column::new("selector", ColType::Text))
                .column(Column::new("validator_hash", ColType::Text))
                .column(Column::new("pw_bind", ColType::Text))
                .column(Column::new("created_at", ColType::Integer))
                .column(Column::new("last_used_at", ColType::Integer))
                .column(Column::new("expires_at", ColType::Integer)),
        )?;
        self.backend
            .execute(
                "CREATE UNIQUE INDEX IF NOT EXISTS remember_selector_unique \
                 ON remember_tokens (selector)",
                &[],
            )
            .map(|_| ())
    }

    /// Mint a token for `user_id`, bound to `pw_bind` (a fingerprint of the
    /// current password hash — [`crate::Auth`] supplies it). Returns the
    /// cookie **value**; wrap it with [`cookie_header`](Remember::cookie_header).
    pub fn issue(&self, user_id: i64, pw_bind: &str) -> Result<String, String> {
        let selector = hex(&random_bytes(9)?);
        let validator = hex(&random_bytes(32)?);
        let now = now_secs();
        self.backend.insert(
            "remember_tokens",
            &[
                ("user_id", Value::Int(user_id)),
                ("selector", Value::Text(selector.clone())),
                ("validator_hash", Value::Text(hash_of(&validator))),
                ("pw_bind", Value::Text(pw_bind.to_string())),
                ("created_at", Value::Int(now)),
                ("last_used_at", Value::Int(now)),
                ("expires_at", Value::Int(now + self.ttl)),
            ],
            "id",
        )?;
        Ok(format!("{selector}.{validator}"))
    }

    /// Redeem a presented cookie value. `current_bind_of` maps a candidate
    /// user id to the fingerprint of their **current** password hash (`None`
    /// = user gone); a stale binding is a dead token. On success the
    /// validator is **rotated** and the fresh cookie value returned alongside
    /// the user id.
    pub fn consume(
        &self,
        presented: &str,
        current_bind_of: impl FnOnce(i64) -> Result<Option<String>, String>,
    ) -> Result<Option<(i64, String)>, String> {
        let Some((selector, validator)) = presented.split_once('.') else {
            return Ok(None);
        };
        let Some(row) = self.backend.query_one(
            "SELECT id, user_id, validator_hash, pw_bind, expires_at \
             FROM remember_tokens WHERE selector = ?",
            &[Value::Text(selector.to_string())],
        )?
        else {
            return Ok(None);
        };
        let int_of = |k: &str| row.get(k).and_then(Json::as_f64).map(|f| f as i64);
        let (Some(id), Some(user_id), Some(expires_at)) =
            (int_of("id"), int_of("user_id"), int_of("expires_at"))
        else {
            return Ok(None);
        };
        let stored = row
            .get("validator_hash")
            .and_then(Json::as_str)
            .unwrap_or("");
        if !constant_time_eq(hash_of(validator).as_bytes(), stored.as_bytes()) {
            // Correct selector, wrong validator: either garbage or a copy
            // that was already rotated away — revoke the row so a possibly
            // stolen token can't be brute-forced in place.
            let _ = self.backend.execute(
                "DELETE FROM remember_tokens WHERE id = ?",
                &[Value::Int(id)],
            );
            return Ok(None);
        }
        if expires_at < now_secs() {
            let _ = self.backend.execute(
                "DELETE FROM remember_tokens WHERE id = ?",
                &[Value::Int(id)],
            );
            return Ok(None);
        }
        let bind = row.get("pw_bind").and_then(Json::as_str).unwrap_or("");
        if current_bind_of(user_id)?.as_deref() != Some(bind) {
            let _ = self.backend.execute(
                "DELETE FROM remember_tokens WHERE id = ?",
                &[Value::Int(id)],
            );
            return Ok(None); // password changed since mint
        }
        // Rotate the validator in place; the old cookie value is now dead.
        let fresh = hex(&random_bytes(32)?);
        self.backend.execute(
            "UPDATE remember_tokens SET validator_hash = ?, last_used_at = ? WHERE id = ?",
            &[
                Value::Text(hash_of(&fresh)),
                Value::Int(now_secs()),
                Value::Int(id),
            ],
        )?;
        Ok(Some((user_id, format!("{selector}.{fresh}"))))
    }

    /// Revoke the token behind a presented cookie value (logout on this
    /// device). Returns `true` if a row was removed.
    pub fn revoke_presented(&self, presented: &str) -> Result<bool, String> {
        let Some((selector, _)) = presented.split_once('.') else {
            return Ok(false);
        };
        Ok(self.backend.execute(
            "DELETE FROM remember_tokens WHERE selector = ?",
            &[Value::Text(selector.to_string())],
        )? > 0)
    }

    /// Revoke every remember token a user holds (logout everywhere).
    pub fn revoke_all(&self, user_id: i64) -> Result<usize, String> {
        self.backend.execute(
            "DELETE FROM remember_tokens WHERE user_id = ?",
            &[Value::Int(user_id)],
        )
    }

    /// The presented cookie value on a request, if any.
    pub fn read(&self, req: &Request) -> Option<String> {
        req.cookie(&self.cookie)
    }

    /// A `Set-Cookie` header value carrying `value` for the token's lifetime.
    pub fn cookie_header(&self, value: &str) -> String {
        let mut c = format!(
            "{}={value}; Path=/; HttpOnly; SameSite=Lax; Max-Age={}",
            self.cookie, self.ttl
        );
        if self.secure {
            c.push_str("; Secure");
        }
        c
    }

    /// A `Set-Cookie` header value that expires the cookie.
    pub fn clear_header(&self) -> String {
        format!("{}=; Path=/; Max-Age=0", self.cookie)
    }
}

fn hash_of(validator: &str) -> String {
    hex(&sha256(validator.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use sutegi_orm::db::Db;

    fn store() -> Remember<Db> {
        let r = Remember::new(Db::memory().unwrap()).insecure();
        r.migrate().unwrap();
        r
    }

    fn bind_ok(_: i64) -> Result<Option<String>, String> {
        Ok(Some("bind".to_string()))
    }

    #[test]
    fn issue_consume_rotates() {
        let r = store();
        let cookie = r.issue(7, "bind").unwrap();
        let (uid, fresh) = r.consume(&cookie, bind_ok).unwrap().unwrap();
        assert_eq!(uid, 7);
        assert_ne!(fresh, cookie);
        // The old value is dead, the rotated one lives.
        assert!(r.consume(&cookie, bind_ok).unwrap().is_none());
        // Old value even revoked the row (theft response) — fresh dies too.
        assert!(r.consume(&fresh, bind_ok).unwrap().is_none());
    }

    #[test]
    fn rotation_chain_survives_when_only_fresh_is_used() {
        let r = store();
        let mut cookie = r.issue(1, "bind").unwrap();
        for _ in 0..3 {
            let (uid, fresh) = r.consume(&cookie, bind_ok).unwrap().unwrap();
            assert_eq!(uid, 1);
            cookie = fresh;
        }
    }

    #[test]
    fn password_change_kills_token() {
        let r = store();
        let cookie = r.issue(1, "old-bind").unwrap();
        let hit = r
            .consume(&cookie, |_| Ok(Some("new-bind".to_string())))
            .unwrap();
        assert!(hit.is_none());
        // And the row is gone: even the original bind can't revive it.
        assert!(r.consume(&cookie, bind_ok).unwrap().is_none());
    }

    #[test]
    fn expiry_and_revocation() {
        let r = Remember::new(Db::memory().unwrap()).ttl(1).insecure();
        r.migrate().unwrap();
        // ttl(1) then a backdated row: simulate expiry directly.
        let cookie = r.issue(2, "bind").unwrap();
        r.backend
            .execute("UPDATE remember_tokens SET expires_at = 1", &[])
            .unwrap();
        assert!(r.consume(&cookie, bind_ok).unwrap().is_none());

        let c2 = r.issue(2, "bind").unwrap();
        let c3 = r.issue(2, "bind").unwrap();
        assert!(r.revoke_presented(&c2).unwrap());
        assert!(!r.revoke_presented(&c2).unwrap());
        assert_eq!(r.revoke_all(2).unwrap(), 1);
        assert!(r.consume(&c3, bind_ok).unwrap().is_none());
    }

    #[test]
    fn garbage_cookies_are_none() {
        let r = store();
        assert!(r.consume("no-dot", bind_ok).unwrap().is_none());
        assert!(r.consume("", bind_ok).unwrap().is_none());
        assert!(r.consume("dead.beef", bind_ok).unwrap().is_none());
        assert!(!r.revoke_presented("no-dot").unwrap());
    }

    #[test]
    fn cookie_headers() {
        let r = Remember::new(Db::memory().unwrap()).ttl(60);
        let h = r.cookie_header("abc.def");
        assert!(h.starts_with("sutegi_remember=abc.def;"));
        assert!(h.contains("HttpOnly") && h.contains("Secure") && h.contains("Max-Age=60"));
        assert!(r.clear_header().contains("Max-Age=0"));
        let ins = Remember::new(Db::memory().unwrap())
            .insecure()
            .cookie_name("r");
        assert!(!ins.cookie_header("v").contains("Secure"));
        assert!(ins.cookie_header("v").starts_with("r=v;"));
    }
}
