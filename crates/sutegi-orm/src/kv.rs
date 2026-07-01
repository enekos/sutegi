//! A small, opinionated **key/value store** over any [`Backend`] — the
//! lightweight companion to the relational query builder.
//!
//! Values are JSON, keys are namespaced (`ns` + `key`), and everything lives in
//! one table. It's for the state that doesn't want a schema: config, caches,
//! feature flags, sessions, scratch. Reads and writes are single-statement and
//! injection-safe.
//!
//! ## The opinion
//!
//! - **SQLite + `Kv` = the single-node default.** Embedded, zero-ops, one
//!   writer. Ideal for a one-instance app's config/cache/sessions/flags.
//! - **Postgres + relational `Model` = the multi-pod default.** A shared,
//!   durable source of truth across replicas.
//! - `Kv` **also runs on Postgres** — handy for small *shared* state (global
//!   feature flags, distributed config) without modeling a table. Use it when
//!   the data is genuinely key/value; reach for `Model` when it has structure
//!   worth querying.
//!
//! ```no_run
//! # #[cfg(feature = "sqlite")] {
//! use sutegi_orm::{db::Db, kv::Kv};
//! use sutegi_json::Json;
//!
//! let kv = Kv::new(Db::open("app.db").unwrap());
//! kv.migrate().unwrap();
//! kv.set("config", "theme", &Json::str("dark")).unwrap();
//! let theme = kv.get("config", "theme").unwrap(); // Some(Json::Str("dark"))
//! # let _ = theme;
//! # }
//! ```

use crate::backend::Backend;
use crate::value::Value;
use std::time::{SystemTime, UNIX_EPOCH};
use sutegi_json::Json;

/// A namespaced JSON key/value store backed by a single table on any
/// [`Backend`]. Cheap to construct; clone the backend to hold several views.
pub struct Kv<B: Backend> {
    backend: B,
    table: String,
}

impl<B: Backend> Kv<B> {
    /// A store over the default `kv` table.
    pub fn new(backend: B) -> Kv<B> {
        Kv {
            backend,
            table: "kv".to_string(),
        }
    }

    /// A store over a named table (e.g. a separate `sessions` or `cache` table).
    pub fn with_table(backend: B, table: &str) -> Kv<B> {
        Kv {
            backend,
            table: table.to_string(),
        }
    }

    /// The underlying backend, for mixing KV and relational access.
    pub fn backend(&self) -> &B {
        &self.backend
    }

    /// Create the backing table if it does not exist. Portable across SQLite
    /// and Postgres (composite primary key, no dialect-specific types).
    pub fn migrate(&self) -> Result<(), String> {
        let sql = format!(
            "CREATE TABLE IF NOT EXISTS {} (\n  \
             ns TEXT NOT NULL,\n  \
             key TEXT NOT NULL,\n  \
             value TEXT NOT NULL,\n  \
             updated_at BIGINT NOT NULL,\n  \
             PRIMARY KEY (ns, key)\n)",
            self.table
        );
        self.backend.execute(&sql, &[]).map(|_| ())
    }

    /// Set `key` in `ns` to `value` (insert or overwrite). Stores the JSON
    /// serialization; `updated_at` is stamped with the current epoch millis.
    pub fn set(&self, ns: &str, key: &str, value: &Json) -> Result<(), String> {
        let sql = format!(
            "INSERT INTO {} (ns, key, value, updated_at) VALUES (?, ?, ?, ?) \
             ON CONFLICT (ns, key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
            self.table
        );
        self.backend
            .execute(
                &sql,
                &[
                    Value::Text(ns.to_string()),
                    Value::Text(key.to_string()),
                    Value::Text(value.to_string()),
                    Value::Int(now_millis()),
                ],
            )
            .map(|_| ())
    }

    /// Get the value at `ns`/`key`, if present. Returns `Err` only on a store
    /// error or if the stored text isn't valid JSON.
    pub fn get(&self, ns: &str, key: &str) -> Result<Option<Json>, String> {
        let sql = format!("SELECT value FROM {} WHERE ns = ? AND key = ?", self.table);
        let row = self.backend.query_one(
            &sql,
            &[Value::Text(ns.to_string()), Value::Text(key.to_string())],
        )?;
        match row
            .as_ref()
            .and_then(|r| r.get("value"))
            .and_then(Json::as_str)
        {
            Some(s) => Json::parse(s).map(Some),
            None => Ok(None),
        }
    }

    /// Whether `ns`/`key` exists.
    pub fn contains(&self, ns: &str, key: &str) -> Result<bool, String> {
        Ok(self.get(ns, key)?.is_some())
    }

    /// Delete `ns`/`key`. Returns `true` if a value was removed.
    pub fn delete(&self, ns: &str, key: &str) -> Result<bool, String> {
        let sql = format!("DELETE FROM {} WHERE ns = ? AND key = ?", self.table);
        Ok(self.backend.execute(
            &sql,
            &[Value::Text(ns.to_string()), Value::Text(key.to_string())],
        )? > 0)
    }

    /// The keys in `ns`, sorted.
    pub fn keys(&self, ns: &str) -> Result<Vec<String>, String> {
        let sql = format!(
            "SELECT key FROM {} WHERE ns = ? ORDER BY key ASC",
            self.table
        );
        let rows = self.backend.query(&sql, &[Value::Text(ns.to_string())])?;
        Ok(rows
            .iter()
            .filter_map(|r| r.get("key").and_then(Json::as_str).map(String::from))
            .collect())
    }

    /// Every `(key, value)` pair in `ns`, sorted by key.
    pub fn scan(&self, ns: &str) -> Result<Vec<(String, Json)>, String> {
        let sql = format!(
            "SELECT key, value FROM {} WHERE ns = ? ORDER BY key ASC",
            self.table
        );
        self.decode_pairs(self.backend.query(&sql, &[Value::Text(ns.to_string())])?)
    }

    /// Every `(key, value)` pair in `ns` whose key starts with `prefix`.
    pub fn scan_prefix(&self, ns: &str, prefix: &str) -> Result<Vec<(String, Json)>, String> {
        let sql = format!(
            "SELECT key, value FROM {} WHERE ns = ? AND key LIKE ? ORDER BY key ASC",
            self.table
        );
        self.decode_pairs(self.backend.query(
            &sql,
            &[
                Value::Text(ns.to_string()),
                Value::Text(format!("{}%", prefix)),
            ],
        )?)
    }

    /// Number of keys in `ns`.
    pub fn count(&self, ns: &str) -> Result<i64, String> {
        let sql = format!("SELECT COUNT(*) AS count FROM {} WHERE ns = ?", self.table);
        Ok(self
            .backend
            .query_one(&sql, &[Value::Text(ns.to_string())])?
            .and_then(|r| r.get("count").and_then(Json::as_f64))
            .map(|f| f as i64)
            .unwrap_or(0))
    }

    /// Delete every key in `ns`. Returns the number removed.
    pub fn clear(&self, ns: &str) -> Result<usize, String> {
        let sql = format!("DELETE FROM {} WHERE ns = ?", self.table);
        self.backend.execute(&sql, &[Value::Text(ns.to_string())])
    }

    fn decode_pairs(&self, rows: Vec<Json>) -> Result<Vec<(String, Json)>, String> {
        rows.iter()
            .map(|r| {
                let key = r
                    .get("key")
                    .and_then(Json::as_str)
                    .ok_or("row missing key")?
                    .to_string();
                let raw = r
                    .get("value")
                    .and_then(Json::as_str)
                    .ok_or("row missing value")?;
                Ok((key, Json::parse(raw)?))
            })
            .collect()
    }
}

fn now_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use super::*;
    use crate::db::Db;

    fn kv() -> Kv<Db> {
        let kv = Kv::new(Db::memory().unwrap());
        kv.migrate().unwrap();
        kv
    }

    #[test]
    fn set_get_overwrite_delete() {
        let kv = kv();
        assert_eq!(kv.get("cfg", "theme").unwrap(), None);
        kv.set("cfg", "theme", &Json::str("dark")).unwrap();
        assert_eq!(kv.get("cfg", "theme").unwrap(), Some(Json::str("dark")));
        assert!(kv.contains("cfg", "theme").unwrap());

        // Overwrite in place — no duplicate row.
        kv.set("cfg", "theme", &Json::str("light")).unwrap();
        assert_eq!(kv.get("cfg", "theme").unwrap(), Some(Json::str("light")));
        assert_eq!(kv.count("cfg").unwrap(), 1);

        assert!(kv.delete("cfg", "theme").unwrap());
        assert!(!kv.delete("cfg", "theme").unwrap());
        assert_eq!(kv.get("cfg", "theme").unwrap(), None);
    }

    #[test]
    fn structured_values_roundtrip() {
        let kv = kv();
        let val = Json::obj(vec![
            ("count", Json::int(3)),
            ("tags", Json::arr(vec![Json::str("a"), Json::str("b")])),
        ]);
        kv.set("state", "session:42", &val).unwrap();
        assert_eq!(kv.get("state", "session:42").unwrap(), Some(val));
    }

    #[test]
    fn namespaces_keys_scan_and_prefix() {
        let kv = kv();
        kv.set("a", "one", &Json::int(1)).unwrap();
        kv.set("a", "two", &Json::int(2)).unwrap();
        kv.set("a", "twenty", &Json::int(20)).unwrap();
        kv.set("b", "other", &Json::int(9)).unwrap();

        assert_eq!(kv.keys("a").unwrap(), vec!["one", "twenty", "two"]);
        assert_eq!(kv.count("a").unwrap(), 3);
        assert_eq!(kv.count("b").unwrap(), 1);

        let tw = kv.scan_prefix("a", "tw").unwrap();
        assert_eq!(tw.len(), 2);
        assert_eq!(tw[0], ("twenty".to_string(), Json::int(20)));
        assert_eq!(tw[1], ("two".to_string(), Json::int(2)));

        // Clearing one namespace leaves the other intact.
        assert_eq!(kv.clear("a").unwrap(), 3);
        assert_eq!(kv.count("a").unwrap(), 0);
        assert_eq!(kv.count("b").unwrap(), 1);
    }

    #[test]
    fn separate_tables_are_isolated() {
        let db = Db::memory().unwrap();
        // Two logical stores backed by different tables.
        let cache = Kv::with_table(db, "cache");
        cache.migrate().unwrap();
        cache.set("http", "GET /x", &Json::str("hit")).unwrap();
        assert_eq!(cache.get("http", "GET /x").unwrap(), Some(Json::str("hit")));
    }
}
