//! A small **Blade-style template engine** over [`sutegi_json::Json`]
//! contexts. Compile once, render many times.
//!
//! ```text
//! {{ user.name }}          HTML-escaped interpolation (dot paths)
//! {!! body_html !!}        raw interpolation (trusted content only)
//! @if(user.admin) … @else … @endif      truthy test; @if(!x) negates
//! @foreach(items as item) … @endforeach with {{ loop.index }} /
//!                                       {{ loop.first }} / {{ loop.last }}
//! @include(partial-name)   render another registered template here
//! @@ and @{{               escape to a literal `@` / `{{`
//! ```
//!
//! Missing values render as empty (Blade-forgiving); truthiness is JSON-ish:
//! `null`, `false`, `0`, `""`, and `[]` are false, everything else true.
//!
//! ```
//! use sutegi_template::Templates;
//! use sutegi_json::Json;
//!
//! let mut t = Templates::new();
//! t.add("hello", "Hi {{ name }}!").unwrap();
//! let out = t.render("hello", &Json::obj(vec![("name", Json::str("Eneko"))])).unwrap();
//! assert_eq!(out, "Hi Eneko!");
//! ```

use std::collections::BTreeMap;
use sutegi_json::Json;

const MAX_INCLUDE_DEPTH: usize = 32;

/// A compiled template. Compile with [`Template::compile`]; render with
/// [`Template::render`] (no `@include`) or through a [`Templates`] registry.
pub struct Template {
    nodes: Vec<Node>,
}

enum Node {
    Text(String),
    Interp {
        path: Vec<String>,
        raw: bool,
    },
    If {
        cond: Cond,
        then: Vec<Node>,
        els: Vec<Node>,
    },
    Foreach {
        path: Vec<String>,
        var: String,
        body: Vec<Node>,
    },
    Include(String),
}

struct Cond {
    path: Vec<String>,
    negated: bool,
}

impl Template {
    /// Compile a source string; errors carry a line number.
    pub fn compile(src: &str) -> Result<Template, String> {
        let tokens = lex(src)?;
        let mut pos = 0;
        let nodes = parse(&tokens, &mut pos, None, src)?;
        Ok(Template { nodes })
    }

    /// Render against a context. `@include` needs a registry — use
    /// [`Templates::render`] for that.
    pub fn render(&self, ctx: &Json) -> Result<String, String> {
        let mut out = String::new();
        render_nodes(&self.nodes, ctx, &mut Vec::new(), None, 0, &mut out)?;
        Ok(out)
    }
}

/// A named-template registry: partials and layouts resolve through it.
#[derive(Default)]
pub struct Templates {
    map: BTreeMap<String, Template>,
}

impl Templates {
    pub fn new() -> Templates {
        Templates::default()
    }

    /// Compile and register `src` under `name` (replacing any previous one).
    pub fn add(&mut self, name: &str, src: &str) -> Result<(), String> {
        let t = Template::compile(src).map_err(|e| format!("template {name}: {e}"))?;
        self.map.insert(name.to_string(), t);
        Ok(())
    }

    pub fn contains(&self, name: &str) -> bool {
        self.map.contains_key(name)
    }

    /// Render a registered template; `@include` resolves against this
    /// registry.
    pub fn render(&self, name: &str, ctx: &Json) -> Result<String, String> {
        let t = self
            .map
            .get(name)
            .ok_or_else(|| format!("unknown template: {name}"))?;
        let mut out = String::new();
        render_nodes(&t.nodes, ctx, &mut Vec::new(), Some(self), 0, &mut out)?;
        Ok(out)
    }
}

/// HTML-escape `&`, `<`, `>`, `"`, `'`.
pub fn html_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Lexer
// ---------------------------------------------------------------------------

enum Tok {
    Text(String),
    Interp { expr: String, raw: bool },
    If(String),
    Else,
    EndIf,
    Foreach(String),
    EndForeach,
    Include(String),
}

fn line_of(src: &str, offset: usize) -> usize {
    src[..offset.min(src.len())].matches('\n').count() + 1
}

fn lex(src: &str) -> Result<Vec<(Tok, usize)>, String> {
    let bytes = src.as_bytes();
    let mut toks = Vec::new();
    let mut text = String::new();
    let mut i = 0;

    let flush = |text: &mut String, toks: &mut Vec<(Tok, usize)>, at: usize| {
        if !text.is_empty() {
            toks.push((Tok::Text(std::mem::take(text)), at));
        }
    };

    while i < bytes.len() {
        let rest = &src[i..];
        if rest.starts_with("@@") {
            text.push('@');
            i += 2;
        } else if rest.starts_with("@{{") {
            text.push_str("{{");
            i += 3;
        } else if rest.starts_with("{!!") {
            let end = rest
                .find("!!}")
                .ok_or_else(|| format!("line {}: unclosed {{!! … !!}}", line_of(src, i)))?;
            flush(&mut text, &mut toks, i);
            toks.push((
                Tok::Interp {
                    expr: rest[3..end].trim().to_string(),
                    raw: true,
                },
                i,
            ));
            i += end + 3;
        } else if rest.starts_with("{{") {
            let end = rest
                .find("}}")
                .ok_or_else(|| format!("line {}: unclosed {{{{ … }}}}", line_of(src, i)))?;
            flush(&mut text, &mut toks, i);
            toks.push((
                Tok::Interp {
                    expr: rest[2..end].trim().to_string(),
                    raw: false,
                },
                i,
            ));
            i += end + 2;
        } else if let Some(tok_len) = directive(rest) {
            flush(&mut text, &mut toks, i);
            let (tok, len) = tok_len;
            let tok = tok.map_err(|e| format!("line {}: {e}", line_of(src, i)))?;
            toks.push((tok, i));
            i += len;
        } else {
            // Advance one char (not byte — keep UTF-8 intact).
            let c = rest.chars().next().unwrap();
            text.push(c);
            i += c.len_utf8();
        }
    }
    flush(&mut text, &mut toks, i);
    Ok(toks)
}

/// Try to lex a `@directive` at the start of `rest`.
#[allow(clippy::type_complexity)]
fn directive(rest: &str) -> Option<(Result<Tok, String>, usize)> {
    if !rest.starts_with('@') {
        return None;
    }
    let arg_of = |kw: &str| -> Option<(Result<String, String>, usize)> {
        let after = rest.strip_prefix(kw)?;
        match after.find(')') {
            Some(close) => Some((Ok(after[..close].trim().to_string()), kw.len() + close + 1)),
            None => Some((Err(format!("unclosed {kw}…)")), kw.len())),
        }
    };
    let word_ends = |kw: &str| {
        rest.strip_prefix(kw)
            .is_some_and(|a| !a.starts_with(|c: char| c.is_alphanumeric() || c == '_'))
    };

    if let Some((arg, len)) = arg_of("@if(") {
        return Some((arg.map(Tok::If), len));
    }
    if let Some((arg, len)) = arg_of("@foreach(") {
        return Some((arg.map(Tok::Foreach), len));
    }
    if let Some((arg, len)) = arg_of("@include(") {
        return Some((
            arg.map(|a| Tok::Include(a.trim_matches(['"', '\'']).to_string())),
            len,
        ));
    }
    if word_ends("@endforeach") {
        return Some((Ok(Tok::EndForeach), "@endforeach".len()));
    }
    if word_ends("@endif") {
        return Some((Ok(Tok::EndIf), "@endif".len()));
    }
    if word_ends("@else") {
        return Some((Ok(Tok::Else), "@else".len()));
    }
    None // a literal '@'; caller pushes it as text
}

// ---------------------------------------------------------------------------
// Parser
// ---------------------------------------------------------------------------

enum Stop {
    Else,
    EndIf,
    EndForeach,
}

fn parse(
    toks: &[(Tok, usize)],
    pos: &mut usize,
    until: Option<&[Stop]>,
    src: &str,
) -> Result<Vec<Node>, String> {
    let mut nodes = Vec::new();
    while *pos < toks.len() {
        let (tok, at) = &toks[*pos];
        let line = line_of(src, *at);
        match tok {
            Tok::Else | Tok::EndIf | Tok::EndForeach => {
                let matched = until.is_some_and(|stops| {
                    stops.iter().any(|s| {
                        matches!(
                            (s, tok),
                            (Stop::Else, Tok::Else)
                                | (Stop::EndIf, Tok::EndIf)
                                | (Stop::EndForeach, Tok::EndForeach)
                        )
                    })
                });
                if matched {
                    return Ok(nodes); // caller consumes the stop token
                }
                let what = match tok {
                    Tok::Else => "@else",
                    Tok::EndIf => "@endif",
                    _ => "@endforeach",
                };
                return Err(format!("line {line}: unexpected {what}"));
            }
            Tok::Text(t) => {
                nodes.push(Node::Text(t.clone()));
                *pos += 1;
            }
            Tok::Interp { expr, raw } => {
                if expr.is_empty() {
                    return Err(format!("line {line}: empty interpolation"));
                }
                nodes.push(Node::Interp {
                    path: parse_path(expr),
                    raw: *raw,
                });
                *pos += 1;
            }
            Tok::Include(name) => {
                if name.is_empty() {
                    return Err(format!("line {line}: @include needs a template name"));
                }
                nodes.push(Node::Include(name.clone()));
                *pos += 1;
            }
            Tok::If(cond) => {
                let (negated, path) = match cond.strip_prefix('!') {
                    Some(rest) => (true, rest.trim()),
                    None => (false, cond.as_str()),
                };
                if path.is_empty() {
                    return Err(format!("line {line}: @if needs a value path"));
                }
                let cond = Cond {
                    path: parse_path(path),
                    negated,
                };
                *pos += 1;
                let then = parse(toks, pos, Some(&[Stop::Else, Stop::EndIf]), src)?;
                let mut els = Vec::new();
                match toks.get(*pos).map(|(t, _)| t) {
                    Some(Tok::Else) => {
                        *pos += 1;
                        els = parse(toks, pos, Some(&[Stop::EndIf]), src)?;
                        match toks.get(*pos).map(|(t, _)| t) {
                            Some(Tok::EndIf) => *pos += 1,
                            _ => return Err(format!("line {line}: @if without @endif")),
                        }
                    }
                    Some(Tok::EndIf) => *pos += 1,
                    _ => return Err(format!("line {line}: @if without @endif")),
                }
                nodes.push(Node::If { cond, then, els });
            }
            Tok::Foreach(header) => {
                let (path, var) = header
                    .split_once(" as ")
                    .map(|(p, v)| (p.trim(), v.trim()))
                    .ok_or_else(|| format!("line {line}: @foreach expects `items as item`"))?;
                if path.is_empty() || var.is_empty() {
                    return Err(format!("line {line}: @foreach expects `items as item`"));
                }
                *pos += 1;
                let body = parse(toks, pos, Some(&[Stop::EndForeach]), src)?;
                match toks.get(*pos).map(|(t, _)| t) {
                    Some(Tok::EndForeach) => *pos += 1,
                    _ => return Err(format!("line {line}: @foreach without @endforeach")),
                }
                nodes.push(Node::Foreach {
                    path: parse_path(path),
                    var: var.to_string(),
                    body,
                });
            }
        }
    }
    if until.is_some() {
        return Err("unexpected end of template inside a block".to_string());
    }
    Ok(nodes)
}

fn parse_path(expr: &str) -> Vec<String> {
    expr.split('.').map(|s| s.trim().to_string()).collect()
}

// ---------------------------------------------------------------------------
// Renderer
// ---------------------------------------------------------------------------

fn render_nodes(
    nodes: &[Node],
    root: &Json,
    scopes: &mut Vec<(String, Json)>,
    registry: Option<&Templates>,
    depth: usize,
    out: &mut String,
) -> Result<(), String> {
    for node in nodes {
        match node {
            Node::Text(t) => out.push_str(t),
            Node::Interp { path, raw } => {
                let s = display(&lookup(path, root, scopes));
                if *raw {
                    out.push_str(&s);
                } else {
                    out.push_str(&html_escape(&s));
                }
            }
            Node::If { cond, then, els } => {
                let mut truthy = is_truthy(&lookup(&cond.path, root, scopes));
                if cond.negated {
                    truthy = !truthy;
                }
                let branch = if truthy { then } else { els };
                render_nodes(branch, root, scopes, registry, depth, out)?;
            }
            Node::Foreach { path, var, body } => {
                let value = lookup(path, root, scopes);
                let items = match value {
                    Json::Arr(items) => items,
                    Json::Null => Vec::new(),
                    other => {
                        return Err(format!(
                            "@foreach over non-array value ({})",
                            kind_of(&other)
                        ))
                    }
                };
                let len = items.len();
                for (idx, item) in items.into_iter().enumerate() {
                    scopes.push((var.clone(), item));
                    scopes.push((
                        "loop".to_string(),
                        Json::obj(vec![
                            ("index", Json::int(idx as i64)),
                            ("first", Json::Bool(idx == 0)),
                            ("last", Json::Bool(idx + 1 == len)),
                        ]),
                    ));
                    let r = render_nodes(body, root, scopes, registry, depth, out);
                    scopes.pop();
                    scopes.pop();
                    r?;
                }
            }
            Node::Include(name) => {
                let registry = registry
                    .ok_or_else(|| format!("@include({name}) needs a Templates registry"))?;
                if depth >= MAX_INCLUDE_DEPTH {
                    return Err(format!("@include({name}): include depth limit reached"));
                }
                let t = registry
                    .map
                    .get(name)
                    .ok_or_else(|| format!("@include: unknown template {name}"))?;
                render_nodes(&t.nodes, root, scopes, Some(registry), depth + 1, out)?;
            }
        }
    }
    Ok(())
}

/// Resolve a dot path: the first segment tries loop/scope variables
/// (innermost first), then the root context.
fn lookup(path: &[String], root: &Json, scopes: &[(String, Json)]) -> Json {
    let first = match path.first() {
        Some(f) => f,
        None => return Json::Null,
    };
    let mut current = scopes
        .iter()
        .rev()
        .find(|(name, _)| name == first)
        .map(|(_, v)| v.clone())
        .or_else(|| root.get(first).cloned())
        .unwrap_or(Json::Null);
    for seg in &path[1..] {
        current = current.get(seg).cloned().unwrap_or(Json::Null);
    }
    current
}

fn display(v: &Json) -> String {
    match v {
        Json::Null => String::new(),
        Json::Str(s) => s.clone(),
        other => other.to_string(),
    }
}

fn is_truthy(v: &Json) -> bool {
    match v {
        Json::Null => false,
        Json::Bool(b) => *b,
        Json::Num(n) => *n != 0.0,
        Json::Str(s) => !s.is_empty(),
        Json::Arr(a) => !a.is_empty(),
        Json::Obj(_) => true,
    }
}

fn kind_of(v: &Json) -> &'static str {
    match v {
        Json::Null => "null",
        Json::Bool(_) => "bool",
        Json::Num(_) => "number",
        Json::Str(_) => "string",
        Json::Arr(_) => "array",
        Json::Obj(_) => "object",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(pairs: Vec<(&str, Json)>) -> Json {
        Json::obj(pairs)
    }

    fn render(src: &str, c: &Json) -> String {
        Template::compile(src).unwrap().render(c).unwrap()
    }

    #[test]
    fn interpolation_escapes_by_default() {
        let c = ctx(vec![("name", Json::str("<b>Eneko & co</b>"))]);
        assert_eq!(
            render("Hi {{ name }}!", &c),
            "Hi &lt;b&gt;Eneko &amp; co&lt;/b&gt;!"
        );
        assert_eq!(render("Hi {!! name !!}!", &c), "Hi <b>Eneko & co</b>!");
    }

    #[test]
    fn dot_paths_and_missing_values() {
        let c = ctx(vec![(
            "user",
            Json::obj(vec![("name", Json::str("Vera")), ("age", Json::int(30))]),
        )]);
        assert_eq!(
            render("{{ user.name }} is {{ user.age }}", &c),
            "Vera is 30"
        );
        assert_eq!(render("[{{ user.missing.deep }}]", &c), "[]");
        assert_eq!(render("[{{ ghost }}]", &c), "[]");
    }

    #[test]
    fn if_else_and_negation() {
        let t = "@if(admin)yes@else no@endif";
        assert_eq!(render(t, &ctx(vec![("admin", Json::Bool(true))])), "yes");
        assert_eq!(render(t, &ctx(vec![("admin", Json::Bool(false))])), " no");
        assert_eq!(render(t, &ctx(vec![])), " no");
        assert_eq!(
            render("@if(!name)anon@endif", &ctx(vec![("name", Json::str(""))])),
            "anon"
        );
    }

    #[test]
    fn truthiness_rules() {
        for (v, expect) in [
            (Json::int(0), "f"),
            (Json::int(7), "t"),
            (Json::str(""), "f"),
            (Json::str("x"), "t"),
            (Json::arr(vec![]), "f"),
            (Json::arr(vec![Json::int(1)]), "t"),
            (Json::Null, "f"),
        ] {
            assert_eq!(
                render("@if(v)t@else f@endif", &ctx(vec![("v", v)])).trim(),
                expect
            );
        }
    }

    #[test]
    fn foreach_with_loop_meta() {
        let c = ctx(vec![(
            "items",
            Json::arr(vec![Json::str("a"), Json::str("b"), Json::str("c")]),
        )]);
        assert_eq!(
            render(
                "@foreach(items as it){{ loop.index }}:{{ it }}@if(!loop.last), @endif@endforeach",
                &c
            ),
            "0:a, 1:b, 2:c"
        );
        // Nested loops restore outer scope.
        let c2 = ctx(vec![(
            "rows",
            Json::arr(vec![Json::arr(vec![Json::int(1), Json::int(2)])]),
        )]);
        assert_eq!(
            render(
                "@foreach(rows as r)@foreach(r as n){{ n }}@endforeach|{{ loop.first }}@endforeach",
                &c2
            ),
            "12|true"
        );
        // Missing collection renders nothing; scalars error.
        assert_eq!(render("@foreach(nope as x)x@endforeach", &ctx(vec![])), "");
        assert!(Template::compile("@foreach(v as x)x@endforeach")
            .unwrap()
            .render(&ctx(vec![("v", Json::int(3))]))
            .is_err());
    }

    #[test]
    fn includes_resolve_through_registry() {
        let mut ts = Templates::new();
        ts.add("button", "<a href=\"{{ url }}\">{{ label }}</a>")
            .unwrap();
        ts.add("page", "before @include(button) after").unwrap();
        let out = ts
            .render(
                "page",
                &ctx(vec![
                    ("url", Json::str("https://x.test/?a=1&b=2")),
                    ("label", Json::str("Go")),
                ]),
            )
            .unwrap();
        assert_eq!(
            out,
            "before <a href=\"https://x.test/?a=1&amp;b=2\">Go</a> after"
        );

        // Includes without a registry, and unknown names, error clearly.
        assert!(Template::compile("@include(x)")
            .unwrap()
            .render(&ctx(vec![]))
            .is_err());
        assert!(ts.render("nope", &ctx(vec![])).is_err());
    }

    #[test]
    fn include_cycles_are_capped() {
        let mut ts = Templates::new();
        ts.add("a", "@include(b)").unwrap();
        ts.add("b", "@include(a)").unwrap();
        let err = ts.render("a", &ctx(vec![])).unwrap_err();
        assert!(err.contains("depth limit"));
    }

    #[test]
    fn escapes_for_literals() {
        assert_eq!(render("a@@if b", &ctx(vec![])), "a@if b");
        assert_eq!(render("show @{{ name }}", &ctx(vec![])), "show {{ name }}");
        assert_eq!(
            render("email @example.com", &ctx(vec![])),
            "email @example.com"
        );
    }

    #[test]
    fn compile_errors_carry_lines() {
        for (src, needle) in [
            ("a\nb {{ oops", "line 2"),
            ("{!! x", "unclosed"),
            ("@if(x) no end", "end of template"),
            ("@endif", "unexpected @endif"),
            ("x @foreach(items) y @endforeach", "items as item"),
            ("@if() y @endif", "needs a value path"),
            ("{{ }}", "empty interpolation"),
        ] {
            let err = Template::compile(src)
                .err()
                .or_else(|| {
                    Template::compile(src)
                        .unwrap()
                        .render(&Json::obj(vec![]))
                        .err()
                })
                .unwrap_or_default();
            assert!(err.contains(needle), "{src:?} → {err:?}");
        }
    }

    #[test]
    fn utf8_text_survives() {
        let c = ctx(vec![("who", Json::str("mundu"))]);
        assert_eq!(
            render("Kaixo {{ who }} — ¡güenas! 🚀", &c),
            "Kaixo mundu — ¡güenas! 🚀"
        );
    }
}
