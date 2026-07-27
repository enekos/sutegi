//! API tokens — how **agents and services** authenticate, where cookie
//! sessions don't fit. The Sanctum-shaped deal:
//!
//! - [`Tokens::issue`] mints a `stg_<64 hex>` bearer token (32 bytes of OS
//!   randomness) and returns the plaintext **exactly once**;
//! - only its SHA-256 lands in the database, so a leaked table leaks no
//!   credentials;
//! - [`Tokens::verify`] maps a presented token back to its user id, and
//!   [`crate::require_token`] guards routes with it.

use crate::users::now_secs;
use sutegi_crypto::{hex, random_bytes, sha256};
use sutegi_json::Json;
use sutegi_orm::{Backend, ColType, Column, TableSchema, Value};

/// Tokens carry this prefix so a leaked one is greppable and identifiable.
pub const TOKEN_PREFIX: &str = "stg_";

/// A token record — the plaintext is never stored, so this is metadata only.
#[derive(Clone, Debug, PartialEq)]
pub struct ApiToken {
    pub id: i64,
    pub user_id: i64,
    /// A human label ("ci-deploy", "claude-agent"), for revocation lists.
    pub name: String,
    pub created_at: i64,
    /// Unix seconds after which the token stops verifying; `0` = never.
    pub expires_at: i64,
    /// Unix seconds of the last successful verify; `0` = never used.
    pub last_used_at: i64,
}

impl ApiToken {
    pub fn to_json(&self) -> Json {
        Json::obj(vec![
            ("id", Json::int(self.id)),
            ("user_id", Json::int(self.user_id)),
            ("name", Json::str(self.name.clone())),
            ("created_at", Json::int(self.created_at)),
            ("expires_at", Json::int(self.expires_at)),
            ("last_used_at", Json::int(self.last_used_at)),
        ])
    }
}

/// The token store, over any ORM [`Backend`].
pub struct Tokens<B: Backend> {
    backend: B,
}

impl<B: Backend> Tokens<B> {
    pub fn new(backend: B) -> Tokens<B> {
        Tokens { backend }
    }

    /// Create the `api_tokens` table and its lookup index if absent.
    pub fn migrate(&self) -> Result<(), String> {
        self.backend.migrate(
            &TableSchema::new("api_tokens")
                .column(Column::new("id", ColType::Integer).primary())
                .column(Column::new("user_id", ColType::Integer))
                .column(Column::new("name", ColType::Text))
                .column(Column::new("token_hash", ColType::Text))
                .column(Column::new("created_at", ColType::Integer))
                .column(Column::new("expires_at", ColType::Integer))
                .column(Column::new("last_used_at", ColType::Integer)),
        )?;
        // Upgrade path for tables created before expiry/usage existed; the
        // "duplicate column" error on an already-current table is expected.
        for col in ["expires_at", "last_used_at"] {
            let _ = self.backend.execute(
                &format!("ALTER TABLE api_tokens ADD COLUMN {col} BIGINT NOT NULL DEFAULT 0"),
                &[],
            );
        }
        self.backend
            .execute(
                "CREATE UNIQUE INDEX IF NOT EXISTS api_tokens_hash_unique ON api_tokens (token_hash)",
                &[],
            )
            .map(|_| ())
    }

    /// Mint a non-expiring token for `user_id`. Returns `(plaintext, record)`
    /// — show or deliver the plaintext now; it cannot be recovered later.
    pub fn issue(&self, user_id: i64, name: &str) -> Result<(String, ApiToken), String> {
        self.issue_with(user_id, name, 0)
    }

    /// Mint a token that stops verifying after `ttl_secs` (Sanctum's token
    /// expiration).
    pub fn issue_expiring(
        &self,
        user_id: i64,
        name: &str,
        ttl_secs: i64,
    ) -> Result<(String, ApiToken), String> {
        self.issue_with(user_id, name, now_secs() + ttl_secs.max(1))
    }

    fn issue_with(
        &self,
        user_id: i64,
        name: &str,
        expires_at: i64,
    ) -> Result<(String, ApiToken), String> {
        let plaintext = format!("{TOKEN_PREFIX}{}", hex(&random_bytes(32)?));
        let created_at = now_secs();
        let id = self.backend.insert(
            "api_tokens",
            &[
                ("user_id", Value::Int(user_id)),
                ("name", Value::Text(name.to_string())),
                ("token_hash", Value::Text(hash_of(&plaintext))),
                ("created_at", Value::Int(created_at)),
                ("expires_at", Value::Int(expires_at)),
                ("last_used_at", Value::Int(0)),
            ],
            "id",
        )?;
        Ok((
            plaintext,
            ApiToken {
                id,
                user_id,
                name: name.to_string(),
                created_at,
                expires_at,
                last_used_at: 0,
            },
        ))
    }

    /// Resolve a presented token to its owning user id, or `None` (unknown
    /// or expired). The lookup is by SHA-256 — equality on an unpredictable
    /// 256-bit value, so an index probe leaks nothing usable. A hit stamps
    /// `last_used_at` (best-effort).
    pub fn verify(&self, presented: &str) -> Result<Option<i64>, String> {
        if !presented.starts_with(TOKEN_PREFIX) {
            return Ok(None);
        }
        let Some(row) = self.backend.query_one(
            "SELECT id, user_id, expires_at FROM api_tokens WHERE token_hash = ?",
            &[Value::Text(hash_of(presented))],
        )?
        else {
            return Ok(None);
        };
        let int_of = |k: &str| row.get(k).and_then(Json::as_f64).map(|f| f as i64);
        let (Some(id), Some(user_id)) = (int_of("id"), int_of("user_id")) else {
            return Ok(None);
        };
        let expires_at = int_of("expires_at").unwrap_or(0);
        if expires_at > 0 && expires_at < now_secs() {
            return Ok(None);
        }
        let _ = self.backend.execute(
            "UPDATE api_tokens SET last_used_at = ? WHERE id = ?",
            &[Value::Int(now_secs()), Value::Int(id)],
        );
        Ok(Some(user_id))
    }

    /// A user's tokens (metadata only), newest first.
    pub fn list(&self, user_id: i64) -> Result<Vec<ApiToken>, String> {
        self.backend
            .query(
                "SELECT id, user_id, name, created_at, expires_at, last_used_at \
                 FROM api_tokens WHERE user_id = ? ORDER BY id DESC",
                &[Value::Int(user_id)],
            )?
            .iter()
            .map(token_of)
            .collect()
    }

    /// Revoke one token by id. Returns `true` if one was removed.
    pub fn revoke(&self, id: i64) -> Result<bool, String> {
        Ok(self
            .backend
            .execute("DELETE FROM api_tokens WHERE id = ?", &[Value::Int(id)])?
            > 0)
    }

    /// Revoke every token a user holds. Returns how many were removed.
    pub fn revoke_all(&self, user_id: i64) -> Result<usize, String> {
        self.backend.execute(
            "DELETE FROM api_tokens WHERE user_id = ?",
            &[Value::Int(user_id)],
        )
    }
}

fn hash_of(token: &str) -> String {
    hex(&sha256(token.as_bytes()))
}

fn token_of(row: &Json) -> Result<ApiToken, String> {
    let int_of = |k: &str| {
        row.get(k)
            .and_then(Json::as_f64)
            .map(|f| f as i64)
            .ok_or_else(|| format!("token row missing {k}"))
    };
    Ok(ApiToken {
        id: int_of("id")?,
        user_id: int_of("user_id")?,
        name: row
            .get("name")
            .and_then(Json::as_str)
            .unwrap_or_default()
            .to_string(),
        created_at: int_of("created_at")?,
        expires_at: int_of("expires_at").unwrap_or(0),
        last_used_at: int_of("last_used_at").unwrap_or(0),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use sutegi_orm::db::Db;

    fn store() -> Tokens<Db> {
        let t = Tokens::new(Db::memory().unwrap());
        t.migrate().unwrap();
        t
    }

    #[test]
    fn issue_verify_revoke() {
        let tokens = store();
        let (plain, rec) = tokens.issue(7, "ci").unwrap();
        assert!(plain.starts_with(TOKEN_PREFIX));
        assert_eq!(plain.len(), TOKEN_PREFIX.len() + 64);
        assert_eq!(rec.user_id, 7);

        assert_eq!(tokens.verify(&plain).unwrap(), Some(7));
        assert_eq!(tokens.verify("stg_deadbeef").unwrap(), None);
        assert_eq!(tokens.verify("Bearer whatever").unwrap(), None);

        assert!(tokens.revoke(rec.id).unwrap());
        assert_eq!(tokens.verify(&plain).unwrap(), None);
        assert!(!tokens.revoke(rec.id).unwrap());
    }

    #[test]
    fn plaintext_never_stored() {
        let tokens = store();
        let (plain, _) = tokens.issue(1, "x").unwrap();
        let rows = tokens
            .backend
            .query("SELECT token_hash FROM api_tokens", &[])
            .unwrap();
        let stored = rows[0].get("token_hash").and_then(Json::as_str).unwrap();
        assert_ne!(stored, plain);
        assert!(!stored.contains(&plain[4..20]));
    }

    #[test]
    fn expiry_and_last_used() {
        let tokens = store();
        let (fresh, rec) = tokens.issue_expiring(3, "short", 3_600).unwrap();
        assert!(rec.expires_at > now_secs());
        assert_eq!(tokens.verify(&fresh).unwrap(), Some(3));
        // A verify stamps last_used_at.
        let listed = &tokens.list(3).unwrap()[0];
        assert!(listed.last_used_at > 0);

        // Backdate the expiry: the token stops verifying.
        tokens
            .backend
            .execute(
                "UPDATE api_tokens SET expires_at = 1 WHERE id = ?",
                &[Value::Int(rec.id)],
            )
            .unwrap();
        assert_eq!(tokens.verify(&fresh).unwrap(), None);

        // Non-expiring tokens (expires_at = 0) still verify.
        let (forever, _) = tokens.issue(3, "forever").unwrap();
        assert_eq!(tokens.verify(&forever).unwrap(), Some(3));
    }

    #[test]
    fn list_and_revoke_all() {
        let tokens = store();
        tokens.issue(1, "a").unwrap();
        tokens.issue(1, "b").unwrap();
        tokens.issue(2, "other").unwrap();

        let mine = tokens.list(1).unwrap();
        assert_eq!(mine.len(), 2);
        assert_eq!(mine[0].name, "b"); // newest first

        assert_eq!(tokens.revoke_all(1).unwrap(), 2);
        assert_eq!(tokens.list(1).unwrap().len(), 0);
        assert_eq!(tokens.list(2).unwrap().len(), 1);
    }
}
