//! The `sutegi` CLI — scaffold and introspect sutegi apps.
//!
//! Scaffolding follows rigid, predictable conventions on purpose: a model is
//! always `crates or src/models/<snake>.rs` with the same struct/impl shape, a
//! route file always exposes a `register(app: App) -> App`. That predictability
//! is the "heuristic" payload — an LLM can generate or extend a sutegi app
//! correctly with almost no context, because there is exactly one right shape.

use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::process::ExitCode;

use sutegi_json::Json;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = args.first().map(String::as_str).unwrap_or("help");

    let result = match cmd {
        "new" => cmd_new(args.get(1)),
        "make:model" => cmd_make_model(args.get(1)),
        "make:route" => cmd_make_route(args.get(1)),
        "introspect" => cmd_introspect(args.get(1)),
        "version" | "--version" | "-V" => {
            println!("sutegi {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        "help" | "--help" | "-h" => {
            print_help();
            Ok(())
        }
        other => {
            eprintln!("unknown command '{}'\n", other);
            print_help();
            return ExitCode::FAILURE;
        }
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {}", e);
            ExitCode::FAILURE
        }
    }
}

fn print_help() {
    println!(
        r#"sutegi {ver} — the forge

USAGE:
    sutegi <command> [args]

COMMANDS:
    new <name>            Scaffold a new sutegi application
    make:model <Name>     Generate a model under src/models/
    make:route <name>     Generate a route module under src/routes/
    introspect [url]      Fetch and pretty-print a running app's /__introspect
                          (default url: http://127.0.0.1:8080/__introspect)
    version               Print the version
    help                  Show this help
"#,
        ver = env!("CARGO_PKG_VERSION")
    );
}

// ---- new ------------------------------------------------------------------

fn cmd_new(name: Option<&String>) -> Result<(), String> {
    let name = name.ok_or("usage: sutegi new <name>")?;
    let root = Path::new(name);
    if root.exists() {
        return Err(format!("'{}' already exists", name));
    }
    write_file(&root.join("Cargo.toml"), &new_cargo_toml(name))?;
    write_file(&root.join("src/main.rs"), NEW_MAIN_RS)?;
    write_file(&root.join("src/models/.keep"), "")?;
    write_file(&root.join("src/routes/.keep"), "")?;
    write_file(&root.join(".gitignore"), "/target\n")?;
    println!("created sutegi app '{}'", name);
    println!("  cd {} && cargo run", name);
    Ok(())
}

fn new_cargo_toml(name: &str) -> String {
    format!(
        r#"[package]
name = "{name}"
version = "0.1.0"
edition = "2021"

[dependencies]
sutegi = {{ git = "https://github.com/enekos/sutegi" }}
"#
    )
}

const NEW_MAIN_RS: &str = r#"use sutegi::prelude::*;

fn main() -> std::io::Result<()> {
    // Handlers take one `Ctx` and return anything `IntoResponse`.
    // `serve()` reads HOST/PORT/WORKERS (or argv[1]) and drains on SIGTERM.
    App::new("my-app")
        .get("/", "Health check", |_| "sutegi up")
        .serve()
}
"#;

// ---- make:model -----------------------------------------------------------

fn cmd_make_model(name: Option<&String>) -> Result<(), String> {
    let name = name.ok_or("usage: sutegi make:model <Name>")?;
    let pascal = to_pascal(name);
    let snake = to_snake(&pascal);
    let table = pluralize(&snake);
    let path = Path::new("src/models").join(format!("{}.rs", snake));
    if path.exists() {
        return Err(format!("{} already exists", path.display()));
    }
    write_file(&path, &model_template(&pascal, &table))?;
    println!("created model {} -> {}", pascal, path.display());
    Ok(())
}

fn model_template(pascal: &str, table: &str) -> String {
    format!(
        r#"use sutegi::prelude::*;

/// The `{table}` table. `#[derive(Model)]` generates the schema, `FromRow`
/// hydration, `save()`, `to_json()`, and `from_input()`. Add `#[derive(Validate)]`
/// with `#[validate(...)]` field attributes for `Ctx::validated::<{pascal}>()`.
#[derive(Model)]
#[model(table = "{table}")]
pub struct {pascal} {{
    #[model(primary)]
    pub id: i64,
    // sutegi convention: add fields below, then `.register_model({pascal}::schema())`
    // in main() so the model shows up in /__introspect.
}}
"#
    )
}

// ---- make:route -----------------------------------------------------------

fn cmd_make_route(name: Option<&String>) -> Result<(), String> {
    let name = name.ok_or("usage: sutegi make:route <name>")?;
    let snake = to_snake(&to_pascal(name));
    let path = Path::new("src/routes").join(format!("{}.rs", snake));
    if path.exists() {
        return Err(format!("{} already exists", path.display()));
    }
    write_file(&path, &route_template(&snake))?;
    println!("created route module {} -> {}", snake, path.display());
    Ok(())
}

fn route_template(snake: &str) -> String {
    format!(
        r#"use sutegi::prelude::*;

/// sutegi convention: every route module exposes `register`, called from main()
/// as `let app = {snake}::register(app);`. Keeps wiring uniform and agent-legible.
pub fn register(app: App) -> App {{
    app.get("/{snake}", "Describe what /{snake} does", |_c| "{snake} ok")
}}
"#
    )
}

// ---- introspect (tiny HTTP client) ----------------------------------------

fn cmd_introspect(url: Option<&String>) -> Result<(), String> {
    let default = "http://127.0.0.1:8080/__introspect".to_string();
    let url = url.unwrap_or(&default);
    let body = http_get(url).map_err(|e| format!("request failed: {}", e))?;
    match Json::parse(&body) {
        Ok(j) => {
            println!("{}", j.to_pretty());
            Ok(())
        }
        // If it isn't JSON, show whatever came back so the user can debug.
        Err(_) => {
            println!("{}", body);
            Ok(())
        }
    }
}

/// A bare-minimum HTTP/1.1 GET client (std only), enough to read an
/// introspection endpoint.
fn http_get(url: &str) -> io::Result<String> {
    let rest = url.strip_prefix("http://").unwrap_or(url);
    let (authority, path) = match rest.split_once('/') {
        Some((a, p)) => (a, format!("/{}", p)),
        None => (rest, "/".to_string()),
    };
    let (host, port) = match authority.split_once(':') {
        Some((h, p)) => (h, p.parse().unwrap_or(80u16)),
        None => (authority, 80u16),
    };

    let mut stream = TcpStream::connect((host, port))?;
    let req = format!(
        "GET {} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\nAccept: application/json\r\n\r\n",
        path, host
    );
    stream.write_all(req.as_bytes())?;
    let mut raw = String::new();
    stream.read_to_string(&mut raw)?;

    Ok(match raw.split_once("\r\n\r\n") {
        Some((_headers, body)) => body.to_string(),
        None => raw,
    })
}

// ---- naming helpers (the convention engine) -------------------------------

fn to_pascal(s: &str) -> String {
    s.split(['_', '-', ' '])
        .filter(|w| !w.is_empty())
        .map(|w| {
            let mut chars = w.chars();
            match chars.next() {
                Some(first) => {
                    first.to_uppercase().collect::<String>() + &chars.as_str().to_lowercase()
                }
                None => String::new(),
            }
        })
        .collect()
}

fn to_snake(pascal: &str) -> String {
    let mut out = String::new();
    for (i, c) in pascal.chars().enumerate() {
        if c.is_uppercase() {
            if i != 0 {
                out.push('_');
            }
            out.extend(c.to_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

/// Naive English pluralization — enough for table-name conventions.
fn pluralize(word: &str) -> String {
    if word.ends_with('s') {
        word.to_string()
    } else if let Some(stem) = word.strip_suffix('y') {
        format!("{}ies", stem)
    } else {
        format!("{}s", word)
    }
}

fn write_file(path: &Path, contents: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("{}: {}", parent.display(), e))?;
    }
    std::fs::write(path, contents).map_err(|e| format!("{}: {}", path.display(), e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pascal_case_from_varied_separators() {
        assert_eq!(to_pascal("blog_post"), "BlogPost");
        assert_eq!(to_pascal("blog-post"), "BlogPost");
        assert_eq!(to_pascal("blog post"), "BlogPost");
        assert_eq!(to_pascal("USER"), "User"); // mixed case is normalized
        assert_eq!(to_pascal("user"), "User");
    }

    #[test]
    fn snake_case_from_pascal() {
        assert_eq!(to_snake("BlogPost"), "blog_post");
        assert_eq!(to_snake("User"), "user");
        assert_eq!(to_snake("HTTPServer"), "h_t_t_p_server"); // naive, but predictable
    }

    #[test]
    fn pluralize_follows_simple_english_rules() {
        assert_eq!(pluralize("user"), "users");
        assert_eq!(pluralize("category"), "categories"); // y → ies
        assert_eq!(pluralize("posts"), "posts"); // already plural, unchanged
    }

    #[test]
    fn make_model_name_pipeline_is_consistent() {
        // The convention chain a user relies on: Name → Pascal → snake → table.
        let pascal = to_pascal("blog_post");
        let snake = to_snake(&pascal);
        let table = pluralize(&snake);
        assert_eq!(
            (pascal.as_str(), snake.as_str(), table.as_str()),
            ("BlogPost", "blog_post", "blog_posts")
        );
    }

    #[test]
    fn model_template_wires_struct_and_table() {
        let tpl = model_template("Category", "categories");
        assert!(tpl.contains("#[derive(Model)]"));
        assert!(tpl.contains("pub struct Category"));
        assert!(tpl.contains(r#"#[model(table = "categories")]"#));
        assert!(tpl.contains("use sutegi::prelude::*;"));
    }

    #[test]
    fn route_template_exposes_register_with_snake_path() {
        let tpl = route_template("health_check");
        assert!(tpl.contains("pub fn register(app: App) -> App"));
        assert!(tpl.contains("/health_check"));
    }

    #[test]
    fn new_cargo_toml_names_package_and_depends_on_sutegi() {
        let toml = new_cargo_toml("my_app");
        assert!(toml.contains(r#"name = "my_app""#));
        assert!(toml.contains("sutegi = {"));
        assert!(toml.contains(r#"edition = "2021""#));
    }
}
