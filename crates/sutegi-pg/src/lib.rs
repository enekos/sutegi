//! A small, **pure-`std`** PostgreSQL client speaking wire protocol v3 over a
//! blocking TCP socket — no async runtime, no C library, no third-party crates.
//! It fits sutegi's thread-per-connection model: one [`Client`] owns one
//! socket; share work across threads with a [`Pool`].
//!
//! What it supports:
//! - Auth: **SCRAM-SHA-256** (the modern default), **MD5**, and cleartext —
//!   the last refused unless opted into via [`Config::require_auth`].
//! - The **extended query protocol** with bound parameters (text format),
//!   so every query is parameterized and injection-safe.
//! - Rows decoded straight into [`sutegi_json::Json`], typed by column OID —
//!   the same machine-readable shape the rest of sutegi speaks.
//! - **Per-connection prepared-statement caching** (on by default): a SQL
//!   string is `Parse`d into a named statement once, then reused with only
//!   `Bind`/`Execute` on repeat. Toggle with [`Config::statement_cache`].
//!
//! What it supports (continued):
//! - **`LISTEN`/`NOTIFY`** via a dedicated [`Listener`] connection — the
//!   primitive behind sutegi's cross-pod pubsub.
//!
//! What it does not (yet): TLS (terminate at the LB / mesh, or run inside the
//! cluster network — `sslmode=require` and stricter are refused rather than
//! silently ignored), binary result format, and `COPY`.

/// The shared hand-rolled, known-answer-tested primitives (SHA-256, HMAC,
/// PBKDF2, MD5, hex, Base64) — extracted to `sutegi-crypto` once storage,
/// sessions, and auth started reusing them; re-exported here so
/// `sutegi_pg::crypto::…` paths keep working.
pub use sutegi_crypto as crypto;
mod listen;
mod pool;
mod protocol;

pub use listen::{Listener, ListenerShutdown, Notification};
pub use pool::Pool;

use std::fmt;
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
#[derive(Clone)]
pub struct Config {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub password: Option<String>,
    pub dbname: String,
    /// Applied as the socket connect/read/write timeout when set, and as the
    /// longest a [`Pool`] checkout waits for a free connection.
    pub timeout: Option<Duration>,
    /// Reuse server-side prepared statements per connection (the default).
    /// Set `false` to force every query through an unnamed statement.
    pub statement_cache: bool,
    /// Which authentication methods the server may ask for. Defaults to
    /// [`AuthMethods::default`]: SCRAM-SHA-256, MD5 and trust, but never a
    /// cleartext password. Set from a URL with libpq's `require_auth` param.
    pub require_auth: AuthMethods,
}

/// The authentication exchanges a client will take part in — libpq's
/// `require_auth`. With no TLS on the wire the *server* (or a MITM) picks the
/// method, so whatever is allowed here is what an attacker can steer to.
///
/// The default refuses a cleartext password (it would hand the password to
/// whoever answers) and keeps MD5 for legacy servers; MD5 leaks only a
/// salted hash, but it is a replayable credential — drop it with
/// `require_auth=scram-sha-256` once every server speaks SCRAM.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AuthMethods {
    /// No challenge at all (`trust`): the server answers `AuthenticationOk`
    /// straight away. libpq name: `none`.
    pub none: bool,
    /// Cleartext password. libpq name: `password`.
    pub password: bool,
    /// MD5-hashed password. libpq name: `md5`.
    pub md5: bool,
    /// SCRAM-SHA-256, which also authenticates the server. libpq name:
    /// `scram-sha-256`.
    pub scram_sha_256: bool,
}

impl Default for AuthMethods {
    fn default() -> Self {
        AuthMethods {
            none: true,
            password: false,
            md5: true,
            scram_sha_256: true,
        }
    }
}

impl std::str::FromStr for AuthMethods {
    type Err = String;

    /// libpq syntax: a comma-separated list of the methods allowed
    /// (`scram-sha-256,md5`), or of methods forbidden, each prefixed `!`
    /// (`!password,!md5`); the two forms cannot be mixed. A negated list
    /// starts from *everything* allowed, as libpq does — including cleartext.
    fn from_str(s: &str) -> Result<Self, String> {
        let entries: Vec<&str> = s.split(',').map(str::trim).collect();
        let negated = entries.first().is_some_and(|e| e.starts_with('!'));
        let mut methods = AuthMethods {
            none: negated,
            password: negated,
            md5: negated,
            scram_sha_256: negated,
        };
        for entry in entries {
            let name = match (negated, entry.strip_prefix('!')) {
                (true, Some(name)) => name,
                (false, None) => entry,
                _ => {
                    return Err(format!(
                        "require_auth cannot mix negated and plain methods: {s}"
                    ))
                }
            };
            let slot = match name {
                "none" => &mut methods.none,
                "password" => &mut methods.password,
                "md5" => &mut methods.md5,
                "scram-sha-256" => &mut methods.scram_sha_256,
                other => return Err(format!("unsupported require_auth method: {other:?}")),
            };
            *slot = !negated;
        }
        Ok(methods)
    }
}

impl fmt::Debug for Config {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Config")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("user", &self.user)
            .field("password", &self.password.as_ref().map(|_| "<redacted>"))
            .field("dbname", &self.dbname)
            .field("timeout", &self.timeout)
            .field("statement_cache", &self.statement_cache)
            .field("require_auth", &self.require_auth)
            .finish()
    }
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
            require_auth: AuthMethods::default(),
        }
    }
}

impl Config {
    /// Parse a `postgres://user:password@host:port/dbname?params` URL.
    /// `postgresql://` is accepted too; password, port and params are
    /// optional. Understood params: `require_auth` (see [`AuthMethods`]) and
    /// `sslmode` (`disable`/`allow`/`prefer` connect in plaintext;
    /// `require`/`verify-ca`/`verify-full` are an error, since there is no TLS).
    pub fn from_url(url: &str) -> Result<Config, String> {
        let rest = url
            .strip_prefix("postgres://")
            .or_else(|| url.strip_prefix("postgresql://"))
            .ok_or_else(|| "url must start with postgres:// or postgresql://".to_string())?;

        let (rest, query) = match rest.split_once('?') {
            Some((r, q)) => (r, Some(q)),
            None => (rest, None),
        };

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

        let mut cfg = Config {
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
            require_auth: AuthMethods::default(),
        };
        if let Some(query) = query {
            cfg.apply_params(query)?;
        }
        Ok(cfg)
    }

    /// Act on the connection parameters this client understands; others
    /// (`application_name`, `connect_timeout`, …) are accepted and ignored.
    fn apply_params(&mut self, query: &str) -> Result<(), String> {
        for pair in query.split('&').filter(|p| !p.is_empty()) {
            let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
            match key {
                "require_auth" => self.require_auth = percent_decode(value).parse()?,
                "sslmode" => check_sslmode(&percent_decode(value))?,
                _ => {}
            }
        }
        Ok(())
    }

    /// Build from the environment: `DATABASE_URL` if present, otherwise the
    /// standard `PGHOST`/`PGPORT`/`PGUSER`/`PGPASSWORD`/`PGDATABASE` vars
    /// (each with a sensible default), plus `PGREQUIREAUTH` and `PGSSLMODE`.
    pub fn from_env() -> Result<Config, String> {
        if let Ok(url) = std::env::var("DATABASE_URL") {
            return Config::from_url(&url);
        }
        let var = |k: &str| std::env::var(k).ok();
        if let Some(mode) = var("PGSSLMODE") {
            check_sslmode(&mode)?;
        }
        let require_auth = match var("PGREQUIREAUTH") {
            Some(v) => v.parse()?,
            None => AuthMethods::default(),
        };
        Ok(Config {
            host: var("PGHOST").unwrap_or_else(|| "localhost".into()),
            port: var("PGPORT").and_then(|p| p.parse().ok()).unwrap_or(5432),
            user: var("PGUSER").unwrap_or_else(|| "postgres".into()),
            password: var("PGPASSWORD"),
            dbname: var("PGDATABASE").unwrap_or_else(|| "postgres".into()),
            timeout: Some(Duration::from_secs(30)),
            statement_cache: true,
            require_auth,
        })
    }
}

/// Validate a libpq `sslmode` against what this client can do: speak
/// plaintext. `disable`, `allow` and `prefer` all mean plaintext here (the
/// latter two permit it in libpq too). `require`, `verify-ca` and
/// `verify-full` are an error rather than a silent downgrade — a caller that
/// asked for TLS must not get a cleartext connection it believes is
/// encrypted. Terminate TLS at a sidecar/mesh and connect to that instead.
fn check_sslmode(mode: &str) -> Result<(), String> {
    match mode {
        "disable" | "allow" | "prefer" => Ok(()),
        "require" | "verify-ca" | "verify-full" => Err(format!(
            "sslmode={mode} needs TLS, which sutegi-pg does not support yet; \
             use sslmode=disable behind a TLS-terminating proxy"
        )),
        other => Err(format!("unknown sslmode: {other:?}")),
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

#[cfg(test)]
mod config_tests {
    use super::Config;

    #[test]
    fn debug_does_not_leak_the_password() {
        let cfg = Config::from_url("postgres://app:top-secret@db:5432/prod").unwrap();
        let printed = format!("{cfg:?}");
        assert!(!printed.contains("top-secret"), "{printed}");
        assert!(printed.contains("<redacted>"), "{printed}");
        assert!(printed.contains("\"app\""), "{printed}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cleartext_is_refused_by_default_and_md5_kept() {
        let cfg = Config::from_url("postgres://u:p@h/db").unwrap();
        assert!(!cfg.require_auth.password);
        assert!(cfg.require_auth.md5 && cfg.require_auth.scram_sha_256);
        assert_eq!(Config::default().require_auth, cfg.require_auth);
    }

    #[test]
    fn require_auth_url_param_follows_libpq() {
        let only_scram = Config::from_url("postgres://u:p@h/db?require_auth=scram-sha-256")
            .unwrap()
            .require_auth;
        assert_eq!(
            only_scram,
            AuthMethods {
                none: false,
                password: false,
                md5: false,
                scram_sha_256: true,
            }
        );
        let opt_in = Config::from_url(
            "postgres://u:p@h/db?application_name=x&require_auth=password%2Cscram-sha-256",
        )
        .unwrap()
        .require_auth;
        assert!(opt_in.password && opt_in.scram_sha_256 && !opt_in.md5 && !opt_in.none);

        let negated: AuthMethods = "!md5,!none".parse().unwrap();
        assert!(negated.password && negated.scram_sha_256 && !negated.md5 && !negated.none);

        assert!("md5,!password".parse::<AuthMethods>().is_err());
        assert!("gss".parse::<AuthMethods>().is_err());
        assert!(Config::from_url("postgres://u:p@h/db?require_auth=").is_err());
    }

    #[test]
    fn sslmode_demanding_tls_is_an_error_not_a_silent_downgrade() {
        for mode in ["require", "verify-ca", "verify-full"] {
            let err = Config::from_url(&format!("postgres://u:p@h/db?sslmode={mode}"))
                .expect_err("TLS was asked for and cannot be given");
            assert!(err.contains("TLS"), "{mode}: {err}");
        }
        for mode in ["disable", "allow", "prefer"] {
            assert!(Config::from_url(&format!("postgres://u:p@h/db?sslmode={mode}")).is_ok());
        }
        assert!(Config::from_url("postgres://u:p@h/db?sslmode=bogus").is_err());
    }
}
