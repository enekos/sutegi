use std::borrow::Cow;
use std::cell::RefCell;
use std::collections::VecDeque;
use std::fmt::{self, Display, Write as _};
use std::io::{IsTerminal, Write as _};
use std::marker::PhantomData;
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, PoisonError, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};

mod macros;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Level {
    Debug,
    Info,
    Warn,
    Error,
}

impl Level {
    pub const ALL: [Level; 4] = [Level::Debug, Level::Info, Level::Warn, Level::Error];

    pub fn as_str(self) -> &'static str {
        match self {
            Level::Debug => "debug",
            Level::Info => "info",
            Level::Warn => "warn",
            Level::Error => "error",
        }
    }

    fn tag(self) -> &'static str {
        match self {
            Level::Debug => "DEBUG",
            Level::Info => "INFO ",
            Level::Warn => "WARN ",
            Level::Error => "ERROR",
        }
    }

    fn index(self) -> usize {
        self as usize
    }
}

impl Display for Level {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Level {
    type Err = String;

    fn from_str(s: &str) -> Result<Level, String> {
        match s.trim().to_ascii_lowercase().as_str() {
            "debug" | "trace" => Ok(Level::Debug),
            "info" => Ok(Level::Info),
            "warn" | "warning" => Ok(Level::Warn),
            "error" => Ok(Level::Error),
            other => Err(format!(
                "unknown log level '{other}' (debug, info, warn, error)"
            )),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Text,
    Json,
}

impl Format {
    pub fn auto() -> Format {
        if std::io::stderr().is_terminal() {
            Format::Text
        } else {
            Format::Json
        }
    }
}

impl FromStr for Format {
    type Err = String;

    fn from_str(s: &str) -> Result<Format, String> {
        match s.trim().to_ascii_lowercase().as_str() {
            "text" => Ok(Format::Text),
            "json" => Ok(Format::Json),
            "auto" | "" => Ok(Format::auto()),
            other => Err(format!("unknown log format '{other}' (text, json, auto)")),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Value<'a> {
    Null,
    Bool(bool),
    Int(i64),
    Uint(u64),
    Float(f64),
    Str(Cow<'a, str>),
}

impl<'a> Value<'a> {
    pub fn display(value: &dyn Display) -> Value<'static> {
        Value::Str(Cow::Owned(value.to_string()))
    }

    pub fn into_owned(self) -> Value<'static> {
        match self {
            Value::Null => Value::Null,
            Value::Bool(b) => Value::Bool(b),
            Value::Int(i) => Value::Int(i),
            Value::Uint(u) => Value::Uint(u),
            Value::Float(f) => Value::Float(f),
            Value::Str(s) => Value::Str(Cow::Owned(s.into_owned())),
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Str(s) => Some(s),
            _ => None,
        }
    }

    fn push_json(&self, out: &mut String) {
        match self {
            Value::Null => out.push_str("null"),
            Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            Value::Int(i) => push_display(out, i),
            Value::Uint(u) => push_display(out, u),
            Value::Float(f) if f.is_finite() => push_display(out, f),
            Value::Float(_) => out.push_str("null"),
            Value::Str(s) => push_json_str(out, clip(s)),
        }
    }

    fn push_text(&self, out: &mut String) {
        match self {
            Value::Str(s) => {
                let s = clip(s);
                let bare = !s.is_empty()
                    && !s
                        .chars()
                        .any(|c| c.is_whitespace() || c.is_control() || c == '"' || c == '=');
                if bare {
                    out.push_str(s);
                } else {
                    push_json_str(out, s);
                }
            }
            other => other.push_json(out),
        }
    }
}

impl Display for Value<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Null => f.write_str("null"),
            Value::Bool(b) => b.fmt(f),
            Value::Int(i) => i.fmt(f),
            Value::Uint(u) => u.fmt(f),
            Value::Float(x) => x.fmt(f),
            Value::Str(s) => f.write_str(s),
        }
    }
}

pub trait ToValue {
    fn to_value(&self) -> Value<'_>;
}

impl ToValue for str {
    fn to_value(&self) -> Value<'_> {
        Value::Str(Cow::Borrowed(self))
    }
}

impl ToValue for String {
    fn to_value(&self) -> Value<'_> {
        Value::Str(Cow::Borrowed(self))
    }
}

impl ToValue for Cow<'_, str> {
    fn to_value(&self) -> Value<'_> {
        Value::Str(Cow::Borrowed(self))
    }
}

impl ToValue for bool {
    fn to_value(&self) -> Value<'_> {
        Value::Bool(*self)
    }
}

impl ToValue for f32 {
    fn to_value(&self) -> Value<'_> {
        Value::Float(f64::from(*self))
    }
}

impl ToValue for f64 {
    fn to_value(&self) -> Value<'_> {
        Value::Float(*self)
    }
}

impl ToValue for Value<'_> {
    fn to_value(&self) -> Value<'_> {
        match self {
            Value::Str(s) => Value::Str(Cow::Borrowed(s)),
            other => other.clone(),
        }
    }
}

impl<T: ToValue> ToValue for Option<T> {
    fn to_value(&self) -> Value<'_> {
        match self {
            Some(v) => v.to_value(),
            None => Value::Null,
        }
    }
}

impl<T: ToValue + ?Sized> ToValue for &T {
    fn to_value(&self) -> Value<'_> {
        (**self).to_value()
    }
}

macro_rules! signed_to_value {
    ($($t:ty),*) => {$(
        impl ToValue for $t {
            fn to_value(&self) -> Value<'_> {
                Value::Int(i64::from(*self))
            }
        }
    )*};
}
signed_to_value!(i8, i16, i32, i64);

macro_rules! unsigned_to_value {
    ($($t:ty),*) => {$(
        impl ToValue for $t {
            fn to_value(&self) -> Value<'_> {
                Value::Uint(u64::from(*self))
            }
        }
    )*};
}
unsigned_to_value!(u8, u16, u32, u64);

impl ToValue for isize {
    fn to_value(&self) -> Value<'_> {
        i64::try_from(*self).map_or_else(|_| Value::display(self), Value::Int)
    }
}

impl ToValue for usize {
    fn to_value(&self) -> Value<'_> {
        u64::try_from(*self).map_or_else(|_| Value::display(self), Value::Uint)
    }
}

impl ToValue for i128 {
    fn to_value(&self) -> Value<'_> {
        i64::try_from(*self).map_or_else(|_| Value::display(self), Value::Int)
    }
}

impl ToValue for u128 {
    fn to_value(&self) -> Value<'_> {
        u64::try_from(*self).map_or_else(|_| Value::display(self), Value::Uint)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Config {
    level: Level,
    targets: Vec<(String, Level)>,
    format: Format,
}

impl Default for Config {
    fn default() -> Config {
        Config {
            level: Level::Info,
            targets: Vec::new(),
            format: Format::auto(),
        }
    }
}

impl Config {
    pub const FILTER_ENV: &'static str = "SUTEGI_LOG";
    pub const FORMAT_ENV: &'static str = "SUTEGI_LOG_FORMAT";

    pub fn new() -> Config {
        Config::default()
    }

    pub fn from_env() -> Config {
        let mut config = Config::default();
        if let Ok(spec) = std::env::var(Config::FILTER_ENV) {
            if let Ok(parsed) = config.clone().filter(&spec) {
                config = parsed;
            }
        }
        if let Ok(format) = std::env::var(Config::FORMAT_ENV) {
            if let Ok(format) = format.parse() {
                config.format = format;
            }
        }
        config
    }

    #[must_use]
    pub fn level(mut self, level: Level) -> Config {
        self.level = level;
        self
    }

    #[must_use]
    pub fn target(mut self, target: &str, level: Level) -> Config {
        self.targets.retain(|(t, _)| t != target);
        self.targets.push((target.to_string(), level));
        self
    }

    #[must_use]
    pub fn format(mut self, format: Format) -> Config {
        self.format = format;
        self
    }

    pub fn filter(mut self, spec: &str) -> Result<Config, String> {
        for part in spec.split(',').map(str::trim).filter(|p| !p.is_empty()) {
            match part.split_once('=') {
                Some((target, level)) => {
                    let target = target.trim();
                    if target.is_empty() {
                        return Err(format!("empty target in log filter '{part}'"));
                    }
                    self = self.target(target, level.parse()?);
                }
                None => self.level = part.parse()?,
            }
        }
        Ok(self)
    }

    pub fn min_level(&self, target: &str) -> Level {
        self.targets
            .iter()
            .filter(|(t, _)| {
                target == t
                    || target
                        .strip_prefix(t.as_str())
                        .is_some_and(|rest| rest.starts_with('.'))
            })
            .max_by_key(|(t, _)| t.len())
            .map_or(self.level, |(_, level)| *level)
    }

    pub fn current_format(&self) -> Format {
        self.format
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Record {
    pub ts_ms: u64,
    pub level: Level,
    pub target: String,
    pub msg: String,
    pub fields: Vec<(String, Value<'static>)>,
}

impl Record {
    pub fn field(&self, key: &str) -> Option<&Value<'static>> {
        self.fields.iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }

    pub fn to_json_line(&self) -> String {
        let mut out = String::with_capacity(160);
        out.push_str("{\"ts\":");
        push_json_str(&mut out, &timestamp(self.ts_ms));
        out.push_str(",\"level\":");
        push_json_str(&mut out, self.level.as_str());
        out.push_str(",\"target\":");
        push_json_str(&mut out, &self.target);
        out.push_str(",\"msg\":");
        push_json_str(&mut out, &self.msg);
        for (k, v) in &self.fields {
            out.push(',');
            if RESERVED.contains(&k.as_str()) {
                let mut renamed = String::with_capacity(k.len() + 1);
                renamed.push('_');
                renamed.push_str(k);
                push_json_str(&mut out, &renamed);
            } else {
                push_json_str(&mut out, k);
            }
            out.push(':');
            v.push_json(&mut out);
        }
        out.push('}');
        out
    }

    pub fn to_text_line(&self) -> String {
        let mut out = String::with_capacity(160);
        out.push_str(&timestamp(self.ts_ms));
        out.push(' ');
        out.push_str(self.level.tag());
        out.push(' ');
        out.push_str(&self.target);
        out.push_str(": ");
        out.push_str(&self.msg);
        for (k, v) in &self.fields {
            out.push(' ');
            out.push_str(k);
            out.push('=');
            v.push_text(&mut out);
        }
        out
    }
}

const RESERVED: [&str; 4] = ["ts", "level", "target", "msg"];
const MAX_VALUE_BYTES: usize = 4096;
const MAX_CONTEXT_FIELDS: usize = 32;
const RECENT_CAPACITY: usize = 512;
const REDACTED: &str = "[redacted]";
const SECRET_SUFFIXES: [&str; 8] = [
    "password",
    "passwd",
    "secret",
    "token",
    "authorization",
    "cookie",
    "api_key",
    "apikey",
];

static CONFIG: RwLock<Option<Config>> = RwLock::new(None);
static PANIC_HOOK: AtomicBool = AtomicBool::new(false);
static COUNTS: [AtomicU64; 4] = [
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
];
static RECENT: Mutex<VecDeque<Record>> = Mutex::new(VecDeque::new());

thread_local! {
    static CONTEXT: RefCell<Vec<(String, Value<'static>)>> = const { RefCell::new(Vec::new()) };
    static CAPTURE: RefCell<Option<Vec<Record>>> = const { RefCell::new(None) };
}

pub fn init(config: Config) {
    set_config(config);
    if !PANIC_HOOK.swap(true, Ordering::Relaxed) {
        std::panic::set_hook(Box::new(|info| {
            let at = info
                .location()
                .map(|l| format!("{}:{}", l.file(), l.line()))
                .unwrap_or_default();
            let payload = info
                .payload()
                .downcast_ref::<&str>()
                .map(|s| (*s).to_string())
                .or_else(|| info.payload().downcast_ref::<String>().cloned())
                .unwrap_or_default();
            let thread = std::thread::current();
            let name = thread.name().unwrap_or("?");
            emit(
                Level::Error,
                "panic",
                "thread panicked",
                &[
                    ("thread", Value::Str(Cow::Borrowed(name))),
                    ("at", Value::Str(Cow::Owned(at))),
                    ("error", Value::Str(Cow::Owned(payload))),
                ],
            );
        }));
    }
}

pub fn set_config(config: Config) {
    *CONFIG.write().unwrap_or_else(PoisonError::into_inner) = Some(config);
}

pub fn config() -> Config {
    if let Some(config) = CONFIG
        .read()
        .unwrap_or_else(PoisonError::into_inner)
        .as_ref()
    {
        return config.clone();
    }
    let mut slot = CONFIG.write().unwrap_or_else(PoisonError::into_inner);
    slot.get_or_insert_with(Config::from_env).clone()
}

fn with_config<R>(f: impl FnOnce(&Config) -> R) -> R {
    {
        let slot = CONFIG.read().unwrap_or_else(PoisonError::into_inner);
        if let Some(config) = slot.as_ref() {
            return f(config);
        }
    }
    let mut slot = CONFIG.write().unwrap_or_else(PoisonError::into_inner);
    f(slot.get_or_insert_with(Config::from_env))
}

pub fn enabled(level: Level, target: &str) -> bool {
    capturing() || with_config(|c| level >= c.min_level(target))
}

pub fn emit(level: Level, target: &str, msg: &str, fields: &[(&str, Value<'_>)]) {
    let capturing = capturing();
    let format = match with_config(|c| (level >= c.min_level(target), c.format)) {
        (true, format) => format,
        (false, _) if capturing => Format::Json,
        (false, _) => return,
    };
    let record = Record {
        ts_ms: now_ms(),
        level,
        target: target.to_string(),
        msg: msg.to_string(),
        fields: merged_fields(fields),
    };
    if capturing {
        CAPTURE.with(|c| {
            if let Some(records) = c.borrow_mut().as_mut() {
                records.push(record);
            }
        });
        return;
    }
    COUNTS[level.index()].fetch_add(1, Ordering::Relaxed);
    let mut line = match format {
        Format::Json => record.to_json_line(),
        Format::Text => record.to_text_line(),
    };
    line.push('\n');
    remember(record);
    std::io::stderr().lock().write_all(line.as_bytes()).ok();
}

fn merged_fields(fields: &[(&str, Value<'_>)]) -> Vec<(String, Value<'static>)> {
    let mut out: Vec<(String, Value<'static>)> = Vec::with_capacity(fields.len() + 4);
    for (k, v) in fields {
        push_unique(&mut out, k, v);
    }
    CONTEXT.with(|c| {
        for (k, v) in c.borrow().iter().rev() {
            push_unique(&mut out, k, v);
        }
    });
    out
}

fn push_unique(out: &mut Vec<(String, Value<'static>)>, key: &str, value: &Value<'_>) {
    if out.iter().any(|(k, _)| k == key) {
        return;
    }
    let value = if is_secret(key) {
        Value::Str(Cow::Borrowed(REDACTED))
    } else {
        value.clone().into_owned()
    };
    out.push((key.to_string(), value));
}

pub fn is_secret(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    SECRET_SUFFIXES.iter().any(|s| key.ends_with(s))
}

fn remember(record: Record) {
    let mut recent = RECENT.lock().unwrap_or_else(PoisonError::into_inner);
    if recent.len() >= RECENT_CAPACITY {
        recent.pop_front();
    }
    recent.push_back(record);
}

pub fn recent() -> Vec<Record> {
    RECENT
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .iter()
        .cloned()
        .collect()
}

pub fn counts() -> [(Level, u64); 4] {
    Level::ALL.map(|l| (l, COUNTS[l.index()].load(Ordering::Relaxed)))
}

#[must_use = "the fields are removed when the scope is dropped"]
#[derive(Debug)]
pub struct Scope {
    len: usize,
    not_send: PhantomData<*const ()>,
}

impl Drop for Scope {
    fn drop(&mut self) {
        CONTEXT.with(|c| c.borrow_mut().truncate(self.len));
    }
}

pub fn scope(fields: &[(&str, Value<'_>)]) -> Scope {
    CONTEXT.with(|c| {
        let mut c = c.borrow_mut();
        let len = c.len();
        for (k, v) in fields {
            push_context(&mut c, k, v);
        }
        Scope {
            len,
            not_send: PhantomData,
        }
    })
}

pub fn annotate(key: &str, value: impl ToValue) {
    CONTEXT.with(|c| push_context(&mut c.borrow_mut(), key, &value.to_value()));
}

fn push_context(context: &mut Vec<(String, Value<'static>)>, key: &str, value: &Value<'_>) {
    if context.len() < MAX_CONTEXT_FIELDS {
        context.push((key.to_string(), value.clone().into_owned()));
    }
}

pub fn context() -> Vec<(String, Value<'static>)> {
    merged_fields(&[])
}

pub fn get(key: &str) -> Option<Value<'static>> {
    CONTEXT.with(|c| {
        c.borrow()
            .iter()
            .rev()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.clone())
    })
}

fn capturing() -> bool {
    CAPTURE.with(|c| c.borrow().is_some())
}

pub fn capture<R>(f: impl FnOnce() -> R) -> (R, Vec<Record>) {
    let outer = CAPTURE.with(|c| c.borrow_mut().replace(Vec::new()));
    let result = f();
    let records = CAPTURE.with(|c| std::mem::replace(&mut *c.borrow_mut(), outer));
    (result, records.unwrap_or_default())
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

pub fn timestamp(ms: u64) -> String {
    let secs = ms / 1000;
    let days = i64::try_from(secs / 86_400).unwrap_or(0);
    let rem = secs % 86_400;
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{:03}Z",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60,
        ms % 1000
    )
}

fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    (
        y,
        u32::try_from(m).unwrap_or(1),
        u32::try_from(d).unwrap_or(1),
    )
}

fn clip(s: &str) -> &str {
    if s.len() <= MAX_VALUE_BYTES {
        return s;
    }
    let mut end = MAX_VALUE_BYTES;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

fn push_display(out: &mut String, value: &dyn Display) {
    write!(out, "{value}").ok();
}

fn push_json_str(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if u32::from(c) < 0x20 || c == '\u{2028}' || c == '\u{2029}' => {
                write!(out, "\\u{:04x}", u32::from(c)).ok();
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levels_order_and_parse() {
        assert!(Level::Error > Level::Warn && Level::Warn > Level::Info);
        assert_eq!("WARNING".parse(), Ok(Level::Warn));
        assert!("loud".parse::<Level>().is_err());
        assert_eq!("json".parse(), Ok(Format::Json));
        assert!("xml".parse::<Format>().is_err());
    }

    #[test]
    fn filter_spec_sets_default_and_targets() {
        let c = Config::new()
            .filter("warn, queue=debug,http=error")
            .unwrap();
        assert_eq!(c.min_level("db"), Level::Warn);
        assert_eq!(c.min_level("queue"), Level::Debug);
        assert_eq!(c.min_level("queue.worker"), Level::Debug);
        assert_eq!(c.min_level("queued"), Level::Warn);
        assert_eq!(c.min_level("http"), Level::Error);
        assert!(Config::new().filter("=debug").is_err());
        assert!(Config::new().filter("queue=loud").is_err());
    }

    #[test]
    fn longest_target_prefix_wins() {
        let c = Config::new()
            .target("llm", Level::Warn)
            .target("llm.gemini", Level::Debug);
        assert_eq!(c.min_level("llm.gemini"), Level::Debug);
        assert_eq!(c.min_level("llm.openai"), Level::Warn);
    }

    #[test]
    fn json_line_keeps_types_and_escapes() {
        let r = Record {
            ts_ms: 1_700_000_000_123,
            level: Level::Warn,
            target: "http".into(),
            msg: "say \"hi\"\n".into(),
            fields: vec![
                ("status".into(), Value::Uint(500)),
                ("ms".into(), Value::Int(-1)),
                ("ok".into(), Value::Bool(false)),
                ("ratio".into(), Value::Float(f64::NAN)),
                ("none".into(), Value::Null),
                ("msg".into(), Value::Str("shadow".into())),
            ],
        };
        assert_eq!(
            r.to_json_line(),
            "{\"ts\":\"2023-11-14T22:13:20.123Z\",\"level\":\"warn\",\"target\":\"http\",\
             \"msg\":\"say \\\"hi\\\"\\n\",\"status\":500,\"ms\":-1,\"ok\":false,\
             \"ratio\":null,\"none\":null,\"_msg\":\"shadow\"}"
        );
    }

    #[test]
    fn text_values_quote_when_needed() {
        let r = Record {
            ts_ms: 0,
            level: Level::Info,
            target: "t".into(),
            msg: "m".into(),
            fields: vec![
                ("path".into(), Value::Str("/api/x".into())),
                ("error".into(), Value::Str("no such table".into())),
                ("empty".into(), Value::Str("".into())),
                ("n".into(), Value::Uint(3)),
            ],
        };
        assert_eq!(
            r.to_text_line(),
            "1970-01-01T00:00:00.000Z INFO  t: m path=/api/x error=\"no such table\" empty=\"\" n=3"
        );
    }

    #[test]
    fn civil_dates() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(20_000), (2024, 10, 4));
        assert_eq!(civil_from_days(19_782), (2024, 2, 29));
        assert_eq!(civil_from_days(-1), (1969, 12, 31));
    }

    #[test]
    fn scope_fields_reach_events_and_leave_on_drop() {
        let ((), records) = capture(|| {
            let _outer = scope(&[("rid", Value::Str("r1".into()))]);
            {
                let _inner = scope(&[("job", Value::Str("mail".into()))]);
                annotate("ws", "w1");
                crate::info!("t", "inside", n = 1);
            }
            crate::info!("t", "outside");
        });
        assert_eq!(records.len(), 2);
        let inside = &records[0];
        assert_eq!(inside.field("n"), Some(&Value::Int(1)));
        assert_eq!(inside.field("rid"), Some(&Value::Str("r1".into())));
        assert_eq!(inside.field("job"), Some(&Value::Str("mail".into())));
        assert_eq!(inside.field("ws"), Some(&Value::Str("w1".into())));
        let outside = &records[1];
        assert_eq!(outside.field("rid"), Some(&Value::Str("r1".into())));
        assert_eq!(outside.field("job"), None);
        assert_eq!(outside.field("ws"), None);
        assert!(get("rid").is_none());
    }

    #[test]
    fn inner_and_event_fields_shadow_outer_ones() {
        let ((), records) = capture(|| {
            let _a = scope(&[("ws", Value::Str("outer".into()))]);
            let _b = scope(&[("ws", Value::Str("inner".into()))]);
            crate::info!("t", "one");
            crate::info!("t", "two", ws = "event");
        });
        assert_eq!(records[0].field("ws"), Some(&Value::Str("inner".into())));
        assert_eq!(records[1].field("ws"), Some(&Value::Str("event".into())));
        assert_eq!(records[1].fields.len(), 1);
    }

    #[test]
    fn secrets_are_redacted_wherever_they_come_from() {
        let ((), records) = capture(|| {
            let _s = scope(&[("session_token", Value::Str("abc".into()))]);
            crate::warn!(
                "auth",
                "login",
                password = "hunter2",
                tokens_in = 12,
                user = "a@b.c"
            );
        });
        let r = &records[0];
        assert_eq!(r.field("password"), Some(&Value::Str(REDACTED.into())));
        assert_eq!(r.field("session_token"), Some(&Value::Str(REDACTED.into())));
        assert_eq!(r.field("tokens_in"), Some(&Value::Int(12)));
        assert_eq!(r.field("user"), Some(&Value::Str("a@b.c".into())));
    }

    #[test]
    fn display_sigil_and_typed_values() {
        struct Oops;
        impl Display for Oops {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("oops")
            }
        }
        let elapsed: u128 = 41;
        let missing: Option<&str> = None;
        let ((), records) = capture(|| {
            crate::error!(
                "t",
                "failed",
                error = %Oops,
                ms = elapsed,
                big = u128::MAX,
                who = missing,
                ok = true,
            );
        });
        let r = &records[0];
        assert_eq!(r.level, Level::Error);
        assert_eq!(r.field("error"), Some(&Value::Str("oops".into())));
        assert_eq!(r.field("ms"), Some(&Value::Uint(41)));
        assert_eq!(
            r.field("big"),
            Some(&Value::Str(u128::MAX.to_string().into()))
        );
        assert_eq!(r.field("who"), Some(&Value::Null));
        assert_eq!(r.field("ok"), Some(&Value::Bool(true)));
    }

    #[test]
    fn capture_sees_levels_the_filter_would_drop() {
        let ((), records) = capture(|| crate::debug!("quiet.target", "detail"));
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].level, Level::Debug);
    }

    #[test]
    fn long_values_are_clipped_on_a_char_boundary() {
        let s = "é".repeat(MAX_VALUE_BYTES);
        let clipped = clip(&s);
        assert!(clipped.len() <= MAX_VALUE_BYTES);
        assert!(clipped.chars().all(|c| c == 'é'));
    }

    #[test]
    fn context_is_bounded() {
        let _s = scope(&[]);
        for i in 0..100 {
            annotate("k", i);
        }
        assert_eq!(CONTEXT.with(|c| c.borrow().len()), MAX_CONTEXT_FIELDS);
    }

    #[test]
    fn json_strings_escape_controls() {
        let mut s = String::new();
        push_json_str(&mut s, "a\u{1}b\u{2028}");
        assert_eq!(s, "\"a\\u0001b\\u2028\"");
    }
}
