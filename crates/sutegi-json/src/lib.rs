//! A tiny, zero-dependency JSON implementation.
//!
//! `sutegi` uses JSON as the lingua franca between the app and AI agents
//! (introspection, tool manifests, tool I/O). Rather than pull in `serde`,
//! we ship a compact hand-written value type, serializer, and parser. Keys
//! are stored in a `BTreeMap` so output is deterministic — important when an
//! agent diffs or caches the introspection surface.

use std::collections::BTreeMap;
use std::fmt::{self, Write};
use std::ops::Index;

/// A JSON value.
#[derive(Clone, Debug, PartialEq)]
pub enum Json {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Vec<Json>),
    Obj(BTreeMap<String, Json>),
}

impl Default for Json {
    /// `Json::Null` — the natural empty value (so structs with a `Json` field
    /// can still `#[derive(Default)]`).
    fn default() -> Json {
        Json::Null
    }
}

impl Json {
    /// Build a `Json::Str` from anything string-like.
    pub fn str(s: impl Into<String>) -> Json {
        Json::Str(s.into())
    }

    /// Build a `Json::Num` from a float-compatible value.
    pub fn num(n: impl Into<f64>) -> Json {
        Json::Num(n.into())
    }

    /// Build a `Json::Num` from a 64-bit integer. (`i64: Into<f64>` is not
    /// provided by std because the conversion is lossy for large values, so we
    /// offer an explicit constructor.)
    pub fn int(n: i64) -> Json {
        Json::Num(n as f64)
    }

    /// Build a `Json::Obj` from key/value pairs, preserving insertion intent
    /// (the map sorts keys for deterministic output).
    pub fn obj(pairs: Vec<(&str, Json)>) -> Json {
        let mut m = BTreeMap::new();
        for (k, v) in pairs {
            m.insert(k.to_string(), v);
        }
        Json::Obj(m)
    }

    /// Build a `Json::Arr`.
    pub fn arr(items: Vec<Json>) -> Json {
        Json::Arr(items)
    }

    /// Look up a key in an object, returning `None` for non-objects.
    pub fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Obj(m) => m.get(key),
            _ => None,
        }
    }

    /// Borrow the string payload, if this is a string.
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Json::Str(s) => Some(s),
            _ => None,
        }
    }

    /// Read the number payload, if this is a number.
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Json::Num(n) => Some(*n),
            _ => None,
        }
    }

    /// Read the boolean payload, if this is a bool.
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Json::Bool(b) => Some(*b),
            _ => None,
        }
    }

    /// Read an integer payload (only if the number is integral).
    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Json::Num(n) if n.fract() == 0.0 && n.is_finite() => Some(*n as i64),
            _ => None,
        }
    }

    /// Borrow the array payload, if this is an array.
    pub fn as_array(&self) -> Option<&Vec<Json>> {
        match self {
            Json::Arr(a) => Some(a),
            _ => None,
        }
    }

    /// Borrow the object payload, if this is an object.
    pub fn as_object(&self) -> Option<&BTreeMap<String, Json>> {
        match self {
            Json::Obj(m) => Some(m),
            _ => None,
        }
    }

    /// Whether this value is `null`.
    pub fn is_null(&self) -> bool {
        matches!(self, Json::Null)
    }

    /// Resolve a JSON Pointer-ish path (`/a/b/0`) — slash-separated keys, with
    /// numeric segments indexing arrays. Returns `None` if any step is missing.
    pub fn pointer(&self, path: &str) -> Option<&Json> {
        let mut node = self;
        for seg in path.split('/').filter(|s| !s.is_empty()) {
            node = match node {
                Json::Obj(m) => m.get(seg)?,
                Json::Arr(a) => a.get(seg.parse::<usize>().ok()?)?,
                _ => return None,
            };
        }
        Some(node)
    }

    /// Serialize to a pretty, indented string (used by `/__introspect`).
    pub fn to_pretty(&self) -> String {
        let mut out = String::new();
        self.write_pretty(&mut out, 0);
        out
    }

    fn write_pretty(&self, out: &mut String, depth: usize) {
        let pad = |n: usize| "  ".repeat(n);
        match self {
            Json::Arr(a) if !a.is_empty() => {
                out.push_str("[\n");
                for (i, v) in a.iter().enumerate() {
                    out.push_str(&pad(depth + 1));
                    v.write_pretty(out, depth + 1);
                    if i + 1 < a.len() {
                        out.push(',');
                    }
                    out.push('\n');
                }
                out.push_str(&pad(depth));
                out.push(']');
            }
            Json::Obj(m) if !m.is_empty() => {
                out.push_str("{\n");
                let len = m.len();
                for (i, (k, v)) in m.iter().enumerate() {
                    out.push_str(&pad(depth + 1));
                    escape_into(out, k);
                    out.push_str(": ");
                    v.write_pretty(out, depth + 1);
                    if i + 1 < len {
                        out.push(',');
                    }
                    out.push('\n');
                }
                out.push_str(&pad(depth));
                out.push('}');
            }
            // Scalars and empty containers fall through to the compact form.
            other => other.write_to(out),
        }
    }

    /// Serialize compactly into an existing buffer — the allocation-free core
    /// that `Display`/`to_string` and `to_pretty` are built on.
    pub fn write_to(&self, out: &mut String) {
        match self {
            Json::Null => out.push_str("null"),
            Json::Bool(true) => out.push_str("true"),
            Json::Bool(false) => out.push_str("false"),
            Json::Num(n) => {
                if n.is_finite() && n.fract() == 0.0 && n.abs() < 1e15 {
                    let _ = write!(out, "{}", *n as i64);
                } else if n.is_finite() {
                    let _ = write!(out, "{}", n);
                } else {
                    out.push_str("null") // JSON has no Inf/NaN
                }
            }
            Json::Str(s) => escape_into(out, s),
            Json::Arr(a) => {
                out.push('[');
                for (i, v) in a.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    v.write_to(out);
                }
                out.push(']');
            }
            Json::Obj(m) => {
                out.push('{');
                for (i, (k, v)) in m.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    escape_into(out, k);
                    out.push(':');
                    v.write_to(out);
                }
                out.push('}');
            }
        }
    }

    /// Parse a JSON document. Returns an error string on malformed input.
    pub fn parse(input: &str) -> Result<Json, String> {
        let mut p = Parser {
            src: input,
            pos: 0,
            depth: 0,
        };
        p.skip_ws();
        let v = p.value()?;
        p.skip_ws();
        if p.pos != p.src.len() {
            return Err(format!("trailing characters at position {}", p.pos));
        }
        Ok(v)
    }
}

// --- ergonomic conversions ---

impl From<&str> for Json {
    fn from(s: &str) -> Json {
        Json::Str(s.to_string())
    }
}
impl From<String> for Json {
    fn from(s: String) -> Json {
        Json::Str(s)
    }
}
impl From<bool> for Json {
    fn from(b: bool) -> Json {
        Json::Bool(b)
    }
}
impl From<i64> for Json {
    fn from(n: i64) -> Json {
        Json::Num(n as f64)
    }
}
impl From<i32> for Json {
    fn from(n: i32) -> Json {
        Json::Num(n as f64)
    }
}
impl From<f64> for Json {
    fn from(n: f64) -> Json {
        Json::Num(n)
    }
}
impl From<usize> for Json {
    fn from(n: usize) -> Json {
        Json::Num(n as f64)
    }
}
impl<T: Into<Json>> From<Vec<T>> for Json {
    fn from(v: Vec<T>) -> Json {
        Json::Arr(v.into_iter().map(Into::into).collect())
    }
}
impl<T: Into<Json>> From<Option<T>> for Json {
    fn from(o: Option<T>) -> Json {
        match o {
            Some(v) => v.into(),
            None => Json::Null,
        }
    }
}

/// A shared `null` so `Index` can return a reference for missing keys.
static NULL: Json = Json::Null;

/// `json["key"]` — returns `Null` (not a panic) for non-objects or missing keys,
/// so deep access like `json["a"]["b"]` is safe to chain.
impl Index<&str> for Json {
    type Output = Json;
    fn index(&self, key: &str) -> &Json {
        self.get(key).unwrap_or(&NULL)
    }
}

/// `json[i]` — returns `Null` for non-arrays or out-of-range indices.
impl Index<usize> for Json {
    type Output = Json;
    fn index(&self, i: usize) -> &Json {
        match self {
            Json::Arr(a) => a.get(i).unwrap_or(&NULL),
            _ => &NULL,
        }
    }
}

/// Compact serialization via `Display` — so `value.to_string()` just works.
/// Serializes into one buffer with [`Json::write_to`] and hands it to the
/// formatter whole: one `fmt` call instead of one per token.
impl fmt::Display for Json {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut out = String::with_capacity(128);
        self.write_to(&mut out);
        f.write_str(&out)
    }
}

/// Append a JSON-escaped, quoted string. Contiguous runs needing no escape
/// are appended as whole slices (a memcpy), not char by char.
fn escape_into(out: &mut String, s: &str) {
    out.push('"');
    let bytes = s.as_bytes();
    let mut run = 0;
    for (i, &b) in bytes.iter().enumerate() {
        let esc: Option<&str> = match b {
            b'"' => Some("\\\""),
            b'\\' => Some("\\\\"),
            b'\n' => Some("\\n"),
            b'\r' => Some("\\r"),
            b'\t' => Some("\\t"),
            0x08 => Some("\\b"),
            0x0C => Some("\\f"),
            b if b < 0x20 => Some(""), // \u escape, formatted below
            _ => None,
        };
        if let Some(esc) = esc {
            // The run boundary is an ASCII byte, so it is a char boundary.
            out.push_str(&s[run..i]);
            if esc.is_empty() {
                let _ = write!(out, "\\u{:04x}", b);
            } else {
                out.push_str(esc);
            }
            run = i + 1;
        }
    }
    out.push_str(&s[run..]);
    out.push('"');
}

// ---- parser ---------------------------------------------------------------

/// A byte-cursor parser over the input `&str`. Structural JSON characters are
/// all ASCII, so scanning bytes is safe; multi-byte UTF-8 sequences only occur
/// inside strings, where whole unescaped runs are copied as slices. Positions
/// in errors are byte offsets.
struct Parser<'a> {
    src: &'a str,
    pos: usize,
    /// Current container-nesting depth, checked against [`MAX_DEPTH`] so that
    /// adversarial input can't drive the recursive descent past the stack.
    depth: usize,
}

/// Maximum container nesting the parser will descend into before erroring.
/// This is the guard against a stack-overflow DoS: a body of just `[[[[…` is a
/// few bytes per level, and a Rust stack overflow *aborts the whole process*,
/// so an unbounded recursive parser would let any request body (or AI tool
/// argument) kill the worker. RFC 8259 §9 explicitly permits such a limit.
const MAX_DEPTH: usize = 128;

impl Parser<'_> {
    fn peek(&self) -> Option<u8> {
        self.src.as_bytes().get(self.pos).copied()
    }

    fn next(&mut self) -> Option<u8> {
        let b = self.peek();
        if b.is_some() {
            self.pos += 1;
        }
        b
    }

    fn skip_ws(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.pos += 1;
        }
    }

    fn value(&mut self) -> Result<Json, String> {
        self.skip_ws();
        match self.peek() {
            Some(b'{') | Some(b'[') => {
                self.depth += 1;
                if self.depth > MAX_DEPTH {
                    return Err(format!("maximum nesting depth {MAX_DEPTH} exceeded"));
                }
                let v = if self.peek() == Some(b'{') {
                    self.object()
                } else {
                    self.array()
                };
                self.depth -= 1;
                v
            }
            Some(b'"') => Ok(Json::Str(self.string()?)),
            Some(b't') | Some(b'f') => self.boolean(),
            Some(b'n') => self.null(),
            Some(c) if c == b'-' || c.is_ascii_digit() => self.number(),
            Some(c) => Err(format!(
                "unexpected character '{}' at {}",
                c as char, self.pos
            )),
            None => Err("unexpected end of input".to_string()),
        }
    }

    fn object(&mut self) -> Result<Json, String> {
        self.next(); // consume '{'
        let mut m = BTreeMap::new();
        self.skip_ws();
        if self.peek() == Some(b'}') {
            self.next();
            return Ok(Json::Obj(m));
        }
        loop {
            self.skip_ws();
            if self.peek() != Some(b'"') {
                return Err(format!("expected object key at {}", self.pos));
            }
            let key = self.string()?;
            self.skip_ws();
            if self.next() != Some(b':') {
                return Err(format!("expected ':' at {}", self.pos));
            }
            let val = self.value()?;
            m.insert(key, val);
            self.skip_ws();
            match self.next() {
                Some(b',') => continue,
                Some(b'}') => break,
                _ => return Err(format!("expected ',' or '}}' at {}", self.pos)),
            }
        }
        Ok(Json::Obj(m))
    }

    fn array(&mut self) -> Result<Json, String> {
        self.next(); // consume '['
        let mut a = Vec::new();
        self.skip_ws();
        if self.peek() == Some(b']') {
            self.next();
            return Ok(Json::Arr(a));
        }
        loop {
            let val = self.value()?;
            a.push(val);
            self.skip_ws();
            match self.next() {
                Some(b',') => continue,
                Some(b']') => break,
                _ => return Err(format!("expected ',' or ']' at {}", self.pos)),
            }
        }
        Ok(Json::Arr(a))
    }

    fn string(&mut self) -> Result<String, String> {
        self.next(); // consume opening quote
        let mut s = String::new();
        let mut run = self.pos; // start of the current escape-free run
        loop {
            match self.peek() {
                Some(b'"') => {
                    // `"` is ASCII, so the run boundary is a char boundary.
                    s.push_str(&self.src[run..self.pos]);
                    self.pos += 1;
                    return Ok(s);
                }
                Some(b'\\') => {
                    s.push_str(&self.src[run..self.pos]);
                    self.pos += 1;
                    match self.next() {
                        Some(b'"') => s.push('"'),
                        Some(b'\\') => s.push('\\'),
                        Some(b'/') => s.push('/'),
                        Some(b'n') => s.push('\n'),
                        Some(b't') => s.push('\t'),
                        Some(b'r') => s.push('\r'),
                        Some(b'b') => s.push('\u{0008}'),
                        Some(b'f') => s.push('\u{000C}'),
                        Some(b'u') => {
                            let hi = self.hex4()?;
                            // UTF-16 surrogate pair: a high surrogate must be
                            // followed by `\uXXXX` low surrogate, and the two
                            // combine into one non-BMP scalar. Without this,
                            // any escaped emoji/astral char decodes to U+FFFD.
                            let ch = if (0xD800..=0xDBFF).contains(&hi) {
                                // High surrogate: try to consume a following
                                // `\uXXXX` low surrogate. If it isn't one, rewind
                                // so those bytes are parsed normally and emit the
                                // replacement char for the unpaired surrogate.
                                let save = self.pos;
                                let paired =
                                    if self.next() == Some(b'\\') && self.next() == Some(b'u') {
                                        let lo = self.hex4()?;
                                        (0xDC00..=0xDFFF).contains(&lo).then(|| {
                                            0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00)
                                        })
                                    } else {
                                        None
                                    };
                                match paired {
                                    Some(c) => char::from_u32(c).unwrap_or('\u{FFFD}'),
                                    None => {
                                        self.pos = save;
                                        '\u{FFFD}'
                                    }
                                }
                            } else {
                                char::from_u32(hi).unwrap_or('\u{FFFD}')
                            };
                            s.push(ch);
                        }
                        _ => return Err("invalid escape sequence".to_string()),
                    }
                    run = self.pos;
                }
                // Any other byte — including UTF-8 continuation bytes — just
                // extends the run; it is copied wholesale at the next boundary.
                Some(_) => self.pos += 1,
                None => return Err("unterminated string".to_string()),
            }
        }
    }

    fn number(&mut self) -> Result<Json, String> {
        let start = self.pos;
        while let Some(c) = self.peek() {
            if c == b'-' || c == b'+' || c == b'.' || c == b'e' || c == b'E' || c.is_ascii_digit() {
                self.pos += 1;
            } else {
                break;
            }
        }
        let raw = &self.src[start..self.pos];
        match raw.parse::<f64>() {
            // A number that overflows f64 parses to ±inf, which the serializer
            // renders as `null` — so accepting it would silently mutate the
            // value across a round-trip. Reject non-finite instead.
            Ok(n) if n.is_finite() => Ok(Json::Num(n)),
            Ok(_) => Err(format!("number out of range '{raw}'")),
            Err(_) => Err(format!("invalid number '{raw}'")),
        }
    }

    fn boolean(&mut self) -> Result<Json, String> {
        if self.literal("true") {
            Ok(Json::Bool(true))
        } else if self.literal("false") {
            Ok(Json::Bool(false))
        } else {
            Err(format!("invalid literal at {}", self.pos))
        }
    }

    fn null(&mut self) -> Result<Json, String> {
        if self.literal("null") {
            Ok(Json::Null)
        } else {
            Err(format!("invalid literal at {}", self.pos))
        }
    }

    /// Read exactly four hex digits (one UTF-16 code unit) after a `\u`.
    fn hex4(&mut self) -> Result<u32, String> {
        let mut code = 0u32;
        for _ in 0..4 {
            let c = self.next().ok_or("unterminated \\u escape")?;
            code = code * 16
                + (c as char)
                    .to_digit(16)
                    .ok_or("invalid hex in \\u escape")?;
        }
        Ok(code)
    }

    fn literal(&mut self, lit: &str) -> bool {
        let end = self.pos + lit.len();
        if end <= self.src.len() && &self.src.as_bytes()[self.pos..end] == lit.as_bytes() {
            self.pos = end;
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_object() {
        let v = Json::obj(vec![
            ("name", Json::str("sutegi")),
            ("count", Json::num(3)),
            ("ok", Json::Bool(true)),
            ("tags", Json::arr(vec![Json::str("a"), Json::str("b")])),
        ]);
        let s = v.to_string();
        let back = Json::parse(&s).unwrap();
        assert_eq!(v, back);
    }

    #[test]
    fn parses_nested_and_escapes() {
        let v = Json::parse(r#"{"a":{"b":[1,2,"he\"llo\n"]},"n":null}"#).unwrap();
        assert_eq!(
            v.get("a").unwrap().get("b").unwrap(),
            &Json::arr(vec![Json::num(1), Json::num(2), Json::str("he\"llo\n"),])
        );
    }

    #[test]
    fn integers_render_without_decimal() {
        assert_eq!(Json::num(42).to_string(), "42");
        assert_eq!(Json::Num(1.5).to_string(), "1.5");
    }

    #[test]
    fn index_and_pointer() {
        let v = Json::parse(r#"{"a":{"b":[10,20]}}"#).unwrap();
        // Index returns Null for missing rather than panicking.
        assert_eq!(v["a"]["b"][1].as_i64(), Some(20));
        assert!(v["a"]["missing"].is_null());
        assert!(v["nope"][5].is_null());
        // JSON Pointer-ish path.
        assert_eq!(v.pointer("/a/b/0").and_then(Json::as_i64), Some(10));
        assert!(v.pointer("/a/x").is_none());
    }

    #[test]
    fn from_conversions() {
        let j: Json = vec![1i64, 2, 3].into();
        assert_eq!(j, Json::arr(vec![Json::num(1), Json::num(2), Json::num(3)]));
        let o: Json = Some("hi").into();
        assert_eq!(o, Json::str("hi"));
        let n: Json = Option::<&str>::None.into();
        assert!(n.is_null());
    }

    #[test]
    fn typed_accessors() {
        let v = Json::parse(r#"{"n":7,"arr":[1],"obj":{"k":1}}"#).unwrap();
        assert_eq!(v["n"].as_i64(), Some(7));
        assert_eq!(v["arr"].as_array().map(Vec::len), Some(1));
        assert!(v["obj"].as_object().is_some());
    }

    #[test]
    fn parse_errors_are_reported() {
        // Trailing characters after a complete value.
        assert!(Json::parse("123 garbage").unwrap_err().contains("trailing"));
        // Unterminated string.
        assert!(Json::parse("\"abc").unwrap_err().contains("unterminated"));
        // Bad escape sequence.
        assert!(Json::parse(r#""a\xb""#).unwrap_err().contains("escape"));
        // Missing object value / malformed object.
        assert!(Json::parse(r#"{"k"}"#).is_err());
        assert!(Json::parse("[1,2").is_err()); // unterminated array
        assert!(Json::parse("").is_err()); // empty input
        assert!(Json::parse("nul").is_err()); // truncated literal
    }

    #[test]
    fn parses_unicode_escape() {
        let v = Json::parse(r#""éA""#).unwrap();
        assert_eq!(v.as_str(), Some("éA"));
    }

    #[test]
    fn pretty_prints_nested() {
        let v = Json::obj(vec![
            ("a", Json::int(1)),
            ("b", Json::arr(vec![Json::int(2), Json::int(3)])),
        ]);
        let pretty = v.to_pretty();
        // Indented, multi-line, and still re-parses to the same value.
        assert!(pretty.contains("\n  \"a\": 1"));
        assert!(pretty.contains('\n'));
        assert_eq!(Json::parse(&pretty).unwrap(), v);
        // Empty containers stay compact even in pretty mode.
        assert_eq!(Json::arr(vec![]).to_pretty(), "[]");
        assert_eq!(Json::obj(vec![]).to_pretty(), "{}");
    }

    #[test]
    fn number_display_edge_cases() {
        // Non-finite numbers are not valid JSON → rendered as null.
        assert_eq!(Json::Num(f64::INFINITY).to_string(), "null");
        assert_eq!(Json::Num(f64::NAN).to_string(), "null");
        // Negative integral floats render without a decimal point.
        assert_eq!(Json::Num(-7.0).to_string(), "-7");
        // as_i64 rejects fractional numbers.
        assert_eq!(Json::Num(2.5).as_i64(), None);
        assert_eq!(Json::int(-42).as_i64(), Some(-42));
    }

    #[test]
    fn accessors_return_none_on_type_mismatch() {
        let v = Json::str("hi");
        assert_eq!(v.as_f64(), None);
        assert_eq!(v.as_bool(), None);
        assert_eq!(v.as_array(), None);
        assert_eq!(v.as_object(), None);
        assert_eq!(v.get("x"), None);
        // Index into a non-array / pointer past the end → Null / None.
        assert!(Json::arr(vec![Json::int(1)])[9].is_null());
        assert!(v.pointer("/0").is_none());
    }

    #[test]
    fn string_escaping_roundtrips_control_chars() {
        let s = "tab\tnewline\nquote\"slash\\bell\u{0008}";
        let encoded = Json::str(s).to_string();
        assert!(encoded.contains("\\t") && encoded.contains("\\n") && encoded.contains("\\b"));
        assert_eq!(Json::parse(&encoded).unwrap().as_str(), Some(s));
    }
}
