//! Local-filesystem storage — the **single-node default**. One directory on
//! disk, keys map to relative paths, writes are atomic (temp file + rename).
//!
//! Content types are recorded in a `.sutegi-meta/` mirror tree next to the
//! objects (one tiny sidecar file per object) so [`Storage::stat`] returns
//! exactly what [`Storage::put`] was told, matching the database backend.
//! Files that appear on disk out-of-band (no sidecar) fall back to an
//! extension-derived guess.

use crate::{ObjectMeta, OpenObject, Storage, StorageCaps, content_type_of, validate_key};
use std::fs;
use std::io::{self, Read, Write};
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
    fn capabilities(&self) -> StorageCaps {
        StorageCaps {
            streaming_write: true,
            seekable_read: true,
        }
    }

    fn put(&self, key: &str, bytes: &[u8], content_type: &str) -> Result<(), String> {
        self.put_reader(key, &mut &bytes[..], bytes.len() as u64, content_type)
    }

    fn put_reader(
        &self,
        key: &str,
        source: &mut dyn Read,
        length: u64,
        content_type: &str,
    ) -> Result<(), String> {
        let path = self.object_path(key)?;
        let parent = path.parent().ok_or("object has no parent")?;
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        let (tmp, mut file) = TempObject::create(parent)?;
        let copied = io::copy(&mut source.take(length), &mut file)
            .map_err(|e| format!("upload {key}: {e}"))?;
        if copied != length {
            return Err(format!(
                "upload {key}: expected {length} bytes, got {copied}"
            ));
        }
        file.flush()
            .and_then(|()| file.sync_all())
            .map_err(|e| e.to_string())?;
        // Prepare metadata before publication so a failed source or sidecar
        // write never exposes a partial object. Sidecar + object are not a
        // transactional pair; applications should use immutable recording keys.
        let meta = self.meta_path(key);
        let meta_parent = meta.parent().ok_or("metadata has no parent")?;
        fs::create_dir_all(meta_parent).map_err(|e| e.to_string())?;
        let (meta_tmp, mut meta_file) = TempObject::create(meta_parent)?;
        let ct = if content_type.is_empty() {
            content_type_of(key)
        } else {
            content_type
        };
        meta_file
            .write_all(ct.as_bytes())
            .and_then(|()| meta_file.sync_all())
            .map_err(|e| e.to_string())?;
        fs::rename(&meta_tmp.0, &meta).map_err(|e| e.to_string())?;
        fs::rename(&tmp.0, &path).map_err(|e| e.to_string())
    }

    fn open(&self, key: &str) -> Result<Option<OpenObject>, String> {
        let path = self.object_path(key)?;
        match fs::File::open(&path) {
            Ok(file) => {
                let md = file.metadata().map_err(|e| e.to_string())?;
                if !md.is_file() {
                    return Ok(None);
                }
                Ok(Some(OpenObject {
                    meta: self.meta_of(key, &md),
                    reader: Box::new(file),
                }))
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.to_string()),
        }
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

// Unique private siblings prevent concurrent uploads from sharing a temp
// path. The reserved prefix also makes incomplete objects unaddressable.
struct TempObject(PathBuf);
impl TempObject {
    fn create(parent: &Path) -> Result<(Self, fs::File), String> {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);
        for _ in 0..64 {
            let path = parent.join(format!(
                ".sutegi-{}-{}{}",
                std::process::id(),
                SEQ.fetch_add(1, Ordering::Relaxed),
                TMP_SUFFIX
            ));
            let mut options = fs::OpenOptions::new();
            options.create_new(true).write(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            match options.open(&path) {
                Ok(file) => return Ok((Self(path), file)),
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e.to_string()),
            }
        }
        Err("could not allocate a unique upload file".into())
    }
}
impl Drop for TempObject {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
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
    fn partial_uploads_preserve_objects_and_clean_temporary_files() {
        let s = temp_store();
        s.put("call.webm", b"old", "video/webm").unwrap();
        assert!(
            s.put_reader("call.webm", &mut &b"short"[..], 100, "video/mp4")
                .is_err()
        );
        assert_eq!(s.get("call.webm").unwrap().unwrap(), b"old");
        assert_eq!(
            s.stat("call.webm").unwrap().unwrap().content_type,
            "video/webm"
        );
        assert!(
            s.put_reader("missing.webm", &mut &b""[..], 1, "video/webm")
                .is_err()
        );
        assert!(!s.exists("missing.webm").unwrap());
        assert!(fs::read_dir(s.root()).unwrap().all(|e| {
            !e.unwrap()
                .file_name()
                .to_string_lossy()
                .ends_with(TMP_SUFFIX)
        }));
        let _ = fs::remove_dir_all(s.root());
    }

    #[test]
    fn large_upload_and_seekable_snapshot_use_bounded_reads() {
        use std::io::{Seek, SeekFrom};
        struct Source(u64);
        impl Read for Source {
            fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
                assert!(buf.len() <= 64 * 1024);
                let n = buf.len().min(self.0 as usize);
                buf[..n].fill(42);
                self.0 -= n as u64;
                Ok(n)
            }
        }
        let s = temp_store();
        let length = 200 * 1024 * 1024;
        s.put_reader("call.webm", &mut Source(length), length, "video/webm")
            .unwrap();
        let mut opened = s.open("call.webm").unwrap().unwrap();
        assert_eq!(opened.meta.size, length);
        s.put("call.webm", b"replacement", "video/webm").unwrap();
        opened.reader.seek(SeekFrom::End(-3)).unwrap();
        let mut tail = Vec::new();
        opened.reader.read_to_end(&mut tail).unwrap();
        assert_eq!(tail, vec![42; 3]);
        let _ = fs::remove_dir_all(s.root());
    }

    #[test]
    fn concurrent_writes_never_mix_bytes() {
        let s = temp_store();
        let mut threads = Vec::new();
        for byte in 1..=8u8 {
            let store = s.clone();
            threads.push(std::thread::spawn(move || {
                store
                    .put_reader("same", &mut io::repeat(byte), 128 * 1024, "")
                    .unwrap();
            }));
        }
        for t in threads {
            t.join().unwrap();
        }
        let bytes = s.get("same").unwrap().unwrap();
        assert_eq!(bytes.len(), 128 * 1024);
        assert!(bytes.iter().all(|b| *b == bytes[0]));
        let _ = fs::remove_dir_all(s.root());
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
