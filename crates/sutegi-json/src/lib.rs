//! A tiny, zero-dependency JSON implementation.
//!
//! `sutegi` uses JSON as the lingua franca between the app and AI agents
//! (introspection, tool manifests, tool I/O). Rather than pull in `serde`,
//! we ship a compact hand-written value type, serializer, and parser. Keys
//! are stored in a `BTreeMap` so output is deterministic — important when an
//! agent diffs or caches the introspection surface.

use std::collections::BTreeMap;
use std::fmt::{self, Write};

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
                    let _ = write_escaped(out, k);
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
            other => {
                let _ = write!(out, "{}", other);
            }
        }
    }

    /// Parse a JSON document. Returns an error string on malformed input.
    pub fn parse(input: &str) -> Result<Json, String> {
        let mut p = Parser {
            chars: input.chars().collect(),
            pos: 0,
        };
        p.skip_ws();
        let v = p.value()?;
        p.skip_ws();
        if p.pos != p.chars.len() {
            return Err(format!("trailing characters at position {}", p.pos));
        }
        Ok(v)
    }
}

/// Compact serialization via `Display` — so `value.to_string()` just works.
impl fmt::Display for Json {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Json::Null => f.write_str("null"),
            Json::Bool(b) => write!(f, "{}", b),
            Json::Num(n) => {
                if n.is_finite() && n.fract() == 0.0 && n.abs() < 1e15 {
                    write!(f, "{}", *n as i64)
                } else if n.is_finite() {
                    write!(f, "{}", n)
                } else {
                    f.write_str("null") // JSON has no Inf/NaN
                }
            }
            Json::Str(s) => write_escaped(f, s),
            Json::Arr(a) => {
                f.write_char('[')?;
                for (i, v) in a.iter().enumerate() {
                    if i > 0 {
                        f.write_char(',')?;
                    }
                    write!(f, "{}", v)?;
                }
                f.write_char(']')
            }
            Json::Obj(m) => {
                f.write_char('{')?;
                for (i, (k, v)) in m.iter().enumerate() {
                    if i > 0 {
                        f.write_char(',')?;
                    }
                    write_escaped(f, k)?;
                    f.write_char(':')?;
                    write!(f, "{}", v)?;
                }
                f.write_char('}')
            }
        }
    }
}

/// Write a JSON-escaped, quoted string into any `fmt::Write`.
fn write_escaped<W: fmt::Write>(w: &mut W, s: &str) -> fmt::Result {
    w.write_char('"')?;
    for c in s.chars() {
        match c {
            '"' => w.write_str("\\\"")?,
            '\\' => w.write_str("\\\\")?,
            '\n' => w.write_str("\\n")?,
            '\r' => w.write_str("\\r")?,
            '\t' => w.write_str("\\t")?,
            '\u{0008}' => w.write_str("\\b")?,
            '\u{000C}' => w.write_str("\\f")?,
            c if (c as u32) < 0x20 => write!(w, "\\u{:04x}", c as u32)?,
            c => w.write_char(c)?,
        }
    }
    w.write_char('"')
}

// ---- parser ---------------------------------------------------------------

struct Parser {
    chars: Vec<char>,
    pos: usize,
}

impl Parser {
    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    fn next(&mut self) -> Option<char> {
        let c = self.peek();
        if c.is_some() {
            self.pos += 1;
        }
        c
    }

    fn skip_ws(&mut self) {
        while matches!(self.peek(), Some(' ' | '\t' | '\n' | '\r')) {
            self.pos += 1;
        }
    }

    fn value(&mut self) -> Result<Json, String> {
        self.skip_ws();
        match self.peek() {
            Some('{') => self.object(),
            Some('[') => self.array(),
            Some('"') => Ok(Json::Str(self.string()?)),
            Some('t') | Some('f') => self.boolean(),
            Some('n') => self.null(),
            Some(c) if c == '-' || c.is_ascii_digit() => self.number(),
            Some(c) => Err(format!("unexpected character '{}' at {}", c, self.pos)),
            None => Err("unexpected end of input".to_string()),
        }
    }

    fn object(&mut self) -> Result<Json, String> {
        self.next(); // consume '{'
        let mut m = BTreeMap::new();
        self.skip_ws();
        if self.peek() == Some('}') {
            self.next();
            return Ok(Json::Obj(m));
        }
        loop {
            self.skip_ws();
            if self.peek() != Some('"') {
                return Err(format!("expected object key at {}", self.pos));
            }
            let key = self.string()?;
            self.skip_ws();
            if self.next() != Some(':') {
                return Err(format!("expected ':' at {}", self.pos));
            }
            let val = self.value()?;
            m.insert(key, val);
            self.skip_ws();
            match self.next() {
                Some(',') => continue,
                Some('}') => break,
                _ => return Err(format!("expected ',' or '}}' at {}", self.pos)),
            }
        }
        Ok(Json::Obj(m))
    }

    fn array(&mut self) -> Result<Json, String> {
        self.next(); // consume '['
        let mut a = Vec::new();
        self.skip_ws();
        if self.peek() == Some(']') {
            self.next();
            return Ok(Json::Arr(a));
        }
        loop {
            let val = self.value()?;
            a.push(val);
            self.skip_ws();
            match self.next() {
                Some(',') => continue,
                Some(']') => break,
                _ => return Err(format!("expected ',' or ']' at {}", self.pos)),
            }
        }
        Ok(Json::Arr(a))
    }

    fn string(&mut self) -> Result<String, String> {
        self.next(); // consume opening quote
        let mut s = String::new();
        loop {
            match self.next() {
                Some('"') => break,
                Some('\\') => match self.next() {
                    Some('"') => s.push('"'),
                    Some('\\') => s.push('\\'),
                    Some('/') => s.push('/'),
                    Some('n') => s.push('\n'),
                    Some('t') => s.push('\t'),
                    Some('r') => s.push('\r'),
                    Some('b') => s.push('\u{0008}'),
                    Some('f') => s.push('\u{000C}'),
                    Some('u') => {
                        let mut code: u32 = 0;
                        for _ in 0..4 {
                            let c = self.next().ok_or("unterminated \\u escape")?;
                            code = code * 16 + c.to_digit(16).ok_or("invalid hex in \\u escape")?;
                        }
                        s.push(char::from_u32(code).unwrap_or('\u{FFFD}'));
                    }
                    _ => return Err("invalid escape sequence".to_string()),
                },
                Some(c) => s.push(c),
                None => return Err("unterminated string".to_string()),
            }
        }
        Ok(s)
    }

    fn number(&mut self) -> Result<Json, String> {
        let start = self.pos;
        while let Some(c) = self.peek() {
            if c == '-' || c == '+' || c == '.' || c == 'e' || c == 'E' || c.is_ascii_digit() {
                self.pos += 1;
            } else {
                break;
            }
        }
        let raw: String = self.chars[start..self.pos].iter().collect();
        raw.parse::<f64>()
            .map(Json::Num)
            .map_err(|_| format!("invalid number '{}'", raw))
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

    fn literal(&mut self, lit: &str) -> bool {
        let end = self.pos + lit.len();
        if end <= self.chars.len() && self.chars[self.pos..end].iter().collect::<String>() == lit {
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
        assert_eq!(v.get("a").unwrap().get("b").unwrap(), &Json::arr(vec![
            Json::num(1),
            Json::num(2),
            Json::str("he\"llo\n"),
        ]));
    }

    #[test]
    fn integers_render_without_decimal() {
        assert_eq!(Json::num(42).to_string(), "42");
        assert_eq!(Json::Num(1.5).to_string(), "1.5");
    }
}
