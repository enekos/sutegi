//! The `Users` store: registration, credential checks, and lookups over any
//! ORM [`Backend`] — SQLite single-node or Postgres multi-pod, same calls.
//!
//! Passwords are stored as PHC strings (see [`crate::password`]) and **never
//! leave this module**: [`User`] carries no hash, and `authenticate` burns
//! comparable time on unknown emails so a missing account is not
//! distinguishable from a wrong password by timing.

use crate::password::{hash_password_with, verify_password, DEFAULT_ITERATIONS};
use std::sync::OnceLock;
use sutegi_json::Json;
use sutegi_orm::{Backend, ColType, Column, TableSchema, Value};

/// A user record, hash-free by construction. `role` is a free-form label
/// (`"user"` by default) checked by [`crate::require_role`].
#[derive(Clone, Debug, PartialEq)]
pub struct User {
    pub id: i64,
    pub email: String,
    pub name: String,
    pub role: String,
    pub created_at: i64,
}

impl User {
    /// The machine-readable shape, for handlers and agent tools.
    pub fn to_json(&self) -> Json {
        Json::obj(vec![
            ("id", Json::int(self.id)),
            ("email", Json::str(self.email.clone())),
            ("name", Json::str(self.name.clone())),
            ("role", Json::str(self.role.clone())),
            ("created_at", Json::int(self.created_at)),
        ])
    }

    /// Whether the user carries `role`.
    pub fn is(&self, role: &str) -> bool {
        self.role == role
    }
}

/// The user store. Construct once, share via `App::state` (it is `Send + Sync`
/// when the backend is, and the pooled `Db`/`Pg` handles are).
pub struct Users<B: Backend> {
    backend: B,
    iterations: u32,
    /// Lazily-built decoy hash: `authenticate` verifies against it when the
    /// email is unknown, so both failure paths cost one PBKDF2 run.
    decoy: OnceLock<String>,
}

/// Minimum accepted password length (bytes).
pub const MIN_PASSWORD_LEN: usize = 8;

impl<B: Backend> Users<B> {
    pub fn new(backend: B) -> Users<B> {
        Users {
            backend,
            iterations: DEFAULT_ITERATIONS,
            decoy: OnceLock::new(),
        }
    }

    /// Override the PBKDF2 work factor (tuning, or fast test/demo setups).
    /// Existing hashes keep their stored count and still verify.
    pub fn iterations(mut self, n: u32) -> Users<B> {
        self.iterations = n;
        self
    }

    /// The underlying backend, for mixing with relational access.
    pub fn backend(&self) -> &B {
        &self.backend
    }

    fn schema() -> TableSchema {
        let col = |name, ty| Column {
            name,
            ty,
            nullable: false,
            primary: false,
        };
        TableSchema {
            table: "users",
            columns: vec![
                Column {
                    name: "id",
                    ty: ColType::Integer,
                    nullable: false,
                    primary: true,
                },
                col("email", ColType::Text),
                col("password_hash", ColType::Text),
                col("name", ColType::Text),
                col("role", ColType::Text),
                col("created_at", ColType::Integer),
            ],
        }
    }

    /// Create the `users` table and its unique email index if absent.
    /// Portable across SQLite and Postgres.
    pub fn migrate(&self) -> Result<(), String> {
        self.backend.migrate(&Self::schema())?;
        self.backend
            .execute(
                "CREATE UNIQUE INDEX IF NOT EXISTS users_email_unique ON users (email)",
                &[],
            )
            .map(|_| ())
    }

    /// Register a user with the default `""` name and `"user"` role.
    pub fn register(&self, email: &str, password: &str) -> Result<User, String> {
        self.register_with(email, password, "", "user")
    }

    /// Register with an explicit name and role. Fails on a malformed email,
    /// a password under [`MIN_PASSWORD_LEN`] bytes, or a taken email.
    pub fn register_with(
        &self,
        email: &str,
        password: &str,
        name: &str,
        role: &str,
    ) -> Result<User, String> {
        let email = normalize_email(email)?;
        if password.len() < MIN_PASSWORD_LEN {
            return Err(format!(
                "password must be at least {MIN_PASSWORD_LEN} characters"
            ));
        }
        if self.find_by_email(&email)?.is_some() {
            return Err("email already registered".to_string());
        }
        let hash = hash_password_with(password, self.iterations)?;
        let created_at = now_secs();
        // The unique index is the backstop for the (benign) find→insert race:
        // a concurrent duplicate registration surfaces as a constraint error.
        let id = self.backend.insert(
            "users",
            &[
                ("email", Value::Text(email.clone())),
                ("password_hash", Value::Text(hash)),
                ("name", Value::Text(name.to_string())),
                ("role", Value::Text(role.to_string())),
                ("created_at", Value::Int(created_at)),
            ],
            "id",
        )?;
        Ok(User {
            id,
            email,
            name: name.to_string(),
            role: role.to_string(),
            created_at,
        })
    }

    /// Check credentials. `Ok(None)` covers both "no such user" and "wrong
    /// password" — with one PBKDF2 run burned either way — and `Err` is
    /// reserved for real store failures.
    pub fn authenticate(&self, email: &str, password: &str) -> Result<Option<User>, String> {
        let email = match normalize_email(email) {
            Ok(e) => e,
            Err(_) => return Ok(None),
        };
        let row = self.backend.query_one(
            "SELECT id, email, password_hash, name, role, created_at FROM users WHERE email = ?",
            &[Value::Text(email)],
        )?;
        match row {
            Some(row) => {
                let hash = row
                    .get("password_hash")
                    .and_then(Json::as_str)
                    .unwrap_or("");
                if verify_password(password, hash) {
                    Ok(Some(user_of(&row)?))
                } else {
                    Ok(None)
                }
            }
            None => {
                let decoy = self.decoy.get_or_init(|| {
                    hash_password_with("decoy", self.iterations).unwrap_or_default()
                });
                let _ = verify_password(password, decoy);
                Ok(None)
            }
        }
    }

    /// The user with `id`, if any.
    pub fn find(&self, id: i64) -> Result<Option<User>, String> {
        self.fetch_where("id = ?", Value::Int(id))
    }

    /// The user with `email` (normalized), if any.
    pub fn find_by_email(&self, email: &str) -> Result<Option<User>, String> {
        let email = normalize_email(email)?;
        self.fetch_where("email = ?", Value::Text(email))
    }

    /// Every user, sorted by id. Hash-free like everything else here.
    pub fn list(&self) -> Result<Vec<User>, String> {
        self.backend
            .query(
                "SELECT id, email, name, role, created_at FROM users ORDER BY id ASC",
                &[],
            )?
            .iter()
            .map(user_of)
            .collect()
    }

    /// Replace a user's password (same length rule as registration).
    pub fn set_password(&self, id: i64, new_password: &str) -> Result<(), String> {
        if new_password.len() < MIN_PASSWORD_LEN {
            return Err(format!(
                "password must be at least {MIN_PASSWORD_LEN} characters"
            ));
        }
        let hash = hash_password_with(new_password, self.iterations)?;
        match self.backend.execute(
            "UPDATE users SET password_hash = ? WHERE id = ?",
            &[Value::Text(hash), Value::Int(id)],
        )? {
            0 => Err(format!("no user with id {id}")),
            _ => Ok(()),
        }
    }

    /// Change a user's role.
    pub fn set_role(&self, id: i64, role: &str) -> Result<(), String> {
        match self.backend.execute(
            "UPDATE users SET role = ? WHERE id = ?",
            &[Value::Text(role.to_string()), Value::Int(id)],
        )? {
            0 => Err(format!("no user with id {id}")),
            _ => Ok(()),
        }
    }

    /// Delete a user. Returns `true` if one was removed.
    pub fn delete(&self, id: i64) -> Result<bool, String> {
        Ok(self
            .backend
            .execute("DELETE FROM users WHERE id = ?", &[Value::Int(id)])?
            > 0)
    }

    /// Number of registered users.
    pub fn count(&self) -> Result<i64, String> {
        Ok(self
            .backend
            .query_one("SELECT COUNT(*) AS count FROM users", &[])?
            .and_then(|r| r.get("count").and_then(Json::as_f64))
            .map(|f| f as i64)
            .unwrap_or(0))
    }

    fn fetch_where(&self, cond: &str, param: Value) -> Result<Option<User>, String> {
        let sql = format!("SELECT id, email, name, role, created_at FROM users WHERE {cond}");
        match self.backend.query_one(&sql, &[param])? {
            Some(row) => Ok(Some(user_of(&row)?)),
            None => Ok(None),
        }
    }
}

fn user_of(row: &Json) -> Result<User, String> {
    let str_of = |k: &str| {
        row.get(k)
            .and_then(Json::as_str)
            .map(String::from)
            .ok_or_else(|| format!("user row missing {k}"))
    };
    let int_of = |k: &str| {
        row.get(k)
            .and_then(Json::as_f64)
            .map(|f| f as i64)
            .ok_or_else(|| format!("user row missing {k}"))
    };
    Ok(User {
        id: int_of("id")?,
        email: str_of("email")?,
        name: str_of("name")?,
        role: str_of("role")?,
        created_at: int_of("created_at")?,
    })
}

/// Trim, lowercase, and shape-check an email. Deliberately minimal — one `@`
/// with something on both sides; real validation is delivery.
fn normalize_email(email: &str) -> Result<String, String> {
    let email = email.trim().to_lowercase();
    let ok = email.len() <= 254
        && email
            .split_once('@')
            .is_some_and(|(l, d)| !l.is_empty() && d.contains('.') && !d.starts_with('.'));
    if ok && !email.contains(char::is_whitespace) {
        Ok(email)
    } else {
        Err("invalid email address".to_string())
    }
}

pub(crate) fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sutegi_orm::db::Db;

    fn store() -> Users<Db> {
        let u = Users::new(Db::memory().unwrap()).iterations(1_000);
        u.migrate().unwrap();
        u
    }

    #[test]
    fn register_authenticate_roundtrip() {
        let users = store();
        let u = users
            .register("Eneko@Example.COM ", "correct horse")
            .unwrap();
        assert_eq!(u.email, "eneko@example.com"); // normalized
        assert_eq!(u.role, "user");
        assert!(u.id > 0);

        let hit = users
            .authenticate("eneko@example.com", "correct horse")
            .unwrap();
        assert_eq!(hit.unwrap().id, u.id);
        assert!(users
            .authenticate("eneko@example.com", "wrong pass!")
            .unwrap()
            .is_none());
        assert!(users
            .authenticate("nobody@example.com", "correct horse")
            .unwrap()
            .is_none());
    }

    #[test]
    fn duplicate_email_rejected() {
        let users = store();
        users.register("a@b.co", "password1").unwrap();
        assert!(users.register("A@B.CO", "password2").is_err());
    }

    #[test]
    fn bad_inputs_rejected() {
        let users = store();
        assert!(users.register("not-an-email", "password1").is_err());
        assert!(users.register("a@b", "password1").is_err()); // no dot in domain
        assert!(users.register("a@b.co", "short").is_err());
        assert!(users.register("", "password1").is_err());
    }

    #[test]
    fn find_list_roles_delete() {
        let users = store();
        let a = users
            .register_with("a@b.co", "password1", "Ana", "admin")
            .unwrap();
        let b = users.register("b@b.co", "password2").unwrap();

        assert_eq!(users.find(a.id).unwrap().unwrap().name, "Ana");
        assert_eq!(users.find_by_email("B@b.co").unwrap().unwrap().id, b.id);
        assert_eq!(users.count().unwrap(), 2);
        let all = users.list().unwrap();
        assert_eq!(all.len(), 2);
        assert!(all[0].is("admin") && !all[1].is("admin"));

        users.set_role(b.id, "admin").unwrap();
        assert!(users.find(b.id).unwrap().unwrap().is("admin"));

        assert!(users.delete(b.id).unwrap());
        assert!(!users.delete(b.id).unwrap());
        assert_eq!(users.count().unwrap(), 1);
    }

    #[test]
    fn set_password_rotates_credentials() {
        let users = store();
        let u = users.register("a@b.co", "oldpassword").unwrap();
        users.set_password(u.id, "newpassword").unwrap();
        assert!(users
            .authenticate("a@b.co", "oldpassword")
            .unwrap()
            .is_none());
        assert!(users
            .authenticate("a@b.co", "newpassword")
            .unwrap()
            .is_some());
        assert!(users.set_password(999, "whatever12").is_err());
    }

    #[test]
    fn user_json_has_no_hash() {
        let users = store();
        let u = users.register("a@b.co", "password1").unwrap();
        let js = u.to_json().to_string();
        assert!(js.contains("a@b.co"));
        assert!(!js.contains("pbkdf2"));
    }
}
