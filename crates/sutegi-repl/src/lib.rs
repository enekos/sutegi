//! A tinker-style interactive shell for sutegi apps — Laravel's `artisan
//! tinker`, reimagined for an agent-native framework.
//!
//! This is **not** a Rust evaluator (that would need a compiler toolchain and
//! third-party machinery). It is a command shell over the surfaces a sutegi
//! app already exposes:
//!
//! - the **agent contract** — `/__introspect`, `/__tools`, tool invocation
//!   (streaming tools print their SSE frames live), and raw HTTP requests
//!   through the app's own routes;
//! - the **data layer** — raw SQL, a `where`-clause query DSL, KV, the event
//!   store, and the job queue, when a [`Backend`] is attached.
//!
//! Two transports, one command set:
//!
//! ```no_run
//! use sutegi_repl::Repl;
//! use sutegi_web::App;
//!
//! // In-process: consume the built App; data commands work via `.db(...)`.
//! let app = App::new("demo").get("/", "Health.", |_| "ok");
//! Repl::new(app).run().unwrap();
//! ```
//!
//! Remote mode is the agent contract exercised by a human: it drives a
//! *running* app over plain HTTP with no source access — exactly the way an
//! LLM does — so `sutegi repl <addr>` works against any sutegi app:
//!
//! ```no_run
//! sutegi_repl::Repl::connect("127.0.0.1:8080").run().unwrap();
//! ```
//!
//! Line editing is deliberately plain `stdin` (zero deps, works everywhere);
//! wrap with `rlwrap` if you want history and arrow keys.

use std::io::{self, BufRead, Read, Write};
use std::net::TcpStream;

use sutegi_http::{Body, Method, Request, Response};
use sutegi_json::Json;
#[cfg(feature = "orm")]
use sutegi_orm::{Backend, QueryBuilder, Value};
use sutegi_web::App;

type Service = Box<dyn Fn(Request) -> Response>;

enum Surface {
    /// The app's request closure, no socket — `App::service()`.
    InProcess(Service),
    /// A running server, spoken to over one-shot HTTP/1.1 connections.
    Remote { host: String, port: u16 },
}

/// The REPL: a [`Surface`] to send requests through, plus an optional data
/// backend for the `sql`/`q`/`kv`/… commands.
pub struct Repl {
    surface: Surface,
    #[cfg(feature = "orm")]
    db: Option<Box<dyn Backend>>,
}

impl Repl {
    /// Wrap a built [`App`] for in-process tinkering. Consumes the app (the
    /// same closure `serve()` would run — routing, middleware, tools, ops
    /// endpoints — minus the socket).
    pub fn new(app: App) -> Repl {
        Repl {
            surface: Surface::InProcess(Box::new(app.service())),
            #[cfg(feature = "orm")]
            db: None,
        }
    }

    /// Attach to a running sutegi app at `addr` (`host:port`, an optional
    /// `http://` prefix is tolerated). Everything the remote REPL does goes
    /// through the app's public HTTP surface.
    pub fn connect(addr: &str) -> Repl {
        let (host, port) = parse_addr(addr);
        Repl {
            surface: Surface::Remote { host, port },
            #[cfg(feature = "orm")]
            db: None,
        }
    }

    /// Attach a data backend (usually the same handle the app holds in
    /// `.state(...)` — `Db` and `Pg` are `Clone`). Enables the data commands.
    #[cfg(feature = "orm")]
    pub fn db(mut self, db: impl Backend + 'static) -> Repl {
        self.db = Some(Box::new(db));
        self
    }

    /// The interactive loop: read a line from stdin, run it, print, repeat.
    pub fn run(self) -> io::Result<()> {
        let stdin = io::stdin();
        let mut out = io::stdout();
        writeln!(out, "sutegi repl — 'help' lists commands, 'exit' leaves")?;
        let mut line = String::new();
        loop {
            write!(out, "sutegi> ")?;
            out.flush()?;
            line.clear();
            if stdin.lock().read_line(&mut line)? == 0 {
                writeln!(out)?;
                return Ok(()); // EOF (ctrl-d / end of piped script)
            }
            let cmd = line.trim();
            if cmd.is_empty() {
                continue;
            }
            if matches!(cmd, "exit" | "quit") {
                return Ok(());
            }
            self.dispatch(cmd, &mut out)?;
        }
    }

    /// Evaluate one command and return its output — the programmatic seam the
    /// interactive loop is built on, and what tests drive.
    pub fn eval(&self, line: &str) -> String {
        let mut buf: Vec<u8> = Vec::new();
        let _ = self.dispatch(line.trim(), &mut buf);
        String::from_utf8_lossy(&buf).into_owned()
    }

    fn dispatch(&self, line: &str, out: &mut dyn Write) -> io::Result<()> {
        let (cmd, rest) = split_first(line);
        match cmd {
            "help" => help(out),
            "introspect" => self.request(Method::Get, "/__introspect", None, out),
            "routes" => self.routes(out),
            "models" => self.manifest_slice("/__introspect", "models", out),
            "tools" => self.tools(out),
            "tool" => self.tool_schema(rest, out),
            "call" => self.call(rest, out),
            "get" | "post" | "put" | "patch" | "delete" => {
                let (path, body) = split_first(rest);
                if path.is_empty() {
                    return writeln!(out, "usage: {cmd} <path> [json body]");
                }
                let body = if body.is_empty() { None } else { Some(body) };
                self.request(Method::parse(&cmd.to_uppercase()), path, body, out)
            }
            "sql" | "table" | "count" | "q" | "kv" | "events" | "jobs" => {
                self.data_cmd(cmd, rest, out)
            }
            other => writeln!(out, "unknown command '{other}' — try 'help'"),
        }
    }

    // ---- surface commands ---------------------------------------------

    fn routes(&self, out: &mut dyn Write) -> io::Result<()> {
        let manifest = match self.fetch_json("/__introspect") {
            Ok(j) => j,
            Err(e) => return writeln!(out, "error: {e}"),
        };
        let routes = manifest.get("routes").and_then(Json::as_array);
        for r in routes.into_iter().flatten() {
            let method = r.get("method").and_then(Json::as_str).unwrap_or("?");
            let pattern = r.get("pattern").and_then(Json::as_str).unwrap_or("?");
            let doc = r.get("doc").and_then(Json::as_str).unwrap_or("");
            writeln!(out, "{method:<7} {pattern:<32} {doc}")?;
        }
        Ok(())
    }

    fn tools(&self, out: &mut dyn Write) -> io::Result<()> {
        let manifest = match self.tool_manifest() {
            Ok(j) => j,
            Err(e) => return writeln!(out, "error: {e}"),
        };
        let tools = manifest.as_array().map(Vec::as_slice).unwrap_or(&[]);
        if tools.is_empty() {
            return writeln!(out, "no tools registered");
        }
        for t in tools {
            let name = t.get("name").and_then(Json::as_str).unwrap_or("?");
            let streaming = t.get("streaming").and_then(Json::as_bool).unwrap_or(false);
            let desc = t.get("description").and_then(Json::as_str).unwrap_or("");
            let tag = if streaming { "streaming" } else { "unary" };
            writeln!(out, "{name:<24} [{tag:<9}] {desc}")?;
        }
        Ok(())
    }

    fn tool_schema(&self, rest: &str, out: &mut dyn Write) -> io::Result<()> {
        let (name, _) = split_first(rest);
        if name.is_empty() {
            return writeln!(out, "usage: tool <name>");
        }
        match self.find_tool(name) {
            Ok(Some(entry)) => writeln!(out, "{}", entry.to_pretty()),
            Ok(None) => writeln!(out, "unknown tool '{name}'"),
            Err(e) => writeln!(out, "error: {e}"),
        }
    }

    /// Invoke a tool by name. Streaming tools are detected from the manifest
    /// and their SSE frames are written through as they arrive.
    fn call(&self, rest: &str, out: &mut dyn Write) -> io::Result<()> {
        let (name, args) = split_first(rest);
        if name.is_empty() {
            return writeln!(out, "usage: call <name> [{{json args}}]");
        }
        let args = if args.is_empty() { "{}" } else { args };
        if Json::parse(args).is_err() {
            return writeln!(out, "arguments must be valid JSON, got: {args}");
        }
        let streaming = match self.find_tool(name) {
            Ok(Some(entry)) => entry
                .get("streaming")
                .and_then(Json::as_bool)
                .unwrap_or(false),
            Ok(None) => return writeln!(out, "unknown tool '{name}' — 'tools' lists them"),
            Err(e) => return writeln!(out, "error: {e}"),
        };
        let path = if streaming {
            format!("/__tools/{name}/stream")
        } else {
            format!("/__tools/{name}")
        };
        self.request(Method::Post, &path, Some(args), out)
    }

    fn manifest_slice(&self, target: &str, key: &str, out: &mut dyn Write) -> io::Result<()> {
        match self.fetch_json(target) {
            Ok(j) => {
                let slice = j.get(key).cloned().unwrap_or(Json::arr(vec![]));
                writeln!(out, "{}", slice.to_pretty())
            }
            Err(e) => writeln!(out, "error: {e}"),
        }
    }

    fn tool_manifest(&self) -> Result<Json, String> {
        // `/__tools` only exists once a tool is registered; report that
        // usefully instead of a bare 404.
        self.fetch_json("/__tools")
            .map_err(|e| format!("{e} (does the app register any tools?)"))
    }

    fn find_tool(&self, name: &str) -> Result<Option<Json>, String> {
        let manifest = self.tool_manifest()?;
        Ok(manifest
            .as_array()
            .into_iter()
            .flatten()
            .find(|t| t.get("name").and_then(Json::as_str) == Some(name))
            .cloned())
    }

    // ---- transport ------------------------------------------------------

    /// Perform a request and print the response: JSON bodies pretty-printed,
    /// streaming bodies written through incrementally, non-200 statuses
    /// prefixed on their own `[status]` line.
    fn request(
        &self,
        method: Method,
        target: &str,
        body: Option<&str>,
        out: &mut dyn Write,
    ) -> io::Result<()> {
        match &self.surface {
            Surface::InProcess(svc) => {
                let resp = svc(build_request(method, target, body));
                if resp.status != 200 {
                    writeln!(out, "[{}]", resp.status)?;
                }
                let ct = header(&resp.headers, "content-type").unwrap_or_default();
                match resp.body {
                    Body::Full(bytes) => print_body(out, &ct, &bytes),
                    Body::Stream(producer) => {
                        // The producer flushes per frame; `out` sees them live.
                        if let Err(e) = producer(out) {
                            writeln!(out, "stream error: {e}")?;
                        }
                        writeln!(out)
                    }
                    // A REPL request is not a socket to hand over.
                    Body::Upgrade(_) => {
                        writeln!(out, "[101] connection upgrade (unsupported here)")
                    }
                }
            }
            Surface::Remote { host, port } => {
                match self.remote(host, *port, method, target, body, out) {
                    Ok(()) => Ok(()),
                    Err(e) => writeln!(
                        out,
                        "request failed: {e} (is the app running at {host}:{port}?)"
                    ),
                }
            }
        }
    }

    /// One `Connection: close` HTTP/1.1 exchange, reading the body
    /// incrementally so close-framed SSE streams print frame by frame.
    fn remote(
        &self,
        host: &str,
        port: u16,
        method: Method,
        target: &str,
        body: Option<&str>,
        out: &mut dyn Write,
    ) -> io::Result<()> {
        let mut stream = TcpStream::connect((host, port))?;
        let body = body.unwrap_or("");
        let mut req = format!(
            "{} {} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\nAccept: application/json\r\n",
            method.as_str(),
            target,
            host
        );
        if !body.is_empty() {
            req.push_str("Content-Type: application/json\r\n");
            req.push_str(&format!("Content-Length: {}\r\n", body.len()));
        }
        req.push_str("\r\n");
        req.push_str(body);
        stream.write_all(req.as_bytes())?;

        let mut head: Vec<u8> = Vec::new();
        let mut json_buf: Option<Vec<u8>> = None; // Some => buffer to pretty-print at EOF
        let mut in_body = false;
        let mut buf = [0u8; 4096];
        loop {
            let n = stream.read(&mut buf)?;
            if n == 0 {
                break;
            }
            let mut chunk: &[u8] = &buf[..n];
            if !in_body {
                head.extend_from_slice(chunk);
                let Some(pos) = find_blank_line(&head) else {
                    continue;
                };
                let header_text = String::from_utf8_lossy(&head[..pos]).into_owned();
                let status = parse_status(&header_text);
                if status != 200 {
                    writeln!(out, "[{status}]")?;
                }
                let json = header_line(&header_text, "content-type")
                    .map(|ct| ct.contains("application/json"))
                    .unwrap_or(false);
                json_buf = json.then(Vec::new);
                in_body = true;
                let rest = head.split_off(pos + 4);
                match &mut json_buf {
                    Some(b) => b.extend_from_slice(&rest),
                    None => {
                        out.write_all(&rest)?;
                        out.flush()?;
                    }
                }
                chunk = &[];
            }
            match &mut json_buf {
                Some(b) => b.extend_from_slice(chunk),
                None => {
                    if !chunk.is_empty() {
                        out.write_all(chunk)?;
                        out.flush()?;
                    }
                }
            }
        }
        match json_buf {
            Some(b) => print_body(out, "application/json", &b),
            None => writeln!(out),
        }
    }

    /// A buffered GET that must yield JSON — the manifest fetches.
    fn fetch_json(&self, target: &str) -> Result<Json, String> {
        match &self.surface {
            Surface::InProcess(svc) => {
                let resp = svc(build_request(Method::Get, target, None));
                if resp.status != 200 {
                    return Err(format!("GET {target} -> {}", resp.status));
                }
                match resp.body {
                    Body::Full(bytes) => Json::parse(&String::from_utf8_lossy(&bytes)),
                    Body::Stream(_) => Err(format!("GET {target} unexpectedly streams")),
                    Body::Upgrade(_) => Err(format!("GET {target} unexpectedly upgrades")),
                }
            }
            Surface::Remote { host, port } => {
                let mut buf: Vec<u8> = Vec::new();
                self.remote(host, *port, Method::Get, target, None, &mut buf)
                    .map_err(|e| e.to_string())?;
                let text = String::from_utf8_lossy(&buf);
                let text = text.trim();
                if let Some(status) = text.strip_prefix('[').and_then(|t| t.split(']').next()) {
                    if text.starts_with('[') && status.chars().all(|c| c.is_ascii_digit()) {
                        return Err(format!("GET {target} -> {status}"));
                    }
                }
                Json::parse(text)
            }
        }
    }

    // ---- data commands ---------------------------------------------------

    #[cfg(feature = "orm")]
    fn data_cmd(&self, cmd: &str, rest: &str, out: &mut dyn Write) -> io::Result<()> {
        let Some(db) = &self.db else {
            return writeln!(
                out,
                "no database attached — in-process, chain `.db(db)`; \
                 a remote REPL reaches data through the app's routes and tools"
            );
        };
        let db = db.as_ref();
        let result = match cmd {
            "sql" => run_sql(db, rest),
            "table" => run_table(db, rest),
            "count" => run_count(db, rest),
            "q" => parse_query(rest)
                .and_then(|qb| db.select(&qb))
                .map(rows_pretty),
            "kv" => run_kv(db, rest),
            "events" => run_events(db, rest),
            "jobs" => run_jobs(db),
            _ => unreachable!("dispatch only routes data commands here"),
        };
        match result {
            Ok(text) => writeln!(out, "{text}"),
            Err(e) => writeln!(out, "error: {e}"),
        }
    }

    #[cfg(not(feature = "orm"))]
    fn data_cmd(&self, _cmd: &str, _rest: &str, out: &mut dyn Write) -> io::Result<()> {
        writeln!(
            out,
            "data commands need the `orm` feature (and an attached db); \
             a remote REPL reaches data through the app's routes and tools"
        )
    }
}

fn help(out: &mut dyn Write) -> io::Result<()> {
    writeln!(
        out,
        "surface (works in-process and remote)
  introspect                       full app manifest (/__introspect)
  routes | models | tools          slices of the manifest
  tool <name>                      one tool's schema
  call <name> [{{json args}}]       invoke a tool; streaming tools print SSE frames live
  get|post|put|patch|delete <path> [json]
                                   raw request through the app

data (needs an attached backend: Repl::db(...))
  sql <statement>                  raw SQL — SELECT prints rows, else the affected count
  table <name> [n]                 peek at a table (default 10 rows)
  count <table> [where ...]        row count
  q <table> [select a,b] [where <col> <op> <val> [and ...]] [order <col> [desc]] [limit n] [offset n]
  kv get|set|del|keys|scan <ns> [key] [json]
  events [stream] [n]              recent events from the event store
  jobs                             queue depth + recent jobs

  exit                             leave"
    )
}

// ---- request plumbing -----------------------------------------------------

fn build_request(method: Method, target: &str, body: Option<&str>) -> Request {
    let (path, query) = match target.split_once('?') {
        Some((p, q)) => (p.to_string(), q.to_string()),
        None => (target.to_string(), String::new()),
    };
    let body: Vec<u8> = body.map(|b| b.as_bytes().to_vec()).unwrap_or_default();
    let mut headers = vec![("accept".to_string(), "application/json".to_string())];
    if !body.is_empty() {
        headers.push(("content-type".to_string(), "application/json".to_string()));
        headers.push(("content-length".to_string(), body.len().to_string()));
    }
    Request {
        method,
        path,
        query,
        version: "HTTP/1.1".to_string(),
        headers,
        body,
        peer: Some("repl".to_string()),
    }
}

fn header(headers: &[(String, String)], name: &str) -> Option<String> {
    headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.clone())
}

fn print_body(out: &mut dyn Write, content_type: &str, bytes: &[u8]) -> io::Result<()> {
    let text = String::from_utf8_lossy(bytes);
    if content_type.contains("application/json") {
        if let Ok(j) = Json::parse(&text) {
            return writeln!(out, "{}", j.to_pretty());
        }
    }
    writeln!(out, "{}", text.trim_end())
}

fn find_blank_line(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n")
}

fn parse_status(header_text: &str) -> u16 {
    header_text
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|s| s.parse().ok())
        .unwrap_or(0)
}

fn header_line(header_text: &str, name: &str) -> Option<String> {
    header_text.lines().skip(1).find_map(|l| {
        let (k, v) = l.split_once(':')?;
        k.trim()
            .eq_ignore_ascii_case(name)
            .then(|| v.trim().to_ascii_lowercase())
    })
}

fn parse_addr(addr: &str) -> (String, u16) {
    let rest = addr.strip_prefix("http://").unwrap_or(addr);
    let rest = rest.trim_end_matches('/');
    match rest.split_once(':') {
        Some((h, p)) => (h.to_string(), p.parse().unwrap_or(80)),
        None => (rest.to_string(), 80),
    }
}

/// Split off the first whitespace-delimited word; the remainder keeps its
/// internal spacing (JSON bodies, SQL, docs).
fn split_first(line: &str) -> (&str, &str) {
    let line = line.trim();
    match line.split_once(char::is_whitespace) {
        Some((first, rest)) => (first, rest.trim()),
        None => (line, ""),
    }
}

// ---- data command implementations ------------------------------------------

/// Shell-ish tokenizer: whitespace-separated, single or double quotes group a
/// token and mark it as quoted (so `'42'` stays a string in the value parser).
#[cfg(feature = "orm")]
fn tokens(s: &str) -> Vec<(String, bool)> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    let mut quote: Option<char> = None;
    for c in s.chars() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => cur.push(c),
            None if c == '\'' || c == '"' => {
                quote = Some(c);
                quoted = true;
            }
            None if c.is_whitespace() => {
                if !cur.is_empty() || quoted {
                    out.push((std::mem::take(&mut cur), quoted));
                    quoted = false;
                }
            }
            None => cur.push(c),
        }
    }
    if !cur.is_empty() || quoted {
        out.push((cur, quoted));
    }
    out
}

#[cfg(feature = "orm")]
fn parse_value(tok: &str, was_quoted: bool) -> Value {
    if was_quoted {
        return Value::Text(tok.to_string());
    }
    match tok {
        "null" => Value::Null,
        "true" => Value::Bool(true),
        "false" => Value::Bool(false),
        _ => {
            if let Ok(i) = tok.parse::<i64>() {
                Value::Int(i)
            } else if let Ok(f) = tok.parse::<f64>() {
                Value::Real(f)
            } else {
                Value::Text(tok.to_string())
            }
        }
    }
}

/// Parse the `q` mini-DSL into a [`QueryBuilder`]. Identifier and operator
/// validation is the builder's own job (`build()` rejects bad ones).
#[cfg(feature = "orm")]
fn parse_query(rest: &str) -> Result<QueryBuilder, String> {
    let toks = tokens(rest);
    let table = toks
        .first()
        .map(|(t, _)| t.clone())
        .ok_or("usage: q <table> [select a,b] [where <col> <op> <val> [and ...]] [order <col> [desc]] [limit n] [offset n]")?;
    let mut qb = QueryBuilder::table(&table);
    let mut i = 1;
    while i < toks.len() {
        match toks[i].0.to_ascii_lowercase().as_str() {
            "select" => {
                let cols = &toks
                    .get(i + 1)
                    .ok_or("select needs a column list (a,b,c)")?
                    .0;
                let cols: Vec<&str> = cols.split(',').map(str::trim).collect();
                qb = qb.select(&cols);
                i += 2;
            }
            "where" | "and" => {
                let [col, op, val] = [toks.get(i + 1), toks.get(i + 2), toks.get(i + 3)];
                let (Some(col), Some(op), Some(val)) = (col, op, val) else {
                    return Err("where/and needs `<col> <op> <value>`".to_string());
                };
                qb = qb.filter(&col.0, &op.0, parse_value(&val.0, val.1));
                i += 4;
            }
            "order" => {
                let col = toks.get(i + 1).ok_or("order needs a column")?;
                let dir = toks.get(i + 2).map(|(t, _)| t.to_ascii_lowercase());
                let desc = dir.as_deref() == Some("desc");
                qb = qb.order_by(&col.0, desc);
                i += if matches!(dir.as_deref(), Some("desc") | Some("asc")) {
                    3
                } else {
                    2
                };
            }
            "limit" => {
                let n = parse_num(toks.get(i + 1), "limit")?;
                qb = qb.limit(n);
                i += 2;
            }
            "offset" => {
                let n = parse_num(toks.get(i + 1), "offset")?;
                qb = qb.offset(n);
                i += 2;
            }
            other => return Err(format!("unexpected '{other}' in query")),
        }
    }
    Ok(qb)
}

#[cfg(feature = "orm")]
fn parse_num(tok: Option<&(String, bool)>, what: &str) -> Result<i64, String> {
    tok.and_then(|(t, _)| t.parse().ok())
        .ok_or_else(|| format!("{what} needs a number"))
}

#[cfg(feature = "orm")]
fn rows_pretty(rows: Vec<Json>) -> String {
    let n = rows.len();
    let plural = if n == 1 { "row" } else { "rows" };
    format!("{}\n({n} {plural})", Json::arr(rows).to_pretty())
}

#[cfg(feature = "orm")]
fn run_sql(db: &dyn Backend, sql: &str) -> Result<String, String> {
    if sql.is_empty() {
        return Err("usage: sql <statement>".to_string());
    }
    let first = sql
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    if matches!(
        first.as_str(),
        "select" | "with" | "pragma" | "explain" | "show" | "values"
    ) {
        db.query(sql, &[]).map(rows_pretty)
    } else {
        db.execute(sql, &[]).map(|n| {
            let plural = if n == 1 { "row" } else { "rows" };
            format!("{n} {plural} affected")
        })
    }
}

#[cfg(feature = "orm")]
fn run_table(db: &dyn Backend, rest: &str) -> Result<String, String> {
    let toks = tokens(rest);
    let table = toks
        .first()
        .map(|(t, _)| t.clone())
        .ok_or("usage: table <name> [n]")?;
    let limit = toks.get(1).and_then(|(t, _)| t.parse().ok()).unwrap_or(10);
    let qb = QueryBuilder::table(&table).limit(limit);
    db.select(&qb).map(rows_pretty)
}

#[cfg(feature = "orm")]
fn run_count(db: &dyn Backend, rest: &str) -> Result<String, String> {
    if rest.is_empty() {
        return Err("usage: count <table> [where ...]".to_string());
    }
    let qb = parse_query(rest)?;
    db.count(&qb).map(|n| n.to_string())
}

#[cfg(feature = "orm")]
fn run_kv(db: &dyn Backend, rest: &str) -> Result<String, String> {
    // Same portable SQL and table (`kv`) as `sutegi_orm::kv::Kv`, so
    // the REPL sees exactly what the app's KV layer wrote.
    const USAGE: &str = "usage: kv get|set|del|keys|scan <ns> [key] [json]";
    let (sub, rest) = split_first(rest);
    let toks = tokens(rest);
    let ns = toks
        .first()
        .map(|(t, _)| Value::Text(t.clone()))
        .ok_or(USAGE)?;
    let key = toks.get(1).map(|(t, _)| Value::Text(t.clone()));
    match (sub, key) {
        ("get", Some(key)) => {
            let row = db.query_one("SELECT value FROM kv WHERE ns = ? AND key = ?", &[ns, key])?;
            match row.and_then(|r| r.get("value").and_then(Json::as_str).map(str::to_string)) {
                Some(v) => Ok(Json::parse(&v).map(|j| j.to_pretty()).unwrap_or(v)),
                None => Ok("(not set)".to_string()),
            }
        }
        ("set", Some(key)) => {
            // `rest` is "<ns> <key> <json...>"; take the raw tail so JSON
            // objects with spaces survive intact.
            let (_, after_ns) = split_first(rest);
            let (_, raw) = split_first(after_ns);
            if raw.is_empty() {
                return Err("kv set needs a JSON value".to_string());
            }
            let value = Json::parse(raw).map_err(|e| format!("value is not JSON: {e}"))?;
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as i64)
                .unwrap_or(0);
            db.execute(
                "INSERT INTO kv (ns, key, value, updated_at) VALUES (?, ?, ?, ?) \
                 ON CONFLICT (ns, key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
                &[ns, key, Value::Text(value.to_string()), Value::Int(now)],
            )?;
            Ok("ok".to_string())
        }
        ("del", Some(key)) => db
            .execute("DELETE FROM kv WHERE ns = ? AND key = ?", &[ns, key])
            .map(|n| {
                if n > 0 {
                    "deleted".to_string()
                } else {
                    "(not set)".to_string()
                }
            }),
        ("keys", None) => {
            let rows = db.query("SELECT key FROM kv WHERE ns = ? ORDER BY key", &[ns])?;
            Ok(rows
                .iter()
                .filter_map(|r| r.get("key").and_then(Json::as_str))
                .collect::<Vec<_>>()
                .join("\n"))
        }
        ("scan", None) => db
            .query("SELECT key, value FROM kv WHERE ns = ? ORDER BY key", &[ns])
            .map(rows_pretty),
        _ => Err(USAGE.to_string()),
    }
}

#[cfg(feature = "orm")]
fn run_events(db: &dyn Backend, rest: &str) -> Result<String, String> {
    let toks = tokens(rest);
    match toks.first() {
        None => db
            .query(
                "SELECT position, stream, version, name, payload FROM sutegi_events \
                 ORDER BY position DESC LIMIT 20",
                &[],
            )
            .map(rows_pretty),
        Some((stream, _)) => {
            let limit = toks.get(1).and_then(|(t, _)| t.parse().ok()).unwrap_or(20);
            db.query(
                "SELECT position, stream, version, name, payload FROM sutegi_events \
                 WHERE stream = ? ORDER BY version DESC LIMIT ?",
                &[Value::Text(stream.clone()), Value::Int(limit)],
            )
            .map(rows_pretty)
        }
    }
}

#[cfg(feature = "orm")]
fn run_jobs(db: &dyn Backend) -> Result<String, String> {
    let counts = db.query_one(
        "SELECT \
            SUM(CASE WHEN failed_at IS NULL AND locked_at IS NULL THEN 1 ELSE 0 END) AS pending, \
            SUM(CASE WHEN failed_at IS NULL AND locked_at IS NOT NULL THEN 1 ELSE 0 END) AS running, \
            SUM(CASE WHEN failed_at IS NOT NULL THEN 1 ELSE 0 END) AS dead, \
            COUNT(*) AS total \
         FROM sutegi_jobs",
        &[],
    )?;
    let recent = db.query(
        "SELECT id, name, attempts, max_attempts, last_error FROM sutegi_jobs \
         ORDER BY id DESC LIMIT 10",
        &[],
    )?;
    let counts = counts.unwrap_or(Json::obj(vec![]));
    Ok(format!(
        "{}\nrecent:\n{}",
        counts.to_pretty(),
        rows_pretty(recent)
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_first_separates_word_and_tail() {
        assert_eq!(
            split_first("call add {\"a\": 1}"),
            ("call", "add {\"a\": 1}")
        );
        assert_eq!(split_first("routes"), ("routes", ""));
        assert_eq!(split_first("  sql select 1  "), ("sql", "select 1"));
    }

    #[test]
    fn parse_addr_accepts_bare_and_prefixed() {
        assert_eq!(
            parse_addr("127.0.0.1:8080"),
            ("127.0.0.1".to_string(), 8080)
        );
        assert_eq!(
            parse_addr("http://localhost:9000/"),
            ("localhost".to_string(), 9000)
        );
        assert_eq!(parse_addr("example.test"), ("example.test".to_string(), 80));
    }

    #[test]
    fn status_and_header_parse_from_head_text() {
        let head = "HTTP/1.1 422 Unprocessable Entity\r\nContent-Type: application/json\r\nX: y";
        assert_eq!(parse_status(head), 422);
        assert_eq!(
            header_line(head, "content-type").as_deref(),
            Some("application/json")
        );
        assert_eq!(header_line(head, "missing"), None);
    }

    #[cfg(feature = "orm")]
    mod orm {
        use super::super::*;

        #[test]
        fn tokenizer_groups_quotes_and_marks_them() {
            let toks = tokens("where title = 'hello world' and n = 42");
            assert_eq!(toks[3], ("hello world".to_string(), true));
            assert_eq!(toks[7], ("42".to_string(), false));
        }

        #[test]
        fn values_parse_by_shape_unless_quoted() {
            assert_eq!(parse_value("42", false), Value::Int(42));
            assert_eq!(parse_value("42", true), Value::Text("42".to_string()));
            assert_eq!(parse_value("1.5", false), Value::Real(1.5));
            assert_eq!(parse_value("true", false), Value::Bool(true));
            assert_eq!(parse_value("null", false), Value::Null);
            assert_eq!(parse_value("open", false), Value::Text("open".to_string()));
        }

        #[test]
        fn query_dsl_builds_parameterized_sql() {
            let qb = parse_query("todos select id,title where done = false and id > 3 order id desc limit 5 offset 2")
                .unwrap();
            let (sql, params) = qb.build().unwrap();
            assert_eq!(
                sql,
                "SELECT id, title FROM todos WHERE done = ? AND id > ? ORDER BY id DESC LIMIT 5 OFFSET 2"
            );
            assert_eq!(params, vec![Value::Bool(false), Value::Int(3)]);
        }

        #[test]
        fn query_dsl_rejects_junk() {
            assert!(parse_query("").is_err());
            assert!(parse_query("todos frobnicate").is_err());
            assert!(parse_query("todos where done =").is_err());
        }
    }
}
