use std::borrow::Cow;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::Instant;

use sutegi_http::{Request, Response};
use sutegi_json::Json;
use sutegi_log::{Level, Value};

type QuietFn = dyn Fn(&str) -> bool + Send + Sync + 'static;
type ClientIpFn = dyn Fn(&Request) -> Option<String> + Send + Sync + 'static;

pub const REQUEST_ID_HEADER: &str = "x-request-id";
const MAX_REQUEST_ID: usize = 64;
const MAX_USER_AGENT_CHARS: usize = 160;

pub struct AccessLog {
    quiet: Box<QuietFn>,
    client_ip: Box<ClientIpFn>,
    trust_request_id: bool,
}

impl std::fmt::Debug for AccessLog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AccessLog")
            .field("trust_request_id", &self.trust_request_id)
            .finish_non_exhaustive()
    }
}

impl Default for AccessLog {
    fn default() -> AccessLog {
        AccessLog {
            quiet: Box::new(|path| path == "/__health" || path == "/__ready"),
            client_ip: Box::new(Request::peer_ip),
            trust_request_id: true,
        }
    }
}

impl AccessLog {
    pub fn new() -> AccessLog {
        AccessLog::default()
    }

    #[must_use]
    pub fn quiet(mut self, quiet: impl Fn(&str) -> bool + Send + Sync + 'static) -> AccessLog {
        self.quiet = Box::new(quiet);
        self
    }

    #[must_use]
    pub fn client_ip(
        mut self,
        client_ip: impl Fn(&Request) -> Option<String> + Send + Sync + 'static,
    ) -> AccessLog {
        self.client_ip = Box::new(client_ip);
        self
    }

    #[must_use]
    pub fn trust_request_id(mut self, trust: bool) -> AccessLog {
        self.trust_request_id = trust;
        self
    }

    pub(crate) fn begin(&self, req: &Request) -> InFlight {
        let rid = self
            .trust_request_id
            .then(|| inbound_request_id(req))
            .flatten()
            .unwrap_or_else(new_request_id);
        let scope = sutegi_log::scope(&[("rid", Value::Str(Cow::Borrowed(&rid)))]);
        InFlight {
            rid,
            started: Instant::now(),
            scope,
        }
    }

    pub(crate) fn finish(&self, flight: InFlight, req: &Request, resp: Response) -> Response {
        let status = resp.status;
        let path = req.path.as_str();
        let level = if status >= 500 {
            Level::Error
        } else if status < 400 && (self.quiet)(path) {
            Level::Debug
        } else {
            Level::Info
        };
        if sutegi_log::enabled(level, "http") {
            let ms = flight.started.elapsed().as_millis();
            let ip = (self.client_ip)(req);
            let ua: String = req
                .header("user-agent")
                .unwrap_or("")
                .chars()
                .take(MAX_USER_AGENT_CHARS)
                .collect();
            sutegi_log::log!(
                level,
                "http",
                "request",
                method = req.method.as_str(),
                path = path,
                status = status,
                ms = ms,
                ip = ip,
                ua = ua,
            );
        }
        let resp = resp.with_header(REQUEST_ID_HEADER, &flight.rid);
        drop(flight.scope);
        resp
    }
}

pub(crate) struct InFlight {
    rid: String,
    started: Instant,
    scope: sutegi_log::Scope,
}

fn inbound_request_id(req: &Request) -> Option<String> {
    let id = req.header(REQUEST_ID_HEADER)?.trim();
    let safe = !id.is_empty()
        && id.len() <= MAX_REQUEST_ID
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.');
    safe.then(|| id.to_string())
}

fn new_request_id() -> String {
    static PREFIX: OnceLock<String> = OnceLock::new();
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let prefix = PREFIX.get_or_init(|| match sutegi_crypto::random_bytes(4) {
        Ok(bytes) => sutegi_crypto::hex(&bytes),
        Err(_) => format!("{:08x}", std::process::id()),
    });
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    format!("{prefix}{seq:08x}")
}

pub(crate) fn recent_logs(req: &Request) -> Json {
    let query = crate::query_params(req);
    let min_level = query
        .get("level")
        .and_then(|l| l.parse::<Level>().ok())
        .unwrap_or(Level::Debug);
    let limit = query
        .get("limit")
        .and_then(|l| l.parse::<usize>().ok())
        .unwrap_or(200)
        .min(1000);
    let filters: Vec<(&str, &str)> = query
        .iter()
        .filter(|(k, _)| !matches!(k.as_str(), "level" | "limit"))
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    let records = sutegi_log::recent();
    let matching: Vec<Json> = records
        .iter()
        .rev()
        .filter(|r| r.level >= min_level)
        .filter(|r| filters.iter().all(|(k, v)| matches_filter(r, k, v)))
        .take(limit)
        .filter_map(|r| Json::parse(&r.to_json_line()).ok())
        .collect();
    Json::arr(matching.into_iter().rev().collect())
}

fn matches_filter(record: &sutegi_log::Record, key: &str, want: &str) -> bool {
    match key {
        "target" => record.target == want,
        "msg" => record.msg == want,
        _ => record.field(key).is_some_and(|v| v.to_string() == want),
    }
}

pub(crate) fn log_metrics() -> String {
    let mut out = String::from(
        "# HELP sutegi_log_events_total Log events written, by level.\n\
         # TYPE sutegi_log_events_total counter\n",
    );
    for (level, n) in sutegi_log::counts() {
        out.push_str(&format!(
            "sutegi_log_events_total{{level=\"{}\"}} {n}\n",
            level.as_str()
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inbound_ids_are_reused_only_when_safe() {
        let with = |v: &str| {
            let r = Request {
                method: sutegi_http::Method::Get,
                path: "/".into(),
                query: String::new(),
                version: "HTTP/1.1".into(),
                headers: vec![(REQUEST_ID_HEADER.into(), v.into())],
                body: vec![],
                peer: None,
            };
            inbound_request_id(&r)
        };
        assert_eq!(with("abc-123_x.y").as_deref(), Some("abc-123_x.y"));
        assert_eq!(with("bad id"), None);
        assert_eq!(with("evil\"}"), None);
        assert_eq!(with(&"a".repeat(65)), None);
    }

    #[test]
    fn generated_ids_are_unique() {
        let a = new_request_id();
        let b = new_request_id();
        assert_ne!(a, b);
        assert_eq!(a.len(), 16);
    }
}
