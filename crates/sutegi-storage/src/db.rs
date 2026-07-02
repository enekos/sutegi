//! Blobs in a database table, over any ORM [`Backend`] — SQLite or Postgres.
//!
//! The point is the **Postgres shape**: for a multi-pod app whose objects are
//! small (uploads, avatars, generated reports), the database you already run
//! is a shared, durable, transactional blob store — **zero new
//! infrastructure**. On SQLite it gives a one-file bundle of app + files.
//!
//! Bytes are stored Base64-encoded in a portable `TEXT` column (the ORM's
//! `Value` speaks text, and it keeps one schema across both backends), so
//! storage costs ~4/3× the object size. That, plus the row round-trip, is why
//! the honest ceiling is **roughly a few MB per object** — past that, use
//! real object storage ([`crate::S3Store`]).

use crate::{now_secs, validate_key, ObjectMeta, Storage};
use sutegi_crypto::{base64_decode, base64_encode};
use sutegi_json::Json;
use sutegi_orm::{Backend, Value};

/// Database-backed [`Storage`] over any ORM [`Backend`]. One table, portable
/// SQL, works inside the same database (and transactions) as your models.
pub struct DbStorage<B: Backend> {
    backend: B,
    table: String,
}

impl<B: Backend> DbStorage<B> {
    /// A store over the default `storage` table.
    pub fn new(backend: B) -> DbStorage<B> {
        DbStorage {
            backend,
            table: "storage".to_string(),
        }
    }

    /// A store over a named table (e.g. separate `uploads` and `exports`).
    pub fn with_table(backend: B, table: &str) -> DbStorage<B> {
        DbStorage {
            backend,
            table: table.to_string(),
        }
    }

    /// The underlying backend, for mixing blob and relational access.
    pub fn backend(&self) -> &B {
        &self.backend
    }

    /// Create the backing table if it does not exist. Portable across SQLite
    /// and Postgres.
    pub fn migrate(&self) -> Result<(), String> {
        let sql = format!(
            "CREATE TABLE IF NOT EXISTS {} (\n  \
             key TEXT NOT NULL,\n  \
             content_type TEXT NOT NULL,\n  \
             size BIGINT NOT NULL,\n  \
             data TEXT NOT NULL,\n  \
             updated_at BIGINT NOT NULL,\n  \
             PRIMARY KEY (key)\n)",
            self.table
        );
        self.backend.execute(&sql, &[]).map(|_| ())
    }

    fn meta_of(row: &Json) -> Result<ObjectMeta, String> {
        Ok(ObjectMeta {
            key: row
                .get("key")
                .and_then(Json::as_str)
                .ok_or("row missing key")?
                .to_string(),
            size: row
                .get("size")
                .and_then(Json::as_f64)
                .map(|f| f as u64)
                .unwrap_or(0),
            content_type: row
                .get("content_type")
                .and_then(Json::as_str)
                .unwrap_or("application/octet-stream")
                .to_string(),
            modified: row
                .get("updated_at")
                .and_then(Json::as_f64)
                .map(|f| f as i64)
                .unwrap_or(0),
        })
    }
}

impl<B: Backend> Storage for DbStorage<B> {
    fn put(&self, key: &str, bytes: &[u8], content_type: &str) -> Result<(), String> {
        validate_key(key)?;
        let ct = if content_type.is_empty() {
            crate::content_type_of(key)
        } else {
            content_type
        };
        let sql = format!(
            "INSERT INTO {} (key, content_type, size, data, updated_at) VALUES (?, ?, ?, ?, ?) \
             ON CONFLICT (key) DO UPDATE SET content_type = excluded.content_type, \
             size = excluded.size, data = excluded.data, updated_at = excluded.updated_at",
            self.table
        );
        self.backend
            .execute(
                &sql,
                &[
                    Value::Text(key.to_string()),
                    Value::Text(ct.to_string()),
                    Value::Int(bytes.len() as i64),
                    Value::Text(base64_encode(bytes)),
                    Value::Int(now_secs()),
                ],
            )
            .map(|_| ())
    }

    fn get(&self, key: &str) -> Result<Option<Vec<u8>>, String> {
        validate_key(key)?;
        let sql = format!("SELECT data FROM {} WHERE key = ?", self.table);
        let row = self
            .backend
            .query_one(&sql, &[Value::Text(key.to_string())])?;
        match row
            .as_ref()
            .and_then(|r| r.get("data"))
            .and_then(Json::as_str)
        {
            Some(b64) => base64_decode(b64).map(Some),
            None => Ok(None),
        }
    }

    fn stat(&self, key: &str) -> Result<Option<ObjectMeta>, String> {
        validate_key(key)?;
        let sql = format!(
            "SELECT key, content_type, size, updated_at FROM {} WHERE key = ?",
            self.table
        );
        match self
            .backend
            .query_one(&sql, &[Value::Text(key.to_string())])?
        {
            Some(row) => Ok(Some(Self::meta_of(&row)?)),
            None => Ok(None),
        }
    }

    fn delete(&self, key: &str) -> Result<bool, String> {
        validate_key(key)?;
        let sql = format!("DELETE FROM {} WHERE key = ?", self.table);
        Ok(self
            .backend
            .execute(&sql, &[Value::Text(key.to_string())])?
            > 0)
    }

    fn list(&self, prefix: &str) -> Result<Vec<ObjectMeta>, String> {
        // LIKE-escape the prefix so `%`/`_` in keys match literally.
        let escaped = prefix
            .replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_");
        let sql = format!(
            "SELECT key, content_type, size, updated_at FROM {} \
             WHERE key LIKE ? ESCAPE '\\' ORDER BY key ASC",
            self.table
        );
        self.backend
            .query(&sql, &[Value::Text(format!("{escaped}%"))])?
            .iter()
            .map(Self::meta_of)
            .collect()
    }
}

// Backed by SQLite in unit tests; the Postgres path is covered by the
// env-gated integration test in `tests/pg_storage.rs` (same pattern as the
// driver and queue suites).
#[cfg(test)]
mod tests {
    use super::*;
    use sutegi_orm::db::Db;

    fn store() -> DbStorage<Db> {
        let s = DbStorage::new(Db::memory().expect("open sqlite"));
        s.migrate().unwrap();
        s
    }

    #[test]
    fn roundtrip_binary() {
        let s = store();
        let blob: Vec<u8> = (0..=255u8).collect();
        s.put("bin/all-bytes", &blob, "application/octet-stream")
            .unwrap();
        assert_eq!(s.get("bin/all-bytes").unwrap().unwrap(), blob);
        let meta = s.stat("bin/all-bytes").unwrap().unwrap();
        assert_eq!(meta.size, 256);
        assert!(meta.modified > 0);
    }

    #[test]
    fn overwrite_delete_exists() {
        let s = store();
        s.put("a.txt", b"one", "text/plain").unwrap();
        s.put("a.txt", b"two!", "text/x-two").unwrap();
        assert_eq!(s.get("a.txt").unwrap().unwrap(), b"two!");
        assert_eq!(s.stat("a.txt").unwrap().unwrap().content_type, "text/x-two");
        assert!(s.exists("a.txt").unwrap());
        assert!(s.delete("a.txt").unwrap());
        assert!(!s.delete("a.txt").unwrap());
        assert_eq!(s.get("a.txt").unwrap(), None);
    }

    #[test]
    fn list_prefix_and_like_escaping() {
        let s = store();
        s.put("logs/a.txt", b"a", "").unwrap();
        s.put("logs/b.txt", b"b", "").unwrap();
        s.put("logs_x/evil.txt", b"e", "").unwrap(); // `_` must not wildcard
        let keys: Vec<String> = s
            .list("logs/")
            .unwrap()
            .into_iter()
            .map(|m| m.key)
            .collect();
        assert_eq!(keys, vec!["logs/a.txt", "logs/b.txt"]);
        assert_eq!(s.list("").unwrap().len(), 3);
    }

    #[test]
    fn empty_content_type_guesses() {
        let s = store();
        s.put("page.html", b"<hi>", "").unwrap();
        assert_eq!(
            s.stat("page.html").unwrap().unwrap().content_type,
            "text/html; charset=utf-8"
        );
    }

    #[test]
    fn invalid_keys_rejected() {
        let s = store();
        assert!(s.put("../up", b"x", "").is_err());
        assert!(s.get("").is_err());
    }
}
