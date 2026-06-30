//! The web spine: a route table, middleware chain, ergonomic extractors, and
//! the runtime **introspection registry** that makes a sutegi app
//! self-describing.
//!
//! Every route is registered with a human/agent-readable doc string. Models
//! and tools register their schemas. At runtime, `GET /__introspect` returns
//! the full surface of the application as JSON — so an AI agent can discover
//! what the app can do without ever reading the source.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::sync::Arc;

use std::time::Duration;
pub use sutegi_http::{Body, Limits, Method, Request, Response, SseSink, StreamSink};
use sutegi_json::Json;

/// Path parameters captured from a route pattern (`:name` segments).
pub type Params = BTreeMap<String, String>;

/// A request handler: receives the request and captured params, returns a
/// response. Must be shareable across worker threads.
pub type Handler = Box<dyn Fn(&Request, &Params) -> Response + Send + Sync + 'static>;

/// A middleware: inspects a request before routing. Returning `Some(resp)`
/// short-circuits (e.g. auth rejection); `None` lets the request continue.
pub type Middleware = Box<dyn Fn(&Request) -> Option<Response> + Send + Sync + 'static>;

/// The shareable form of a middleware, used by route groups.
pub type MwFn = dyn Fn(&Request) -> Option<Response> + Send + Sync + 'static;

/// A reference-counted, shareable middleware (build with [`mw`]).
pub type Mw = std::sync::Arc<MwFn>;

/// An after-middleware: transforms the outgoing response (e.g. adds headers).
pub type AfterMiddleware = Box<dyn Fn(&Request, Response) -> Response + Send + Sync + 'static>;

/// Wrap a closure as a shareable group middleware.
pub fn mw(f: impl Fn(&Request) -> Option<Response> + Send + Sync + 'static) -> Mw {
    std::sync::Arc::new(f)
}

struct Route {
    method: Method,
    pattern: String,
    doc: String,
    handler: Handler,
    /// Group-scoped middleware run before this route's handler.
    middleware: Vec<Mw>,
}

/// The application: a builder you configure with routes, models, tools, and
/// middleware, then `run()`.
pub struct App {
    name: String,
    routes: Vec<Route>,
    middleware: Vec<Middleware>,
    models: Vec<Json>,
    tools: Vec<Json>,
    workers: usize,
    readiness: Option<Box<dyn Fn() -> bool + Send + Sync + 'static>>,
    after: Vec<AfterMiddleware>,
    limits: Limits,
}

/// Process-wide request counters, exposed at `/__metrics` in Prometheus text
/// format for pod scraping.
#[derive(Default)]
struct Metrics {
    total: AtomicU64,
    in_flight: AtomicI64,
    c2xx: AtomicU64,
    c4xx: AtomicU64,
    c5xx: AtomicU64,
    other: AtomicU64,
}

impl Metrics {
    fn record(&self, status: u16) {
        self.total.fetch_add(1, Ordering::Relaxed);
        let bucket = match status / 100 {
            2 => &self.c2xx,
            4 => &self.c4xx,
            5 => &self.c5xx,
            _ => &self.other,
        };
        bucket.fetch_add(1, Ordering::Relaxed);
    }

    fn prometheus(&self) -> String {
        let load = |a: &AtomicU64| a.load(Ordering::Relaxed);
        format!(
            "# HELP sutegi_requests_total Total HTTP requests handled.\n\
             # TYPE sutegi_requests_total counter\n\
             sutegi_requests_total {}\n\
             # HELP sutegi_requests_in_flight Requests currently being handled.\n\
             # TYPE sutegi_requests_in_flight gauge\n\
             sutegi_requests_in_flight {}\n\
             # HELP sutegi_responses_total Responses by status class.\n\
             # TYPE sutegi_responses_total counter\n\
             sutegi_responses_total{{class=\"2xx\"}} {}\n\
             sutegi_responses_total{{class=\"4xx\"}} {}\n\
             sutegi_responses_total{{class=\"5xx\"}} {}\n\
             sutegi_responses_total{{class=\"other\"}} {}\n",
            load(&self.total),
            self.in_flight.load(Ordering::Relaxed),
            load(&self.c2xx),
            load(&self.c4xx),
            load(&self.c5xx),
            load(&self.other),
        )
    }
}

impl App {
    pub fn new(name: &str) -> App {
        App {
            name: name.to_string(),
            routes: Vec::new(),
            middleware: Vec::new(),
            models: Vec::new(),
            tools: Vec::new(),
            workers: 8,
            readiness: None,
            after: Vec::new(),
            limits: Limits::default(),
        }
    }

    /// Replace the server resource limits (body/header caps, socket timeout).
    pub fn limits(mut self, limits: Limits) -> App {
        self.limits = limits;
        self
    }

    /// Max accepted request-body size in bytes (HTTP 413 above it).
    pub fn max_body(mut self, bytes: usize) -> App {
        self.limits.max_body = bytes;
        self
    }

    /// Per-socket read/write timeout (slowloris protection); `None` disables it.
    pub fn request_timeout(mut self, timeout: Option<Duration>) -> App {
        self.limits.timeout = timeout;
        self
    }

    /// Register an after-middleware that transforms every outgoing response
    /// (e.g. [`cors`] to add CORS headers). Applied in registration order.
    pub fn after(
        mut self,
        transform: impl Fn(&Request, Response) -> Response + Send + Sync + 'static,
    ) -> App {
        self.after.push(Box::new(transform));
        self
    }

    /// Set the worker thread count (default 8).
    pub fn workers(mut self, n: usize) -> App {
        self.workers = n;
        self
    }

    /// Register a readiness probe used by `GET /__ready` (returns 503 when it
    /// yields `false`). Use it to gate traffic on dependencies — e.g. a live DB
    /// connection — so Kubernetes won't route to a pod that isn't ready.
    pub fn readiness(mut self, check: impl Fn() -> bool + Send + Sync + 'static) -> App {
        self.readiness = Some(Box::new(check));
        self
    }

    /// Register a route for any method, with a doc string surfaced via
    /// `/__introspect`.
    pub fn route(
        mut self,
        method: Method,
        pattern: &str,
        doc: &str,
        handler: impl Fn(&Request, &Params) -> Response + Send + Sync + 'static,
    ) -> App {
        self.routes.push(Route {
            method,
            pattern: pattern.to_string(),
            doc: doc.to_string(),
            handler: Box::new(handler),
            middleware: Vec::new(),
        });
        self
    }

    /// Register a group of routes sharing a path `prefix` and group-scoped
    /// `middleware`. Laravel-style: `app.group("/api", vec![mw(auth)], |g| g.get(...))`.
    pub fn group(
        mut self,
        prefix: &str,
        middleware: Vec<Mw>,
        build: impl FnOnce(Group) -> Group,
    ) -> App {
        let group = build(Group {
            prefix: prefix.to_string(),
            middleware,
            routes: Vec::new(),
        });
        self.routes.extend(group.routes);
        self
    }

    pub fn get(
        self,
        pattern: &str,
        doc: &str,
        handler: impl Fn(&Request, &Params) -> Response + Send + Sync + 'static,
    ) -> App {
        self.route(Method::Get, pattern, doc, handler)
    }

    pub fn post(
        self,
        pattern: &str,
        doc: &str,
        handler: impl Fn(&Request, &Params) -> Response + Send + Sync + 'static,
    ) -> App {
        self.route(Method::Post, pattern, doc, handler)
    }

    pub fn put(
        self,
        pattern: &str,
        doc: &str,
        handler: impl Fn(&Request, &Params) -> Response + Send + Sync + 'static,
    ) -> App {
        self.route(Method::Put, pattern, doc, handler)
    }

    pub fn delete(
        self,
        pattern: &str,
        doc: &str,
        handler: impl Fn(&Request, &Params) -> Response + Send + Sync + 'static,
    ) -> App {
        self.route(Method::Delete, pattern, doc, handler)
    }

    /// Add a middleware to the front-of-request chain.
    pub fn middleware(
        mut self,
        mw: impl Fn(&Request) -> Option<Response> + Send + Sync + 'static,
    ) -> App {
        self.middleware.push(Box::new(mw));
        self
    }

    /// Record a model schema (see `sutegi_orm::schema_json`) for introspection.
    pub fn register_model(mut self, schema: Json) -> App {
        self.models.push(schema);
        self
    }

    /// Record a tool schema for introspection (used by `sutegi-ai`).
    pub fn register_tool(mut self, schema: Json) -> App {
        self.tools.push(schema);
        self
    }

    /// Build the JSON description of the entire application surface.
    fn introspection(&self) -> Json {
        let routes = self
            .routes
            .iter()
            .map(|r| {
                Json::obj(vec![
                    ("method", Json::str(r.method.as_str())),
                    ("pattern", Json::str(r.pattern.clone())),
                    ("doc", Json::str(r.doc.clone())),
                ])
            })
            .collect();
        Json::obj(vec![
            ("name", Json::str(self.name.clone())),
            ("framework", Json::str("sutegi")),
            ("version", Json::str(env!("CARGO_PKG_VERSION"))),
            ("routes", Json::arr(routes)),
            ("models", Json::arr(self.models.clone())),
            ("tools", Json::arr(self.tools.clone())),
            (
                "endpoints",
                Json::obj(vec![
                    ("introspect", Json::str("/__introspect")),
                    ("health", Json::str("/__health")),
                    ("ready", Json::str("/__ready")),
                    ("metrics", Json::str("/__metrics")),
                ]),
            ),
        ])
    }

    /// Build the request service closure (shared by every `run*` variant).
    fn into_service(
        self,
    ) -> (
        usize,
        Limits,
        impl Fn(Request) -> Response + Send + Sync + 'static,
    ) {
        let limits = self.limits;
        let introspect = self.introspection();
        let routes = Arc::new(self.routes);
        let middleware = Arc::new(self.middleware);
        let after = Arc::new(self.after);
        let readiness = Arc::new(self.readiness);
        let metrics = Arc::new(Metrics::default());
        let workers = self.workers;

        let service = move |req: Request| -> Response {
            metrics.in_flight.fetch_add(1, Ordering::Relaxed);

            // Inner closure so we can post-process (record metrics) on every path.
            let resp = (|| {
                // Operational endpoints first — pods hit these constantly.
                match req.path.as_str() {
                    "/__health" => return json(200, &Json::obj(vec![("status", Json::str("ok"))])),
                    "/__ready" => {
                        let ready = readiness.as_ref().as_ref().map(|f| f()).unwrap_or(true);
                        let body = Json::obj(vec![(
                            "status",
                            Json::str(if ready { "ready" } else { "not ready" }),
                        )]);
                        return json(if ready { 200 } else { 503 }, &body);
                    }
                    "/__metrics" => {
                        return Response::new(200)
                            .with_header("content-type", "text/plain; version=0.0.4")
                            .with_body(metrics.prometheus().into_bytes());
                    }
                    "/__introspect" => return json(200, &introspect),
                    _ => {}
                }

                // Global middleware chain — first responder wins.
                for mw in middleware.iter() {
                    if let Some(resp) = mw(&req) {
                        return resp;
                    }
                }

                // Route table (run group-scoped middleware before the handler).
                if let Some((route, params)) = match_route(&routes, req.method, &req.path) {
                    for mw in &route.middleware {
                        if let Some(resp) = mw(&req) {
                            return resp;
                        }
                    }
                    return (route.handler)(&req, &params);
                }

                // Distinguish "no such path" from "wrong method".
                if routes
                    .iter()
                    .any(|r| match_pattern(&r.pattern, &req.path).is_some())
                {
                    return text(405, "405 Method Not Allowed");
                }
                not_found()
            })();

            // After-middleware: transform the outgoing response (CORS, etc.).
            let resp = after.iter().fold(resp, |r, hook| hook(&req, r));

            metrics.record(resp.status);
            metrics.in_flight.fetch_sub(1, Ordering::Relaxed);
            resp
        };

        (workers, limits, service)
    }

    /// Bind to `addr` and serve forever.
    pub fn run(self, addr: &str) -> std::io::Result<()> {
        let (workers, limits, service) = self.into_service();
        sutegi_http::serve(addr, workers, limits, service)
    }

    /// Serve until `shutdown` is set, then drain in-flight requests and return.
    /// Flip the flag from a signal handler (see [`App::run_graceful`]) or your
    /// own logic for zero-drop rolling deploys.
    pub fn run_until(self, addr: &str, shutdown: Arc<AtomicBool>) -> std::io::Result<()> {
        let (workers, limits, service) = self.into_service();
        sutegi_http::serve_until(addr, workers, limits, service, shutdown)
    }

    /// Serve until SIGTERM/SIGINT (what Kubernetes sends on pod termination),
    /// then gracefully drain. Requires the `graceful` feature.
    #[cfg(feature = "graceful")]
    pub fn run_graceful(self, addr: &str) -> std::io::Result<()> {
        let flag = Arc::new(AtomicBool::new(false));
        crate::signal::install(Arc::clone(&flag));
        self.run_until(addr, flag)
    }
}

/// SIGTERM/SIGINT handling for graceful shutdown (only with the `graceful` feature).
#[cfg(feature = "graceful")]
mod signal {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, OnceLock};

    static FLAG: OnceLock<Arc<AtomicBool>> = OnceLock::new();

    extern "C" fn on_signal(_sig: libc::c_int) {
        // Signal-handler-safe: only an atomic store on an already-published Arc.
        if let Some(flag) = FLAG.get() {
            flag.store(true, Ordering::SeqCst);
        }
    }

    pub fn install(flag: Arc<AtomicBool>) {
        let _ = FLAG.set(flag);
        // SAFETY: registering a handler that only does an atomic store.
        let handler = on_signal as extern "C" fn(libc::c_int) as libc::sighandler_t;
        unsafe {
            libc::signal(libc::SIGTERM, handler);
            libc::signal(libc::SIGINT, handler);
        }
    }
}

/// A builder for a group of routes sharing a prefix and middleware. Created via
/// [`App::group`]. Patterns added here are prefixed; group middleware runs
/// before each route's handler.
pub struct Group {
    prefix: String,
    middleware: Vec<Mw>,
    routes: Vec<Route>,
}

impl Group {
    pub fn route(
        mut self,
        method: Method,
        pattern: &str,
        doc: &str,
        handler: impl Fn(&Request, &Params) -> Response + Send + Sync + 'static,
    ) -> Group {
        self.routes.push(Route {
            method,
            pattern: join_prefix(&self.prefix, pattern),
            doc: doc.to_string(),
            handler: Box::new(handler),
            middleware: self.middleware.clone(),
        });
        self
    }

    pub fn get(
        self,
        p: &str,
        doc: &str,
        h: impl Fn(&Request, &Params) -> Response + Send + Sync + 'static,
    ) -> Group {
        self.route(Method::Get, p, doc, h)
    }
    pub fn post(
        self,
        p: &str,
        doc: &str,
        h: impl Fn(&Request, &Params) -> Response + Send + Sync + 'static,
    ) -> Group {
        self.route(Method::Post, p, doc, h)
    }
    pub fn put(
        self,
        p: &str,
        doc: &str,
        h: impl Fn(&Request, &Params) -> Response + Send + Sync + 'static,
    ) -> Group {
        self.route(Method::Put, p, doc, h)
    }
    pub fn delete(
        self,
        p: &str,
        doc: &str,
        h: impl Fn(&Request, &Params) -> Response + Send + Sync + 'static,
    ) -> Group {
        self.route(Method::Delete, p, doc, h)
    }
}

/// Join a group prefix and a route pattern into a single normalized path.
fn join_prefix(prefix: &str, pattern: &str) -> String {
    let p = prefix.trim_end_matches('/');
    let s = pattern.trim_start_matches('/');
    if p.is_empty() {
        format!("/{}", s)
    } else if s.is_empty() {
        p.to_string()
    } else {
        format!("{}/{}", p, s)
    }
}

fn match_route<'a>(routes: &'a [Route], method: Method, path: &str) -> Option<(&'a Route, Params)> {
    for r in routes {
        if r.method != method {
            continue;
        }
        if let Some(params) = match_pattern(&r.pattern, path) {
            return Some((r, params));
        }
    }
    None
}

/// Match a path against a pattern with `:name` segments, capturing params.
fn match_pattern(pattern: &str, path: &str) -> Option<Params> {
    let p: Vec<&str> = pattern.trim_matches('/').split('/').collect();
    let q: Vec<&str> = path.trim_matches('/').split('/').collect();
    if p.len() != q.len() {
        return None;
    }
    let mut params = Params::new();
    for (seg, val) in p.iter().zip(q.iter()) {
        if let Some(name) = seg.strip_prefix(':') {
            params.insert(name.to_string(), val.to_string());
        } else if seg != val {
            return None;
        }
    }
    Some(params)
}

// ---- response helpers -----------------------------------------------------

/// A `text/plain` response.
pub fn text(status: u16, body: &str) -> Response {
    Response::new(status).with_body(body.as_bytes().to_vec())
}

/// An `application/json` response from a `Json` value.
pub fn json(status: u16, value: &Json) -> Response {
    Response::new(status)
        .with_header("content-type", "application/json; charset=utf-8")
        .with_body(value.to_string().into_bytes())
}

/// A canned 404.
pub fn not_found() -> Response {
    text(404, "404 Not Found")
}

/// A `text/html` response.
pub fn html(status: u16, body: &str) -> Response {
    Response::new(status)
        .with_header("content-type", "text/html; charset=utf-8")
        .with_body(body.as_bytes().to_vec())
}

/// A `302 Found` redirect to `location`.
pub fn redirect(location: &str) -> Response {
    Response::new(302).with_header("location", location)
}

/// An empty response with just a status code.
pub fn status(code: u16) -> Response {
    Response::new(code)
}

/// A `204 No Content`.
pub fn no_content() -> Response {
    Response::new(204)
}

/// A pre-middleware that logs `METHOD /path` for each request and continues.
pub fn logger() -> impl Fn(&Request) -> Option<Response> + Send + Sync + 'static {
    |req: &Request| {
        println!("[{}] {}", req.method.as_str(), req.path);
        None
    }
}

/// A pre-middleware that answers CORS preflight (`OPTIONS`) with `204` and the
/// permitted origin. Pair with [`cors`] (an after-middleware) for full CORS.
pub fn cors_preflight(
    origin: &str,
) -> impl Fn(&Request) -> Option<Response> + Send + Sync + 'static {
    let origin = origin.to_string();
    move |req: &Request| {
        if req.method == Method::Options {
            Some(
                Response::new(204)
                    .with_header("access-control-allow-origin", &origin)
                    .with_header(
                        "access-control-allow-methods",
                        "GET,POST,PUT,DELETE,OPTIONS",
                    )
                    .with_header("access-control-allow-headers", "*"),
            )
        } else {
            None
        }
    }
}

/// An after-middleware that stamps `Access-Control-Allow-Origin` onto every
/// response. Register with [`App::after`].
pub fn cors(origin: &str) -> impl Fn(&Request, Response) -> Response + Send + Sync + 'static {
    let origin = origin.to_string();
    move |_req: &Request, resp: Response| resp.with_header("access-control-allow-origin", &origin)
}

/// A pre-middleware requiring `Authorization: Bearer <token>`; else `401`.
pub fn bearer(token: &str) -> impl Fn(&Request) -> Option<Response> + Send + Sync + 'static {
    let expected = format!("Bearer {}", token);
    move |req: &Request| match req.header("authorization") {
        Some(h) if h == expected => None,
        _ => Some(text(401, "401 Unauthorized").with_header("www-authenticate", "Bearer")),
    }
}

/// A pre-middleware requiring HTTP Basic auth for `user`/`pass`; else `401`.
pub fn basic(
    user: &str,
    pass: &str,
) -> impl Fn(&Request) -> Option<Response> + Send + Sync + 'static {
    let expected = format!(
        "Basic {}",
        base64_encode(format!("{}:{}", user, pass).as_bytes())
    );
    move |req: &Request| match req.header("authorization") {
        Some(h) if h == expected => None,
        _ => Some(
            text(401, "401 Unauthorized").with_header("www-authenticate", "Basic realm=\"sutegi\""),
        ),
    }
}

/// An after-middleware adding a baseline set of security headers (nosniff,
/// frame-deny, referrer-policy, HSTS). Register with [`App::after`].
pub fn secure_headers() -> impl Fn(&Request, Response) -> Response + Send + Sync + 'static {
    |_req: &Request, resp: Response| {
        resp.with_header("x-content-type-options", "nosniff")
            .with_header("x-frame-options", "DENY")
            .with_header("referrer-policy", "no-referrer")
            .with_header(
                "strict-transport-security",
                "max-age=31536000; includeSubDomains",
            )
    }
}

/// A per-client-IP token-bucket rate limiter (pre-middleware). Allows bursts up
/// to `max_requests`, refilling over `per`; returns `429` when exhausted.
pub fn rate_limit(
    max_requests: u32,
    per: Duration,
) -> impl Fn(&Request) -> Option<Response> + Send + Sync + 'static {
    let buckets: std::sync::Arc<
        std::sync::Mutex<std::collections::HashMap<String, (f64, std::time::Instant)>>,
    > = std::sync::Arc::new(std::sync::Mutex::new(std::collections::HashMap::new()));
    let capacity = max_requests as f64;
    let refill_per_sec = if per.as_secs_f64() > 0.0 {
        capacity / per.as_secs_f64()
    } else {
        capacity
    };
    move |req: &Request| {
        let key = req.peer_ip().unwrap_or_else(|| "unknown".to_string());
        let now = std::time::Instant::now();
        let mut map = buckets.lock().unwrap();
        let entry = map.entry(key).or_insert((capacity, now));
        let elapsed = now.duration_since(entry.1).as_secs_f64();
        entry.0 = (entry.0 + elapsed * refill_per_sec).min(capacity);
        entry.1 = now;
        if entry.0 >= 1.0 {
            entry.0 -= 1.0;
            None
        } else {
            Some(text(429, "429 Too Many Requests").with_header("retry-after", "1"))
        }
    }
}

/// Minimal standard base64 encoder (used by `basic`).
fn base64_encode(input: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b0 = chunk[0];
        let b1 = *chunk.get(1).unwrap_or(&0);
        let b2 = *chunk.get(2).unwrap_or(&0);
        out.push(T[(b0 >> 2) as usize] as char);
        out.push(T[(((b0 & 0x03) << 4) | (b1 >> 4)) as usize] as char);
        out.push(if chunk.len() > 1 {
            T[(((b1 & 0x0f) << 2) | (b2 >> 6)) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            T[(b2 & 0x3f) as usize] as char
        } else {
            '='
        });
    }
    out
}

/// A streaming response: `producer` writes (and flushes) bytes over time via a
/// [`StreamSink`]. Sets the given `content_type`; no `Content-Length`.
pub fn stream(
    status: u16,
    content_type: &str,
    producer: impl FnOnce(&mut StreamSink) -> std::io::Result<()> + Send + 'static,
) -> Response {
    Response::new(status)
        .with_header("content-type", content_type)
        .with_stream(move |w| {
            let mut sink = StreamSink::new(w);
            producer(&mut sink)
        })
}

/// A Server-Sent Events response. `producer` pushes events through an
/// [`SseSink`] (`data` / `event` / `comment`), each flushed immediately — the
/// natural transport for streaming LLM tokens to a browser or agent.
pub fn sse(
    producer: impl FnOnce(&mut SseSink) -> std::io::Result<()> + Send + 'static,
) -> Response {
    Response::new(200)
        .with_header("content-type", "text/event-stream")
        .with_header("cache-control", "no-cache")
        .with_stream(move |w| {
            let mut sink = SseSink::new(w);
            producer(&mut sink)
        })
}

// ---- extractors -----------------------------------------------------------

/// Parse the request body as JSON.
pub fn json_body(req: &Request) -> Result<Json, String> {
    let s = std::str::from_utf8(&req.body).map_err(|_| "body is not valid UTF-8".to_string())?;
    if s.trim().is_empty() {
        return Ok(Json::Obj(BTreeMap::new()));
    }
    Json::parse(s)
}

/// Parse the query string into a map (`a=1&b=2` → `{a:1, b:2}`).
pub fn query_params(req: &Request) -> BTreeMap<String, String> {
    parse_urlencoded(&req.query)
}

/// Parse an `application/x-www-form-urlencoded` request body into a map.
pub fn form_body(req: &Request) -> BTreeMap<String, String> {
    match std::str::from_utf8(&req.body) {
        Ok(s) => parse_urlencoded(s),
        Err(_) => BTreeMap::new(),
    }
}

/// Shared `key=value&...` parser used by query strings and form bodies.
fn parse_urlencoded(input: &str) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    for pair in input.split('&') {
        if pair.is_empty() {
            continue;
        }
        match pair.split_once('=') {
            Some((k, v)) => {
                map.insert(url_decode(k), url_decode(v));
            }
            None => {
                map.insert(url_decode(pair), String::new());
            }
        }
    }
    map
}

/// Minimal `%`-decoding plus `+` → space.
fn url_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < bytes.len() => {
                let h = (hex_val(bytes[i + 1]), hex_val(bytes[i + 2]));
                if let (Some(hi), Some(lo)) = h {
                    out.push(hi * 16 + lo);
                    i += 3;
                } else {
                    out.push(bytes[i]);
                    i += 1;
                }
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pattern_matches_and_captures() {
        let params = match_pattern("/users/:id/posts/:slug", "/users/42/posts/hello").unwrap();
        assert_eq!(params.get("id").map(String::as_str), Some("42"));
        assert_eq!(params.get("slug").map(String::as_str), Some("hello"));
    }

    #[test]
    fn pattern_rejects_length_mismatch() {
        assert!(match_pattern("/users/:id", "/users/42/extra").is_none());
    }

    #[test]
    fn root_matches() {
        assert!(match_pattern("/", "/").is_some());
    }

    #[test]
    fn parses_form_body() {
        let req = Request {
            method: Method::Post,
            path: "/x".into(),
            query: String::new(),
            version: "HTTP/1.1".into(),
            headers: vec![],
            body: b"title=hello+world&done=true".to_vec(),
            peer: None,
        };
        let form = form_body(&req);
        assert_eq!(form.get("title").map(String::as_str), Some("hello world"));
        assert_eq!(form.get("done").map(String::as_str), Some("true"));
    }

    #[test]
    fn security_middleware() {
        let mut req = Request {
            method: Method::Get,
            path: "/".into(),
            query: String::new(),
            version: "HTTP/1.1".into(),
            headers: vec![],
            body: vec![],
            peer: Some("1.2.3.4:55000".into()),
        };
        let guard = bearer("s3cr3t");
        assert_eq!(guard(&req).map(|r| r.status), Some(401)); // no header
        req.headers
            .push(("Authorization".into(), "Bearer s3cr3t".into()));
        assert!(guard(&req).is_none()); // authorized

        // Rate limit: 2 requests allowed, 3rd blocked.
        let rl = rate_limit(2, std::time::Duration::from_secs(60));
        assert!(rl(&req).is_none());
        assert!(rl(&req).is_none());
        assert_eq!(rl(&req).map(|r| r.status), Some(429));

        assert_eq!(req.peer_ip().as_deref(), Some("1.2.3.4"));
    }

    #[test]
    fn response_helpers() {
        assert_eq!(redirect("/login").status, 302);
        assert_eq!(no_content().status, 204);
        let h = html(200, "<p>hi</p>");
        assert!(h
            .headers
            .iter()
            .any(|(k, v)| k == "content-type" && v.contains("text/html")));
    }

    #[test]
    fn decodes_query() {
        let req = Request {
            method: Method::Get,
            path: "/s".into(),
            query: "q=hello+world&page=2".into(),
            version: "HTTP/1.1".into(),
            headers: vec![],
            body: vec![],
            peer: None,
        };
        let q = query_params(&req);
        assert_eq!(q.get("q").map(String::as_str), Some("hello world"));
        assert_eq!(q.get("page").map(String::as_str), Some("2"));
    }

    fn req(method: Method, body: &[u8]) -> Request {
        Request {
            method,
            path: "/".into(),
            query: String::new(),
            version: "HTTP/1.1".into(),
            headers: vec![],
            body: body.to_vec(),
            peer: Some("9.9.9.9:1".into()),
        }
    }

    #[test]
    fn json_body_parses_and_defaults_empty() {
        let r = req(Method::Post, br#"{"a":1}"#);
        assert_eq!(
            json_body(&r).unwrap().get("a").and_then(Json::as_i64),
            Some(1)
        );
        // Empty body → empty object, not an error.
        assert_eq!(
            json_body(&req(Method::Post, b"")).unwrap(),
            Json::Obj(BTreeMap::new())
        );
        // Malformed JSON is an error.
        assert!(json_body(&req(Method::Post, b"{not json")).is_err());
    }

    #[test]
    fn url_decode_percent_and_plus() {
        let r = Request {
            query: "name=a%20b%2Bc&plus=x+y".into(),
            ..req(Method::Get, b"")
        };
        let q = query_params(&r);
        assert_eq!(q.get("name").map(String::as_str), Some("a b+c"));
        assert_eq!(q.get("plus").map(String::as_str), Some("x y"));
    }

    #[test]
    fn json_and_text_helpers_set_content_type() {
        let j = json(201, &Json::obj(vec![("ok", Json::Bool(true))]));
        assert_eq!(j.status, 201);
        assert!(j
            .headers
            .iter()
            .any(|(k, v)| k == "content-type" && v.contains("application/json")));
        assert_eq!(status(204).status, 204);
        assert_eq!(not_found().status, 404);
    }

    #[test]
    fn cors_preflight_and_after_middleware() {
        let pre = cors_preflight("https://join.com");
        // OPTIONS is answered with 204 + allow-origin.
        let resp = pre(&req(Method::Options, b"")).unwrap();
        assert_eq!(resp.status, 204);
        assert!(resp
            .headers
            .iter()
            .any(|(k, v)| k == "access-control-allow-origin" && v == "https://join.com"));
        // Non-OPTIONS passes through.
        assert!(pre(&req(Method::Get, b"")).is_none());
        // The after-middleware stamps the header on any response.
        let after = cors("*");
        let stamped = after(&req(Method::Get, b""), Response::new(200));
        assert!(stamped
            .headers
            .iter()
            .any(|(k, v)| k == "access-control-allow-origin" && v == "*"));
    }

    #[test]
    fn secure_headers_added() {
        let resp = secure_headers()(&req(Method::Get, b""), Response::new(200));
        for h in [
            "x-content-type-options",
            "x-frame-options",
            "referrer-policy",
            "strict-transport-security",
        ] {
            assert!(resp.headers.iter().any(|(k, _)| k == h), "missing {h}");
        }
    }

    #[test]
    fn basic_auth_checks_base64_credentials() {
        let guard = basic("user", "pass");
        // Wrong / missing creds → 401 with a Basic challenge.
        let denied = guard(&req(Method::Get, b"")).unwrap();
        assert_eq!(denied.status, 401);
        assert!(denied
            .headers
            .iter()
            .any(|(k, v)| k == "www-authenticate" && v.contains("Basic")));
        // base64("user:pass") == "dXNlcjpwYXNz".
        let mut r = req(Method::Get, b"");
        r.headers
            .push(("Authorization".into(), "Basic dXNlcjpwYXNz".into()));
        assert!(guard(&r).is_none());
    }

    #[test]
    fn rate_limit_refills_over_time() {
        let rl = rate_limit(1, std::time::Duration::from_millis(20));
        let r = req(Method::Get, b"");
        assert!(rl(&r).is_none()); // first allowed
        assert_eq!(rl(&r).map(|x| x.status), Some(429)); // bucket empty
        std::thread::sleep(std::time::Duration::from_millis(30));
        assert!(rl(&r).is_none()); // refilled
    }

    #[test]
    fn group_prefixes_routes_and_introspects() {
        // Build an app with a prefixed group; introspection reflects the joined paths.
        let app = App::new("t").group("/api", vec![], |g| {
            g.get("/users", "list", |_r, _p| text(200, "ok")).post(
                "/users/:id",
                "update",
                |_r, _p| text(200, "ok"),
            )
        });
        let intro = app.introspection();
        let routes = intro.get("routes").and_then(Json::as_array).unwrap();
        let patterns: Vec<&str> = routes
            .iter()
            .filter_map(|r| r.get("pattern").and_then(Json::as_str))
            .collect();
        assert!(patterns.contains(&"/api/users"));
        assert!(patterns.contains(&"/api/users/:id"));
    }
}
