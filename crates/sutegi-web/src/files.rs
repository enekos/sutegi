//! Static file serving for [`App::static_dir`](crate::App::static_dir).
//!
//! Deliberately small: read the file per request (the OS page cache does
//! the real work at this framework's scale), a fixed extension→MIME table,
//! and a conservative path policy — any `..`, hidden (dot-prefixed), empty
//! or backslash segment is a 404 before the filesystem is touched, so a
//! request can never name a file outside the root or a dotfile inside it.
//! A directory (or the empty rest) serves its `index.html`.

use std::path::{Path, PathBuf};

use sutegi_http::Response;

use crate::text;

/// MIME by extension; `application/wasm` matters — without it the browser
/// refuses `WebAssembly.instantiateStreaming`.
fn mime_of(path: &Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()).unwrap_or("") {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "application/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "wasm" => "application/wasm",
        "json" | "map" => "application/json; charset=utf-8",
        "txt" => "text/plain; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "ico" => "image/x-icon",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "pdf" => "application/pdf",
        _ => "application/octet-stream",
    }
}

fn rejected(seg: &str) -> bool {
    seg.is_empty() || seg.starts_with('.') || seg.contains('\\') || seg.contains('\0')
}

/// Serve `rel` (a `*rest` capture — never starts with `/`) from `root`.
pub(crate) fn serve(root: &Path, rel: &str) -> Response {
    if !rel.is_empty() && rel.split('/').any(rejected) {
        return text(404, "not found");
    }
    let mut path: PathBuf = root.join(rel);
    if rel.is_empty() || path.is_dir() {
        path = path.join("index.html");
    }
    match std::fs::read(&path) {
        Ok(bytes) => Response::new(200)
            .with_header("content-type", mime_of(&path))
            .with_body(bytes),
        Err(_) => text(404, "not found"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header<'a>(r: &'a Response, name: &str) -> Option<&'a str> {
        r.headers
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }

    fn tmp_root() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("sutegi-files-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("pkg")).unwrap();
        std::fs::write(dir.join("index.html"), "<h1>hi</h1>").unwrap();
        std::fs::write(dir.join("app.js"), "export {}").unwrap();
        std::fs::write(dir.join("pkg/app.wasm"), b"\0asm").unwrap();
        std::fs::write(dir.join(".secret"), "nope").unwrap();
        dir
    }

    #[test]
    fn serves_files_with_mime_and_index() {
        let root = tmp_root();
        assert_eq!(serve(&root, "app.js").status, 200);
        let wasm = serve(&root, "pkg/app.wasm");
        assert_eq!(header(&wasm, "content-type"), Some("application/wasm"));
        // "" and a directory both resolve to index.html
        for rel in ["", "index.html"] {
            let r = serve(&root, rel);
            assert_eq!(r.status, 200);
            assert_eq!(header(&r, "content-type"), Some("text/html; charset=utf-8"));
        }
    }

    #[test]
    fn rejects_traversal_dotfiles_and_missing() {
        let root = tmp_root();
        for rel in [
            "../etc/passwd",
            "pkg/../../etc/passwd",
            ".secret",
            "pkg/.hidden",
            "a\\b",
            "nope.js",
            "pkg//app.wasm",
        ] {
            assert_eq!(serve(&root, rel).status, 404, "{rel} should 404");
        }
    }
}
