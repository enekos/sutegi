//! Finds `#[pg_function]` items in an extension's sources and maps their
//! signatures to SQL — the SQL half of what the attribute macro does for
//! Rust. A small lexer is enough: only attributes, doc comments and `fn`
//! signatures matter, and everything else is skipped token by token.

use std::fmt;

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Ident(String),
    Punct(char),
    Str { body: String, raw: bool },
    Lit(String),
    Lifetime,
    Doc(String),
}

#[derive(Debug, Clone)]
struct Token {
    tok: Tok,
    line: usize,
}

/// A scan or type-mapping failure, located in the source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanError {
    pub file: String,
    pub line: usize,
    pub message: String,
}

impl fmt::Display for ScanError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}: {}", self.file, self.line, self.message)
    }
}

const MAX_SOURCE_BYTES: usize = 16 * 1024 * 1024;

fn lex(src: &str) -> Result<Vec<Token>, (usize, String)> {
    if src.len() > MAX_SOURCE_BYTES {
        return Err((1, "source file too large".into()));
    }
    let chars: Vec<char> = src.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    let mut line = 1;
    let at = |i: usize| chars.get(i).copied();
    while let Some(c) = at(i) {
        if c == '\n' {
            line += 1;
            i += 1;
        } else if c.is_whitespace() {
            i += 1;
        } else if c == '/' && at(i + 1) == Some('/') {
            let start = i;
            while at(i).is_some_and(|c| c != '\n') {
                i += 1;
            }
            let text: String = chars[start..i].iter().collect();
            if let Some(doc) = text.strip_prefix("///")
                && !doc.starts_with('/')
            {
                out.push(Token {
                    tok: Tok::Doc(doc.strip_prefix(' ').unwrap_or(doc).trim_end().to_owned()),
                    line,
                });
            }
        } else if c == '/' && at(i + 1) == Some('*') {
            let mut depth = 0usize;
            loop {
                match (at(i), at(i + 1)) {
                    (Some('/'), Some('*')) => {
                        depth += 1;
                        i += 2;
                    }
                    (Some('*'), Some('/')) => {
                        depth -= 1;
                        i += 2;
                        if depth == 0 {
                            break;
                        }
                    }
                    (Some(ch), _) => {
                        if ch == '\n' {
                            line += 1;
                        }
                        i += 1;
                    }
                    (None, _) => return Err((line, "unterminated block comment".into())),
                }
            }
        } else if c == '"' {
            let start_line = line;
            i += 1;
            let begin = i;
            loop {
                match at(i) {
                    Some('\\') => {
                        if at(i + 1) == Some('\n') {
                            line += 1;
                        }
                        i += 2;
                    }
                    Some('"') => {
                        i += 1;
                        break;
                    }
                    Some(ch) => {
                        if ch == '\n' {
                            line += 1;
                        }
                        i += 1;
                    }
                    None => return Err((start_line, "unterminated string".into())),
                }
            }
            out.push(Token {
                tok: Tok::Str {
                    body: chars[begin..i - 1].iter().collect(),
                    raw: false,
                },
                line: start_line,
            });
        } else if c == '\'' {
            if at(i + 1) == Some('\\') {
                // Skip the quote, the backslash and the escaped character
                // (which may itself be a quote: '\''), then up to the close.
                i += 3;
                while at(i).is_some_and(|c| c != '\'') {
                    i += 1;
                }
                i += 1;
                out.push(Token {
                    tok: Tok::Lit(String::new()),
                    line,
                });
            } else if at(i + 2) == Some('\'') {
                i += 3;
                out.push(Token {
                    tok: Tok::Lit(String::new()),
                    line,
                });
            } else {
                i += 1;
                while at(i).is_some_and(|c| c.is_alphanumeric() || c == '_') {
                    i += 1;
                }
                out.push(Token {
                    tok: Tok::Lifetime,
                    line,
                });
            }
        } else if c.is_alphabetic() || c == '_' {
            let start = i;
            while at(i).is_some_and(|c| c.is_alphanumeric() || c == '_') {
                i += 1;
            }
            let word: String = chars[start..i].iter().collect();
            let raw_prefix = matches!(word.as_str(), "r" | "br" | "cr");
            if word == "r"
                && at(i) == Some('#')
                && at(i + 1).is_some_and(|c| c.is_alphabetic() || c == '_')
            {
                // A raw identifier, r#type: the identifier is what follows.
                i += 1;
                let begin = i;
                while at(i).is_some_and(|c| c.is_alphanumeric() || c == '_') {
                    i += 1;
                }
                out.push(Token {
                    tok: Tok::Ident(chars[begin..i].iter().collect()),
                    line,
                });
            } else if raw_prefix && matches!(at(i), Some('"' | '#')) {
                let mut hashes = 0;
                while at(i) == Some('#') {
                    hashes += 1;
                    i += 1;
                }
                if at(i) != Some('"') {
                    out.push(Token {
                        tok: Tok::Ident(word),
                        line,
                    });
                    continue;
                }
                let start_line = line;
                i += 1;
                let begin = i;
                loop {
                    match at(i) {
                        Some('"') if (1..=hashes).all(|k| at(i + k) == Some('#')) => break,
                        Some(ch) => {
                            if ch == '\n' {
                                line += 1;
                            }
                            i += 1;
                        }
                        None => return Err((start_line, "unterminated raw string".into())),
                    }
                }
                let s: String = chars[begin..i].iter().collect();
                i += 1 + hashes;
                out.push(Token {
                    tok: Tok::Str { body: s, raw: true },
                    line: start_line,
                });
            } else if matches!(word.as_str(), "b" | "c") && at(i) == Some('"') {
                // A byte/C string: lex the literal itself on the next turn.
            } else {
                out.push(Token {
                    tok: Tok::Ident(word),
                    line,
                });
            }
        } else if c.is_ascii_digit() {
            let start = i;
            while at(i).is_some_and(|c| c.is_alphanumeric() || c == '_' || c == '.') {
                i += 1;
            }
            out.push(Token {
                tok: Tok::Lit(chars[start..i].iter().collect()),
                line,
            });
        } else {
            out.push(Token {
                tok: Tok::Punct(c),
                line,
            });
            i += 1;
        }
    }
    Ok(out)
}

/// A Rust type, reduced to what the SQL mapping needs.
#[derive(Debug, Clone, PartialEq)]
enum Ty {
    Unit,
    Ref(Box<Ty>),
    Slice(Box<Ty>),
    Path(String, Vec<Ty>),
    Other,
}

/// One SQL function found in the sources.
#[derive(Debug, Clone, PartialEq)]
pub struct Function {
    pub rust_name: String,
    pub sql_name: String,
    pub params: Vec<Param>,
    pub returns: &'static str,
    pub setof: bool,
    pub strict: bool,
    pub volatility: &'static str,
    pub parallel: Option<&'static str>,
    pub leakproof: bool,
    pub security_definer: bool,
    pub cost: Option<u32>,
    pub rows: Option<u32>,
    pub doc: String,
    pub file: String,
    pub line: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Param {
    pub name: String,
    pub sql_type: &'static str,
    pub nullable: bool,
}

struct Parser<'t> {
    toks: &'t [Token],
    pos: usize,
    file: &'t str,
}

impl Parser<'_> {
    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.pos).map(|t| &t.tok)
    }

    fn peek_at(&self, k: usize) -> Option<&Tok> {
        self.toks.get(self.pos + k).map(|t| &t.tok)
    }

    fn line(&self) -> usize {
        self.toks
            .get(self.pos)
            .or_else(|| self.toks.last())
            .map_or(1, |t| t.line)
    }

    fn err(&self, message: impl Into<String>) -> ScanError {
        ScanError {
            file: self.file.to_owned(),
            line: self.line(),
            message: message.into(),
        }
    }

    fn bump(&mut self) -> Option<Tok> {
        let t = self.peek().cloned();
        self.pos += 1;
        t
    }

    fn is_punct(&self, c: char) -> bool {
        self.peek() == Some(&Tok::Punct(c))
    }

    fn is_ident(&self, word: &str) -> bool {
        matches!(self.peek(), Some(Tok::Ident(w)) if w == word)
    }

    /// Returns the tokens up to the bracket matching the one at `pos`.
    fn group(&mut self) -> Result<&[Token], ScanError> {
        let (open, close) = match self.peek() {
            Some(Tok::Punct('(')) => ('(', ')'),
            Some(Tok::Punct('[')) => ('[', ']'),
            Some(Tok::Punct('{')) => ('{', '}'),
            _ => return Err(self.err("expected a bracket")),
        };
        self.pos += 1;
        let start = self.pos;
        let mut depth = 1usize;
        while let Some(t) = self.bump() {
            if t == Tok::Punct(open) {
                depth += 1;
            } else if t == Tok::Punct(close) {
                depth -= 1;
                if depth == 0 {
                    return Ok(&self.toks[start..self.pos - 1]);
                }
            }
        }
        Err(self.err(format!("unclosed '{open}'")))
    }

    fn ty(&mut self) -> Result<Ty, ScanError> {
        if self.is_punct('&') {
            self.pos += 1;
            if self.peek() == Some(&Tok::Lifetime) {
                self.pos += 1;
            }
            if self.is_ident("mut") {
                return Err(self.err("a pg_function can't take &mut parameters"));
            }
            return Ok(Ty::Ref(Box::new(self.ty()?)));
        }
        if self.is_punct('(') {
            let inner = self.group()?;
            return Ok(if inner.is_empty() {
                Ty::Unit
            } else {
                Ty::Other
            });
        }
        if self.is_punct('[') {
            self.pos += 1;
            let elem = self.ty()?;
            if !self.is_punct(']') {
                return Ok(Ty::Other);
            }
            self.pos += 1;
            return Ok(Ty::Slice(Box::new(elem)));
        }
        let name = loop {
            let Some(Tok::Ident(w)) = self.bump() else {
                return Ok(Ty::Other);
            };
            if self.is_punct(':') && self.peek_at(1) == Some(&Tok::Punct(':')) {
                self.pos += 2;
            } else {
                break w;
            }
        };
        let mut args = Vec::new();
        if self.is_punct('<') {
            self.pos += 1;
            loop {
                if self.is_punct('>') {
                    self.pos += 1;
                    break;
                }
                if self.peek() == Some(&Tok::Lifetime) {
                    self.pos += 1;
                } else {
                    args.push(self.ty()?);
                }
                if self.is_punct(',') {
                    self.pos += 1;
                } else if !self.is_punct('>') {
                    return Err(self.err(format!("can't read the type arguments of {name}")));
                }
            }
        }
        Ok(Ty::Path(name, args))
    }
}

fn scalar(ty: &Ty) -> Option<&'static str> {
    match ty {
        Ty::Path(n, a) if a.is_empty() => match n.as_str() {
            "bool" => Some("boolean"),
            "i16" => Some("smallint"),
            "i32" => Some("integer"),
            "i64" => Some("bigint"),
            "f32" => Some("real"),
            "f64" => Some("double precision"),
            "String" => Some("text"),
            _ => None,
        },
        Ty::Path(n, a) if n == "Vec" && a.as_slice() == [Ty::Path("u8".into(), vec![])] => {
            Some("bytea")
        }
        Ty::Ref(inner) => match &**inner {
            Ty::Path(n, a) if n == "str" && a.is_empty() => Some("text"),
            Ty::Slice(e) if **e == Ty::Path("u8".into(), vec![]) => Some("bytea"),
            _ => None,
        },
        _ => None,
    }
}

fn nullable_scalar(ty: &Ty) -> Option<(&'static str, bool)> {
    match ty {
        Ty::Path(n, a) if n == "Option" && a.len() == 1 => scalar(&a[0]).map(|s| (s, true)),
        _ => scalar(ty).map(|s| (s, false)),
    }
}

const TYPES_HINT: &str =
    "supported: bool, i16, i32, i64, f32, f64, &str, String, &[u8], Vec<u8>, Option<T> of those";

/// Maps a return type to (SQL type, SETOF).
fn return_type(ty: &Ty) -> Result<(&'static str, bool), String> {
    match ty {
        Ty::Unit => Ok(("void", false)),
        Ty::Path(n, a) if n == "Result" && (1..=2).contains(&a.len()) => return_type(&a[0]),
        Ty::Path(n, a) if n == "SetOf" && a.len() == 1 => nullable_scalar(&a[0])
            .map(|(s, _)| (s, true))
            .ok_or_else(|| format!("unsupported SetOf row type; {TYPES_HINT}")),
        _ => nullable_scalar(ty).map(|(s, _)| (s, false)).ok_or_else(|| {
            format!("unsupported return type; {TYPES_HINT}, (), Result<T>, SetOf<T>")
        }),
    }
}

#[derive(Default)]
struct Options {
    name: Option<String>,
    volatility: Option<&'static str>,
    parallel: Option<&'static str>,
    leakproof: bool,
    security_definer: bool,
    cost: Option<u32>,
    rows: Option<u32>,
}

/// The macro permits only SQL identifier characters in the decoded name.
/// Hex, Unicode and line continuation are the Rust escapes that can produce one.
fn sql_name(mut body: &str) -> Option<String> {
    let mut name = String::new();
    while let Some((prefix, escape)) = body.split_once('\\') {
        name.push_str(prefix);
        if let Some(rest) = escape.strip_prefix('x') {
            let (hex, tail) = rest.split_at_checked(2)?;
            name.push(char::from(u8::from_str_radix(hex, 16).ok()?));
            body = tail;
        } else if let Some(rest) = escape.strip_prefix("u{") {
            let (hex, tail) = rest.split_once('}')?;
            let value = u32::from_str_radix(&hex.replace('_', ""), 16).ok()?;
            name.push(char::from_u32(value)?);
            body = tail;
        } else {
            let tail = escape
                .strip_prefix("\r\n")
                .or_else(|| escape.strip_prefix('\n'))?;
            body = tail.trim_start_matches([' ', '\t', '\r', '\n']);
        }
    }
    name.push_str(body);
    Some(name)
}

/// Read the numeric value independently of its Rust radix, separators and suffix.
/// The attribute macro validates the literal syntax before the extension is linked.
fn planner_hint(literal: &str) -> Option<u32> {
    let (radix, digits) = match literal.as_bytes() {
        [b'0', b'x', ..] => (16, &literal[2..]),
        [b'0', b'o', ..] => (8, &literal[2..]),
        [b'0', b'b', ..] => (2, &literal[2..]),
        _ => (10, literal),
    };
    let end = digits
        .find(|c: char| c != '_' && !c.is_digit(radix))
        .unwrap_or(digits.len());
    let (digits, suffix) = digits.split_at(end);
    // An exponent is part of a decimal float, even without a decimal point.
    let exponent = radix == 10
        && suffix.strip_prefix(['e', 'E']).is_some_and(|s| {
            s.trim_start_matches('_')
                .starts_with(|c: char| c.is_ascii_digit())
        });
    if exponent || (!suffix.is_empty() && !suffix.starts_with(char::is_alphabetic)) {
        return None;
    }
    u32::from_str_radix(&digits.replace('_', ""), radix).ok()
}

fn options(p: &Parser<'_>, toks: &[Token]) -> Result<Options, ScanError> {
    let mut o = Options::default();
    for part in toks.split(|t| t.tok == Tok::Punct(',')) {
        let words: Vec<&Tok> = part.iter().map(|t| &t.tok).collect();
        match words.as_slice() {
            [] => {}
            [Tok::Ident(w)] => match w.as_str() {
                "immutable" => o.volatility = Some("IMMUTABLE"),
                "stable" => o.volatility = Some("STABLE"),
                "volatile" => o.volatility = Some("VOLATILE"),
                "parallel_safe" => o.parallel = Some("SAFE"),
                "parallel_restricted" => o.parallel = Some("RESTRICTED"),
                "parallel_unsafe" => o.parallel = Some("UNSAFE"),
                "leakproof" => o.leakproof = true,
                "security_definer" => o.security_definer = true,
                other => return Err(p.err(format!("unknown pg_function option '{other}'"))),
            },
            [Tok::Ident(k), Tok::Punct('='), Tok::Str { body, raw }] if k == "name" => {
                o.name = Some(if *raw {
                    body.clone()
                } else {
                    sql_name(body).ok_or_else(|| p.err("invalid escape in SQL function name"))?
                });
            }
            [Tok::Ident(k), Tok::Punct('='), Tok::Lit(v)] => {
                let hint = match k.as_str() {
                    "cost" => &mut o.cost,
                    "rows" => &mut o.rows,
                    other => return Err(p.err(format!("unknown pg_function option '{other}'"))),
                };
                *hint =
                    Some(planner_hint(v).ok_or_else(|| {
                        p.err(format!("{k} must be an integer in the u32 range"))
                    })?);
            }
            _ => return Err(p.err("can't read the pg_function options")),
        }
    }
    Ok(o)
}

fn signature(p: &mut Parser<'_>, opts: Options, doc: String) -> Result<Function, ScanError> {
    let line = p.line();
    loop {
        if p.is_ident("pub") {
            p.pos += 1;
            if p.is_punct('(') {
                p.group()?;
            }
        } else if p.is_ident("const") {
            p.pos += 1;
        } else if p.is_ident("fn") {
            p.pos += 1;
            break;
        } else {
            return Err(p.err("#[pg_function] must be on a plain fn"));
        }
    }
    let rust_name = match p.bump() {
        Some(Tok::Ident(n)) => n,
        _ => return Err(p.err("expected the function name")),
    };
    if p.is_punct('<') {
        let mut depth = 0usize;
        while let Some(t) = p.bump() {
            match t {
                Tok::Punct('<') => depth += 1,
                Tok::Punct('>') => {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
                _ => {}
            }
        }
    }
    let file = p.file;
    let param_toks = p.group()?.to_vec();
    let mut params = Vec::new();
    let mut depth = 0i32;
    let mut start = 0;
    let mut chunks = Vec::new();
    for (i, t) in param_toks.iter().enumerate() {
        match t.tok {
            Tok::Punct('(' | '[' | '<') => depth += 1,
            Tok::Punct(')' | ']' | '>') => depth -= 1,
            Tok::Punct(',') if depth == 0 => {
                chunks.push(&param_toks[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    if start < param_toks.len() {
        chunks.push(&param_toks[start..]);
    }
    for (index, chunk) in chunks.into_iter().enumerate() {
        let colon = (0..chunk.len()).find(|&k| {
            chunk[k].tok == Tok::Punct(':')
                && chunk.get(k + 1).map(|t| &t.tok) != Some(&Tok::Punct(':'))
                && (k == 0 || chunk[k - 1].tok != Tok::Punct(':'))
        });
        let Some(colon) = colon else {
            return Err(ScanError {
                file: file.to_owned(),
                line: chunk.first().map_or(line, |t| t.line),
                message: "a pg_function can't take self".into(),
            });
        };
        let mut name = match &chunk[..colon] {
            [
                Token {
                    tok: Tok::Ident(n), ..
                },
            ]
            | [
                Token {
                    tok: Tok::Ident(_), ..
                },
                Token {
                    tok: Tok::Ident(n), ..
                },
            ] if n != "_" => n.trim_start_matches('_').to_owned(),
            _ => String::new(),
        };
        // PostgreSQL truncates identifiers to 63 bytes at a character boundary.
        // Compare the names callers will actually see, including prior fallbacks.
        name.truncate(name.floor_char_boundary(63));
        let mut position = index + 1;
        while name.is_empty() || params.iter().any(|p: &Param| p.name == name) {
            name = format!("arg{position}");
            position += 1;
        }
        let mut sub = Parser {
            toks: &chunk[colon + 1..],
            pos: 0,
            file,
        };
        let ty = sub.ty()?;
        let (sql_type, nullable) = nullable_scalar(&ty).ok_or_else(|| ScanError {
            file: file.to_owned(),
            line: chunk[colon].line,
            message: format!("parameter '{name}' has an unsupported type; {TYPES_HINT}"),
        })?;
        params.push(Param {
            name,
            sql_type,
            nullable,
        });
    }
    let (returns, setof) = if p.is_punct('-') && p.peek_at(1) == Some(&Tok::Punct('>')) {
        p.pos += 2;
        let ty = p.ty()?;
        return_type(&ty).map_err(|m| p.err(m))?
    } else {
        ("void", false)
    };
    if opts.rows.is_some() && !setof {
        return Err(p.err("rows = N only applies to a function returning SetOf<T>"));
    }
    Ok(Function {
        sql_name: opts.name.unwrap_or_else(|| rust_name.clone()),
        rust_name,
        strict: params.iter().all(|p| !p.nullable),
        params,
        returns,
        setof,
        volatility: opts.volatility.unwrap_or("VOLATILE"),
        parallel: opts.parallel,
        leakproof: opts.leakproof,
        security_definer: opts.security_definer,
        cost: opts.cost,
        rows: opts.rows,
        doc,
        file: file.to_owned(),
        line,
    })
}

/// Scans one source file. `file` is the path reported in errors and in the
/// generated SQL's comments.
///
/// # Errors
///
/// A lexing error, a malformed `#[pg_function]` or an unsupported type.
pub fn scan(file: &str, src: &str) -> Result<Vec<Function>, ScanError> {
    let toks = lex(src).map_err(|(line, message)| ScanError {
        file: file.to_owned(),
        line,
        message,
    })?;
    let mut p = Parser {
        toks: &toks,
        pos: 0,
        file,
    };
    let mut found = Vec::new();
    let mut docs: Vec<String> = Vec::new();
    let mut pending: Option<Options> = None;
    while let Some(tok) = p.peek().cloned() {
        match tok {
            Tok::Doc(d) => {
                docs.push(d);
                p.pos += 1;
            }
            Tok::Punct('#') if p.peek_at(1) == Some(&Tok::Punct('[')) => {
                p.pos += 1;
                let attr = p.group()?.to_vec();
                let path_end = attr
                    .iter()
                    .position(|t| t.tok == Tok::Punct('('))
                    .unwrap_or(attr.len());
                let is_pg = matches!(attr[..path_end].last(), Some(Token { tok: Tok::Ident(w), .. }) if w == "pg_function");
                if is_pg {
                    let args = if path_end < attr.len() {
                        let end = attr.len().saturating_sub(1).max(path_end + 1);
                        &attr[path_end + 1..end]
                    } else {
                        &[]
                    };
                    pending = Some(options(&p, args)?);
                }
            }
            _ => {
                if let Some(opts) = pending.take() {
                    let doc = docs.join("\n").trim().to_owned();
                    found.push(signature(&mut p, opts, doc)?);
                } else {
                    p.pos += 1;
                }
                docs.clear();
            }
        }
    }
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one(src: &str) -> Function {
        let mut fs = scan("lib.rs", src).unwrap();
        assert_eq!(fs.len(), 1, "{fs:?}");
        fs.remove(0)
    }

    fn fails(src: &str) -> String {
        scan("lib.rs", src).unwrap_err().to_string()
    }

    #[test]
    fn maps_scalars_docs_and_options() {
        let f = one(r#"
            use sutegi_pgext::pg_function;
            /// Greets someone.
            ///
            /// Politely.
            #[pg_function(immutable, parallel_safe, name = "greet", cost = 5)]
            pub fn hello<'a>(name: &'a str, times: i32) -> String { todo!() }
            "#);
        assert_eq!(f.sql_name, "greet");
        assert_eq!(f.rust_name, "hello");
        assert_eq!(f.doc, "Greets someone.\n\nPolitely.");
        assert_eq!(f.returns, "text");
        assert!(f.strict && !f.setof);
        assert_eq!(f.volatility, "IMMUTABLE");
        assert_eq!(f.parallel, Some("SAFE"));
        assert_eq!(f.cost, Some(5));
        assert_eq!(f.line, 7);
        assert_eq!(
            f.params,
            vec![
                Param {
                    name: "name".into(),
                    sql_type: "text",
                    nullable: false
                },
                Param {
                    name: "times".into(),
                    sql_type: "integer",
                    nullable: false
                },
            ]
        );
    }

    #[test]
    fn option_params_make_the_function_non_strict() {
        let f =
            one("#[pg_function] fn f(a: Option<i64>, _b: &[u8], mut c: Vec<u8>) -> Option<f64> {}");
        assert!(!f.strict);
        assert_eq!(f.returns, "double precision");
        let names: Vec<_> = f
            .params
            .iter()
            .map(|p| (p.name.as_str(), p.sql_type))
            .collect();
        assert_eq!(names, [("a", "bigint"), ("b", "bytea"), ("c", "bytea")]);
    }

    #[test]
    fn results_sets_and_void() {
        assert_eq!(
            one("#[pg_function] fn f() -> Result<i16> {}").returns,
            "smallint"
        );
        assert_eq!(
            one("#[pg_function] fn f() -> sutegi_pgext::Result<(), Error> {}").returns,
            "void"
        );
        assert_eq!(one("#[pg_function] fn f() {}").returns, "void");
        let s = one("#[pg_function(rows = 10)] fn f(n: i64) -> SetOf<Option<String>> {}");
        assert!(s.setof);
        assert_eq!((s.returns, s.rows), ("text", Some(10)));
    }

    #[test]
    fn path_qualified_attribute_and_other_attributes() {
        let fs = scan(
            "lib.rs",
            "#[inline]\n#[sutegi_pgext::pg_function(stable)]\n#[allow(dead_code)]\npub(crate) fn g(x: bool) -> bool { x }\n#[test] fn not_me() {}",
        )
        .unwrap();
        assert_eq!(fs.len(), 1);
        assert_eq!(fs[0].volatility, "STABLE");
    }

    #[test]
    fn ignores_lookalikes_in_comments_and_strings() {
        let src = r##"
            // #[pg_function] fn a() {}
            /* #[pg_function] fn b() {} /* nested */ */
            const S: &str = "#[pg_function] fn c() {}";
            const R: &str = r#"#[pg_function] fn d() {}"#;
            const C: char = '#';
            fn e<'a>(x: &'a str) -> &'a str { x }
        "##;
        assert!(scan("lib.rs", src).unwrap().is_empty());
    }

    #[test]
    fn rejects_unsupported_types_with_a_location() {
        let e = fails("\n#[pg_function]\nfn f(x: u32) {}");
        assert!(e.starts_with("lib.rs:3:"), "{e}");
        assert!(e.contains("parameter 'x'"));
        assert!(fails("#[pg_function] fn f() -> u8 {}").contains("unsupported return type"));
        assert!(fails("#[pg_function] fn f() -> SetOf<Vec<i32>> {}").contains("SetOf row"));
        assert!(fails("#[pg_function] fn f(x: &mut String) {}").contains("&mut"));
        assert!(fails("#[pg_function(rows = 3)] fn f() -> i32 {}").contains("rows = N"));
        assert!(fails("#[pg_function(bogus)] fn f() {}").contains("unknown pg_function option"));
        assert!(fails("#[pg_function] struct S;").contains("plain fn"));
    }

    #[test]
    fn raw_identifiers_and_escaped_quotes() {
        let f = one("const Q: char = '\\''; #[pg_function] fn r#type(r#match: &str) -> i32 { 0 }");
        assert_eq!(
            (f.rust_name.as_str(), f.params[0].name.as_str()),
            ("type", "match")
        );
        assert_eq!(
            one("const B: u8 = b'\\\\'; #[pg_function] fn g() {}").rust_name,
            "g"
        );
    }

    #[test]
    fn duplicate_parameter_names_get_positional_ones() {
        let f = one("#[pg_function] fn f(_a: i32, a: i32) {}");
        let names: Vec<_> = f.params.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["a", "arg2"]);
    }

    #[test]
    fn generated_parameter_names_skip_occupied_positions() {
        let f = one("#[pg_function] fn f(arg4: i32, arg5: i32, arg6: i32, _arg4: i32, _: i32) {}");
        let names: Vec<_> = f.params.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["arg4", "arg5", "arg6", "arg7", "arg8"]);
    }

    #[test]
    fn parameter_names_stay_distinct_after_postgres_truncation() {
        let prefix = "a".repeat(62);
        let f = one(&format!(
            "#[pg_function] fn f({prefix}é: i32, {prefix}ê: i32) {{}}"
        ));
        assert_eq!(f.params[0].name, prefix);
        assert_eq!(f.params[1].name, "arg2");
    }

    #[test]
    fn escaped_sql_names_match_rust_strings() {
        let f = one(r#"#[pg_function(name = "line\x5f\u{74}o\
                           tal")] fn f() {}"#);
        assert_eq!(f.sql_name, "line_total");
        assert_eq!(
            one(r##"#[pg_function(name = r#"raw_name"#)] fn f() {}"##).sql_name,
            "raw_name"
        );
    }

    #[test]
    fn planner_hints_match_rust_integers() {
        let f = one("#[pg_function(rows = 1_000, cost = 0x1_f4u32)] fn f() -> SetOf<i32> {}");
        assert_eq!((f.rows, f.cost), (Some(1000), Some(500)));
        let f = one("#[pg_function(rows = 0b1_01, cost = 0o12)] fn f() -> SetOf<i32> {}");
        assert_eq!((f.rows, f.cost), (Some(5), Some(10)));
        for invalid in ["4_294_967_296", "1.5", "1e3"] {
            assert!(
                fails(&format!("#[pg_function(cost = {invalid})] fn f() {{}}")).contains("cost")
            );
        }
    }

    #[test]
    fn foreign_error_types_map_like_result() {
        assert_eq!(
            one("#[pg_function] fn f(s: &str) -> Result<i32, std::num::ParseIntError> {}").returns,
            "integer"
        );
    }

    /// Mutates a representative source with a tiny deterministic PRNG and
    /// checks the scanner never panics, whatever it's fed.
    #[test]
    fn mutated_sources_never_panic() {
        let base = r##"
            //! crate docs
            use sutegi_pgext::{pg_function, SetOf, Result};
            /// Doc "quoted" 'x' /* not a comment */
            #[pg_function(immutable, parallel_safe, name = "x_y", cost = 3, rows = 9)]
            pub(crate) fn f<'a>(r#in: &'a str, b: Option<Vec<u8>>, c: &[u8]) -> Result<SetOf<Option<String>>> {
                let s = r#"raw "str" "#; let c = '\''; let b = b"bytes\"";
                /* nested /* block */ */ todo!()
            }
        "##;
        let bytes: Vec<char> = base.chars().collect();
        let alphabet: Vec<char> = "#[](){}<>&'\"/*!:;,=-r_ab\n\\".chars().collect();
        let mut seed: u64 = 0x9e37_79b9_7f4a_7c15;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for _ in 0..20_000 {
            let mut src = bytes.clone();
            for _ in 0..(next() % 6 + 1) {
                let at = usize::try_from(next() % (src.len() as u64 + 1)).unwrap_or(0);
                let pick = alphabet[usize::try_from(next() % alphabet.len() as u64).unwrap_or(0)];
                match next() % 3 {
                    0 if at < src.len() => {
                        src.remove(at);
                    }
                    1 if at < src.len() => src[at] = pick,
                    _ => src.insert(at, pick),
                }
            }
            let text: String = src.into_iter().collect();
            let _ = scan("fuzz.rs", &text);
        }
    }

    #[test]
    fn unterminated_input_is_an_error_not_a_panic() {
        assert!(fails("fn f() { \"abc").contains("unterminated string"));
        assert!(fails("/* never closed").contains("unterminated block comment"));
        assert!(fails("#[pg_function] fn f(").contains("unclosed"));
    }
}
