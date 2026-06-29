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
}

impl Request {
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
/// Streaming leans on the connection-per-request, `Connection: close` model:
/// we omit `Content-Length`, write the headers, then hand the raw socket to a
/// producer closure that flushes bytes over time. The client reads until the
/// connection closes (a valid HTTP/1.1 framing). No chunked encoding, no async.
pub enum Body {
    Full(Vec<u8>),
    Stream(Box<dyn FnOnce(&mut dyn Write) -> io::Result<()> + Send + 'static>),
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

    /// Whether this response streams (no `Content-Length`).
    pub fn is_stream(&self) -> bool {
        matches!(self.body, Body::Stream(_))
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
            write!(self.w, "data: {}\n", line)?;
        }
        self.w.write_all(b"\n")?;
        self.w.flush()
    }

    /// Send a named event.
    pub fn event(&mut self, event: &str, data: &str) -> io::Result<()> {
        write!(self.w, "event: {}\n", event)?;
        for line in data.split('\n') {
            write!(self.w, "data: {}\n", line)?;
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

/// Parse a single request off a buffered stream. Returns `Ok(None)` if the
/// peer closed the connection before sending anything.
pub fn parse_request<R: BufRead>(reader: &mut R) -> io::Result<Option<Request>> {
    let mut request_line = String::new();
    if reader.read_line(&mut request_line)? == 0 {
        return Ok(None);
    }
    let mut parts = request_line.trim_end().split_whitespace();
    let method = Method::parse(parts.next().unwrap_or(""));
    let target = parts.next().unwrap_or("/").to_string();
    let version = parts.next().unwrap_or("HTTP/1.1").to_string();

    let (path, query) = match target.split_once('?') {
        Some((p, q)) => (p.to_string(), q.to_string()),
        None => (target, String::new()),
    };

    let mut headers = Vec::new();
    let mut content_length = 0usize;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            break;
        }
        let line = line.trim_end_matches(['\r', '\n']);
        if line.is_empty() {
            break;
        }
        if let Some((k, v)) = line.split_once(':') {
            let k = k.trim().to_string();
            let v = v.trim().to_string();
            if k.eq_ignore_ascii_case("content-length") {
                content_length = v.parse().unwrap_or(0);
            }
            headers.push((k, v));
        }
    }

    let mut body = vec![0u8; content_length];
    if content_length > 0 {
        reader.read_exact(&mut body)?;
    }

    Ok(Some(Request {
        method,
        path,
        query,
        version,
        headers,
        body,
    }))
}

/// Write a response to the stream. Always closes the connection (no keep-alive)
/// to keep the server stateless and simple. Takes the response by value so a
/// streaming body's `FnOnce` producer can be invoked.
pub fn write_response<W: Write>(w: &mut W, resp: Response) -> io::Result<()> {
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
            head.push_str("connection: close\r\n\r\n");
            w.write_all(head.as_bytes())?;
            w.write_all(&bytes)?;
            w.flush()
        }
        Body::Stream(producer) => {
            // No content-length: framing is "read until close".
            head.push_str("connection: close\r\n\r\n");
            w.write_all(head.as_bytes())?;
            w.flush()?;
            producer(w)
        }
    }
}

/// Map a status code to its canonical reason phrase.
pub fn status_reason(status: u16) -> &'static str {
    match status {
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
        s if (200..300).contains(&s) => "OK",
        s if (300..400).contains(&s) => "Redirect",
        s if (400..500).contains(&s) => "Client Error",
        _ => "Server Error",
    }
}

/// Bind to `addr` and serve requests with `handler` until the process exits.
/// `handler` is shared across worker threads, so it must be `Send + Sync`.
pub fn serve<H>(addr: &str, workers: usize, handler: H) -> io::Result<()>
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
        let handler = Arc::clone(&handler);
        pool.execute(move || {
            let _ = handle_connection(stream, &*handler);
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

    while !shutdown.load(Ordering::Relaxed) {
        match listener.accept() {
            Ok((stream, _addr)) => {
                // Hand the connection back to blocking mode for the worker.
                let _ = stream.set_nonblocking(false);
                let handler = Arc::clone(&handler);
                pool.execute(move || {
                    let _ = handle_connection(stream, &*handler);
                });
            }
            Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(50));
            }
            Err(_) => {}
        }
    }

    // Dropping the pool closes the job channel and joins workers, so any
    // in-flight requests run to completion before we return.
    drop(pool);
    Ok(())
}

fn handle_connection<H>(stream: TcpStream, handler: &H) -> io::Result<()>
where
    H: Fn(Request) -> Response,
{
    let mut reader = BufReader::new(stream.try_clone()?);
    if let Some(req) = parse_request(&mut reader)? {
        let resp = handler(req);
        let mut writer = stream;
        write_response(&mut writer, resp)?;
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
        let req = parse_request(&mut reader).unwrap().unwrap();
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
        write_response(&mut buf, resp).unwrap();
        let s = String::from_utf8(buf).unwrap();
        assert!(s.starts_with("HTTP/1.1 200 OK\r\n"));
        assert!(s.contains("content-length: 2\r\n"));
        assert!(s.ends_with("\r\n\r\nhi"));
    }

    #[test]
    fn request_content_type_and_cookies() {
        let raw = "GET / HTTP/1.1\r\nContent-Type: application/json\r\nCookie: sid=abc; theme=dark\r\n\r\n";
        let mut reader = BufReader::new(raw.as_bytes());
        let req = parse_request(&mut reader).unwrap().unwrap();
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
        write_response(&mut buf, resp).unwrap();
        let s = String::from_utf8(buf).unwrap();
        assert!(s.contains("content-type: text/event-stream\r\n"));
        assert!(!s.to_lowercase().contains("content-length"));
        assert!(s.contains("data: one\n\n"));
        assert!(s.contains("data: two\n\n"));
    }
}
