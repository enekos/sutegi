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
        "new" => {
            let name = args.iter().skip(1).find(|a| !a.starts_with("--"));
            if args.iter().any(|a| a == "--fullstack") {
                cmd_new_fullstack(name)
            } else {
                cmd_new(name)
            }
        }
        "dev" => cmd_dev(),
        "schema:zu" => cmd_schema_zu(args.get(1)).map(|_| ()),
        "make:model" => cmd_make_model(args.get(1)),
        "make:route" => cmd_make_route(args.get(1)),
        "introspect" => cmd_introspect(args.get(1)),
        "repl" => cmd_repl(args.get(1)),
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
    new <name> --fullstack
                          Scaffold a fullstack app: server/ (sutegi) +
                          app/*.zu (zumar frontend) + www/ (static root)
    dev                   Run a fullstack project: the server plus a zuc
                          watch loop (gc backend, ms rebuilds, live reload).
                          Needs `zuc` on PATH (or $ZUC); run from the root.
    schema:zu [url]       Generate app/schema.zu record declarations from a
                          running app's /__introspect models — the frontend
                          compiles against the DB schema (a renamed column
                          becomes a .zu compile error). `sutegi dev` runs
                          this automatically once the server is up.
    make:model <Name>     Generate a model under src/models/
    make:route <name>     Generate a route module under src/routes/
    introspect [url]      Fetch and pretty-print a running app's /__introspect
                          (default url: http://127.0.0.1:8080/__introspect)
    repl [addr]           Interactive REPL against a running app's HTTP surface
                          (default addr: 127.0.0.1:8080)
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

// ---- new --fullstack --------------------------------------------------------
//
// The Phoenix-shaped layout: one repo, one mental model. `server/` is a
// plain sutegi app that owns the API and serves `www/` as the site root;
// `app/*.zu` is the zumar frontend, compiled by `sutegi dev` (via zuc, gc
// backend) into `www/pkg/`. The framework JS in www/ is a zuc artifact,
// refreshed on every `sutegi dev` start — only index.html is yours there.

fn cmd_new_fullstack(name: Option<&String>) -> Result<(), String> {
    let name = name.ok_or("usage: sutegi new <name> --fullstack")?;
    let root = Path::new(name);
    if root.exists() {
        return Err(format!("'{}' already exists", name));
    }
    let dep = sutegi_dep(std::env::var("SUTEGI_HOME").ok().as_deref());
    write_file(
        &root.join("server/Cargo.toml"),
        &fullstack_cargo_toml(name, &dep),
    )?;
    write_file(&root.join("server/src/main.rs"), &fullstack_main_rs(name))?;
    write_file(&root.join("app/main.zu"), &fullstack_main_zu(name))?;
    write_file(&root.join("www/index.html"), &fullstack_index_html(name))?;
    write_file(&root.join(".gitignore"), FULLSTACK_GITIGNORE)?;
    println!("created fullstack sutegi app '{}'", name);
    println!("  {name}/server/   the sutegi app (API + serves www/)");
    println!("  {name}/app/      the zumar frontend (.zu)");
    println!("  {name}/www/      static root (index.html is yours; the rest is zuc's)");
    println!("next: cd {name} && sutegi dev    (needs zuc: cargo install --path <zumar>/crates/zumar-cli)");
    Ok(())
}

/// The scaffold's sutegi dependency: a path dep against a local checkout when
/// `SUTEGI_HOME` names one (fast, offline, pre-release features), git otherwise.
fn sutegi_dep(home: Option<&str>) -> String {
    match home {
        Some(p) => format!(
            "sutegi = {{ path = \"{}/crates/sutegi\" }}",
            p.trim_end_matches('/')
        ),
        None => "sutegi = { git = \"https://github.com/enekos/sutegi\" }".to_string(),
    }
}

fn fullstack_cargo_toml(name: &str, dep: &str) -> String {
    format!(
        r#"[package]
name = "{name}-server"
version = "0.1.0"
edition = "2021"

[dependencies]
{dep}
"#
    )
}

fn fullstack_main_rs(name: &str) -> String {
    format!(
        r#"use sutegi::prelude::*;

fn main() -> std::io::Result<()> {{
    // The .zu frontend (../app) is compiled by `sutegi dev` into ../www/pkg;
    // this server owns the API and serves www/ as the site root. Register
    // API routes before `static_dir` — routes match in order.
    App::new("{name}")
        .get("/api/hello", "The greeting the frontend fetches.", |_| {{
            text(200, "aupa from the sutegi server")
        }})
        .static_dir("/", concat!(env!("CARGO_MANIFEST_DIR"), "/../www"))
        .serve()
}}
"#
    )
}

fn fullstack_main_zu(name: &str) -> String {
    let app = to_pascal(name);
    format!(
        r#"# main.zu — the frontend. `sutegi dev` compiles this (zuc, gc backend)
# into www/pkg/app.wasm on every save; the page reloads itself.

app {app}

model {{ count: Int, greeting: String }}

init = {{ count = 0, greeting = "" }}

msg Inc | Dec | Fetch | Got String

update Inc = {{ count = model.count + 1 }}
update Dec = {{ count = model.count - 1 }}
update Fetch = {{ greeting = "..." }} then httpGet("/api/hello", Got)
update Got s = {{ greeting = s }}

view =
  div [class "app"] [
    h1 [] [ text "{name}" ],
    p [class "sub"] [ text "counter in the browser, greeting from the server" ],
    div [class "row"] [
      button [onClick Dec] [ text "-" ],
      span [class "count"] [ text show(model.count) ],
      button [onClick Inc] [ text "+" ]
    ],
    div [class "row"] [
      button [class "fetch", onClick Fetch] [ text "fetch /api/hello" ],
      span [class "greeting"] [ text model.greeting ]
    ]
  ]
"#
    )
}

fn fullstack_index_html(name: &str) -> String {
    format!(
        r#"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8" />
  <meta name="viewport" content="width=device-width, initial-scale=1" />
  <title>{name}</title>
  <style>
    :root {{ color-scheme: dark; }}
    body {{ margin: 0; min-height: 100vh; display: grid; place-items: center;
           background: #16130f; color: #e8ddcf;
           font-family: ui-monospace, "SF Mono", Menlo, monospace; }}
    .app {{ text-align: center; }}
    .app h1 {{ letter-spacing: 0.2em; color: #d9a76a; }}
    .sub {{ color: #6f7362; font-size: 0.8rem; }}
    .row {{ display: flex; align-items: center; justify-content: center; gap: 1.2rem; margin: 1.6rem 0; }}
    .count {{ font-size: 3rem; min-width: 4ch; font-variant-numeric: tabular-nums; }}
    .greeting {{ color: #d9a76a; min-height: 1em; }}
    button {{ background: #1e2318; color: #e8ddcf; border: 1px solid #3a4230;
             border-radius: 8px; font: inherit; font-size: 1.4rem;
             width: 3rem; height: 3rem; cursor: pointer; }}
    button:hover {{ border-color: #d9a76a; }}
    button.fetch {{ width: auto; height: auto; font-size: 0.9rem; padding: 0.4rem 1rem; }}
  </style>
</head>
<body>
  <div id="app"></div>
  <script type="module" src="/boot.js"></script>
  <script>
    // sutegi dev live reload: poll the build stamp zuc writes on rebuild.
    // Gated to localhost so a deployed www/ never polls.
    if (["localhost", "127.0.0.1"].includes(location.hostname)) (async () => {{
      let last = null;
      for (;;) {{
        try {{
          const r = await fetch("/pkg/build-id", {{ cache: "no-store" }});
          if (r.ok) {{
            const b = await r.text();
            if (last !== null && b !== last) location.reload();
            last = b;
          }}
        }} catch {{}}
        await new Promise((res) => setTimeout(res, 400));
      }}
    }})();
  </script>
</body>
</html>
"#
    )
}

const FULLSTACK_GITIGNORE: &str = "\
/server/target
/www/pkg/
/www/boot.js
/www/zumar.js
/www/zumar-wire.js
/www/zumar-gc.js
/www/zumar-live.js
";

// ---- schema:zu (the P2 typed data bridge) -----------------------------------
//
// The DB schema flows into the frontend's type system: every registered
// model's columns become a `.zu` record declaration, compiled into every
// page via zuc's `--with`. Rename a column and the next frontend build is
// a caret error at the exact line that still uses the old name.

/// Fetch `/__introspect` and write `app/schema.zu`. Returns true if the
/// file's content changed (callers use this to trigger a rebuild).
fn cmd_schema_zu(url: Option<&String>) -> Result<bool, String> {
    let port = std::env::var("PORT").unwrap_or_else(|_| "8080".to_string());
    let default = format!("http://127.0.0.1:{port}/__introspect");
    let url = url.map(String::as_str).unwrap_or(&default);
    let body = http_get(url).map_err(|e| format!("{url}: {e}"))?;
    let json = Json::parse(&body).map_err(|e| format!("{url}: not JSON: {e}"))?;
    let generated = schema_zu(&json);
    let path = Path::new("app/schema.zu");
    let changed = std::fs::read_to_string(path).ok().as_deref() != Some(generated.as_str());
    if changed {
        write_file(path, &generated)?;
    }
    let n = generated.matches("\nrecord ").count();
    println!(
        "app/schema.zu: {n} record(s) from {url}{}",
        if changed { "" } else { " (unchanged)" }
    );
    Ok(changed)
}

/// Render introspection JSON (`{ models: [{ table, columns: [...] }] }`)
/// as a `.zu` declarations fragment. Column types with no .zu equivalent
/// (real/json/vector) are skipped with a note; nullable columns map to
/// `Maybe T`.
fn schema_zu(introspect: &Json) -> String {
    let mut out = String::from(
        "# generated by `sutegi schema:zu` from the server's /__introspect —\n\
         # do not edit; it is rewritten when `sutegi dev` starts.\n",
    );
    let models = introspect
        .get("models")
        .and_then(Json::as_array)
        .cloned()
        .unwrap_or_default();
    for model in &models {
        let Some(table) = model.get("table").and_then(Json::as_str) else {
            continue;
        };
        let name = to_pascal(&singularize(table));
        let mut fields = Vec::new();
        let mut skipped = Vec::new();
        for col in model
            .get("columns")
            .and_then(Json::as_array)
            .cloned()
            .unwrap_or_default()
        {
            let (Some(cname), Some(cty)) = (
                col.get("name").and_then(Json::as_str).map(str::to_string),
                col.get("type").and_then(Json::as_str).map(str::to_string),
            ) else {
                continue;
            };
            let zu = match cty.as_str() {
                "integer" => "Int",
                "text" => "String",
                "boolean" => "Bool",
                other => {
                    skipped.push(format!("{cname} ({other})"));
                    continue;
                }
            };
            let nullable = col.get("nullable").and_then(Json::as_bool).unwrap_or(false);
            let ty = if nullable {
                format!("Maybe {zu}")
            } else {
                zu.to_string()
            };
            fields.push(format!("{cname}: {ty}"));
        }
        out.push('\n');
        if !skipped.is_empty() {
            out.push_str(&format!(
                "# {name}: skipped {} — no .zu type\n",
                skipped.join(", ")
            ));
        }
        out.push_str(&format!("record {name} {{ {} }}\n", fields.join(", ")));
    }
    out
}

/// Naive inverse of [`pluralize`] — enough for table→record naming.
fn singularize(table: &str) -> String {
    if let Some(stem) = table.strip_suffix("ies") {
        format!("{stem}y")
    } else if table.ends_with("ss") {
        table.to_string()
    } else {
        table.strip_suffix('s').unwrap_or(table).to_string()
    }
}

// ---- dev (fullstack) --------------------------------------------------------
//
// `sutegi dev` = the server (cargo run, inherited stdio) + a zuc watch loop.
// zuc's standalone dev server retires here: sutegi serves everything, zuc
// only compiles. Live reload is a stamp file under www/pkg/ the scaffolded
// index.html polls — no server cooperation needed beyond static serving.

fn cmd_dev() -> Result<(), String> {
    if !Path::new("server/Cargo.toml").exists() || !Path::new("app").is_dir() {
        return Err(
            "not a fullstack project (expected server/Cargo.toml and app/) — \
             scaffold one with `sutegi new <name> --fullstack`"
                .into(),
        );
    }
    let zuc = std::env::var("ZUC").unwrap_or_else(|_| "zuc".to_string());

    // Refresh the framework JS so the shim always matches the installed zuc.
    zuc_run(&zuc, &["assets", "www"])?;

    let mut stamp: u64 = 1;
    for f in zu_files()? {
        match build_zu(&zuc, &f) {
            Ok(secs) => println!(
                "sutegi dev: built {} in {:.0}ms",
                f.display(),
                secs * 1000.0
            ),
            Err(e) => eprintln!("{e}\nsutegi dev: fix {} and save again", f.display()),
        }
    }
    write_stamp(stamp)?;

    let mut server = std::process::Command::new("cargo")
        .args(["run", "--manifest-path", "server/Cargo.toml"])
        .spawn()
        .map_err(|e| format!("cargo run: {e}"))?;
    println!("sutegi dev: watching app/*.zu (gc backend) — save and the page reloads");

    let index = Path::new("www/index.html").to_path_buf();
    let mut seen: std::collections::BTreeMap<std::path::PathBuf, Option<std::time::SystemTime>> =
        zu_files()?
            .into_iter()
            .map(|f| (f.clone(), mtime(&f)))
            .collect();
    seen.insert(index.clone(), mtime(&index));
    let schema = Path::new("app/schema.zu").to_path_buf();
    seen.insert(schema.clone(), mtime(&schema));
    // Once the server answers, pull its /__introspect into app/schema.zu —
    // from then on the frontend compiles against the live DB schema.
    let mut schema_synced = false;

    loop {
        std::thread::sleep(std::time::Duration::from_millis(300));
        if let Some(status) = server.try_wait().map_err(|e| e.to_string())? {
            return Err(format!(
                "server exited ({status}) — sutegi dev stops with it"
            ));
        }
        let mut reload = false;
        let mut rebuild_all = false;

        if !schema_synced {
            if let Ok(changed) = cmd_schema_zu(None) {
                schema_synced = true;
                rebuild_all = changed;
                seen.insert(schema.clone(), mtime(&schema));
            }
        }
        // Manual `sutegi schema:zu` runs (or hand edits) take effect live too.
        let m = mtime(&schema);
        if seen.get(&schema) != Some(&m) {
            seen.insert(schema.clone(), m);
            println!("sutegi dev: schema.zu changed — rebuilding all pages");
            rebuild_all = true;
        }

        for f in zu_files()? {
            let m = mtime(&f);
            if !rebuild_all && seen.get(&f) == Some(&m) {
                continue;
            }
            seen.insert(f.clone(), m);
            match build_zu(&zuc, &f) {
                Ok(secs) => {
                    println!(
                        "sutegi dev: rebuilt {} in {:.0}ms — reloading",
                        f.display(),
                        secs * 1000.0
                    );
                    reload = true;
                }
                Err(e) => eprintln!("{e}\nsutegi dev: still serving the last good build"),
            }
        }
        let m = mtime(&index);
        if seen.get(&index) != Some(&m) {
            seen.insert(index.clone(), m);
            println!("sutegi dev: index.html changed — reloading");
            reload = true;
        }
        if reload {
            stamp += 1;
            write_stamp(stamp)?;
        }
    }
}

/// The page sources: every `app/*.zu` except `schema.zu`, which is a shared
/// declarations fragment injected into each page's build via `--with`.
fn zu_files() -> Result<Vec<std::path::PathBuf>, String> {
    let mut files: Vec<_> = std::fs::read_dir("app")
        .map_err(|e| format!("app/: {e}"))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.extension().is_some_and(|x| x == "zu")
                && p.file_name() != Some(std::ffi::OsStr::new("schema.zu"))
        })
        .collect();
    files.sort();
    Ok(files)
}

fn mtime(p: &Path) -> Option<std::time::SystemTime> {
    std::fs::metadata(p).and_then(|m| m.modified()).ok()
}

/// `main.zu` becomes `pkg/app.wasm` (what boot.js loads); any other `.zu`
/// keeps its stem, ready for multi-page setups.
fn wasm_out(stem: &str) -> String {
    if stem == "main" {
        "www/pkg/app.wasm".to_string()
    } else {
        format!("www/pkg/{stem}.wasm")
    }
}

fn build_zu(zuc: &str, file: &Path) -> Result<f64, String> {
    let stem = file.file_stem().and_then(|s| s.to_str()).unwrap_or("app");
    let out = wasm_out(stem);
    std::fs::create_dir_all("www/pkg").map_err(|e| format!("www/pkg: {e}"))?;
    let started = std::time::Instant::now();
    let file = file.to_str().ok_or("non-utf8 path")?;
    let mut args = vec!["build", file, "--backend", "gc", "--out", &out];
    if Path::new("app/schema.zu").exists() {
        args.extend(["--with", "app/schema.zu"]);
    }
    zuc_run(zuc, &args)?;
    Ok(started.elapsed().as_secs_f64())
}

fn zuc_run(zuc: &str, args: &[&str]) -> Result<(), String> {
    let output = std::process::Command::new(zuc)
        .args(args)
        .output()
        .map_err(|e| {
            format!(
                "{zuc}: {e}\nsutegi dev needs the zumar compiler — \
             cargo install --path <zumar>/crates/zumar-cli (or set ZUC=<path>)"
            )
        })?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr)
            .trim_end()
            .to_string());
    }
    Ok(())
}

fn write_stamp(stamp: u64) -> Result<(), String> {
    std::fs::create_dir_all("www/pkg").map_err(|e| format!("www/pkg: {e}"))?;
    std::fs::write("www/pkg/build-id", stamp.to_string())
        .map_err(|e| format!("www/pkg/build-id: {e}"))
}

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

// ---- repl -------------------------------------------------------------------

/// Attach the tinker REPL to a running app — remote mode drives the app's
/// public HTTP surface (introspection, routes, tools), no source access needed.
fn cmd_repl(addr: Option<&String>) -> Result<(), String> {
    let default = "127.0.0.1:8080".to_string();
    let addr = addr.unwrap_or(&default);
    sutegi_repl::Repl::connect(addr)
        .run()
        .map_err(|e| e.to_string())
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

    #[test]
    fn sutegi_dep_prefers_a_local_checkout() {
        assert!(sutegi_dep(None).contains("git = \"https://github.com/enekos/sutegi\""));
        assert_eq!(
            sutegi_dep(Some("/home/e/sutegi/")),
            "sutegi = { path = \"/home/e/sutegi/crates/sutegi\" }"
        );
    }

    #[test]
    fn fullstack_templates_wire_the_three_tiers_together() {
        let toml = fullstack_cargo_toml("shop", &sutegi_dep(None));
        assert!(toml.contains(r#"name = "shop-server""#));

        // server: API before static_dir, www served manifest-relative
        let main = fullstack_main_rs("shop");
        assert!(main.contains(r#".get("/api/hello""#));
        let api_pos = main.find("/api/hello").unwrap();
        let static_pos = main.find(".static_dir").unwrap();
        assert!(
            api_pos < static_pos,
            "API routes must register before static_dir"
        );
        assert!(main.contains(r#"concat!(env!("CARGO_MANIFEST_DIR"), "/../www")"#));

        // frontend: pascal app name, fetches the same endpoint the server serves
        let zu = fullstack_main_zu("shop");
        assert!(zu.contains("app Shop"));
        assert!(zu.contains(r#"httpGet("/api/hello", Got)"#));

        // page: boots zuc's loader and polls the reload stamp sutegi dev bumps
        let html = fullstack_index_html("shop");
        assert!(html.contains(r#"src="/boot.js""#));
        assert!(html.contains("/pkg/build-id"));
        assert!(html.contains("location.hostname"));
    }

    #[test]
    fn build_output_paths_follow_the_stem_rule() {
        // main.zu is the page boot.js loads; other stems keep their names.
        assert_eq!(wasm_out("main"), "www/pkg/app.wasm");
        assert_eq!(wasm_out("admin"), "www/pkg/admin.wasm");
    }

    #[test]
    fn singularize_inverts_table_naming() {
        assert_eq!(singularize("todos"), "todo");
        assert_eq!(singularize("categories"), "category");
        assert_eq!(singularize("progress"), "progress"); // ss stays
    }

    #[test]
    fn schema_zu_renders_records_from_introspect_json() {
        let j = Json::parse(
            r#"{"models":[{"table":"todos","columns":[
                {"name":"id","type":"integer","nullable":false,"primary":true},
                {"name":"title","type":"text","nullable":false,"primary":false},
                {"name":"done","type":"boolean","nullable":false,"primary":false},
                {"name":"note","type":"text","nullable":true,"primary":false},
                {"name":"embedding","type":"vector","nullable":false,"primary":false}
            ]}]}"#,
        )
        .unwrap();
        let out = schema_zu(&j);
        assert!(
            out.contains("record Todo { id: Int, title: String, done: Bool, note: Maybe String }"),
            "{out}"
        );
        assert!(out.contains("# Todo: skipped embedding (vector)"), "{out}");
        // no models → header only, no record lines
        let empty = schema_zu(&Json::parse(r#"{"models":[]}"#).unwrap());
        assert!(!empty.contains("\nrecord "));
    }
}
