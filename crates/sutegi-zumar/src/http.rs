//! Minimal blocking HTTP GET/POST for server-side `httpGet`/`httpPost`
//! effects. Plain `http://` only (sutegi's TLS posture: terminate at the
//! LB); relative URLs — the common case, a live page fetching or posting to
//! its own API — resolve against the bridge's base (the app itself by
//! default).

use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, TcpStream, ToSocketAddrs};
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(10);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// SSRF guard: reject connections to non-public IP ranges for attacker-
/// influenceable (absolute-URL) fetches — loopback, RFC-1918 private, CGNAT,
/// link-local (incl. the `169.254.169.254` cloud metadata endpoint),
/// unspecified/broadcast, and their IPv6 equivalents / v4-mapped forms.
fn is_blocked_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_blocked_v4(v4),
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_blocked_v4(v4);
            }
            let seg = v6.segments();
            v6.is_loopback()
                || v6.is_unspecified()
                || (seg[0] & 0xfe00) == 0xfc00 // fc00::/7 unique-local
                || (seg[0] & 0xffc0) == 0xfe80 // fe80::/10 link-local
        }
    }
}

fn is_blocked_v4(ip: Ipv4Addr) -> bool {
    let o = ip.octets();
    ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_unspecified()
        || ip.is_broadcast()
        || o[0] == 0 // 0.0.0.0/8
        || (o[0] == 100 && (o[1] & 0xc0) == 64) // 100.64.0.0/10 CGNAT
}

/// Returns `(ok, status, body)` in the shape `Program::resolve` expects:
/// transport failures are `(false, 0, error text)`, HTTP responses report
/// their real status with `ok = 2xx`.
pub(crate) fn get(base: &str, url: &str) -> (bool, u16, String) {
    send("GET", base, url, None)
}

/// POST `body` (JSON) to `url` for a server-side `httpPost` effect. Same
/// `(ok, status, body)` contract as [`get`].
pub(crate) fn post(base: &str, url: &str, body: &str) -> (bool, u16, String) {
    send("POST", base, url, Some(body))
}

fn send(method: &str, base: &str, url: &str, body: Option<&str>) -> (bool, u16, String) {
    // `trusted` = the URL was relative and resolved against `base` (the app's
    // own configured address, so allowed to be loopback). Absolute `http://`
    // targets are attacker-influenceable and get SSRF vetting in `request`.
    let (absolute, trusted) = if url.starts_with("http://") {
        (url.to_string(), false)
    } else if url.starts_with("https://") {
        return (false, 0, "https not supported for server-side http".into());
    } else {
        (
            format!(
                "{}/{}",
                base.trim_end_matches('/'),
                url.trim_start_matches('/')
            ),
            true,
        )
    };
    match request(method, &absolute, body, trusted) {
        Ok((status, body)) => ((200..300).contains(&status), status, body),
        Err(e) => (false, 0, e),
    }
}

fn request(
    method: &str,
    url: &str,
    body: Option<&str>,
    trusted: bool,
) -> Result<(u16, String), String> {
    let rest = url.strip_prefix("http://").ok_or("only http:// URLs")?;
    let (authority, path) = match rest.split_once('/') {
        Some((a, p)) => (a, format!("/{p}")),
        None => (rest, "/".to_string()),
    };
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) => (h, p.parse::<u16>().map_err(|_| "bad port")?),
        None => (authority, 80),
    };

    // Resolve first, then vet + connect to the SAME address (no re-resolve, so
    // no DNS-rebinding window). An untrusted (absolute-URL) target may not
    // point at a loopback / private / link-local address — the SSRF guard that
    // stops a live program being driven to the cloud metadata endpoint or an
    // internal-only service. A connect timeout bounds a blackholed target.
    let addr = {
        let mut addrs = (host, port).to_socket_addrs().map_err(|e| e.to_string())?;
        if trusted {
            addrs.next().ok_or("could not resolve host")?
        } else {
            addrs.find(|a| !is_blocked_ip(a.ip())).ok_or_else(|| {
                format!("blocked: {host} resolves only to a private/loopback address (SSRF)")
            })?
        }
    };
    let stream = TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT).map_err(|e| e.to_string())?;
    stream.set_read_timeout(Some(TIMEOUT)).ok();
    stream.set_write_timeout(Some(TIMEOUT)).ok();
    let mut stream = stream;
    let mut head = format!("{method} {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n");
    if let Some(body) = body {
        head.push_str("Content-Type: application/json\r\n");
        head.push_str(&format!("Content-Length: {}\r\n", body.len()));
    }
    head.push_str("\r\n");
    // Write head + body in one shot: fewer syscalls, and a peer that reads
    // once then responds/closes can't race a separate body write.
    let mut out = head.into_bytes();
    if let Some(body) = body {
        out.extend_from_slice(body.as_bytes());
    }
    stream.write_all(&out).map_err(|e| e.to_string())?;

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
    fn post_sends_body_and_content_length() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let got = std::thread::spawn(move || {
            let (mut sock, _) = listener.accept().unwrap();
            let mut buf = [0u8; 512];
            let n = sock.read(&mut buf).unwrap();
            let req = String::from_utf8_lossy(&buf[..n]).to_string();
            sock.write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\nconnection: close\r\n\r\nok")
                .unwrap();
            req
        });
        let (ok, status, body) = post(&format!("http://{addr}"), "/api/login", "{\"e\":\"a\"}");
        assert!(ok);
        assert_eq!(status, 200);
        assert_eq!(body, "ok");
        let req = got.join().unwrap();
        assert!(req.starts_with("POST /api/login HTTP/1.1"), "{req}");
        assert!(req.contains("Content-Length: 9"), "{req}");
        assert!(req.ends_with("{\"e\":\"a\"}"), "{req}");
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

    #[test]
    fn absolute_url_to_private_or_metadata_ip_is_blocked() {
        // Absolute URL to the cloud metadata endpoint is refused (SSRF guard).
        let (ok, status, body) = get("http://x", "http://169.254.169.254/latest/meta-data/");
        assert!(!ok);
        assert_eq!(status, 0);
        assert!(body.contains("SSRF") || body.contains("blocked"), "{body}");
        // Absolute loopback / private targets are refused too.
        assert!(!get("http://x", "http://127.0.0.1:80/").0);
        assert!(!post("http://x", "http://10.0.0.5/", "{}").0);
    }

    #[test]
    fn relative_urls_bypass_the_ssrf_guard() {
        // A relative fetch resolves against the trusted base (the app itself),
        // loopback by default — that must still be allowed. Connect to a closed
        // port so we hit "connection refused", NOT an SSRF block.
        let (ok, status, _) = get("http://127.0.0.1:1", "/health");
        assert!(!ok);
        assert_eq!(status, 0);
    }

    #[test]
    fn blocks_private_and_metadata_ranges() {
        let b = |s: &str| is_blocked_ip(s.parse::<IpAddr>().unwrap());
        assert!(b("127.0.0.1"));
        assert!(b("169.254.169.254")); // cloud metadata
        assert!(b("10.0.0.5"));
        assert!(b("172.16.0.1"));
        assert!(b("192.168.1.1"));
        assert!(b("100.64.0.1")); // CGNAT
        assert!(b("0.0.0.0"));
        assert!(b("::1"));
        assert!(b("fe80::1"));
        assert!(b("fc00::1"));
        assert!(b("::ffff:127.0.0.1")); // v4-mapped loopback
        assert!(!b("8.8.8.8"));
        assert!(!b("1.1.1.1"));
    }
}
