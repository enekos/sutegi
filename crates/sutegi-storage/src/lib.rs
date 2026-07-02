//! File/object storage behind one trait — the storage counterpart to the ORM's
//! `Backend` seam: **swap the backend, not the call sites.**
//!
//! - [`FsStorage`] — local filesystem. The **single-node default**: zero-ops,
//!   zero deps, real streaming reads. The SQLite of file storage.
//! - [`DbStorage`] (feature `db`) — blobs in a database table over any ORM
//!   `Backend`. On Postgres this is **multi-pod file storage with zero new
//!   infrastructure** — the database you already run is the blob store. Good
//!   to roughly a few MB per object; past that, reach for real object storage.
//! - [`S3Store`] — a pure-`std` **SigV4 presigner** for S3-compatible object
//!   stores (AWS, R2, MinIO, …). It mints time-limited GET/PUT/DELETE URLs;
//!   the bytes flow **directly between the client (or agent) and S3**, never
//!   through sutegi. That is why it needs no HTTP client and no TLS — signing
//!   is HMAC-SHA256, reused from the Postgres driver's SCRAM implementation.
//!
//! `S3Store` deliberately does **not** implement [`Storage`]: handing out a
//! URL is a different contract than moving bytes. When a full S3 client lands
//! (blocked on TLS), it will join the trait; until then the swap seam spans
//! fs ↔ db, and S3 is the presign-only escape hatch.
//!
//! Keys are `/`-separated paths (`avatars/42.png`), validated identically
//! across backends — see [`validate_key`].
//!
//! ```no_run
//! use sutegi_storage::{FsStorage, Storage};
//!
//! let store = FsStorage::new("data/files").unwrap();
//! store.put("hello.txt", b"hi", "text/plain").unwrap();
//! let bytes = store.get("hello.txt").unwrap(); // Some(vec![...])
//! # let _ = bytes;
//! ```

use std::io::Read;
use sutegi_json::Json;

pub mod fs;
pub mod s3;
pub use fs::FsStorage;
pub use s3::S3Store;

#[cfg(feature = "db")]
pub mod db;
#[cfg(feature = "db")]
pub use db::DbStorage;

/// What a backend knows about a stored object.
#[derive(Clone, Debug, PartialEq)]
pub struct ObjectMeta {
    /// The full key, e.g. `avatars/42.png`.
    pub key: String,
    /// Size in bytes.
    pub size: u64,
    /// MIME type recorded at `put` time (or derived from the extension for
    /// files that appeared on disk out-of-band). `application/octet-stream`
    /// when unknown.
    pub content_type: String,
    /// Last-modified time, unix seconds.
    pub modified: i64,
}

impl ObjectMeta {
    /// The machine-readable shape, for handlers and agent tools.
    pub fn to_json(&self) -> Json {
        Json::obj(vec![
            ("key", Json::str(self.key.clone())),
            ("size", Json::num(self.size as f64)),
            ("content_type", Json::str(self.content_type.clone())),
            ("modified", Json::num(self.modified as f64)),
        ])
    }
}

/// A byte-level object store. Implemented by [`FsStorage`] and [`DbStorage`];
/// app code holds `impl Storage` (or a concrete type) and swaps backends by
/// changing the type it constructs, not the call sites.
pub trait Storage {
    /// Store `bytes` at `key` (create or overwrite), recording `content_type`.
    fn put(&self, key: &str, bytes: &[u8], content_type: &str) -> Result<(), String>;

    /// The bytes at `key`, or `None` if absent.
    fn get(&self, key: &str) -> Result<Option<Vec<u8>>, String>;

    /// Metadata for `key`, or `None` if absent.
    fn stat(&self, key: &str) -> Result<Option<ObjectMeta>, String>;

    /// Remove `key`. Returns `true` if an object was removed.
    fn delete(&self, key: &str) -> Result<bool, String>;

    /// Metadata for every object whose key starts with `prefix`, sorted by
    /// key. An empty prefix lists everything.
    fn list(&self, prefix: &str) -> Result<Vec<ObjectMeta>, String>;

    /// Whether `key` exists.
    fn exists(&self, key: &str) -> Result<bool, String> {
        Ok(self.stat(key)?.is_some())
    }

    /// A reader over the bytes at `key`, for streaming responses
    /// (`web::stream`). The default buffers via [`get`](Storage::get);
    /// [`FsStorage`] overrides it with a real file handle.
    fn get_reader(&self, key: &str) -> Result<Option<Box<dyn Read + Send>>, String> {
        Ok(self
            .get(key)?
            .map(|b| Box::new(std::io::Cursor::new(b)) as Box<dyn Read + Send>))
    }
}

/// Validate an object key. Enforced identically by every backend so a key
/// valid on one is valid on all: non-empty, at most 1024 bytes, `/`-separated
/// non-empty segments, no `.`/`..` segments (path traversal), no backslashes
/// or control characters, and no segment starting with `.sutegi-` (reserved
/// for backend internals such as [`FsStorage`]'s metadata mirror).
pub fn validate_key(key: &str) -> Result<(), String> {
    if key.is_empty() {
        return Err("storage key is empty".to_string());
    }
    if key.len() > 1024 {
        return Err(format!("storage key too long ({} > 1024 bytes)", key.len()));
    }
    if key.contains('\\') {
        return Err(format!("storage key contains a backslash: {key}"));
    }
    if key.chars().any(|c| c.is_control()) {
        return Err("storage key contains control characters".to_string());
    }
    if key.starts_with('/') || key.ends_with('/') {
        return Err(format!("storage key must not start or end with '/': {key}"));
    }
    for seg in key.split('/') {
        if seg.is_empty() {
            return Err(format!("storage key has an empty segment: {key}"));
        }
        if seg == "." || seg == ".." {
            return Err(format!("storage key has a path-traversal segment: {key}"));
        }
        if seg.starts_with(".sutegi-") {
            return Err(format!(
                "storage key segment uses the reserved '.sutegi-' prefix: {key}"
            ));
        }
    }
    Ok(())
}

/// A best-effort MIME type from a key's extension;
/// `application/octet-stream` when unknown.
pub fn content_type_of(key: &str) -> &'static str {
    let ext = key.rsplit('.').next().unwrap_or("");
    match ext {
        "html" | "htm" => "text/html; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "json" => "application/json",
        "txt" | "md" => "text/plain; charset=utf-8",
        "csv" => "text/csv; charset=utf-8",
        "xml" => "application/xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "ico" => "image/x-icon",
        "pdf" => "application/pdf",
        "wasm" => "application/wasm",
        "mp3" => "audio/mpeg",
        "mp4" => "video/mp4",
        "zip" => "application/zip",
        "gz" => "application/gzip",
        _ => "application/octet-stream",
    }
}

#[cfg(feature = "db")]
pub(crate) fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_valid() {
        for k in [
            "a",
            "a/b/c.txt",
            "avatars/42.png",
            "with space.txt",
            "pct%us_x",
        ] {
            assert!(validate_key(k).is_ok(), "{k} should be valid");
        }
    }

    #[test]
    fn keys_invalid() {
        for k in [
            "",
            "/lead",
            "trail/",
            "a//b",
            "../etc/passwd",
            "a/../b",
            "a/./b",
            "a\\b",
            "a\nb",
            ".sutegi-meta/x",
            "a/.sutegi-tmp",
        ] {
            assert!(validate_key(k).is_err(), "{k:?} should be rejected");
        }
        assert!(validate_key(&"x".repeat(1025)).is_err());
    }

    #[test]
    fn content_types() {
        assert_eq!(content_type_of("a/b.png"), "image/png");
        assert_eq!(content_type_of("x.json"), "application/json");
        assert_eq!(content_type_of("noext"), "application/octet-stream");
    }
}
