//! Minimal blocking HTTP GET for server-side `httpGet` effects. Plain
//! `http://` only (sutegi's TLS posture: terminate at the LB); relative
//! URLs — the common case, a live page fetching its own API — resolve
//! against the bridge's base (the app itself by default).

use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(10);

/// Returns `(ok, status, body)` in the shape `Program::resolve` expects:
/// transport failures are `(false, 0, error text)`, HTTP responses report
/// their real status with `ok = 2xx`.
pub(crate) fn get(base: &str, url: &str) -> (bool, u16, String) {
    let absolute = if url.starts_with("http://") {
        url.to_string()
    } else if url.starts_with("https://") {
        return (
            false,
            0,
            "https not supported for server-side httpGet".into(),
        );
    } else {
        format!(
            "{}/{}",
            base.trim_end_matches('/'),
            url.trim_start_matches('/')
        )
    };
    match request(&absolute) {
        Ok((status, body)) => ((200..300).contains(&status), status, body),
        Err(e) => (false, 0, e),
    }
}

fn request(url: &str) -> Result<(u16, String), String> {
    let rest = url.strip_prefix("http://").ok_or("only http:// URLs")?;
    let (authority, path) = match rest.split_once('/') {
        Some((a, p)) => (a, format!("/{p}")),
        None => (rest, "/".to_string()),
    };
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) => (h, p.parse::<u16>().map_err(|_| "bad port")?),
        None => (authority, 80),
    };

    let stream = TcpStream::connect((host, port)).map_err(|e| e.to_string())?;
    stream.set_read_timeout(Some(TIMEOUT)).ok();
    stream.set_write_timeout(Some(TIMEOUT)).ok();
    let mut stream = stream;
    stream
        .write_all(
            format!("GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n").as_bytes(),
        )
        .map_err(|e| e.to_string())?;

    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).map_err(|e| e.to_string())?;
    let raw = String::from_utf8_lossy(&raw);
    let (head, body) = raw.split_once("\r\n\r\n").unwrap_or((&raw, ""));
    let status: u16 = head
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .ok_or("malformed status line")?;
    Ok((status, body.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    #[test]
    fn fetches_relative_urls_against_the_base() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            let (mut sock, _) = listener.accept().unwrap();
            let mut buf = [0u8; 512];
            let _ = sock.read(&mut buf);
            sock.write_all(
                b"HTTP/1.1 200 OK\r\ncontent-length: 4\r\nconnection: close\r\n\r\naupa",
            )
            .unwrap();
        });
        let (ok, status, body) = get(&format!("http://{addr}"), "/api/hello");
        assert!(ok);
        assert_eq!(status, 200);
        assert_eq!(body, "aupa");
    }

    #[test]
    fn transport_failures_resolve_as_errors_not_panics() {
        let (ok, status, body) = get("http://127.0.0.1:1", "/nope");
        assert!(!ok);
        assert_eq!(status, 0);
        assert!(!body.is_empty());
        let (ok, _, body) = get("http://x", "https://example.com/secret");
        assert!(!ok);
        assert!(body.contains("https"));
    }
}
