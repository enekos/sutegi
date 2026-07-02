//! Local-filesystem storage — the **single-node default**. One directory on
//! disk, keys map to relative paths, writes are atomic (temp file + rename).
//!
//! Content types are recorded in a `.sutegi-meta/` mirror tree next to the
//! objects (one tiny sidecar file per object) so [`Storage::stat`] returns
//! exactly what [`Storage::put`] was told, matching the database backend.
//! Files that appear on disk out-of-band (no sidecar) fall back to an
//! extension-derived guess.

use crate::{content_type_of, validate_key, ObjectMeta, Storage};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

const META_DIR: &str = ".sutegi-meta";
const TMP_SUFFIX: &str = ".sutegi-tmp";

/// Filesystem-backed [`Storage`] rooted at a directory.
///
/// Cheap to clone (a `PathBuf`); `Send + Sync`, so it drops straight into
/// `App::state`.
#[derive(Clone, Debug)]
pub struct FsStorage {
    root: PathBuf,
}

impl FsStorage {
    /// Open (creating if needed) a store rooted at `root`.
    pub fn new(root: impl Into<PathBuf>) -> Result<FsStorage, String> {
        let root = root.into();
        fs::create_dir_all(&root).map_err(|e| format!("create {}: {e}", root.display()))?;
        Ok(FsStorage { root })
    }

    /// The root directory.
    pub fn root(&self) -> &Path {
        &self.root
    }

    fn object_path(&self, key: &str) -> Result<PathBuf, String> {
        validate_key(key)?;
        Ok(self.root.join(key))
    }

    fn meta_path(&self, key: &str) -> PathBuf {
        self.root.join(META_DIR).join(key)
    }

    fn stored_content_type(&self, key: &str) -> String {
        match fs::read_to_string(self.meta_path(key)) {
            Ok(ct) if !ct.trim().is_empty() => ct.trim().to_string(),
            _ => content_type_of(key).to_string(),
        }
    }

    fn meta_of(&self, key: &str, md: &fs::Metadata) -> ObjectMeta {
        let modified = md
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        ObjectMeta {
            key: key.to_string(),
            size: md.len(),
            content_type: self.stored_content_type(key),
            modified,
        }
    }
}

impl Storage for FsStorage {
    fn put(&self, key: &str, bytes: &[u8], content_type: &str) -> Result<(), String> {
        let path = self.object_path(key)?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| format!("create {}: {e}", parent.display()))?;
        }
        // Atomic on the same filesystem: write a sibling temp file, then
        // rename over the target. Readers see the old bytes or the new ones,
        // never a torn write.
        let tmp = path.with_file_name(format!(
            "{}.{}{}",
            path.file_name().and_then(|n| n.to_str()).unwrap_or("obj"),
            std::process::id(),
            TMP_SUFFIX
        ));
        fs::write(&tmp, bytes).map_err(|e| format!("write {}: {e}", tmp.display()))?;
        fs::rename(&tmp, &path).map_err(|e| {
            let _ = fs::remove_file(&tmp);
            format!("rename to {}: {e}", path.display())
        })?;

        let meta = self.meta_path(key);
        if let Some(parent) = meta.parent() {
            fs::create_dir_all(parent).map_err(|e| format!("create {}: {e}", parent.display()))?;
        }
        let ct = if content_type.is_empty() {
            content_type_of(key)
        } else {
            content_type
        };
        fs::write(&meta, ct).map_err(|e| format!("write {}: {e}", meta.display()))
    }

    fn get(&self, key: &str) -> Result<Option<Vec<u8>>, String> {
        let path = self.object_path(key)?;
        match fs::read(&path) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(format!("read {}: {e}", path.display())),
        }
    }

    fn stat(&self, key: &str) -> Result<Option<ObjectMeta>, String> {
        let path = self.object_path(key)?;
        match fs::metadata(&path) {
            Ok(md) if md.is_file() => Ok(Some(self.meta_of(key, &md))),
            Ok(_) => Ok(None),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(format!("stat {}: {e}", path.display())),
        }
    }

    fn delete(&self, key: &str) -> Result<bool, String> {
        let path = self.object_path(key)?;
        match fs::remove_file(&path) {
            Ok(()) => {
                let _ = fs::remove_file(self.meta_path(key));
                Ok(true)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(format!("delete {}: {e}", path.display())),
        }
    }

    fn list(&self, prefix: &str) -> Result<Vec<ObjectMeta>, String> {
        let mut out = Vec::new();
        let mut stack = vec![self.root.clone()];
        while let Some(dir) = stack.pop() {
            let entries = match fs::read_dir(&dir) {
                Ok(e) => e,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => return Err(format!("read {}: {e}", dir.display())),
            };
            for entry in entries {
                let entry = entry.map_err(|e| format!("read {}: {e}", dir.display()))?;
                let path = entry.path();
                let name = entry.file_name().to_string_lossy().into_owned();
                if path.is_dir() {
                    if !(dir == self.root && name == META_DIR) {
                        stack.push(path);
                    }
                    continue;
                }
                if name.ends_with(TMP_SUFFIX) {
                    continue; // in-flight write
                }
                let key = match path.strip_prefix(&self.root) {
                    Ok(rel) => rel
                        .components()
                        .map(|c| c.as_os_str().to_string_lossy().into_owned())
                        .collect::<Vec<_>>()
                        .join("/"),
                    Err(_) => continue,
                };
                if !key.starts_with(prefix) {
                    continue;
                }
                let md = entry
                    .metadata()
                    .map_err(|e| format!("stat {}: {e}", path.display()))?;
                out.push(self.meta_of(&key, &md));
            }
        }
        out.sort_by(|a, b| a.key.cmp(&b.key));
        Ok(out)
    }

    fn get_reader(&self, key: &str) -> Result<Option<Box<dyn Read + Send>>, String> {
        let path = self.object_path(key)?;
        match fs::File::open(&path) {
            Ok(f) => Ok(Some(Box::new(f))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(format!("open {}: {e}", path.display())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_store() -> FsStorage {
        // Parallel test threads can share a clock tick — an atomic counter
        // keeps every test in its own directory.
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "sutegi-storage-{}-{seq}-{nanos}",
            std::process::id()
        ));
        FsStorage::new(dir).unwrap()
    }

    #[test]
    fn roundtrip_and_stat() {
        let s = temp_store();
        s.put("a/b/hello.txt", b"hi there", "text/plain").unwrap();
        assert_eq!(s.get("a/b/hello.txt").unwrap().unwrap(), b"hi there");
        let meta = s.stat("a/b/hello.txt").unwrap().unwrap();
        assert_eq!(meta.size, 8);
        assert_eq!(meta.content_type, "text/plain");
        assert!(meta.modified > 0);
        assert!(s.exists("a/b/hello.txt").unwrap());
        assert!(!s.exists("a/b/missing.txt").unwrap());
        let _ = fs::remove_dir_all(s.root());
    }

    #[test]
    fn overwrite_and_delete() {
        let s = temp_store();
        s.put("x.bin", b"one", "").unwrap();
        s.put("x.bin", b"two!", "application/x-two").unwrap();
        assert_eq!(s.get("x.bin").unwrap().unwrap(), b"two!");
        assert_eq!(
            s.stat("x.bin").unwrap().unwrap().content_type,
            "application/x-two"
        );
        assert!(s.delete("x.bin").unwrap());
        assert!(!s.delete("x.bin").unwrap());
        assert_eq!(s.get("x.bin").unwrap(), None);
        let _ = fs::remove_dir_all(s.root());
    }

    #[test]
    fn list_prefix_sorted_and_meta_hidden() {
        let s = temp_store();
        s.put("logs/b.txt", b"b", "text/plain").unwrap();
        s.put("logs/a.txt", b"a", "text/plain").unwrap();
        s.put("other/c.txt", b"c", "text/plain").unwrap();
        let logs = s.list("logs/").unwrap();
        assert_eq!(
            logs.iter().map(|m| m.key.as_str()).collect::<Vec<_>>(),
            vec!["logs/a.txt", "logs/b.txt"]
        );
        let all = s.list("").unwrap();
        assert_eq!(all.len(), 3);
        assert!(all.iter().all(|m| !m.key.contains(".sutegi-")));
        let _ = fs::remove_dir_all(s.root());
    }

    #[test]
    fn traversal_rejected() {
        let s = temp_store();
        assert!(s.put("../escape.txt", b"x", "").is_err());
        assert!(s.get("a/../../b").is_err());
        let _ = fs::remove_dir_all(s.root());
    }

    #[test]
    fn reader_streams() {
        let s = temp_store();
        s.put("r.txt", b"stream me", "text/plain").unwrap();
        let mut buf = Vec::new();
        s.get_reader("r.txt")
            .unwrap()
            .unwrap()
            .read_to_end(&mut buf)
            .unwrap();
        assert_eq!(buf, b"stream me");
        assert!(s.get_reader("missing").unwrap().is_none());
        let _ = fs::remove_dir_all(s.root());
    }

    #[test]
    fn sidecar_fallback_guesses_from_extension() {
        let s = temp_store();
        // A file dropped in out-of-band, no sidecar.
        fs::write(s.root().join("raw.json"), b"{}").unwrap();
        assert_eq!(
            s.stat("raw.json").unwrap().unwrap().content_type,
            "application/json"
        );
        let _ = fs::remove_dir_all(s.root());
    }
}
