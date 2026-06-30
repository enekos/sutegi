//! A small, std-only configuration layer.
//!
//! Reads configuration from the environment (and optionally a `.env` file) into
//! an immutable [`Config`] snapshot with typed accessors, required-variable
//! validation, and prefix scoping. No dependencies, no macros.
//!
//! ```no_run
//! use sutegi::config::Config;
//!
//! let cfg = Config::load();                 // .env (if present) + process env (env wins)
//! let host = cfg.string("HOST", "0.0.0.0");
//! let port = cfg.int("PORT", 8080);
//! let debug = cfg.bool("DEBUG", false);
//! let key = cfg.require("API_KEY").expect("API_KEY must be set");
//!
//! // Scope to a subsystem: DB_HOST/DB_PORT → HOST/PORT
//! let db = cfg.prefixed("DB_");
//! ```

use std::collections::BTreeMap;
use std::fmt;

/// Error returned when required configuration is missing.
#[derive(Debug, Clone, PartialEq)]
pub struct ConfigError {
    pub missing: Vec<String>,
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "missing required configuration: {}",
            self.missing.join(", ")
        )
    }
}

impl std::error::Error for ConfigError {}

/// An immutable snapshot of configuration values.
#[derive(Debug, Clone, Default)]
pub struct Config {
    vars: BTreeMap<String, String>,
}

impl Config {
    /// Snapshot the current process environment.
    pub fn from_env() -> Config {
        Config {
            vars: std::env::vars().collect(),
        }
    }

    /// Load `.env` from the current directory (if present), then overlay the
    /// process environment — **real env vars win** over `.env` (12-factor).
    pub fn load() -> Config {
        Config::load_from(".env")
    }

    /// Like [`load`](Config::load) but from a specific `.env` path. A missing
    /// file is not an error (you just get the process environment).
    pub fn load_from(path: &str) -> Config {
        let mut vars: BTreeMap<String, String> = match std::fs::read_to_string(path) {
            Ok(contents) => parse_dotenv(&contents).into_iter().collect(),
            Err(_) => BTreeMap::new(),
        };
        // Process env overrides .env.
        for (k, v) in std::env::vars() {
            vars.insert(k, v);
        }
        Config { vars }
    }

    /// Build from an explicit map (handy for tests).
    pub fn from_map(vars: BTreeMap<String, String>) -> Config {
        Config { vars }
    }

    /// Raw lookup.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.vars.get(key).map(String::as_str)
    }

    /// String value, or `default` if unset.
    pub fn string(&self, key: &str, default: &str) -> String {
        self.get(key).unwrap_or(default).to_string()
    }

    /// Integer value, or `default` if unset/unparseable.
    pub fn int(&self, key: &str, default: i64) -> i64 {
        self.get(key)
            .and_then(|v| v.trim().parse().ok())
            .unwrap_or(default)
    }

    /// Float value, or `default` if unset/unparseable.
    pub fn float(&self, key: &str, default: f64) -> f64 {
        self.get(key)
            .and_then(|v| v.trim().parse().ok())
            .unwrap_or(default)
    }

    /// Boolean value (`1/true/yes/on` ⇒ true, `0/false/no/off` ⇒ false,
    /// case-insensitive), or `default` for anything else/unset.
    pub fn bool(&self, key: &str, default: bool) -> bool {
        match self.get(key).map(|v| v.trim().to_ascii_lowercase()) {
            Some(v) if matches!(v.as_str(), "1" | "true" | "yes" | "on") => true,
            Some(v) if matches!(v.as_str(), "0" | "false" | "no" | "off") => false,
            _ => default,
        }
    }

    /// Comma-separated list (trimmed, empties dropped). Empty if unset.
    pub fn list(&self, key: &str) -> Vec<String> {
        self.get(key)
            .map(|v| {
                v.split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Require a value; `Err` (listing the key) if it's unset.
    pub fn require(&self, key: &str) -> Result<String, ConfigError> {
        self.get(key).map(String::from).ok_or_else(|| ConfigError {
            missing: vec![key.to_string()],
        })
    }

    /// Require several values at once; the error lists **all** missing keys, so
    /// startup fails with one actionable message instead of one-at-a-time.
    pub fn require_all(&self, keys: &[&str]) -> Result<(), ConfigError> {
        let missing: Vec<String> = keys
            .iter()
            .filter(|k| self.get(k).is_none())
            .map(|k| k.to_string())
            .collect();
        if missing.is_empty() {
            Ok(())
        } else {
            Err(ConfigError { missing })
        }
    }

    /// A sub-view of keys starting with `prefix`, with the prefix stripped:
    /// `prefixed("DB_")` turns `DB_HOST`/`DB_PORT` into `HOST`/`PORT`.
    pub fn prefixed(&self, prefix: &str) -> Config {
        let vars = self
            .vars
            .iter()
            .filter_map(|(k, v)| {
                k.strip_prefix(prefix)
                    .map(|stripped| (stripped.to_string(), v.clone()))
            })
            .collect();
        Config { vars }
    }
}

/// Parse `.env`-style content into key/value pairs. Supports blank lines, `#`
/// comments, optional `export ` prefixes, and single/double-quoted values.
pub fn parse_dotenv(contents: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for raw in contents.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line);
        let (key, value) = match line.split_once('=') {
            Some(kv) => kv,
            None => continue,
        };
        let key = key.trim();
        if key.is_empty() {
            continue;
        }
        let mut value = value.trim();
        // Strip a matching pair of surrounding quotes.
        if value.len() >= 2
            && ((value.starts_with('"') && value.ends_with('"'))
                || (value.starts_with('\'') && value.ends_with('\'')))
        {
            value = &value[1..value.len() - 1];
        }
        out.push((key.to_string(), value.to_string()));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> Config {
        let mut m = BTreeMap::new();
        m.insert("PORT".into(), "9090".into());
        m.insert("DEBUG".into(), "TRUE".into());
        m.insert("HOSTS".into(), "a, b ,c".into());
        m.insert("DB_HOST".into(), "db.local".into());
        m.insert("DB_PORT".into(), "5432".into());
        Config::from_map(m)
    }

    #[test]
    fn typed_accessors() {
        let c = cfg();
        assert_eq!(c.int("PORT", 8080), 9090);
        assert_eq!(c.int("MISSING", 8080), 8080);
        assert!(c.bool("DEBUG", false));
        assert!(!c.bool("MISSING", false));
        assert_eq!(c.string("HOST", "0.0.0.0"), "0.0.0.0");
        assert_eq!(c.list("HOSTS"), vec!["a", "b", "c"]);
    }

    #[test]
    fn require_collects_all_missing() {
        let c = cfg();
        assert!(c.require("PORT").is_ok());
        let err = c.require_all(&["PORT", "API_KEY", "SECRET"]).unwrap_err();
        assert_eq!(err.missing, vec!["API_KEY", "SECRET"]);
    }

    #[test]
    fn prefix_scoping() {
        let db = cfg().prefixed("DB_");
        assert_eq!(db.string("HOST", ""), "db.local");
        assert_eq!(db.int("PORT", 0), 5432);
    }

    #[test]
    fn dotenv_parsing() {
        let content =
            "# comment\n\nexport NAME=\"sutegi\"\nPORT=8080\nQUOTED='a b'\nbad line\nEMPTY=\n";
        let parsed: BTreeMap<_, _> = parse_dotenv(content).into_iter().collect();
        assert_eq!(parsed.get("NAME").map(String::as_str), Some("sutegi"));
        assert_eq!(parsed.get("PORT").map(String::as_str), Some("8080"));
        assert_eq!(parsed.get("QUOTED").map(String::as_str), Some("a b"));
        assert_eq!(parsed.get("EMPTY").map(String::as_str), Some(""));
        assert!(!parsed.contains_key("bad line"));
    }
}
