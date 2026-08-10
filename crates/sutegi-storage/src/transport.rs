//! The **outbound-HTTP seam** — one trait, two built-in implementations. This
//! is what lets [`S3Storage`](crate::S3Storage) move real bytes without sutegi
//! growing a TLS stack or a third-party dependency.
//!
//! - [`PlainHttp`] — pure-`std` HTTP/1.1 over `TcpStream`. **Refuses `https`**
//!   rather than pretending: point it at an in-cluster MinIO/Garage or a dev
//!   container. Same stance as the Postgres driver and the SMTP transport.
//! - [`SystemCurl`] — `https` by delegating the TLS handshake and certificate
//!   verification to the system `curl` binary. This is how AWS S3 and
//!   Cloudflare R2 work out of the box with zero Cargo dependencies: the
//!   crypto that must not be hand-rolled isn't.
//! - your own — implement [`HttpTransport`] over `ureq`/`reqwest`/`hyper` in
//!   *your* crate if you already pay for one.
//!
//! Requests arriving here are **already signed**: the transport must send the
//! given headers verbatim and must not follow redirects (a 3xx would replay
//! the `Authorization` header at an attacker-chosen host).

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

/// Default cap on a single response body (64 MiB). A hostile or misconfigured
/// endpoint cannot make the process allocate more than this.
pub const DEFAULT_MAX_BODY: usize = 64 * 1024 * 1024;

const MAX_HEADERS: usize = 100;
const MAX_LINE: usize = 8 * 1024;

/// One outbound request, fully formed and already signed.
///
/// `headers` is the exact set the signature covers (lowercase names, including
/// `host`) — send them as given, add nothing that could collide, drop nothing.
#[derive(Debug)]
pub struct HttpRequest<'a> {
    /// Uppercase HTTP method.
    pub method: &'a str,
    /// Absolute URL: `scheme://host[:port]/path[?query]`.
    pub url: String,
    /// Signed headers, lowercase names, `host` included.
    pub headers: Vec<(String, String)>,
    /// Request body; empty for GET/HEAD/DELETE.
    pub body: &'a [u8],
}

/// One response: status, headers, body.
#[derive(Clone, Debug)]
pub struct HttpResponse {
    /// HTTP status code (never 1xx — interim responses are skipped).
    pub status: u16,
    /// Response headers with names lowercased.
    pub headers: Vec<(String, String)>,
    /// Response body (empty for HEAD).
    pub body: Vec<u8>,
}

impl HttpResponse {
    /// The first value for `name` (case-insensitive).
    pub fn header(&self, name: &str) -> Option<&str> {
        let name = name.to_ascii_lowercase();
        self.headers
            .iter()
            .find(|(k, _)| *k == name)
            .map(|(_, v)| v.as_str())
    }
}

/// How [`S3Storage`](crate::S3Storage) reaches the object store. One method.
///
/// Implementations **must**: send `req.headers` unchanged, not follow
/// redirects, and bound the response body they buffer.
pub trait HttpTransport: Send + Sync {
    /// Perform `req` and return the response. `Err` is a transport failure
    /// (DNS, connect, TLS, timeout) — an HTTP error *status* is a successful
    /// send and belongs in [`HttpResponse::status`].
    fn send(&self, req: &HttpRequest<'_>) -> Result<HttpResponse, String>;
}

impl<T: HttpTransport + ?Sized> HttpTransport for std::sync::Arc<T> {
    fn send(&self, req: &HttpRequest<'_>) -> Result<HttpResponse, String> {
        (**self).send(req)
    }
}

/// Reject header injection before it reaches the wire. Signed values are ours,
/// but `content_type` originates with the caller.
fn check_header(name: &str, value: &str) -> Result<(), String> {
    let bad_name = name.is_empty()
        || !name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b));
    if bad_name {
        return Err(format!("invalid header name: {name:?}"));
    }
    if value.bytes().any(|b| b < 0x20 || b == 0x7f) {
        return Err(format!("header {name} contains control characters"));
    }
    Ok(())
}

/// Split an absolute URL into `(scheme, host[:port], path?query)`.
fn split_url(url: &str) -> Result<(&str, &str, &str), String> {
    if url.bytes().any(|b| b <= 0x20 || b == 0x7f) {
        return Err("url contains whitespace or control characters".to_string());
    }
    let (scheme, rest) = url
        .split_once("://")
        .ok_or_else(|| format!("url has no scheme: {url}"))?;
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    if authority.is_empty() {
        return Err(format!("url has no host: {url}"));
    }
    // Credentials in the authority would be sent to an unexpected host.
    if authority.contains('@') {
        return Err("url must not carry userinfo".to_string());
    }
    Ok((scheme, authority, path))
}

// ---------------------------------------------------------------- PlainHttp

/// Pure-`std` HTTP/1.1 transport: blocking `TcpStream`, one connection per
/// request (`Connection: close`), bounded reads and hard timeouts.
///
/// **`https` URLs are rejected.** This is for object stores you reach over a
/// trusted network path — an in-cluster MinIO, a sidecar Garage, a dev
/// container. Note that SigV4 still signs the payload hash end-to-end, so a
/// tampered request body is rejected by the store even in plaintext; secrecy,
/// not integrity, is what plaintext costs you. For anything crossing the
/// public internet use [`SystemCurl`].
#[derive(Clone, Debug)]
pub struct PlainHttp {
    connect_timeout: Duration,
    io_timeout: Duration,
    max_body: usize,
}

impl Default for PlainHttp {
    fn default() -> PlainHttp {
        PlainHttp::new()
    }
}

impl PlainHttp {
    /// 5 s connect, 30 s read/write, 64 MiB body cap.
    pub fn new() -> PlainHttp {
        PlainHttp {
            connect_timeout: Duration::from_secs(5),
            io_timeout: Duration::from_secs(30),
            max_body: DEFAULT_MAX_BODY,
        }
    }

    /// Override the connect and per-read/write timeouts.
    pub fn timeouts(mut self, connect: Duration, io: Duration) -> PlainHttp {
        self.connect_timeout = connect;
        self.io_timeout = io;
        self
    }

    /// Override the response-body cap.
    pub fn max_body(mut self, bytes: usize) -> PlainHttp {
        self.max_body = bytes;
        self
    }
}

impl HttpTransport for PlainHttp {
    fn send(&self, req: &HttpRequest<'_>) -> Result<HttpResponse, String> {
        let (scheme, authority, path) = split_url(&req.url)?;
        if !scheme.eq_ignore_ascii_case("http") {
            return Err(format!(
                "PlainHttp speaks http only (got {scheme}://); use SystemCurl \
                 or your own HttpTransport for TLS"
            ));
        }
        let host_port = if authority.contains(':') {
            authority.to_string()
        } else {
            format!("{authority}:80")
        };

        let addrs = host_port
            .to_socket_addrs()
            .map_err(|e| format!("resolve {host_port}: {e}"))?;
        let mut last = format!("resolve {host_port}: no addresses");
        let mut stream = None;
        for addr in addrs {
            match TcpStream::connect_timeout(&addr, self.connect_timeout) {
                Ok(s) => {
                    stream = Some(s);
                    break;
                }
                Err(e) => last = format!("connect {addr}: {e}"),
            }
        }
        let mut stream = stream.ok_or(last)?;
        stream
            .set_read_timeout(Some(self.io_timeout))
            .and_then(|()| stream.set_write_timeout(Some(self.io_timeout)))
            .map_err(|e| format!("set timeouts: {e}"))?;
        let _ = stream.set_nodelay(true);

        let mut head = format!("{} {path} HTTP/1.1\r\n", req.method);
        for (k, v) in &req.headers {
            check_header(k, v)?;
            head.push_str(&format!("{k}: {v}\r\n"));
        }
        head.push_str(&format!("content-length: {}\r\n", req.body.len()));
        head.push_str("connection: close\r\n\r\n");

        stream
            .write_all(head.as_bytes())
            .and_then(|()| stream.write_all(req.body))
            .and_then(|()| stream.flush())
            .map_err(|e| format!("write request: {e}"))?;

        let head_only = req.method.eq_ignore_ascii_case("HEAD");
        let mut reader = BufReader::new(stream);
        read_response(&mut reader, self.max_body, head_only)
    }
}

// -------------------------------------------------------------- SystemCurl

/// `https` transport that delegates TLS to the system `curl` binary — the
/// zero-dependency way to talk to AWS S3 and Cloudflare R2.
///
/// Hardening, all of it on purpose:
/// - **Credentials never touch `argv`.** The URL and every signed header go
///   into a config file fed on `curl`'s **stdin** (`--config -`), so
///   `Authorization` is not visible in `ps` or `/proc/<pid>/cmdline`.
/// - `--proto =https` — the URL cannot downgrade the protocol.
/// - No redirect following, so a 3xx cannot replay the signature elsewhere.
/// - TLS 1.2 floor; certificate and hostname verification left **on** (there
///   is deliberately no knob here that turns it off).
/// - `--max-filesize` makes `curl` itself refuse an oversized body.
/// - Interim `1xx` header blocks (`Expect: 100-continue`) are skipped.
///
/// The one thing that does hit disk: a `PUT` body is handed over as a
/// `0600`, `O_EXCL`-created temp file (`curl` reads its upload from a file, and
/// stdin is already carrying the config). It is removed as soon as `curl`
/// exits. Point [`tmp_dir`](SystemCurl::tmp_dir) at a `tmpfs` if object bytes
/// must never land on a real filesystem.
#[derive(Clone, Debug)]
pub struct SystemCurl {
    program: String,
    connect_timeout: u64,
    max_time: u64,
    max_body: usize,
    allow_http: bool,
    tmp_dir: Option<std::path::PathBuf>,
}

impl Default for SystemCurl {
    fn default() -> SystemCurl {
        SystemCurl::new()
    }
}

impl SystemCurl {
    /// `curl` from `PATH`, 10 s connect, 300 s total, 64 MiB cap.
    pub fn new() -> SystemCurl {
        SystemCurl {
            program: "curl".to_string(),
            connect_timeout: 10,
            max_time: 300,
            max_body: DEFAULT_MAX_BODY,
            allow_http: false,
            tmp_dir: None,
        }
    }

    /// Pin the binary by absolute path — the right call in a hardened image,
    /// where resolving `curl` through `PATH` is one more thing to trust.
    pub fn at(path: &str) -> SystemCurl {
        SystemCurl {
            program: path.to_string(),
            ..SystemCurl::new()
        }
    }

    /// Connect and total-operation timeouts, in seconds.
    pub fn timeouts(mut self, connect_secs: u64, total_secs: u64) -> SystemCurl {
        self.connect_timeout = connect_secs;
        self.max_time = total_secs;
        self
    }

    /// Override the response-body cap.
    pub fn max_body(mut self, bytes: usize) -> SystemCurl {
        self.max_body = bytes;
        self
    }

    /// Also allow plaintext `http` URLs (a MinIO inside your own network).
    /// Certificate verification for `https` is unaffected — and unreachable
    /// by configuration.
    pub fn allow_http(mut self) -> SystemCurl {
        self.allow_http = true;
        self
    }

    /// Where `PUT` bodies are staged (default: [`std::env::temp_dir`]).
    pub fn tmp_dir(mut self, dir: impl Into<std::path::PathBuf>) -> SystemCurl {
        self.tmp_dir = Some(dir.into());
        self
    }

    /// The `--config` document for `req`. Everything secret lives here, and
    /// this goes to `curl` on stdin.
    fn config(&self, req: &HttpRequest<'_>, upload: Option<&std::path::Path>) -> String {
        let mut c = String::new();
        c.push_str(&format!("url = \"{}\"\n", curl_escape(&req.url)));
        // `=https` sets the allowed set to exactly https; a bare entry after it
        // adds to that set. Never two `=` entries — the second would disable
        // the first.
        c.push_str(&format!(
            "proto = \"=https{}\"\n",
            if self.allow_http { ",http" } else { "" }
        ));
        c.push_str("tlsv1.2\n");
        c.push_str("silent\nshow-error\ninclude\nno-location\nhttp1.1\n");
        c.push_str(&format!("connect-timeout = \"{}\"\n", self.connect_timeout));
        c.push_str(&format!("max-time = \"{}\"\n", self.max_time));
        c.push_str(&format!("max-filesize = \"{}\"\n", self.max_body));
        if req.method.eq_ignore_ascii_case("HEAD") {
            c.push_str("head\n");
        } else {
            c.push_str(&format!("request = \"{}\"\n", curl_escape(req.method)));
        }
        for (k, v) in &req.headers {
            c.push_str(&format!(
                "header = \"{}: {}\"\n",
                curl_escape(k),
                curl_escape(v)
            ));
        }
        if let Some(path) = upload {
            c.push_str(&format!(
                "upload-file = \"{}\"\n",
                curl_escape(&path.to_string_lossy())
            ));
        }
        c
    }
}

/// Escape a value for a double-quoted `curl` config parameter.
fn curl_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            c => out.push(c),
        }
    }
    out
}

/// A `0600`, exclusively-created temp file, removed on drop.
struct TempFile(std::path::PathBuf);

impl TempFile {
    fn write(dir: &std::path::Path, bytes: &[u8]) -> Result<TempFile, String> {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let path = dir.join(format!(
            "sutegi-s3-{}-{}.tmp",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut f = opts
            .open(&path)
            .map_err(|e| format!("create {}: {e}", path.display()))?;
        let guard = TempFile(path);
        f.write_all(bytes)
            .and_then(|()| f.flush())
            .map_err(|e| format!("write {}: {e}", guard.0.display()))?;
        Ok(guard)
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

impl HttpTransport for SystemCurl {
    fn send(&self, req: &HttpRequest<'_>) -> Result<HttpResponse, String> {
        use std::process::{Command, Stdio};

        let (scheme, _, _) = split_url(&req.url)?;
        let ok_scheme = scheme.eq_ignore_ascii_case("https")
            || (self.allow_http && scheme.eq_ignore_ascii_case("http"));
        if !ok_scheme {
            return Err(format!(
                "SystemCurl refuses {scheme}:// (call .allow_http() to permit plaintext)"
            ));
        }
        for (k, v) in &req.headers {
            check_header(k, v)?;
        }

        let staged = if req.body.is_empty() {
            None
        } else {
            let dir = self.tmp_dir.clone().unwrap_or_else(std::env::temp_dir);
            Some(TempFile::write(&dir, req.body)?)
        };
        let config = self.config(req, staged.as_ref().map(|t| t.0.as_path()));

        let mut child = Command::new(&self.program)
            .arg("--config")
            .arg("-")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("spawn {}: {e}", self.program))?;
        child
            .stdin
            .take()
            .ok_or("curl stdin unavailable")?
            .write_all(config.as_bytes())
            .map_err(|e| format!("write curl config: {e}"))?;
        let out = child
            .wait_with_output()
            .map_err(|e| format!("wait for curl: {e}"))?;
        drop(staged);

        if !out.status.success() {
            return Err(format!(
                "curl exited {}: {}",
                out.status,
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        let head_only = req.method.eq_ignore_ascii_case("HEAD");
        let mut reader = BufReader::new(&out.stdout[..]);
        read_response(&mut reader, self.max_body, head_only)
    }
}

// ---------------------------------------------------- HTTP/1.1 response read

/// Read one response: status line, headers, body. Interim `1xx` blocks are
/// consumed and skipped; `chunked` bodies are de-chunked; everything is
/// bounded.
fn read_response<R: BufRead>(
    r: &mut R,
    max_body: usize,
    head_only: bool,
) -> Result<HttpResponse, String> {
    let (status, headers) = loop {
        let status = parse_status(&read_line(r)?)?;
        let headers = read_headers(r)?;
        if !(100..200).contains(&status) {
            break (status, headers);
        }
    };

    let mut body = Vec::new();
    let chunked = headers
        .iter()
        .any(|(k, v)| k == "transfer-encoding" && v.to_ascii_lowercase().contains("chunked"));
    let length = headers
        .iter()
        .find(|(k, _)| k == "content-length")
        .map(|(_, v)| {
            v.trim()
                .parse::<u64>()
                .map_err(|_| format!("bad content-length: {v:?}"))
        })
        .transpose()?;

    let bodyless = head_only || status == 204 || status == 304;
    if !bodyless {
        if chunked {
            read_chunked(r, max_body, &mut body)?;
        } else if let Some(n) = length {
            if n > max_body as u64 {
                return Err(format!("response body {n} exceeds cap {max_body}"));
            }
            body.resize(n as usize, 0);
            r.read_exact(&mut body)
                .map_err(|e| format!("read body: {e}"))?;
        } else {
            // No framing: read to EOF, still bounded.
            r.by_ref()
                .take(max_body as u64 + 1)
                .read_to_end(&mut body)
                .map_err(|e| format!("read body: {e}"))?;
            if body.len() > max_body {
                return Err(format!("response body exceeds cap {max_body}"));
            }
        }
    }
    Ok(HttpResponse {
        status,
        headers,
        body,
    })
}

fn read_line<R: BufRead>(r: &mut R) -> Result<String, String> {
    let mut line = String::new();
    let n = r
        .by_ref()
        .take(MAX_LINE as u64)
        .read_line(&mut line)
        .map_err(|e| format!("read: {e}"))?;
    if n == 0 {
        return Err("connection closed mid-response".to_string());
    }
    if !line.ends_with('\n') {
        return Err(format!("response line exceeds {MAX_LINE} bytes"));
    }
    Ok(line.trim_end_matches(['\r', '\n']).to_string())
}

fn parse_status(line: &str) -> Result<u16, String> {
    let mut parts = line.split(' ');
    let version = parts.next().unwrap_or("");
    if !version.starts_with("HTTP/") {
        return Err(format!("not an HTTP response: {line:?}"));
    }
    parts
        .next()
        .and_then(|c| c.parse::<u16>().ok())
        .filter(|c| (100..600).contains(c))
        .ok_or_else(|| format!("bad status line: {line:?}"))
}

fn read_headers<R: BufRead>(r: &mut R) -> Result<Vec<(String, String)>, String> {
    let mut headers = Vec::new();
    loop {
        let line = read_line(r)?;
        if line.is_empty() {
            return Ok(headers);
        }
        if headers.len() >= MAX_HEADERS {
            return Err(format!("response has more than {MAX_HEADERS} headers"));
        }
        let (name, value) = line
            .split_once(':')
            .ok_or_else(|| format!("bad header line: {line:?}"))?;
        headers.push((name.trim().to_ascii_lowercase(), value.trim().to_string()));
    }
}

fn read_chunked<R: BufRead>(r: &mut R, max_body: usize, out: &mut Vec<u8>) -> Result<(), String> {
    loop {
        let line = read_line(r)?;
        let size_hex = line.split(';').next().unwrap_or("").trim();
        let size = u64::from_str_radix(size_hex, 16)
            .map_err(|_| format!("bad chunk size: {size_hex:?}"))?;
        if size == 0 {
            // Trailers, then the terminating blank line.
            while !read_line(r)?.is_empty() {}
            return Ok(());
        }
        if out.len() as u64 + size > max_body as u64 {
            return Err(format!("chunked body exceeds cap {max_body}"));
        }
        let start = out.len();
        out.resize(start + size as usize, 0);
        r.read_exact(&mut out[start..])
            .map_err(|e| format!("read chunk: {e}"))?;
        if !read_line(r)?.is_empty() {
            return Err("chunk not terminated by CRLF".to_string());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(raw: &str, head_only: bool) -> Result<HttpResponse, String> {
        read_response(&mut BufReader::new(raw.as_bytes()), 1024, head_only)
    }

    #[test]
    fn parses_content_length_body() {
        let r = parse(
            "HTTP/1.1 200 OK\r\nETag: \"abc\"\r\nContent-Length: 5\r\n\r\nhello",
            false,
        )
        .unwrap();
        assert_eq!(r.status, 200);
        assert_eq!(r.body, b"hello");
        assert_eq!(r.header("etag"), Some("\"abc\""));
        assert_eq!(r.header("ETAG"), Some("\"abc\""));
    }

    #[test]
    fn skips_interim_100_continue() {
        let r = parse(
            "HTTP/1.1 100 Continue\r\n\r\nHTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok",
            false,
        )
        .unwrap();
        assert_eq!((r.status, r.body), (200, b"ok".to_vec()));
    }

    #[test]
    fn decodes_chunked() {
        let r = parse(
            "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n\
             4\r\nsute\r\n2;ext\r\ngi\r\n0\r\n\r\n",
            false,
        )
        .unwrap();
        assert_eq!(r.body, b"sutegi");
    }

    #[test]
    fn head_and_204_have_no_body() {
        // A HEAD response advertises the length it *would* have sent.
        let r = parse("HTTP/1.1 200 OK\r\nContent-Length: 99\r\n\r\n", true).unwrap();
        assert!(r.body.is_empty());
        assert_eq!(r.header("content-length"), Some("99"));
        let r = parse("HTTP/1.1 204 No Content\r\n\r\n", false).unwrap();
        assert_eq!((r.status, r.body.len()), (204, 0));
    }

    #[test]
    fn body_cap_is_enforced() {
        let raw = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: 4096\r\n\r\n{}",
            "x".repeat(4096)
        );
        assert!(parse(&raw, false).unwrap_err().contains("exceeds cap"));
        let chunked = "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n1000\r\n";
        assert!(parse(chunked, false).unwrap_err().contains("exceeds cap"));
    }

    #[test]
    fn rejects_garbage_and_truncation() {
        assert!(parse("220 smtp.example.com ESMTP\r\n\r\n", false).is_err());
        assert!(parse("HTTP/1.1 999 Nope\r\n\r\n", false).is_err());
        assert!(parse("HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nhi", false).is_err());
    }

    #[test]
    fn url_split_and_guards() {
        assert_eq!(
            split_url("https://a.example.com/x/y?q=1").unwrap(),
            ("https", "a.example.com", "/x/y?q=1")
        );
        assert_eq!(
            split_url("http://localhost:9000").unwrap(),
            ("http", "localhost:9000", "/")
        );
        assert!(split_url("no-scheme/x").is_err());
        assert!(split_url("http://user:pw@evil/x").is_err());
        assert!(split_url("http://host/a b").is_err());
        assert!(split_url("http://host/a\r\nX: y").is_err());
    }

    #[test]
    fn header_injection_rejected() {
        assert!(check_header("content-type", "text/plain").is_ok());
        assert!(check_header("content-type", "a\r\nauthorization: leak").is_err());
        assert!(check_header("bad name", "v").is_err());
    }

    #[test]
    fn plain_http_refuses_tls() {
        let err = PlainHttp::new()
            .send(&HttpRequest {
                method: "GET",
                url: "https://bucket.s3.amazonaws.com/k".to_string(),
                headers: vec![],
                body: b"",
            })
            .unwrap_err();
        assert!(err.contains("http only"), "{err}");
    }

    #[test]
    fn curl_config_hides_secrets_and_pins_protocol() {
        let req = HttpRequest {
            method: "PUT",
            url: "https://acct.r2.cloudflarestorage.com/bkt/a%20b.txt".to_string(),
            headers: vec![
                (
                    "authorization".to_string(),
                    "AWS4-HMAC-SHA256 Credential=AK/x".to_string(),
                ),
                ("content-type".to_string(), "text/plain".to_string()),
            ],
            body: b"hi",
        };
        let cfg = SystemCurl::new().config(&req, Some(std::path::Path::new("/tmp/x.tmp")));
        assert!(cfg.contains("url = \"https://acct.r2.cloudflarestorage.com/bkt/a%20b.txt\"\n"));
        assert!(cfg.contains("proto = \"=https\"\n"));
        assert!(cfg.contains("no-location\n") && cfg.contains("tlsv1.2\n"));
        assert!(cfg.contains("request = \"PUT\"\n"));
        assert!(cfg.contains("header = \"authorization: AWS4-HMAC-SHA256 Credential=AK/x\"\n"));
        assert!(cfg.contains("upload-file = \"/tmp/x.tmp\"\n"));
        assert!(cfg.contains("max-filesize = \"67108864\"\n"));
        // HEAD uses --head, never -X HEAD (which would hang waiting for a body).
        let head = HttpRequest {
            method: "HEAD",
            ..req
        };
        let cfg = SystemCurl::new().config(&head, None);
        assert!(cfg.contains("head\n") && !cfg.contains("request ="));
        assert!(SystemCurl::new()
            .allow_http()
            .config(&head, None)
            .contains("proto = \"=https,http\"\n"));
    }

    #[test]
    fn curl_refuses_plaintext_by_default() {
        let req = HttpRequest {
            method: "GET",
            url: "http://127.0.0.1:9000/bkt/k".to_string(),
            headers: vec![],
            body: b"",
        };
        let err = SystemCurl::new().send(&req).unwrap_err();
        assert!(err.contains("refuses http://"), "{err}");
        // …and there is no knob anywhere that disables certificate checking.
        assert!(!SystemCurl::new()
            .allow_http()
            .config(&req, None)
            .contains("insecure"));
    }

    #[test]
    fn curl_escapes_quotes_and_newlines() {
        assert_eq!(curl_escape("a\"b\\c\r\nd"), "a\\\"b\\\\c\\r\\nd");
    }

    #[test]
    fn temp_file_is_private_and_removed() {
        let t = TempFile::write(&std::env::temp_dir(), b"secret").unwrap();
        let path = t.0.clone();
        assert_eq!(std::fs::read(&path).unwrap(), b"secret");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(
                mode & 0o777,
                0o600,
                "temp upload must not be world-readable"
            );
        }
        drop(t);
        assert!(!path.exists());
    }
}
