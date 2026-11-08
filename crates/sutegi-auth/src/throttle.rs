//! **Login throttling** — Laravel's `ThrottlesLogins`, DB-backed so every pod
//! counts the same attempts. Fixed window: `max` attempts per `per` seconds
//! per key, where the key is whatever you rate.
//!
//! Gate with [`attempt`](Throttle::attempt), which counts the attempt and
//! decides in **one atomic statement** — a separate `too_many` check followed
//! by a later `hit` lets N concurrent requests all pass the check before any
//! of them counts. Rate two keys for login: the email+IP pair, and the email
//! alone (so rotating IPs cannot buy more guesses against one account). Build
//! the email part with [`login_key`] so `A@x.co ` and `a@x.co` share a
//! counter.
//!
//! ```ignore
//! let throttle = Throttle::new(db.clone()); // 5 attempts / 60 s
//! let keys = [login_key(&email, Some(&ip)), login_key(&email, None)];
//! for key in &keys {
//!     if let Some(retry) = throttle.attempt(key)? {
//!         return Err(Error::too_many_requests(format!("retry in {retry}s")));
//!     }
//! }
//! match auth.users.authenticate(&email, &pw)? {
//!     Some(user) => { for k in &keys { throttle.clear(k)?; } /* login */ }
//!     None => { /* 401 — the attempt is already counted */ }
//! }
//! ```

use crate::users::{normalize_email, now_secs};
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

    /// Count one attempt against `key` and decide, atomically: `Ok(None)`
    /// admits it, `Ok(Some(secs))` rejects it (more than `max` attempts in
    /// the window; `secs` until it reopens). Increment and check are one
    /// `INSERT … ON CONFLICT DO UPDATE … RETURNING` statement, so concurrent
    /// attempts — across threads or pods — can never all slip under the
    /// limit. Every attempt counts, successful or not, until
    /// [`clear`](Throttle::clear) — call that on success.
    pub fn attempt(&self, key: &str) -> Result<Option<i64>, String> {
        let now = now_secs();
        let (attempts, window_start) = self.count(key, now)?;
        if attempts > self.max {
            Ok(Some((window_start + self.per - now).max(1)))
        } else {
            Ok(None)
        }
    }

    /// Whether `key` is locked out right now; `Some(secs)` says how long
    /// until the window reopens. Read-only and **not** a gate on its own:
    /// checking here and counting later races under concurrency — gate with
    /// [`attempt`](Throttle::attempt) instead.
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

    /// Record an attempt; returns the attempt count in the current window.
    /// Atomic (the same single statement as [`attempt`](Throttle::attempt)),
    /// so concurrent hits across pods all count.
    pub fn hit(&self, key: &str) -> Result<i64, String> {
        self.count(key, now_secs()).map(|(attempts, _)| attempts)
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

    /// Increment `key`'s counter — restarting it if the window elapsed —
    /// and return `(attempts, window_start)` after the increment, in one
    /// statement. `SET` expressions all read the pre-update row.
    fn count(&self, key: &str, now: i64) -> Result<(i64, i64), String> {
        let row = self.backend.query_one(
            "INSERT INTO auth_throttle (key, attempts, window_start) VALUES (?, 1, ?) \
             ON CONFLICT (key) DO UPDATE SET \
             attempts = CASE WHEN auth_throttle.window_start + ? <= ? \
                        THEN 1 ELSE auth_throttle.attempts + 1 END, \
             window_start = CASE WHEN auth_throttle.window_start + ? <= ? \
                            THEN ? ELSE auth_throttle.window_start END \
             RETURNING attempts, window_start",
            &[
                Value::Text(key.to_string()),
                Value::Int(now),
                Value::Int(self.per),
                Value::Int(now),
                Value::Int(self.per),
                Value::Int(now),
                Value::Int(now),
            ],
        )?;
        row.as_ref()
            .and_then(|r| {
                let int_of = |k: &str| r.get(k).and_then(Json::as_f64).map(|f| f as i64);
                Some((int_of("attempts")?, int_of("window_start")?))
            })
            .ok_or_else(|| "auth_throttle upsert returned no row".to_string())
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

/// The throttle key for a login attempt on `email`: normalized exactly as
/// [`Users`](crate::Users) stores it (trimmed, lowercased), so case or
/// whitespace variants of one address share a counter. `ip: Some(..)` gives
/// the per-client `login:<email>|<ip>` key; `None` the per-account
/// `login:<email>` key, which rotating IPs cannot dodge.
pub fn login_key(email: &str, ip: Option<&str>) -> String {
    // A malformed address can't log in anyway; trim+lowercase it the same
    // way so its variants still collapse onto one key.
    let email = normalize_email(email).unwrap_or_else(|_| email.trim().to_lowercase());
    match ip {
        Some(ip) => format!("login:{email}|{ip}"),
        None => format!("login:{email}"),
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
    fn attempt_admits_exactly_max_then_locks() {
        let t = store(5, 60);
        let admitted = (0..20)
            .filter(|_| t.attempt("k").unwrap().is_none())
            .count();
        assert_eq!(admitted, 5);
        let retry = t.attempt("k").unwrap().unwrap();
        assert!(retry > 0 && retry <= 60, "retry_after = {retry}");
        t.clear("k").unwrap();
        assert_eq!(t.attempt("k").unwrap(), None);
    }

    #[test]
    fn concurrent_attempts_admit_only_max() {
        let dir = std::env::temp_dir().join(format!(
            "sutegi-throttle-{}-{}",
            std::process::id(),
            sutegi_crypto::now_millis()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.db");
        let t = Throttle::new(Db::open(path.to_str().unwrap()).unwrap()).max(5);
        t.migrate().unwrap();
        let admitted = std::sync::atomic::AtomicUsize::new(0);
        std::thread::scope(|s| {
            for _ in 0..20 {
                s.spawn(|| {
                    if t.attempt("login:a@b.co").unwrap().is_none() {
                        admitted.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    }
                });
            }
        });
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(admitted.into_inner(), 5);
    }

    #[test]
    fn attempt_window_restarts() {
        let t = store(1, 60);
        assert_eq!(t.attempt("k").unwrap(), None);
        assert!(t.attempt("k").unwrap().is_some());
        t.backend
            .execute("UPDATE auth_throttle SET window_start = 1", &[])
            .unwrap();
        assert_eq!(t.attempt("k").unwrap(), None);
    }

    #[test]
    fn login_key_normalizes_email() {
        assert_eq!(
            login_key("  Eneko@Example.COM ", Some("1.2.3.4")),
            login_key("eneko@example.com", Some("1.2.3.4"))
        );
        assert_eq!(login_key("A@B.co", None), "login:a@b.co");
        assert_ne!(login_key("a@b.co", None), login_key("a@b.co", Some("ip")));
        // Malformed input still collapses case/whitespace variants.
        assert_eq!(login_key(" NOPE ", None), login_key("nope", None));
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
