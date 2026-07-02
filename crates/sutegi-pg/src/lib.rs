//! A small, **pure-`std`** PostgreSQL client speaking wire protocol v3 over a
//! blocking TCP socket — no async runtime, no C library, no third-party crates.
//! It fits sutegi's thread-per-connection model: one [`Client`] owns one
//! socket; share work across threads with a [`Pool`].
//!
//! What it supports:
//! - Auth: **SCRAM-SHA-256** (the modern default), **MD5**, and cleartext.
//! - The **extended query protocol** with bound parameters (text format),
//!   so every query is parameterized and injection-safe.
//! - Rows decoded straight into [`sutegi_json::Json`], typed by column OID —
//!   the same machine-readable shape the rest of sutegi speaks.
//! - **Per-connection prepared-statement caching** (on by default): a SQL
//!   string is `Parse`d into a named statement once, then reused with only
//!   `Bind`/`Execute` on repeat. Toggle with [`Config::statement_cache`].
//!
//! What it does not (yet): TLS (terminate at the LB / mesh, or run inside the
//! cluster network), binary result format, `COPY`, and `LISTEN/NOTIFY`.

/// The shared hand-rolled, known-answer-tested primitives (SHA-256, HMAC,
/// PBKDF2, MD5, hex, Base64) — extracted to `sutegi-crypto` once storage,
/// sessions, and auth started reusing them; re-exported here so
/// `sutegi_pg::crypto::…` paths keep working.
pub use sutegi_crypto as crypto;
mod pool;
mod protocol;

pub use pool::Pool;

use std::time::Duration;

/// A scalar bound to a query parameter (sent in PostgreSQL's text format).
#[derive(Clone, Debug, PartialEq)]
pub enum PgValue {
    Null,
    Int(i64),
    Real(f64),
    Text(String),
    Bool(bool),
    /// A JSON document, sent as text. The server coerces it to `json`/`jsonb`
    /// from the target column type (parameter types are inferred).
    Json(String),
    /// An embedding vector in pgvector's `[1,2,3]` text form (target column
    /// type `vector`, provided by the pgvector extension).
    Vector(String),
}

/// Connection settings. Build from a URL ([`Config::from_url`]), the
/// environment ([`Config::from_env`]), or field by field.
#[derive(Clone, Debug)]
pub struct Config {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub password: Option<String>,
    pub dbname: String,
    /// Applied as the socket connect/read/write timeout when set.
    pub timeout: Option<Duration>,
    /// Reuse server-side prepared statements per connection (the default).
    /// Set `false` to force every query through an unnamed statement.
    pub statement_cache: bool,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            host: "localhost".into(),
            port: 5432,
            user: "postgres".into(),
            password: None,
            dbname: "postgres".into(),
            timeout: Some(Duration::from_secs(30)),
            statement_cache: true,
        }
    }
}

impl Config {
    /// Parse a `postgres://user:password@host:port/dbname` URL. `postgresql://`
    /// is accepted too; password and port are optional.
    pub fn from_url(url: &str) -> Result<Config, String> {
        let rest = url
            .strip_prefix("postgres://")
            .or_else(|| url.strip_prefix("postgresql://"))
            .ok_or_else(|| "url must start with postgres:// or postgresql://".to_string())?;

        // Strip any query string (`?sslmode=…`); we don't act on params yet.
        let rest = rest.split('?').next().unwrap_or(rest);

        let (authority, dbname) = match rest.split_once('/') {
            Some((a, db)) => (a, db.to_string()),
            None => (rest, "postgres".to_string()),
        };

        let (userinfo, hostport) = match authority.rsplit_once('@') {
            Some((u, h)) => (Some(u), h),
            None => (None, authority),
        };

        let (user, password) = match userinfo {
            Some(ui) => match ui.split_once(':') {
                Some((u, p)) => (percent_decode(u), Some(percent_decode(p))),
                None => (percent_decode(ui), None),
            },
            None => ("postgres".to_string(), None),
        };

        let (host, port) = match hostport.rsplit_once(':') {
            Some((h, p)) => (
                h.to_string(),
                p.parse::<u16>().map_err(|_| format!("bad port: {p}"))?,
            ),
            None => (hostport.to_string(), 5432),
        };

        Ok(Config {
            host: if host.is_empty() {
                "localhost".into()
            } else {
                host
            },
            port,
            user,
            password,
            dbname: if dbname.is_empty() {
                "postgres".into()
            } else {
                percent_decode(&dbname)
            },
            timeout: Some(Duration::from_secs(30)),
            statement_cache: true,
        })
    }

    /// Build from the environment: `DATABASE_URL` if present, otherwise the
    /// standard `PGHOST`/`PGPORT`/`PGUSER`/`PGPASSWORD`/`PGDATABASE` vars
    /// (each with a sensible default).
    pub fn from_env() -> Result<Config, String> {
        if let Ok(url) = std::env::var("DATABASE_URL") {
            return Config::from_url(&url);
        }
        let var = |k: &str| std::env::var(k).ok();
        Ok(Config {
            host: var("PGHOST").unwrap_or_else(|| "localhost".into()),
            port: var("PGPORT").and_then(|p| p.parse().ok()).unwrap_or(5432),
            user: var("PGUSER").unwrap_or_else(|| "postgres".into()),
            password: var("PGPASSWORD"),
            dbname: var("PGDATABASE").unwrap_or_else(|| "postgres".into()),
            timeout: Some(Duration::from_secs(30)),
            statement_cache: true,
        })
    }
}

/// Minimal `%XX` percent-decoding for URL userinfo/dbname.
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hi = (bytes[i + 1] as char).to_digit(16);
            let lo = (bytes[i + 2] as char).to_digit(16);
            if let (Some(h), Some(l)) = (hi, lo) {
                out.push((h * 16 + l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

pub use protocol::Client;
