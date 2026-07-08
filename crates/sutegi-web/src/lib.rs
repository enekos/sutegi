//! The web spine: a route table, middleware chain, ergonomic extractors, and
//! the runtime **introspection registry** that makes a sutegi app
//! self-describing.
//!
//! Every route is registered with a human/agent-readable doc string. Models
//! and tools register their schemas. At runtime, `GET /__introspect` returns
//! the full surface of the application as JSON — so an AI agent can discover
//! what the app can do without ever reading the source.

use std::any::{Any, TypeId};
use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::sync::Arc;

use std::time::Duration;
pub use sutegi_http::{Body, Limits, Method, Request, Response, SseSink, StreamSink};
use sutegi_json::Json;

mod files;
mod respond;
pub use respond::{Error, IntoResponse};

pub mod schema;

#[cfg(feature = "ws")]
pub mod ws;
#[cfg(feature = "ws")]
pub use ws::Ws;

/// Path parameters captured from a route pattern (`:name` segments).
pub type Params = BTreeMap<String, String>;

/// A type-indexed bag of shared application state, populated by [`App::state`]
/// and read back with [`Ctx::state`].
type StateMap = HashMap<TypeId, Arc<dyn Any + Send + Sync>>;

fn state_ref<T: Any + Send + Sync>(map: &StateMap) -> Option<&T> {
    map.get(&TypeId::of::<T>())
        .and_then(|a| a.downcast_ref::<T>())
}

fn state_or_panic<T: Any + Send + Sync>(map: &StateMap) -> &T {
    state_ref(map).unwrap_or_else(|| {
        panic!(
            "no state of type `{}` registered — add it with `App::state(...)`",
            std::any::type_name::<T>()
        )
    })
}

/// Everything a request handler needs: the [`Request`], the captured path
/// [`Params`], and typed access to shared application state.
///
/// Handlers take a single `&Ctx` and return anything that is [`IntoResponse`]
/// — including `Result<_, Error>`, so `?` works:
///
/// ```ignore
/// app.post("/todos", "create", |c| {
///     let todo: Todo = c.validated()?;      // parse + validate the body
///     let id = todo.save(c.db::<Db>())?;    // shared state, no Arc/Mutex
///     Ok((201, Todo { id, ..todo }.to_json()))
/// });
/// ```
pub struct Ctx<'a> {
    /// The raw request.
    pub req: &'a Request,
    /// Path parameters captured from the route pattern.
    pub params: Params,
    state: Arc<StateMap>,
}

impl Ctx<'_> {
    /// A captured path parameter (`:name`), if present.
    pub fn param(&self, key: &str) -> Option<&str> {
        self.params.get(key).map(String::as_str)
    }

    /// A request header, case-insensitively.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.req.header(name)
    }

    /// The query string parsed into a map.
    pub fn query(&self) -> BTreeMap<String, String> {
        query_params(self.req)
    }

    /// A form-encoded body parsed into a map.
    pub fn form(&self) -> BTreeMap<String, String> {
        form_body(self.req)
    }

    /// The request body parsed as JSON (empty body → empty object; malformed
    /// body → a `400` [`Error`]).
    pub fn json(&self) -> Result<Json, Error> {
        json_body(self.req).map_err(Error::bad_request)
    }

    /// Shared state of type `T`, registered via [`App::state`].
    ///
    /// Panics if no state of that type was registered — a wiring bug, caught on
    /// the first request. Use [`Ctx::try_state`] for the fallible form.
    pub fn state<T: Any + Send + Sync>(&self) -> &T {
        state_or_panic(&self.state)
    }

    /// Shared state of type `T`, or `None` if it was never registered.
    pub fn try_state<T: Any + Send + Sync>(&self) -> Option<&T> {
        state_ref(&self.state)
    }

    /// The database backend held in state — sugar for [`Ctx::state`] pinned to a
    /// [`Backend`](sutegi_orm::Backend) type. `c.db::<Db>()` / `c.db::<Pg>()`.
    #[cfg(feature = "orm")]
    pub fn db<B: sutegi_orm::Backend + Any + Send + Sync>(&self) -> &B {
        self.state::<B>()
    }

    /// Route-model binding: hydrate model `T` from the `key` path parameter over
    /// the backend `B` held in state, or return a ready `404`/`500` [`Error`].
    /// `c.model::<Todo, Db>("id")?`.
    #[cfg(feature = "orm")]
    pub fn model<T, B>(&self, key: &str) -> Result<T, Error>
    where
        T: sutegi_orm::Model + sutegi_orm::row::FromRow,
        B: sutegi_orm::Backend + Any + Send + Sync,
    {
        let raw = self
            .param(key)
            .ok_or_else(|| Error::not_found("not found"))?;
        let id = match raw.parse::<i64>() {
            Ok(n) => sutegi_orm::Value::Int(n),
            Err(_) => sutegi_orm::Value::Text(raw.to_string()),
        };
        match T::find_typed(self.db::<B>(), id) {
            Ok(Some(m)) => Ok(m),
            Ok(None) => Err(Error::not_found("not found")),
            Err(e) => Err(Error::internal(e)),
        }
    }

    /// Parse the JSON body and validate it against `rules`, returning the parsed
    /// [`Json`] on success or a `422` [`Error`] carrying the field errors.
    #[cfg(feature = "validate")]
    pub fn validate(&self, rules: &sutegi_validate::Ruleset) -> Result<Json, Error> {
        let body = self.json()?;
        if let Err(errs) = rules.validate(&body) {
            return Err(Error::unprocessable("validation failed").with_fields(errs.to_json()));
        }
        Ok(body)
    }

    /// Parse **and** validate the JSON body into a typed model in one step,
    /// using the model's own [`Validate`](sutegi_validate::Validate) rules and
    /// the lenient [`FromInput`](sutegi_orm::FromInput) hydrator (so a partial
    /// create payload — no `id`, no defaulted flags — still works).
    /// `let todo: Todo = c.validated()?;`
    #[cfg(all(feature = "validate", feature = "orm"))]
    pub fn validated<T>(&self) -> Result<T, Error>
    where
        T: sutegi_orm::FromInput + sutegi_validate::Validate,
    {
        let body = self.json()?;
        if let Err(errs) = T::rules().validate(&body) {
            return Err(Error::unprocessable("validation failed").with_fields(errs.to_json()));
        }
        T::from_input(&body).map_err(Error::unprocessable)
    }
}

/// The owned context handed to AI tool closures ([`App::tool`] /
/// [`App::stream_tool`]). Like [`Ctx`] it exposes typed state, but it owns its
/// data so a streaming tool can keep using it after the response has begun.
pub struct ToolCtx {
    state: Arc<StateMap>,
    /// Path parameters (always empty for `/__tools/:name` beyond `name`).
    pub params: Params,
}

impl ToolCtx {
    /// Shared state of type `T`, registered via [`App::state`].
    pub fn state<T: Any + Send + Sync>(&self) -> &T {
        state_or_panic(&self.state)
    }

    /// Shared state of type `T`, or `None` if it was never registered.
    pub fn try_state<T: Any + Send + Sync>(&self) -> Option<&T> {
        state_ref(&self.state)
    }

    /// The database backend held in state (see [`Ctx::db`]).
    #[cfg(feature = "orm")]
    pub fn db<B: sutegi_orm::Backend + Any + Send + Sync>(&self) -> &B {
        self.state::<B>()
    }
}

/// A request handler: takes a [`Ctx`], returns a [`Response`]. Registered forms
/// accept any [`IntoResponse`]; this is the boxed, erased form the router runs.
pub type Handler = Box<dyn Fn(&Ctx) -> Response + Send + Sync + 'static>;

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

/// A non-streaming tool body: validated JSON args in, JSON (or [`Error`]) out.
type UnaryToolFn = Box<dyn Fn(&ToolCtx, Json) -> Result<Json, Error> + Send + Sync + 'static>;

/// A streaming tool body: emits Server-Sent Events through the [`SseSink`].
type StreamToolFn =
    Box<dyn Fn(&ToolCtx, Json, &mut SseSink) -> std::io::Result<()> + Send + Sync + 'static>;

/// The callable body of an AI tool.
enum ToolBody {
    Unary(UnaryToolFn),
    Stream(StreamToolFn),
}

/// A registered AI tool: name, description, JSON-Schema parameters, and body.
struct ToolDef {
    name: String,
    description: String,
    schema: Json,
    body: ToolBody,
}

impl ToolDef {
    fn is_stream(&self) -> bool {
        matches!(self.body, ToolBody::Stream(_))
    }
}

/// One compiled segment of a route pattern: a literal to compare, a `:name`
/// parameter to capture, or a trailing `*name` rest segment that captures
/// everything remaining (zero or more segments, slashes included) — what
/// static file serving needs for nested paths like `/assets/img/logo.png`.
enum Seg {
    Lit(String),
    Param(String),
    Rest(String),
}

/// Compile a `/users/:id`-style pattern once, at registration time, so
/// matching never re-splits (or re-allocates) the pattern per request.
fn compile_pattern(pattern: &str) -> Vec<Seg> {
    pattern
        .trim_matches('/')
        .split('/')
        .map(|s| {
            if let Some(name) = s.strip_prefix(':') {
                Seg::Param(name.to_string())
            } else if let Some(name) = s.strip_prefix('*') {
                Seg::Rest(name.to_string())
            } else {
                Seg::Lit(s.to_string())
            }
        })
        .collect()
}

struct Route {
    method: Method,
    pattern: String,
    /// `pattern`, compiled by [`compile_pattern`].
    segs: Vec<Seg>,
    doc: String,
    handler: Handler,
    /// Group-scoped middleware run before this route's handler.
    middleware: Vec<Mw>,
}

/// The application: a builder you configure with routes, state, tools, and
/// middleware, then `serve()` (or `run()`).
pub struct App {
    name: String,
    routes: Vec<Route>,
    middleware: Vec<Middleware>,
    ops_guard: Option<Middleware>,
    models: Vec<Json>,
    tools: Vec<Json>,
    tool_defs: Vec<ToolDef>,
    state: StateMap,
    workers: usize,
    readiness: Option<Box<dyn Fn() -> bool + Send + Sync + 'static>>,
    after: Vec<AfterMiddleware>,
    limits: Limits,
    #[cfg(feature = "ws")]
    ws_config: sutegi_ws::WsConfig,
    #[cfg(feature = "ws")]
    ws_runtime: Option<Arc<sutegi_ws::WsRuntime>>,
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
            ops_guard: None,
            models: Vec::new(),
            tools: Vec::new(),
            tool_defs: Vec::new(),
            state: StateMap::new(),
            workers: 8,
            readiness: None,
            after: Vec::new(),
            limits: Limits::default(),
            #[cfg(feature = "ws")]
            ws_config: sutegi_ws::WsConfig::default(),
            #[cfg(feature = "ws")]
            ws_runtime: None,
        }
    }

    /// Register a piece of shared application state, retrievable in any handler
    /// via [`Ctx::state`] (or [`Ctx::db`] for a database backend). One value per
    /// type; registering the same type again replaces it. The value must be
    /// `Send + Sync` (e.g. a pooled [`Db`](sutegi_orm::db::Db) or
    /// [`Pg`](sutegi_orm::pg::Pg), or your own `AppState` struct).
    pub fn state<T: Any + Send + Sync>(mut self, value: T) -> App {
        self.state.insert(TypeId::of::<T>(), Arc::new(value));
        self
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
    /// `/__introspect`. The handler takes a [`Ctx`] and returns any
    /// [`IntoResponse`].
    pub fn route<R: IntoResponse>(
        mut self,
        method: Method,
        pattern: &str,
        doc: &str,
        handler: impl Fn(&Ctx) -> R + Send + Sync + 'static,
    ) -> App {
        self.routes.push(Route {
            method,
            pattern: pattern.to_string(),
            segs: compile_pattern(pattern),
            doc: doc.to_string(),
            handler: Box::new(move |c: &Ctx| handler(c).into_response()),
            middleware: Vec::new(),
        });
        self
    }

    /// Tune the WebSocket engine (shard count, frame/message caps, ping and
    /// idle timers, buffering, connection cap). Call **before** the first
    /// [`App::ws`] registration — that's what starts the reactor.
    #[cfg(feature = "ws")]
    pub fn ws_config(mut self, config: sutegi_ws::WsConfig) -> App {
        debug_assert!(
            self.ws_runtime.is_none(),
            "ws_config must come before the first .ws(...) route"
        );
        self.ws_config = config;
        self
    }

    /// Register a WebSocket endpoint. `GET pattern` answers the RFC 6455
    /// handshake, hands the socket to the shared reactor, and frees the HTTP
    /// worker; `ws` holds the endpoint's callbacks (see [`Ws`]).
    ///
    /// The first registration starts the reactor threads (configure them
    /// first via [`App::ws_config`]).
    #[cfg(feature = "ws")]
    pub fn ws(mut self, pattern: &str, doc: &str, ws: Ws) -> App {
        let runtime = match &self.ws_runtime {
            Some(rt) => Arc::clone(rt),
            None => {
                let rt = sutegi_ws::WsRuntime::start(self.ws_config.clone())
                    .expect("sutegi-ws: failed to start reactor (kqueue/epoll/pipe unavailable)");
                self.ws_runtime = Some(Arc::clone(&rt));
                rt
            }
        };
        let handlers = Arc::new(ws.handlers);
        self.route(Method::Get, pattern, doc, move |c: &Ctx| {
            ws::upgrade_response(c.req, &runtime, &handlers)
        })
    }

    /// Register a non-streaming AI tool: a `name`, a `description`, its JSON
    /// Schema `parameters` (build with [`schema`]), and a closure that receives
    /// the shared state via [`ToolCtx`] and the validated JSON `args`. Mounted
    /// automatically at `POST /__tools/:name` and listed in the `/__tools`
    /// manifest + `/__introspect`.
    pub fn tool<F>(mut self, name: &str, description: &str, parameters: Json, call: F) -> App
    where
        F: Fn(&ToolCtx, Json) -> Result<Json, Error> + Send + Sync + 'static,
    {
        self.tool_defs.push(ToolDef {
            name: name.to_string(),
            description: description.to_string(),
            schema: parameters,
            body: ToolBody::Unary(Box::new(call)),
        });
        self
    }

    /// Register a streaming AI tool, invoked at `POST /__tools/:name/stream` and
    /// answered as Server-Sent Events. The closure emits tokens through the
    /// [`SseSink`]; it owns its [`ToolCtx`], so it may keep streaming after the
    /// response has begun.
    pub fn stream_tool<F>(mut self, name: &str, description: &str, parameters: Json, run: F) -> App
    where
        F: Fn(&ToolCtx, Json, &mut SseSink) -> std::io::Result<()> + Send + Sync + 'static,
    {
        self.tool_defs.push(ToolDef {
            name: name.to_string(),
            description: description.to_string(),
            schema: parameters,
            body: ToolBody::Stream(Box::new(run)),
        });
        self
    }

    /// Register a group of routes sharing a path `prefix` and group-scoped
    /// `middleware`: `app.group("/api", vec![mw(auth)], |g| g.get(...))`.
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

    pub fn get<R: IntoResponse>(
        self,
        pattern: &str,
        doc: &str,
        handler: impl Fn(&Ctx) -> R + Send + Sync + 'static,
    ) -> App {
        self.route(Method::Get, pattern, doc, handler)
    }

    /// Serve a directory of static files under `prefix` (a trailing `*path`
    /// rest route). `prefix = "/"` makes `dir` the site root; a directory
    /// (or the bare prefix) serves its `index.html`. Traversal, dotfiles
    /// and backslashes are 404s. Routes match in registration order, so
    /// register API routes first and `static_dir` last.
    pub fn static_dir(self, prefix: &str, dir: impl Into<std::path::PathBuf>) -> App {
        let root: std::path::PathBuf = dir.into();
        let pattern = format!("{}/*path", prefix.trim_end_matches('/'));
        let doc = format!("Static files from {}.", root.display());
        self.get(&pattern, &doc, move |c: &Ctx| {
            files::serve(&root, c.param("path").unwrap_or(""))
        })
    }

    pub fn post<R: IntoResponse>(
        self,
        pattern: &str,
        doc: &str,
        handler: impl Fn(&Ctx) -> R + Send + Sync + 'static,
    ) -> App {
        self.route(Method::Post, pattern, doc, handler)
    }

    pub fn put<R: IntoResponse>(
        self,
        pattern: &str,
        doc: &str,
        handler: impl Fn(&Ctx) -> R + Send + Sync + 'static,
    ) -> App {
        self.route(Method::Put, pattern, doc, handler)
    }

    pub fn delete<R: IntoResponse>(
        self,
        pattern: &str,
        doc: &str,
        handler: impl Fn(&Ctx) -> R + Send + Sync + 'static,
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

    /// Guard the agent/ops surface. The closure runs for every `/__`-prefixed
    /// request *except* the `/__health`/`/__ready` probes — i.e. for
    /// `/__introspect`, `/__metrics`, `/__tools*` (tool invocation) and any
    /// `/__`-mounted route such as `/__migrations`. Returning `Some(resp)`
    /// denies the request with that response; `None` lets it through.
    ///
    /// Introspection and tool invocation are **open by default** (that's the
    /// agent-native contract). In any deployment where the agent surface must
    /// not be public, set a guard here — e.g. require a shared token or an
    /// internal network. This runs ahead of the global [`middleware`](Self::middleware)
    /// chain, and unlike the probes and pre-`0.5.2` behaviour, it *can* gate
    /// `/__introspect` and `/__metrics`.
    /// ```ignore
    /// app.ops_guard(|req| match req.header("authorization") {
    ///     Some(tok) if tok == expected => None,             // allow
    ///     _ => Some(Response::new(401).with_body(&b"unauthorized"[..])),
    /// })
    /// ```
    pub fn ops_guard(
        mut self,
        guard: impl Fn(&Request) -> Option<Response> + Send + Sync + 'static,
    ) -> App {
        self.ops_guard = Some(Box::new(guard));
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
        mut self,
    ) -> (
        usize,
        Limits,
        impl Fn(Request) -> Response + Send + Sync + 'static,
    ) {
        // Mount any registered AI tools as routes + introspection entries.
        if !self.tool_defs.is_empty() {
            self = self.mount_tools();
        }
        let limits = self.limits;
        let introspect = self.introspection();
        let routes = Arc::new(self.routes);
        let middleware = Arc::new(self.middleware);
        let ops_guard = Arc::new(self.ops_guard);
        let after = Arc::new(self.after);
        let readiness = Arc::new(self.readiness);
        let state = Arc::new(self.state);
        let metrics = Arc::new(Metrics::default());
        let workers = self.workers;

        let service = move |req: Request| -> Response {
            metrics.in_flight.fetch_add(1, Ordering::Relaxed);

            // Inner closure so we can post-process (record metrics) on every path.
            let resp = (|| {
                // Liveness/readiness probes: always open and matched before any
                // guard — orchestrator probes must not need app credentials and
                // they disclose nothing sensitive.
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
                    _ => {}
                }

                // Agent/ops surface guard. Gates every other `/__`-prefixed
                // endpoint — `/__introspect`, `/__metrics`, `/__tools*` (tool
                // invocation) and any `/__`-mounted route like `/__migrations`.
                // Runs before the global middleware chain AND before routing, so
                // it protects the internal surface (tool routes live in the
                // route table; introspect/metrics are matched below). Unset =
                // open, the historical default.
                if req.path.starts_with("/__") {
                    if let Some(guard) = &*ops_guard {
                        if let Some(resp) = guard(&req) {
                            return resp;
                        }
                    }
                }

                // Global middleware chain — first responder wins. Now runs
                // ahead of `/__metrics` and `/__introspect` (matched just
                // below), so an app-wide auth / rate-limit guard covers them —
                // previously they were dispatched before this loop and could
                // not be protected by middleware at all.
                for mw in middleware.iter() {
                    if let Some(resp) = mw(&req) {
                        return resp;
                    }
                }

                // Sensitive operational endpoints: reachable only past the ops
                // guard and the global middleware chain above.
                match req.path.as_str() {
                    "/__metrics" => {
                        return Response::new(200)
                            .with_header("content-type", "text/plain; version=0.0.4")
                            .with_body(metrics.prometheus().into_bytes());
                    }
                    "/__introspect" => return json(200, &introspect),
                    _ => {}
                }

                // Route table (run group-scoped middleware before the handler).
                let segs = split_segments(&req.path);
                if let Some((route, params)) = match_route(&routes, req.method, &segs) {
                    for mw in &route.middleware {
                        if let Some(resp) = mw(&req) {
                            return resp;
                        }
                    }
                    let ctx = Ctx {
                        req: &req,
                        params,
                        state: Arc::clone(&state),
                    };
                    return (route.handler)(&ctx);
                }

                // Distinguish "no such path" from "wrong method".
                if routes.iter().any(|r| match_segs(&r.segs, &segs).is_some()) {
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

    /// The app as its bare request-service closure — the same function every
    /// `run*` variant serves over TCP, minus the socket. Feed it a [`Request`]
    /// and get the [`Response`] back: in-process tests and benchmarks without
    /// binding a port.
    pub fn service(self) -> impl Fn(Request) -> Response + Send + Sync + 'static {
        let (_, _, service) = self.into_service();
        service
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

    /// The one-call production entrypoint. Resolves the bind address from
    /// `argv[1]`, else `HOST:PORT` (defaults `0.0.0.0:8080`); honours a `WORKERS`
    /// env override; prints a short banner; and drains gracefully on SIGTERM
    /// when the `graceful` feature is on (falling back to [`App::run`]).
    pub fn serve(mut self) -> std::io::Result<()> {
        if let Some(n) = std::env::var("WORKERS").ok().and_then(|w| w.parse().ok()) {
            self.workers = n;
        }
        let addr = std::env::args().nth(1).unwrap_or_else(|| {
            let host = std::env::var("HOST").unwrap_or_else(|_| "0.0.0.0".to_string());
            let port = std::env::var("PORT").unwrap_or_else(|_| "8080".to_string());
            format!("{host}:{port}")
        });
        println!("sutegi · {} on http://{addr}", self.name);
        println!("  ops: /__health /__ready /__metrics /__introspect");
        #[cfg(feature = "graceful")]
        {
            self.run_graceful(&addr)
        }
        #[cfg(not(feature = "graceful"))]
        {
            self.run(&addr)
        }
    }

    /// Turn registered [`ToolDef`]s into `/__tools*` routes + introspection
    /// entries. Called once from [`App::into_service`].
    fn mount_tools(mut self) -> App {
        let defs = Arc::new(std::mem::take(&mut self.tool_defs));
        for d in defs.iter() {
            self = self.register_tool(manifest_entry(
                &d.name,
                &d.description,
                d.schema.clone(),
                d.is_stream(),
            ));
        }
        let manifest = Json::arr(
            defs.iter()
                .map(|d| manifest_entry(&d.name, &d.description, d.schema.clone(), d.is_stream()))
                .collect(),
        );
        self = self.get(
            "/__tools",
            "List callable AI tools as an LLM tool-calling manifest.",
            move |_c| json(200, &manifest),
        );
        let invoke = Arc::clone(&defs);
        self = self.post(
            "/__tools/:name",
            "Invoke an AI tool by name with a JSON argument object.",
            move |c: &Ctx| tool_invoke(&invoke, c),
        );
        let stream = Arc::clone(&defs);
        self = self.post(
            "/__tools/:name/stream",
            "Invoke a streaming AI tool by name; response is text/event-stream (SSE).",
            move |c: &Ctx| tool_stream(&stream, c),
        );
        self
    }
}

fn manifest_entry(name: &str, description: &str, parameters: Json, streaming: bool) -> Json {
    Json::obj(vec![
        ("name", Json::str(name)),
        ("description", Json::str(description)),
        ("input_schema", parameters),
        ("streaming", Json::Bool(streaming)),
    ])
}

/// Validate `args` against a tool's declared schema (when the `validate` feature
/// is on); returns a ready `422` [`Error`] on failure.
#[allow(unused_variables)]
fn validate_tool_args(schema: &Json, args: &Json) -> Result<(), Error> {
    #[cfg(feature = "validate")]
    if let Err(errs) = sutegi_validate::validate_schema(schema, args) {
        return Err(Error::unprocessable("validation failed").with_fields(errs.to_json()));
    }
    Ok(())
}

fn tool_invoke(defs: &[ToolDef], c: &Ctx) -> Response {
    let name = c.param("name").unwrap_or("").to_string();
    let args = match c.json() {
        Ok(v) => v,
        Err(e) => return e.into_response(),
    };
    let def = match defs.iter().find(|d| d.name == name && !d.is_stream()) {
        Some(d) => d,
        None => return Error::not_found(format!("unknown tool '{name}'")).into_response(),
    };
    if let Err(e) = validate_tool_args(&def.schema, &args) {
        return e.into_response();
    }
    let tctx = ToolCtx {
        state: Arc::clone(&c.state),
        params: c.params.clone(),
    };
    match &def.body {
        ToolBody::Unary(f) => match f(&tctx, args) {
            Ok(out) => json(200, &out),
            Err(e) => e.into_response(),
        },
        ToolBody::Stream(_) => Error::bad_request(format!(
            "'{name}' is a streaming tool; POST /__tools/{name}/stream"
        ))
        .into_response(),
    }
}

fn tool_stream(defs: &Arc<Vec<ToolDef>>, c: &Ctx) -> Response {
    let name = c.param("name").unwrap_or("").to_string();
    let args = match c.json() {
        Ok(v) => v,
        Err(e) => return e.into_response(),
    };
    let def = match defs.iter().find(|d| d.name == name && d.is_stream()) {
        Some(d) => d,
        None => {
            return Error::not_found(format!("unknown streaming tool '{name}'")).into_response()
        }
    };
    if let Err(e) = validate_tool_args(&def.schema, &args) {
        return e.into_response();
    }
    // Everything the SSE producer needs must be owned (it runs after we return).
    let tctx = ToolCtx {
        state: Arc::clone(&c.state),
        params: c.params.clone(),
    };
    let defs = Arc::clone(defs);
    sse(move |sink| {
        if let Some(def) = defs.iter().find(|d| d.name == name && d.is_stream()) {
            if let ToolBody::Stream(f) = &def.body {
                return f(&tctx, args, sink);
            }
        }
        Ok(())
    })
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
    pub fn route<R: IntoResponse>(
        mut self,
        method: Method,
        pattern: &str,
        doc: &str,
        handler: impl Fn(&Ctx) -> R + Send + Sync + 'static,
    ) -> Group {
        let pattern = join_prefix(&self.prefix, pattern);
        self.routes.push(Route {
            method,
            segs: compile_pattern(&pattern),
            pattern,
            doc: doc.to_string(),
            handler: Box::new(move |c: &Ctx| handler(c).into_response()),
            middleware: self.middleware.clone(),
        });
        self
    }

    pub fn get<R: IntoResponse>(
        self,
        p: &str,
        doc: &str,
        h: impl Fn(&Ctx) -> R + Send + Sync + 'static,
    ) -> Group {
        self.route(Method::Get, p, doc, h)
    }
    pub fn post<R: IntoResponse>(
        self,
        p: &str,
        doc: &str,
        h: impl Fn(&Ctx) -> R + Send + Sync + 'static,
    ) -> Group {
        self.route(Method::Post, p, doc, h)
    }
    pub fn put<R: IntoResponse>(
        self,
        p: &str,
        doc: &str,
        h: impl Fn(&Ctx) -> R + Send + Sync + 'static,
    ) -> Group {
        self.route(Method::Put, p, doc, h)
    }
    pub fn delete<R: IntoResponse>(
        self,
        p: &str,
        doc: &str,
        h: impl Fn(&Ctx) -> R + Send + Sync + 'static,
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

/// Split a request path into segments — once per request, shared by route
/// matching and the 405 method check.
fn split_segments(path: &str) -> Vec<&str> {
    path.trim_matches('/').split('/').collect()
}

fn match_route<'a>(
    routes: &'a [Route],
    method: Method,
    segs: &[&str],
) -> Option<(&'a Route, Params)> {
    for r in routes {
        if r.method != method {
            continue;
        }
        if let Some(params) = match_segs(&r.segs, segs) {
            return Some((r, params));
        }
    }
    None
}

/// Match pre-split path segments against a compiled pattern, capturing params.
/// A trailing `Rest` segment absorbs everything left (possibly nothing).
fn match_segs(pattern: &[Seg], path: &[&str]) -> Option<Params> {
    let has_rest = matches!(pattern.last(), Some(Seg::Rest(_)));
    if has_rest {
        if path.len() + 1 < pattern.len() {
            return None;
        }
    } else if pattern.len() != path.len() {
        return None;
    }
    let mut params = Params::new();
    for (i, seg) in pattern.iter().enumerate() {
        match seg {
            Seg::Lit(lit) => {
                if lit != path[i] {
                    return None;
                }
            }
            Seg::Param(name) => {
                params.insert(name.clone(), path[i].to_string());
            }
            Seg::Rest(name) => {
                params.insert(name.clone(), path.get(i..).unwrap_or(&[]).join("/"));
                break;
            }
        }
    }
    Some(params)
}

// ---- response helpers -----------------------------------------------------

/// A `text/plain` response.
pub fn text(status: u16, body: &str) -> Response {
    Response::new(status).with_body(body.as_bytes().to_vec())
}

/// An `application/json` response from a `Json` value. Serializes straight
/// into one buffer via [`Json::write_to`] — `to_string()` would build that
/// buffer inside `Display` and then copy it again.
pub fn json(status: u16, value: &Json) -> Response {
    let mut body = String::with_capacity(128);
    value.write_to(&mut body);
    Response::new(status)
        .with_header("content-type", "application/json; charset=utf-8")
        .with_body(body.into_bytes())
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
        sutegi_crypto::base64_encode(format!("{}:{}", user, pass).as_bytes())
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

    /// Compile-then-match, the way the service closure does it.
    fn match_pattern(pattern: &str, path: &str) -> Option<Params> {
        match_segs(&compile_pattern(pattern), &split_segments(path))
    }

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
    fn rest_segment_captures_remainder() {
        let p = match_pattern("/assets/*path", "/assets/img/logo.png").unwrap();
        assert_eq!(p.get("path").map(String::as_str), Some("img/logo.png"));
        // zero remaining segments is a valid (empty) rest
        let p = match_pattern("/assets/*path", "/assets").unwrap();
        assert_eq!(p.get("path").map(String::as_str), Some(""));
        // a root-level rest matches everything, "/" included
        let p = match_pattern("/*path", "/pkg/app.wasm").unwrap();
        assert_eq!(p.get("path").map(String::as_str), Some("pkg/app.wasm"));
        assert_eq!(
            match_pattern("/*path", "/")
                .unwrap()
                .get("path")
                .map(String::as_str),
            Some("")
        );
        // literals before the rest still gate the match
        assert!(match_pattern("/assets/*path", "/api/x").is_none());
    }

    #[test]
    fn root_matches() {
        assert!(match_pattern("/", "/").is_some());
    }

    #[test]
    fn literal_segments_must_match_exactly() {
        assert!(match_pattern("/users/:id", "/teams/42").is_none());
        assert!(match_pattern("/a/b", "/a/b").is_some());
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
            g.get("/users", "list", |_c| text(200, "ok"))
                .post("/users/:id", "update", |_c| text(200, "ok"))
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

    #[test]
    fn ops_guard_gates_internal_surface_but_not_probes() {
        // A guard that denies everything it is asked about.
        let app = App::new("t")
            .get("/", "home", |_c| text(200, "ok"))
            .ops_guard(|_req| Some(text(401, "nope")));
        let svc = app.service();
        let call = |path: &str, method: Method| {
            svc(Request {
                path: path.into(),
                ..req(method, b"")
            })
        };
        // Liveness/readiness probes must stay open regardless of the guard.
        assert_eq!(call("/__health", Method::Get).status, 200);
        // Introspection + metrics are now gated (were previously un-gatable).
        assert_eq!(call("/__introspect", Method::Get).status, 401);
        assert_eq!(call("/__metrics", Method::Get).status, 401);
        // Tool invocation is gated even for a name that isn't a mounted route.
        assert_eq!(call("/__tools/anything", Method::Post).status, 401);
        // Ordinary routes are untouched by the ops guard.
        assert_eq!(call("/", Method::Get).status, 200);
    }

    #[test]
    fn introspect_is_open_without_an_ops_guard() {
        // Unchanged default: no guard => the agent surface is served.
        let svc = App::new("t")
            .get("/", "home", |_c| text(200, "ok"))
            .service();
        let resp = svc(Request {
            path: "/__introspect".into(),
            ..req(Method::Get, b"")
        });
        assert_eq!(resp.status, 200);
    }

    #[test]
    fn into_response_covers_common_return_types() {
        assert_eq!("hi".into_response().status, 200);
        assert_eq!(String::from("hi").into_response().status, 200);
        assert_eq!(().into_response().status, 204);
        assert_eq!((201, Json::Null).into_response().status, 201);
        assert_eq!((404, "nope").into_response().status, 404);
        let ok: Result<&str, Error> = Ok("ok");
        assert_eq!(ok.into_response().status, 200);
        let err: Result<&str, Error> = Err(Error::not_found("gone"));
        assert_eq!(err.into_response().status, 404);
    }

    #[test]
    fn error_renders_message_and_fields() {
        let resp = Error::unprocessable("bad")
            .with_fields(Json::obj(vec![("title", Json::str("required"))]))
            .into_response();
        assert_eq!(resp.status, 422);
        // 500 by default from a String via `?`.
        let from_string: Error = "boom".to_string().into();
        assert_eq!(from_string.status, 500);
    }

    #[test]
    fn ctx_reads_typed_state_and_params() {
        let mut state = StateMap::new();
        state.insert(TypeId::of::<u32>(), Arc::new(7u32));
        let state = Arc::new(state);
        let req = Request {
            method: Method::Get,
            path: "/x/9".into(),
            query: "a=1".into(),
            version: "HTTP/1.1".into(),
            headers: vec![],
            body: vec![],
            peer: None,
        };
        let mut params = Params::new();
        params.insert("id".into(), "9".into());
        let ctx = Ctx {
            req: &req,
            params,
            state: Arc::clone(&state),
        };
        assert_eq!(ctx.param("id"), Some("9"));
        assert_eq!(*ctx.state::<u32>(), 7);
        assert_eq!(ctx.try_state::<String>(), None);
        assert_eq!(ctx.query().get("a").map(String::as_str), Some("1"));
    }
}
