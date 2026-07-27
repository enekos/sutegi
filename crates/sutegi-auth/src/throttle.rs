//! **Login throttling** — Laravel's `ThrottlesLogins`, DB-backed so every pod
//! counts the same attempts. Fixed window: `max` hits per `per` seconds per
//! key, where the key is whatever you rate — the convention for login is
//! `login:<email>|<ip>`.
//!
//! ```ignore
//! let throttle = Throttle::new(db.clone()); // 5 attempts / 60 s
//! let key = format!("login:{email}|{ip}");
//! if let Some(retry) = throttle.too_many(&key)? {
//!     return Err(Error::too_many_requests(format!("retry in {retry}s")));
//! }
//! match auth.users.authenticate(&email, &pw)? {
//!     Some(user) => { throttle.clear(&key)?; /* login */ }
//!     None => { throttle.hit(&key)?; /* 401 */ }
//! }
//! ```

use crate::users::now_secs;
use sutegi_json::Json;
use sutegi_orm::{Backend, ColType, Column, TableSchema, Value};

/// The throttle store, over any ORM [`Backend`].
pub struct Throttle<B: Backend> {
    backend: B,
    max: i64,
    per: i64,
}

impl<B: Backend> Throttle<B> {
    /// Laravel's defaults: 5 attempts per 60-second window.
    pub fn new(backend: B) -> Throttle<B> {
        Throttle {
            backend,
            max: 5,
            per: 60,
        }
    }

    /// Attempts allowed per window.
    pub fn max(mut self, n: i64) -> Throttle<B> {
        self.max = n.max(1);
        self
    }

    /// Window length in seconds.
    pub fn per(mut self, secs: i64) -> Throttle<B> {
        self.per = secs.max(1);
        self
    }

    /// Create the `auth_throttle` table and its key index if absent.
    pub fn migrate(&self) -> Result<(), String> {
        self.backend.migrate(
            &TableSchema::new("auth_throttle")
                .column(Column::new("id", ColType::Integer).primary())
                .column(Column::new("key", ColType::Text))
                .column(Column::new("attempts", ColType::Integer))
                .column(Column::new("window_start", ColType::Integer)),
        )?;
        self.backend
            .execute(
                "CREATE UNIQUE INDEX IF NOT EXISTS auth_throttle_key_unique \
                 ON auth_throttle (key)",
                &[],
            )
            .map(|_| ())
    }

    /// Whether `key` is locked out right now; `Some(secs)` says how long
    /// until the window reopens. Read-only — pair with [`hit`](Throttle::hit)
    /// on failures and [`clear`](Throttle::clear) on success.
    pub fn too_many(&self, key: &str) -> Result<Option<i64>, String> {
        let Some((attempts, window_start)) = self.row(key)? else {
            return Ok(None);
        };
        let now = now_secs();
        if attempts >= self.max && now < window_start + self.per {
            Ok(Some(window_start + self.per - now))
        } else {
            Ok(None)
        }
    }

    /// Record a failed attempt; returns the attempt count in the current
    /// window. The increment is a single atomic `UPDATE`, so concurrent
    /// failures across pods all count.
    pub fn hit(&self, key: &str) -> Result<i64, String> {
        let now = now_secs();
        match self.row(key)? {
            None => {
                // The unique index backstops the insert race: a concurrent
                // first-hit surfaces as a constraint error → count via UPDATE.
                if self
                    .backend
                    .insert(
                        "auth_throttle",
                        &[
                            ("key", Value::Text(key.to_string())),
                            ("attempts", Value::Int(1)),
                            ("window_start", Value::Int(now)),
                        ],
                        "id",
                    )
                    .is_ok()
                {
                    return Ok(1);
                }
                self.bump(key)
            }
            Some((_, window_start)) if now >= window_start + self.per => {
                // Window elapsed: restart it.
                self.backend.execute(
                    "UPDATE auth_throttle SET attempts = 1, window_start = ? WHERE key = ?",
                    &[Value::Int(now), Value::Text(key.to_string())],
                )?;
                Ok(1)
            }
            Some(_) => self.bump(key),
        }
    }

    /// Forget `key` (successful login).
    pub fn clear(&self, key: &str) -> Result<(), String> {
        self.backend
            .execute(
                "DELETE FROM auth_throttle WHERE key = ?",
                &[Value::Text(key.to_string())],
            )
            .map(|_| ())
    }

    fn bump(&self, key: &str) -> Result<i64, String> {
        self.backend.execute(
            "UPDATE auth_throttle SET attempts = attempts + 1 WHERE key = ?",
            &[Value::Text(key.to_string())],
        )?;
        Ok(self.row(key)?.map(|(a, _)| a).unwrap_or(1))
    }

    fn row(&self, key: &str) -> Result<Option<(i64, i64)>, String> {
        Ok(self
            .backend
            .query_one(
                "SELECT attempts, window_start FROM auth_throttle WHERE key = ?",
                &[Value::Text(key.to_string())],
            )?
            .and_then(|r| {
                let int_of = |k: &str| r.get(k).and_then(Json::as_f64).map(|f| f as i64);
                Some((int_of("attempts")?, int_of("window_start")?))
            }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sutegi_orm::db::Db;

    fn store(max: i64, per: i64) -> Throttle<Db> {
        let t = Throttle::new(Db::memory().unwrap()).max(max).per(per);
        t.migrate().unwrap();
        t
    }

    #[test]
    fn locks_after_max_and_reports_retry() {
        let t = store(3, 60);
        assert_eq!(t.too_many("k").unwrap(), None);
        assert_eq!(t.hit("k").unwrap(), 1);
        assert_eq!(t.hit("k").unwrap(), 2);
        assert_eq!(t.too_many("k").unwrap(), None); // 2 < 3
        assert_eq!(t.hit("k").unwrap(), 3);
        let retry = t.too_many("k").unwrap().unwrap();
        assert!(retry > 0 && retry <= 60, "retry_after = {retry}");
        // Other keys are unaffected.
        assert_eq!(t.too_many("other").unwrap(), None);
    }

    #[test]
    fn clear_reopens() {
        let t = store(1, 60);
        t.hit("k").unwrap();
        assert!(t.too_many("k").unwrap().is_some());
        t.clear("k").unwrap();
        assert_eq!(t.too_many("k").unwrap(), None);
    }

    #[test]
    fn elapsed_window_restarts() {
        let t = store(2, 60);
        t.hit("k").unwrap();
        t.hit("k").unwrap();
        assert!(t.too_many("k").unwrap().is_some());
        // Backdate the window past its length: the lock is over and the next
        // hit starts a fresh count.
        t.backend
            .execute("UPDATE auth_throttle SET window_start = 1", &[])
            .unwrap();
        assert_eq!(t.too_many("k").unwrap(), None);
        assert_eq!(t.hit("k").unwrap(), 1);
    }
}
