//! A minimal HTTP/1.1 server built directly on `std::net`.
//!
//! No async runtime, no `hyper`, no `tokio`. Connections are handled by a
//! fixed thread pool. This is deliberately small: it keeps the binary tiny
//! and the request lifecycle trivial to reason about — for a human reading
//! the source, or an agent reasoning about the running app.

use std::io::{self, BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::Duration;

/// HTTP request methods sutegi recognizes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Method {
    Get,
    Post,
    Put,
    Patch,
    Delete,
    Head,
    Options,
    Other,
}

impl Method {
    pub fn parse(s: &str) -> Method {
        match s {
            "GET" => Method::Get,
            "POST" => Method::Post,
            "PUT" => Method::Put,
            "PATCH" => Method::Patch,
            "DELETE" => Method::Delete,
            "HEAD" => Method::Head,
            "OPTIONS" => Method::Options,
            _ => Method::Other,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Method::Get => "GET",
            Method::Post => "POST",
            Method::Put => "PUT",
            Method::Patch => "PATCH",
            Method::Delete => "DELETE",
            Method::Head => "HEAD",
            Method::Options => "OPTIONS",
            Method::Other => "OTHER",
        }
    }
}

/// A parsed HTTP request.
#[derive(Clone, Debug)]
pub struct Request {
    pub method: Method,
    /// Path with query string stripped, e.g. `/users/42`.
    pub path: String,
    /// Raw query string without the `?`, e.g. `page=2&q=foo`.
    pub query: String,
    pub version: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    /// The peer socket address (`ip:port`), if known. Set by the server.
    pub peer: Option<String>,
}

impl Request {
    /// The client's IP (port stripped), e.g. for rate limiting / logging.
    pub fn peer_ip(&self) -> Option<String> {
        let p = self.peer.as_ref()?;
        if let Some(end) = p.find(']') {
            Some(p[..=end].to_string()) // [ipv6]
        } else if let Some(i) = p.rfind(':') {
            Some(p[..i].to_string()) // ipv4:port
        } else {
            Some(p.clone())
        }
    }

    /// Case-insensitive header lookup.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    /// The `Content-Type` header, if present.
    pub fn content_type(&self) -> Option<&str> {
        self.header("content-type")
    }

    /// Whether the body is declared as JSON.
    pub fn is_json(&self) -> bool {
        self.content_type()
            .map(|ct| ct.contains("application/json"))
            .unwrap_or(false)
    }

    /// Read a cookie by name from the `Cookie` header.
    pub fn cookie(&self, name: &str) -> Option<String> {
        let header = self.header("cookie")?;
        for pair in header.split(';') {
            let pair = pair.trim();
            if let Some((k, v)) = pair.split_once('=') {
                if k.trim() == name {
                    return Some(v.trim().to_string());
                }
            }
        }
        None
    }
}

/// A response body: either fully buffered, or streamed incrementally.
///
/// Fully-buffered bodies are framed by `Content-Length` and may keep the
/// connection alive. Streaming responses opt out of keep-alive: we omit
/// `Content-Length`, announce `Connection: close`, write the headers, then
/// hand the raw socket to a producer closure that flushes bytes over time.
/// The client reads until the connection closes (a valid HTTP/1.1 framing).
/// No chunked encoding, no async.
/// Producer closure for a streaming [`Body`]: hands the raw socket to the
/// caller, which flushes bytes over time until it returns (and the connection
/// closes).
pub type StreamProducer = Box<dyn FnOnce(&mut dyn Write) -> io::Result<()> + Send + 'static>;

/// Takeover closure for a protocol upgrade (WebSocket): receives the raw
/// socket plus any bytes the client pipelined after its upgrade request that
/// the request parser had already buffered. The closure MUST NOT block — it
/// should hand the socket to an event loop and return, freeing this worker
/// thread. That handoff is what lets one process hold hundreds of thousands
/// of live sockets while the HTTP side stays thread-per-connection.
pub type UpgradeTakeover = Box<dyn FnOnce(TcpStream, Vec<u8>) + Send + 'static>;

pub enum Body {
    Full(Vec<u8>),
    Stream(StreamProducer),
    Upgrade(UpgradeTakeover),
}

/// An HTTP response.
pub struct Response {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Body,
}

impl Response {
    pub fn new(status: u16) -> Response {
        Response {
            status,
            headers: Vec::new(),
            body: Body::Full(Vec::new()),
        }
    }

    pub fn with_header(mut self, name: &str, value: &str) -> Response {
        self.headers.push((name.to_string(), value.to_string()));
        self
    }

    pub fn with_body(mut self, body: impl Into<Vec<u8>>) -> Response {
        self.body = Body::Full(body.into());
        self
    }

    /// Stream the body: `producer` is given the raw writer and flushes chunks
    /// as they become available. Prefer the higher-level `StreamSink` /
    /// `SseSink` wrappers (see `sutegi-web`'s `stream()` / `sse()`).
    pub fn with_stream(
        mut self,
        producer: impl FnOnce(&mut dyn Write) -> io::Result<()> + Send + 'static,
    ) -> Response {
        self.body = Body::Stream(Box::new(producer));
        self
    }

    /// A `101 Switching Protocols` response: the server writes the status
    /// line + `headers`, then detaches the connection from the HTTP loop and
    /// hands the raw socket (plus any already-buffered bytes) to `takeover`.
    pub fn upgrade(takeover: impl FnOnce(TcpStream, Vec<u8>) + Send + 'static) -> Response {
        Response {
            status: 101,
            headers: Vec::new(),
            body: Body::Upgrade(Box::new(takeover)),
        }
    }

    /// Whether this response streams (no `Content-Length`).
    pub fn is_stream(&self) -> bool {
        matches!(self.body, Body::Stream(_))
    }

    /// Whether this response upgrades the connection (`Body::Upgrade`).
    pub fn is_upgrade(&self) -> bool {
        matches!(self.body, Body::Upgrade(_))
    }
}

/// A flushing sink for raw streamed bytes. Every `write` is flushed so the
/// client sees data immediately.
pub struct StreamSink<'a> {
    w: &'a mut dyn Write,
}

impl<'a> StreamSink<'a> {
    pub fn new(w: &'a mut dyn Write) -> StreamSink<'a> {
        StreamSink { w }
    }

    pub fn write(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.w.write_all(bytes)?;
        self.w.flush()
    }

    pub fn write_str(&mut self, s: &str) -> io::Result<()> {
        self.write(s.as_bytes())
    }
}

/// A flushing sink that formats Server-Sent Events (`text/event-stream`).
/// Each call emits one event frame and flushes — exactly what LLM token
/// streaming wants.
pub struct SseSink<'a> {
    w: &'a mut dyn Write,
}

impl<'a> SseSink<'a> {
    pub fn new(w: &'a mut dyn Write) -> SseSink<'a> {
        SseSink { w }
    }

    /// Send a `data:` event (multi-line data is split across `data:` lines).
    pub fn data(&mut self, data: &str) -> io::Result<()> {
        for line in data.split('\n') {
            writeln!(self.w, "data: {}", line)?;
        }
        self.w.write_all(b"\n")?;
        self.w.flush()
    }

    /// Send a named event.
    pub fn event(&mut self, event: &str, data: &str) -> io::Result<()> {
        writeln!(self.w, "event: {}", event)?;
        for line in data.split('\n') {
            writeln!(self.w, "data: {}", line)?;
        }
        self.w.write_all(b"\n")?;
        self.w.flush()
    }

    /// Send a comment line (`: ...`) — useful as a keep-alive heartbeat.
    pub fn comment(&mut self, text: &str) -> io::Result<()> {
        write!(self.w, ": {}\n\n", text)?;
        self.w.flush()
    }

    /// Suggest a client reconnection delay (ms).
    pub fn retry(&mut self, millis: u64) -> io::Result<()> {
        write!(self.w, "retry: {}\n\n", millis)?;
        self.w.flush()
    }
}

/// Server resource limits — the difference between "demo" and "won't fall over".
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    /// Reject request bodies larger than this (HTTP 413). Default 2 MiB.
    pub max_body: usize,
    /// Reject requests whose headers exceed this many bytes (HTTP 413).
    pub max_header_bytes: usize,
    /// Per-socket read/write timeout (slowloris protection). Default 30s.
    pub timeout: Option<Duration>,
    /// How long a kept-alive connection may sit idle between requests before
    /// the worker hangs up. Deliberately much shorter than `timeout`: in the
    /// blocking thread-per-connection model an idle keep-alive connection
    /// pins a worker thread. Default 5s.
    pub keep_alive_idle: Duration,
    /// Maximum number of requests served over one connection before the
    /// server closes it (resource-pinning bound). Default 100.
    pub keep_alive_max: usize,
}

impl Default for Limits {
    fn default() -> Limits {
        Limits {
            max_body: 2 * 1024 * 1024,
            max_header_bytes: 64 * 1024,
            timeout: Some(Duration::from_secs(30)),
            keep_alive_idle: Duration::from_secs(5),
            keep_alive_max: 100,
        }
    }
}

/// The outcome of parsing: a request, or a refusal the server turns into 413.
pub enum Incoming {
    Request(Request),
    TooLarge,
}

/// Outcome of a single capped line read.
enum Line {
    /// A line of `usize` bytes (terminator included) sits in the buffer.
    Read(usize),
    /// The peer closed with nothing buffered.
    Eof,
    /// The line would exceed the remaining header budget.
    TooLarge,
}

/// Read one `\n`-terminated line into `buf` (cleared first) without ever
/// buffering more than `max` bytes. A plain [`BufRead::read_line`] on a stream
/// that never sends a newline allocates without bound — a memory DoS reachable
/// before any post-read length check can fire — so the read itself is capped.
fn read_line_capped<R: BufRead>(reader: &mut R, buf: &mut Vec<u8>, max: usize) -> io::Result<Line> {
    buf.clear();
    loop {
        let chunk = match reader.fill_buf() {
            Ok(c) => c,
            Err(ref e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        if chunk.is_empty() {
            break; // EOF
        }
        let (take, done) = match chunk.iter().position(|&b| b == b'\n') {
            Some(i) => (i + 1, true),
            None => (chunk.len(), false),
        };
        if buf.len() + take > max {
            return Ok(Line::TooLarge);
        }
        buf.extend_from_slice(&chunk[..take]);
        reader.consume(take);
        if done {
            break;
        }
    }
    if buf.is_empty() {
        Ok(Line::Eof)
    } else {
        Ok(Line::Read(buf.len()))
    }
}

/// Decode a header/request line as UTF-8, mirroring `read_line`'s contract of
/// rejecting invalid UTF-8 rather than lossily mangling it.
fn decode_line(buf: &[u8]) -> io::Result<&str> {
    std::str::from_utf8(buf).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "request line was not valid UTF-8",
        )
    })
}

/// Parse a single request off a buffered stream, enforcing `limits`. Returns
/// `Ok(None)` if the peer closed before sending anything, or
/// `Ok(Some(Incoming::TooLarge))` if headers/body exceed the limits (so the
/// server can reply 413 without allocating an attacker-chosen buffer).
pub fn parse_request<R: BufRead>(reader: &mut R, limits: &Limits) -> io::Result<Option<Incoming>> {
    // One byte buffer, reused for the request line and every header line —
    // this function runs per request, so allocation churn is latency.
    let mut buf: Vec<u8> = Vec::with_capacity(128);

    // Request line. Bounded by the header budget: a plain `read_line` on a
    // stream that never sends a newline would buffer without limit (a memory
    // DoS), so the request line and each header line are read through a
    // capped reader that stops at `max_header_bytes`.
    match read_line_capped(reader, &mut buf, limits.max_header_bytes)? {
        Line::Eof => return Ok(None),
        Line::TooLarge => return Ok(Some(Incoming::TooLarge)),
        Line::Read(_) => {}
    }
    // Parse the request line into owned pieces before `buf` is reused below.
    let (method, path, query, version, mut header_bytes) = {
        let req_line = decode_line(&buf)?;
        let header_bytes = req_line.len();
        let mut parts = req_line.split_whitespace();
        let method = Method::parse(parts.next().unwrap_or(""));
        let target = parts.next().unwrap_or("/");
        let version = parts.next().unwrap_or("HTTP/1.1").to_string();
        let (path, query) = match target.split_once('?') {
            Some((p, q)) => (p.to_string(), q.to_string()),
            None => (target.to_string(), String::new()),
        };
        (method, path, query, version, header_bytes)
    };

    let mut headers = Vec::with_capacity(8);
    let mut content_length = 0usize;
    loop {
        let remaining = limits.max_header_bytes.saturating_sub(header_bytes);
        let n = match read_line_capped(reader, &mut buf, remaining)? {
            Line::Eof => break,
            Line::TooLarge => return Ok(Some(Incoming::TooLarge)),
            Line::Read(n) => n,
        };
        header_bytes += n;
        let line = decode_line(&buf)?;
        let l = line.trim_end_matches(['\r', '\n']);
        if l.is_empty() {
            break;
        }
        if let Some((k, v)) = l.split_once(':') {
            let k = k.trim().to_string();
            let v = v.trim().to_string();
            if k.eq_ignore_ascii_case("content-length") {
                content_length = v.parse().unwrap_or(0);
            }
            headers.push((k, v));
        }
    }

    // Refuse oversized bodies before allocating an attacker-controlled buffer.
    if content_length > limits.max_body {
        return Ok(Some(Incoming::TooLarge));
    }
    let mut body = vec![0u8; content_length];
    if content_length > 0 {
        reader.read_exact(&mut body)?;
    }

    Ok(Some(Incoming::Request(Request {
        method,
        path,
        query,
        version,
        headers,
        body,
        peer: None,
    })))
}

/// Write a response to the stream. `keep_alive` controls the `Connection`
/// header on fully-buffered bodies (framed by `Content-Length`); streaming
/// bodies always announce `Connection: close`, because their framing IS the
/// close. Takes the response by value so a streaming body's `FnOnce` producer
/// can be invoked.
pub fn write_response<W: Write>(w: &mut W, resp: Response, keep_alive: bool) -> io::Result<()> {
    let reason = status_reason(resp.status);
    let mut head = format!("HTTP/1.1 {} {}\r\n", resp.status, reason);
    let mut has_content_type = false;
    for (k, v) in &resp.headers {
        if k.eq_ignore_ascii_case("content-type") {
            has_content_type = true;
        }
        head.push_str(&format!("{}: {}\r\n", k, v));
    }
    if !has_content_type {
        head.push_str("content-type: text/plain; charset=utf-8\r\n");
    }

    match resp.body {
        Body::Full(bytes) => {
            head.push_str(&format!("content-length: {}\r\n", bytes.len()));
            head.push_str(if keep_alive {
                "connection: keep-alive\r\n\r\n"
            } else {
                "connection: close\r\n\r\n"
            });
            // One write per response when the body is small (the common API
            // case): a single TCP segment instead of a head segment + a body
            // segment, which interacts badly with Nagle + delayed ACK. Large
            // bodies aren't worth the extra copy.
            if bytes.len() <= 16 * 1024 {
                let mut out = head.into_bytes();
                out.extend_from_slice(&bytes);
                w.write_all(&out)?;
            } else {
                w.write_all(head.as_bytes())?;
                w.write_all(&bytes)?;
            }
            w.flush()
        }
        Body::Stream(producer) => {
            // No content-length: framing is "read until close".
            head.push_str("connection: close\r\n\r\n");
            w.write_all(head.as_bytes())?;
            w.flush()?;
            producer(w)
        }
        // Upgrades need the raw `TcpStream`, which only the server loop has:
        // `handle_connection` intercepts them before ever calling this
        // function. Reaching this arm means an upgrade response was written
        // to a plain writer (a test, a proxy layer) — emit the head so the
        // output is still valid HTTP, and drop the takeover.
        Body::Upgrade(_) => {
            head.push_str("\r\n");
            w.write_all(head.as_bytes())?;
            w.flush()
        }
    }
}

/// Write the head of an upgrade response: status line + headers + blank line,
/// with no content-type/length/connection defaults injected (RFC 6455 clients
/// reject a 101 carrying unexpected framing headers).
fn write_upgrade_head<W: Write>(
    w: &mut W,
    status: u16,
    headers: &[(String, String)],
) -> io::Result<()> {
    let mut head = format!("HTTP/1.1 {} {}\r\n", status, status_reason(status));
    for (k, v) in headers {
        head.push_str(k);
        head.push_str(": ");
        head.push_str(v);
        head.push_str("\r\n");
    }
    head.push_str("\r\n");
    w.write_all(head.as_bytes())?;
    w.flush()
}

/// Whether the client asked to keep the connection open: HTTP/1.1 defaults to
/// keep-alive unless `Connection: close`; HTTP/1.0 defaults to close unless
/// `Connection: keep-alive`.
fn wants_keep_alive(req: &Request) -> bool {
    let conn = req.header("connection").map(str::to_ascii_lowercase);
    if req.version.eq_ignore_ascii_case("HTTP/1.0") {
        matches!(conn.as_deref(), Some(c) if c.contains("keep-alive"))
    } else {
        !matches!(conn.as_deref(), Some(c) if c.contains("close"))
    }
}

/// Map a status code to its canonical reason phrase.
pub fn status_reason(status: u16) -> &'static str {
    match status {
        100 => "Continue",
        101 => "Switching Protocols",
        200 => "OK",
        201 => "Created",
        202 => "Accepted",
        204 => "No Content",
        301 => "Moved Permanently",
        302 => "Found",
        303 => "See Other",
        304 => "Not Modified",
        307 => "Temporary Redirect",
        308 => "Permanent Redirect",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        409 => "Conflict",
        422 => "Unprocessable Entity",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        501 => "Not Implemented",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        504 => "Gateway Timeout",
        // Fall back to a class-appropriate phrase rather than a misleading "OK".
        s if (100..200).contains(&s) => "Informational",
        s if (200..300).contains(&s) => "OK",
        s if (300..400).contains(&s) => "Redirect",
        s if (400..500).contains(&s) => "Client Error",
        _ => "Server Error",
    }
}

/// Bind to `addr` and serve requests with `handler` until the process exits.
/// `handler` is shared across worker threads, so it must be `Send + Sync`.
pub fn serve<H>(addr: &str, workers: usize, limits: Limits, handler: H) -> io::Result<()>
where
    H: Fn(Request) -> Response + Send + Sync + 'static,
{
    let listener = TcpListener::bind(addr)?;
    let handler = Arc::new(handler);
    let pool = ThreadPool::new(workers.max(1));

    for stream in listener.incoming() {
        let stream = match stream {
            Ok(s) => s,
            Err(_) => continue,
        };
        // HTTP responses are written whole; never wait for a delayed ACK.
        let _ = stream.set_nodelay(true);
        let handler = Arc::clone(&handler);
        pool.execute(move || {
            let _ = handle_connection(stream, &*handler, &limits);
        });
    }
    Ok(())
}

/// Like [`serve`], but stops accepting new connections once `shutdown` is set,
/// then drains in-flight requests (by dropping the pool, which joins workers)
/// and returns. This is what makes a sutegi process safe to roll in a pod: on
/// SIGTERM you flip the flag, stop taking traffic, and let live requests finish
/// within the termination grace period.
pub fn serve_until<H>(
    addr: &str,
    workers: usize,
    limits: Limits,
    handler: H,
    shutdown: Arc<AtomicBool>,
) -> io::Result<()>
where
    H: Fn(Request) -> Response + Send + Sync + 'static,
{
    let listener = TcpListener::bind(addr)?;
    listener.set_nonblocking(true)?;
    let handler = Arc::new(handler);
    let pool = ThreadPool::new(workers.max(1));

    // Non-blocking accept so the shutdown flag is honored, with an adaptive
    // backoff: hot accepts poll at 250µs (sub-ms accept latency under load),
    // an idle listener decays to 10ms sleeps (~100 wakeups/s, negligible CPU).
    // std has no accept-with-timeout, and poll(2) would cost a libc dep.
    const IDLE_MIN: Duration = Duration::from_micros(250);
    const IDLE_MAX: Duration = Duration::from_millis(10);
    let mut idle = IDLE_MIN;
    while !shutdown.load(Ordering::Relaxed) {
        match listener.accept() {
            Ok((stream, _addr)) => {
                idle = IDLE_MIN;
                // Hand the connection back to blocking mode for the worker.
                let _ = stream.set_nonblocking(false);
                // HTTP responses are written whole; never wait for a delayed ACK.
                let _ = stream.set_nodelay(true);
                let handler = Arc::clone(&handler);
                pool.execute(move || {
                    let _ = handle_connection(stream, &*handler, &limits);
                });
            }
            Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {
                thread::sleep(idle);
                idle = (idle * 2).min(IDLE_MAX);
            }
            Err(_) => {}
        }
    }

    // Dropping the pool closes the job channel and joins workers, so any
    // in-flight requests run to completion before we return.
    drop(pool);
    Ok(())
}

fn handle_connection<H>(stream: TcpStream, handler: &H, limits: &Limits) -> io::Result<()>
where
    H: Fn(Request) -> Response,
{
    // Slowloris protection: bound how long a slow client can hold this worker.
    if let Some(t) = limits.timeout {
        let _ = stream.set_read_timeout(Some(t));
        let _ = stream.set_write_timeout(Some(t));
    }
    let peer = stream.peer_addr().ok().map(|a| a.to_string());
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut writer = stream;

    // HTTP/1.1 keep-alive: serve requests off this connection until the
    // client hangs up, asks to close, streams (close-framed), idles past
    // `keep_alive_idle`, or hits the per-connection request cap.
    for served in 0..limits.keep_alive_max.max(1) {
        if served == 1 {
            // Between requests an open connection pins this worker thread, so
            // wait for the next request under the (much shorter) idle timeout.
            let _ = writer.set_read_timeout(Some(limits.keep_alive_idle));
        }
        match parse_request(&mut reader, limits) {
            Ok(Some(Incoming::Request(mut req))) => {
                req.peer = peer.clone();
                let keep = wants_keep_alive(&req);
                // Panic isolation: a panicking handler returns 500 instead of
                // silently killing this worker thread.
                let resp = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| handler(req)))
                    .unwrap_or_else(|_| {
                        Response::new(500).with_body(&b"500 Internal Server Error"[..])
                    });
                if let Body::Upgrade(takeover) = resp.body {
                    // Protocol upgrade: write the 101 head, then detach. Any
                    // bytes the client pipelined after its request (e.g. the
                    // first WebSocket frames) are sitting in the parser's
                    // buffer — recover them so the new protocol sees them.
                    write_upgrade_head(&mut writer, resp.status, &resp.headers)?;
                    let leftover = reader.buffer().to_vec();
                    drop(reader); // closes the try_clone'd read handle
                                  // Upgraded sockets are event-loop-managed; per-op
                                  // timeouts belong to the blocking HTTP path.
                    let _ = writer.set_read_timeout(None);
                    let _ = writer.set_write_timeout(None);
                    takeover(writer, leftover);
                    return Ok(());
                }
                let persist = keep && !resp.is_stream() && served + 1 < limits.keep_alive_max;
                write_response(&mut writer, resp, persist)?;
                if !persist {
                    return Ok(());
                }
            }
            Ok(Some(Incoming::TooLarge)) => {
                return write_response(
                    &mut writer,
                    Response::new(413).with_body(&b"413 Payload Too Large"[..]),
                    false,
                );
            }
            // Peer closed between requests: a normal keep-alive hang-up.
            Ok(None) => return Ok(()),
            // Idle timeout while waiting for the next request: hang up quietly.
            Err(e)
                if served > 0
                    && matches!(
                        e.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                    ) =>
            {
                return Ok(());
            }
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

// ---- thread pool ----------------------------------------------------------

type Job = Box<dyn FnOnce() + Send + 'static>;

/// A fixed-size pool of worker threads pulling jobs off a shared channel.
pub struct ThreadPool {
    sender: Option<mpsc::Sender<Job>>,
    workers: Vec<thread::JoinHandle<()>>,
}

impl ThreadPool {
    pub fn new(size: usize) -> ThreadPool {
        let (sender, receiver) = mpsc::channel::<Job>();
        let receiver = Arc::new(Mutex::new(receiver));
        let mut workers = Vec::with_capacity(size);
        for _ in 0..size {
            let receiver = Arc::clone(&receiver);
            workers.push(thread::spawn(move || loop {
                let job = receiver.lock().unwrap().recv();
                match job {
                    Ok(job) => job(),
                    Err(_) => break, // channel closed: shut down
                }
            }));
        }
        ThreadPool {
            sender: Some(sender),
            workers,
        }
    }

    pub fn execute<F>(&self, f: F)
    where
        F: FnOnce() + Send + 'static,
    {
        if let Some(sender) = &self.sender {
            let _ = sender.send(Box::new(f));
        }
    }
}

impl Drop for ThreadPool {
    fn drop(&mut self) {
        // Dropping the sender closes the channel, so workers exit their loop.
        drop(self.sender.take());
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_request_with_body() {
        let raw = "POST /todos?x=1 HTTP/1.1\r\nHost: localhost\r\nContent-Length: 5\r\n\r\nhello";
        let mut reader = BufReader::new(raw.as_bytes());
        let req = match parse_request(&mut reader, &Limits::default())
            .unwrap()
            .unwrap()
        {
            Incoming::Request(r) => r,
            Incoming::TooLarge => panic!("unexpected 413"),
        };
        assert_eq!(req.method, Method::Post);
        assert_eq!(req.path, "/todos");
        assert_eq!(req.query, "x=1");
        assert_eq!(req.body, b"hello");
        assert_eq!(req.header("host"), Some("localhost"));
    }

    #[test]
    fn writes_response_with_default_content_type() {
        let resp = Response::new(200).with_body("hi");
        let mut buf = Vec::new();
        write_response(&mut buf, resp, false).unwrap();
        let s = String::from_utf8(buf).unwrap();
        assert!(s.starts_with("HTTP/1.1 200 OK\r\n"));
        assert!(s.contains("content-length: 2\r\n"));
        assert!(s.ends_with("\r\n\r\nhi"));
    }

    #[test]
    fn rejects_oversized_body() {
        let raw = "POST / HTTP/1.1\r\nContent-Length: 5000\r\n\r\n";
        let mut reader = BufReader::new(raw.as_bytes());
        let limits = Limits {
            max_body: 100,
            ..Limits::default()
        };
        assert!(matches!(
            parse_request(&mut reader, &limits).unwrap(),
            Some(Incoming::TooLarge)
        ));
    }

    #[test]
    fn request_content_type_and_cookies() {
        let raw = "GET / HTTP/1.1\r\nContent-Type: application/json\r\nCookie: sid=abc; theme=dark\r\n\r\n";
        let mut reader = BufReader::new(raw.as_bytes());
        let req = match parse_request(&mut reader, &Limits::default())
            .unwrap()
            .unwrap()
        {
            Incoming::Request(r) => r,
            Incoming::TooLarge => panic!("unexpected 413"),
        };
        assert!(req.is_json());
        assert_eq!(req.cookie("sid").as_deref(), Some("abc"));
        assert_eq!(req.cookie("theme").as_deref(), Some("dark"));
        assert_eq!(req.cookie("missing"), None);
    }

    #[test]
    fn streams_without_content_length() {
        let resp = Response::new(200)
            .with_header("content-type", "text/event-stream")
            .with_stream(|w| {
                let mut sink = SseSink::new(w);
                sink.data("one")?;
                sink.data("two")?;
                Ok(())
            });
        let mut buf = Vec::new();
        write_response(&mut buf, resp, false).unwrap();
        let s = String::from_utf8(buf).unwrap();
        assert!(s.contains("content-type: text/event-stream\r\n"));
        assert!(!s.to_lowercase().contains("content-length"));
        assert!(s.contains("data: one\n\n"));
        assert!(s.contains("data: two\n\n"));
    }

    #[test]
    fn method_parse_and_as_str_roundtrip() {
        for (s, m) in [
            ("GET", Method::Get),
            ("POST", Method::Post),
            ("PUT", Method::Put),
            ("PATCH", Method::Patch),
            ("DELETE", Method::Delete),
            ("HEAD", Method::Head),
            ("OPTIONS", Method::Options),
        ] {
            assert_eq!(Method::parse(s), m);
            assert_eq!(m.as_str(), s);
        }
        assert_eq!(Method::parse("BREW"), Method::Other);
    }

    #[test]
    fn status_reason_known_and_fallbacks() {
        assert_eq!(status_reason(200), "OK");
        assert_eq!(status_reason(404), "Not Found");
        assert_eq!(status_reason(422), "Unprocessable Entity");
        // Unknown codes fall back to a class-appropriate phrase, never a wrong "OK".
        assert_eq!(status_reason(299), "OK");
        assert_eq!(status_reason(399), "Redirect");
        assert_eq!(status_reason(418), "Client Error");
        assert_eq!(status_reason(599), "Server Error");
    }

    #[test]
    fn peer_ip_handles_ipv4_and_ipv6() {
        let mk = |peer: &str| Request {
            method: Method::Get,
            path: "/".into(),
            query: String::new(),
            version: "HTTP/1.1".into(),
            headers: vec![],
            body: vec![],
            peer: Some(peer.into()),
        };
        assert_eq!(mk("1.2.3.4:55000").peer_ip().as_deref(), Some("1.2.3.4"));
        assert_eq!(mk("[::1]:8080").peer_ip().as_deref(), Some("[::1]"));
        // No peer at all → None.
        let mut r = mk("x");
        r.peer = None;
        assert_eq!(r.peer_ip(), None);
    }

    #[test]
    fn rejects_oversized_headers() {
        let big = "X-Pad: ".to_string() + &"a".repeat(500) + "\r\n";
        let raw = format!("GET / HTTP/1.1\r\n{}\r\n", big);
        let mut reader = BufReader::new(raw.as_bytes());
        let limits = Limits {
            max_header_bytes: 100,
            ..Limits::default()
        };
        assert!(matches!(
            parse_request(&mut reader, &limits).unwrap(),
            Some(Incoming::TooLarge)
        ));
    }

    #[test]
    fn empty_stream_yields_none() {
        let mut reader = BufReader::new(&b""[..]);
        assert!(parse_request(&mut reader, &Limits::default())
            .unwrap()
            .is_none());
    }

    #[test]
    fn explicit_content_type_is_not_overridden() {
        let resp = Response::new(200)
            .with_header("content-type", "application/json")
            .with_body("{}");
        let mut buf = Vec::new();
        write_response(&mut buf, resp, false).unwrap();
        let s = String::from_utf8(buf).unwrap();
        assert!(s.contains("content-type: application/json\r\n"));
        // The default text/plain header must not also be appended.
        assert!(!s.contains("text/plain"));
    }

    #[test]
    fn stream_sink_writes_and_flushes() {
        let mut buf = Vec::new();
        {
            let mut sink = StreamSink::new(&mut buf);
            sink.write_str("chunk-").unwrap();
            sink.write(b"bytes").unwrap();
        }
        assert_eq!(buf, b"chunk-bytes");
    }

    #[test]
    fn sse_named_event_and_comment() {
        let mut buf = Vec::new();
        {
            let mut sink = SseSink::new(&mut buf);
            sink.event("tick", "1\n2").unwrap(); // multi-line data splits across data: lines
            sink.comment("keep-alive").unwrap();
            sink.retry(3000).unwrap();
        }
        let s = String::from_utf8(buf).unwrap();
        assert!(s.contains("event: tick\n"));
        assert!(s.contains("data: 1\ndata: 2\n\n"));
        assert!(s.contains(": keep-alive\n\n"));
        assert!(s.contains("retry: 3000\n\n"));
    }

    #[test]
    fn keep_alive_negotiation() {
        let mk = |version: &str, conn: Option<&str>| Request {
            method: Method::Get,
            path: "/".into(),
            query: String::new(),
            version: version.into(),
            headers: conn
                .map(|c| vec![("connection".to_string(), c.to_string())])
                .unwrap_or_default(),
            body: vec![],
            peer: None,
        };
        // HTTP/1.1: keep-alive unless the client says close.
        assert!(wants_keep_alive(&mk("HTTP/1.1", None)));
        assert!(wants_keep_alive(&mk("HTTP/1.1", Some("keep-alive"))));
        assert!(!wants_keep_alive(&mk("HTTP/1.1", Some("close"))));
        assert!(!wants_keep_alive(&mk("HTTP/1.1", Some("Close"))));
        // HTTP/1.0: close unless the client asks to keep alive.
        assert!(!wants_keep_alive(&mk("HTTP/1.0", None)));
        assert!(wants_keep_alive(&mk("HTTP/1.0", Some("Keep-Alive"))));
    }

    #[test]
    fn keep_alive_response_header() {
        let mut buf = Vec::new();
        write_response(&mut buf, Response::new(200).with_body("hi"), true).unwrap();
        let s = String::from_utf8(buf).unwrap();
        assert!(s.contains("connection: keep-alive\r\n"));
        assert!(s.contains("content-length: 2\r\n"));
    }

    #[test]
    fn streaming_response_always_closes() {
        let resp = Response::new(200).with_stream(|w| w.write_all(b"x"));
        let mut buf = Vec::new();
        // Even when the caller asks for keep-alive: framing is the close.
        write_response(&mut buf, resp, true).unwrap();
        let s = String::from_utf8(buf).unwrap();
        assert!(s.contains("connection: close\r\n"));
    }

    #[test]
    fn serves_multiple_requests_on_one_connection() {
        use std::io::Read;
        let addr = "127.0.0.1:18461";
        let shutdown = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&shutdown);
        let server = thread::spawn(move || {
            serve_until(
                addr,
                2,
                Limits::default(),
                |_req| Response::new(200).with_body("ok"),
                flag,
            )
        });
        // Wait for the listener.
        for _ in 0..100 {
            if TcpStream::connect(addr).is_ok() {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }

        // Read one full response: headers, then the 2-byte "ok" body.
        let read_response = |stream: &mut TcpStream| {
            let mut buf = Vec::new();
            let mut chunk = [0u8; 512];
            loop {
                let n = stream.read(&mut chunk).unwrap();
                assert!(n > 0, "connection closed mid-response");
                buf.extend_from_slice(&chunk[..n]);
                if let Some(head_end) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                    if buf.len() >= head_end + 4 + 2 {
                        return String::from_utf8_lossy(&buf).into_owned();
                    }
                }
            }
        };

        let mut stream = TcpStream::connect(addr).unwrap();
        for i in 0..3 {
            stream
                .write_all(b"GET / HTTP/1.1\r\nHost: x\r\n\r\n")
                .unwrap();
            let s = read_response(&mut stream);
            assert!(s.starts_with("HTTP/1.1 200"), "request {i} failed: {s}");
            assert!(s.contains("connection: keep-alive"));
        }
        // A Connection: close request ends the session.
        stream
            .write_all(b"GET / HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n")
            .unwrap();
        let mut rest = Vec::new();
        stream.read_to_end(&mut rest).unwrap();
        assert!(String::from_utf8_lossy(&rest).contains("connection: close"));

        shutdown.store(true, Ordering::Relaxed);
        server.join().unwrap().unwrap();
    }

    #[test]
    fn upgrade_detaches_socket_with_pipelined_bytes() {
        use std::io::Read;
        let addr = "127.0.0.1:18462";
        let shutdown = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&shutdown);
        let server = thread::spawn(move || {
            serve_until(
                addr,
                2,
                Limits::default(),
                |_req| {
                    Response::upgrade(|mut stream, leftover| {
                        // Echo whatever the client pipelined behind the
                        // request, then speak the "new protocol" directly.
                        let _ = stream.write_all(b"echo:");
                        let _ = stream.write_all(&leftover);
                        let _ = stream.write_all(b":done");
                    })
                    .with_header("upgrade", "test-proto")
                    .with_header("connection", "Upgrade")
                },
                flag,
            )
        });
        for _ in 0..100 {
            if TcpStream::connect(addr).is_ok() {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }

        let mut stream = TcpStream::connect(addr).unwrap();
        // Upgrade request with bytes of the next protocol pipelined behind it.
        stream
            .write_all(b"GET /ws HTTP/1.1\r\nHost: x\r\nUpgrade: test-proto\r\n\r\nHELLO")
            .unwrap();
        let mut buf = Vec::new();
        stream.read_to_end(&mut buf).unwrap();
        let s = String::from_utf8_lossy(&buf);
        assert!(s.starts_with("HTTP/1.1 101 "), "got: {s}");
        assert!(s.contains("upgrade: test-proto\r\n"));
        // No framing headers on a 101.
        assert!(!s.to_lowercase().contains("content-length"));
        assert!(!s.to_lowercase().contains("content-type"));
        // The pipelined bytes reached the takeover closure intact.
        assert!(s.ends_with("echo:HELLO:done"), "got: {s}");

        shutdown.store(true, Ordering::Relaxed);
        server.join().unwrap().unwrap();
    }

    #[test]
    fn thread_pool_runs_jobs() {
        use std::sync::atomic::{AtomicU32, Ordering};
        let count = Arc::new(AtomicU32::new(0));
        {
            let pool = ThreadPool::new(3);
            for _ in 0..30 {
                let c = Arc::clone(&count);
                pool.execute(move || {
                    c.fetch_add(1, Ordering::Relaxed);
                });
            }
            // Dropping the pool joins all workers, so every job has run.
        }
        assert_eq!(count.load(Ordering::Relaxed), 30);
    }
}
