//! The web spine: a route table, middleware chain, ergonomic extractors, and
//! the runtime **introspection registry** that makes a sutegi app
//! self-describing.
//!
//! Every route is registered with a human/agent-readable doc string. Models
//! and tools register their schemas. At runtime, `GET /__introspect` returns
//! the full surface of the application as JSON — so an AI agent can discover
//! what the app can do without ever reading the source.

use std::collections::BTreeMap;
use std::sync::Arc;

pub use sutegi_http::{Method, Request, Response};
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
        }
    }

    /// Set the worker thread count (default 8).
    pub fn workers(mut self, n: usize) -> App {
        self.workers = n;
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
    pub fn group(mut self, prefix: &str, middleware: Vec<Mw>, build: impl FnOnce(Group) -> Group) -> App {
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
        ])
    }

    /// Bind to `addr` and serve forever.
    pub fn run(self, addr: &str) -> std::io::Result<()> {
        // Freeze the introspection document and move everything into the handler.
        let introspect = self.introspection();
        let routes = Arc::new(self.routes);
        let middleware = Arc::new(self.middleware);
        let workers = self.workers;

        sutegi_http::serve(addr, workers, move |req| {
            // 1. Middleware chain — first responder wins.
            for mw in middleware.iter() {
                if let Some(resp) = mw(&req) {
                    return resp;
                }
            }

            // 2. Built-in introspection endpoint.
            if req.path == "/__introspect" {
                return json(200, &introspect);
            }

            // 3. Route table (run group-scoped middleware before the handler).
            if let Some((route, params)) = match_route(&routes, req.method, &req.path) {
                for mw in &route.middleware {
                    if let Some(resp) = mw(&req) {
                        return resp;
                    }
                }
                return (route.handler)(&req, &params);
            }

            // 4. Distinguish "no such path" from "wrong method".
            if routes.iter().any(|r| match_pattern(&r.pattern, &req.path).is_some()) {
                return text(405, "405 Method Not Allowed");
            }
            not_found()
        })
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

    pub fn get(self, p: &str, doc: &str, h: impl Fn(&Request, &Params) -> Response + Send + Sync + 'static) -> Group {
        self.route(Method::Get, p, doc, h)
    }
    pub fn post(self, p: &str, doc: &str, h: impl Fn(&Request, &Params) -> Response + Send + Sync + 'static) -> Group {
        self.route(Method::Post, p, doc, h)
    }
    pub fn put(self, p: &str, doc: &str, h: impl Fn(&Request, &Params) -> Response + Send + Sync + 'static) -> Group {
        self.route(Method::Put, p, doc, h)
    }
    pub fn delete(self, p: &str, doc: &str, h: impl Fn(&Request, &Params) -> Response + Send + Sync + 'static) -> Group {
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
    let mut map = BTreeMap::new();
    for pair in req.query.split('&') {
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
    fn decodes_query() {
        let req = Request {
            method: Method::Get,
            path: "/s".into(),
            query: "q=hello+world&page=2".into(),
            version: "HTTP/1.1".into(),
            headers: vec![],
            body: vec![],
        };
        let q = query_params(&req);
        assert_eq!(q.get("q").map(String::as_str), Some("hello world"));
        assert_eq!(q.get("page").map(String::as_str), Some("2"));
    }
}
