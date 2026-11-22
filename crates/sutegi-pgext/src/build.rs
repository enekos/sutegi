//! The extension build pipeline behind `sutegi pgext` (feature `build`).
//!
//! 1. `cargo metadata` locates the crate (it must be a `staticlib`).
//! 2. [`scan`] reads every `#[pg_function]` signature under the lib's source
//!    directory and maps it to SQL.
//! 3. `cargo rustc` builds the static library and reports the native
//!    libraries Rust's std needs at link time.
//! 4. The work dir (`<target>/sutegi-pgext/<name>/`) receives the C runtime,
//!    the generated `functions.c`, an export list, the control file, the
//!    install script (generated `CREATE FUNCTION`s, then the crate's
//!    `sql/*.sql` in name order) and a PGXS Makefile.
//! 5. `make` (and `make install`) builds and installs the module with the
//!    server's own PGXS, so compiler flags and the module suffix always match
//!    the server.

mod codegen;
mod scan;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use sutegi_json::Json;

pub use scan::{Function, Param, ScanError, scan};

/// The C runtime compiled into every extension.
pub const RUNTIME_C: &str = include_str!("../csrc/sutegi_pgext.c");
/// The C runtime's ABI header.
pub const RUNTIME_H: &str = include_str!("../csrc/sutegi_pgext.h");

const MAX_SOURCE_FILES: usize = 10_000;

/// What to build, and how.
#[derive(Debug, Clone)]
pub struct Options {
    /// The extension crate's directory (holding its `Cargo.toml`).
    pub dir: PathBuf,
    /// The `pg_config` of the target server.
    pub pg_config: String,
    /// Build the Rust code with the release profile.
    pub release: bool,
    /// Run `make install` after building.
    pub install: bool,
}

/// A built extension.
#[derive(Debug, Clone)]
pub struct Built {
    pub name: String,
    pub version: String,
    /// Where the module, control file and SQL script were generated.
    pub work_dir: PathBuf,
    pub functions: Vec<Function>,
}

struct Package {
    name: String,
    version: String,
    description: String,
    lib_name: String,
    src_dir: PathBuf,
    target_dir: PathBuf,
}

/// The cargo running us (when invoked through `cargo run`), else the one on
/// PATH.
fn cargo() -> Command {
    Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()))
}

fn run_capture(cmd: &mut Command) -> Result<String, String> {
    let out = cmd
        .stderr(Stdio::inherit())
        .output()
        .map_err(|e| format!("can't run {:?}: {e}", cmd.get_program()))?;
    if !out.status.success() {
        return Err(format!("{:?} failed ({})", cmd.get_program(), out.status));
    }
    String::from_utf8(out.stdout)
        .map_err(|_| format!("{:?} printed invalid UTF-8", cmd.get_program()))
}

fn run_inherit(cmd: &mut Command) -> Result<(), String> {
    let status = cmd
        .status()
        .map_err(|e| format!("can't run {:?}: {e}", cmd.get_program()))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!(
            "{:?} {:?} failed ({status})",
            cmd.get_program(),
            cmd.get_args().collect::<Vec<_>>()
        ))
    }
}

fn str_field<'j>(j: &'j Json, key: &str) -> Option<&'j str> {
    j.get(key).and_then(Json::as_str)
}

fn has_kind(target: &Json, kind: &str) -> bool {
    ["kind", "crate_types"].iter().any(|k| {
        target
            .get(k)
            .and_then(Json::as_array)
            .is_some_and(|a| a.iter().any(|v| v.as_str() == Some(kind)))
    })
}

fn package(manifest: &Path) -> Result<Package, String> {
    let out = run_capture(
        Command::new("cargo")
            .args([
                "metadata",
                "--no-deps",
                "--format-version",
                "1",
                "--manifest-path",
            ])
            .arg(manifest),
    )?;
    let meta = Json::parse(&out).map_err(|e| format!("can't read cargo metadata: {e}"))?;
    let pkgs = meta
        .get("packages")
        .and_then(Json::as_array)
        .ok_or("cargo metadata has no packages")?;
    let pkg = pkgs
        .iter()
        .find(|p| str_field(p, "manifest_path").map(Path::new) == Some(manifest))
        .ok_or_else(|| format!("no package with manifest {}", manifest.display()))?;
    let lib = pkg
        .get("targets")
        .and_then(Json::as_array)
        .and_then(|ts| {
            ts.iter().find(|t| {
                ["lib", "rlib", "staticlib", "cdylib", "dylib"]
                    .iter()
                    .any(|k| has_kind(t, k))
            })
        })
        .ok_or("the crate has no library target: add src/lib.rs")?;
    let src_path = str_field(lib, "src_path").ok_or("the lib target has no src_path")?;
    Ok(Package {
        name: str_field(pkg, "name")
            .ok_or("package without a name")?
            .to_owned(),
        version: str_field(pkg, "version")
            .ok_or("package without a version")?
            .to_owned(),
        description: str_field(pkg, "description").unwrap_or_default().to_owned(),
        lib_name: str_field(lib, "name")
            .ok_or("lib target without a name")?
            .to_owned(),
        src_dir: Path::new(src_path)
            .parent()
            .ok_or("bad lib src_path")?
            .to_path_buf(),
        target_dir: PathBuf::from(
            str_field(&meta, "target_directory").ok_or("cargo metadata has no target_directory")?,
        ),
    })
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), String> {
    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .map_err(|e| format!("{}: {e}", dir.display()))?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .collect();
    entries.sort();
    for path in entries {
        if out.len() > MAX_SOURCE_FILES {
            return Err(format!("more than {MAX_SOURCE_FILES} source files"));
        }
        if path.is_dir() {
            rust_files(&path, out)?;
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
    Ok(())
}

/// Scans every `.rs` file under `src_dir`; paths are reported relative to
/// `root`.
///
/// # Errors
///
/// An unreadable file, a scan error, no `#[pg_function]` at all, or two
/// functions with the same Rust name (their C symbols would collide).
pub fn scan_dir(root: &Path, src_dir: &Path) -> Result<Vec<Function>, String> {
    let mut files = Vec::new();
    rust_files(src_dir, &mut files)?;
    let mut functions: Vec<Function> = Vec::new();
    for path in files {
        let src = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        let shown = path
            .strip_prefix(root)
            .unwrap_or(&path)
            .display()
            .to_string();
        for f in scan(&shown, &src).map_err(|e| e.to_string())? {
            if let Some(prev) = functions.iter().find(|g| g.rust_name == f.rust_name) {
                return Err(format!(
                    "{}:{}: a pg_function named {} already exists at {}:{}",
                    f.file, f.line, f.rust_name, prev.file, prev.line
                ));
            }
            let types = |g: &Function| g.params.iter().map(|p| p.sql_type).collect::<Vec<_>>();
            if let Some(prev) = functions
                .iter()
                .find(|g| g.sql_name == f.sql_name && types(g) == types(&f))
            {
                return Err(format!(
                    "{}:{}: SQL function {}({}) is already defined by {} at {}:{}",
                    f.file,
                    f.line,
                    f.sql_name,
                    types(&f).join(", "),
                    prev.rust_name,
                    prev.file,
                    prev.line
                ));
            }
            functions.push(f);
        }
    }
    if functions.is_empty() {
        return Err(format!(
            "no #[pg_function] found under {}",
            src_dir.display()
        ));
    }
    Ok(functions)
}

fn extra_sql(dir: &Path) -> Result<Vec<(String, String)>, String> {
    let sql_dir = dir.join("sql");
    if !sql_dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut paths: Vec<_> = std::fs::read_dir(&sql_dir)
        .map_err(|e| format!("{}: {e}", sql_dir.display()))?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "sql"))
        .collect();
    paths.sort();
    paths
        .into_iter()
        .map(|p| {
            let body = std::fs::read_to_string(&p).map_err(|e| format!("{}: {e}", p.display()))?;
            Ok((
                p.strip_prefix(dir).unwrap_or(&p).display().to_string(),
                body,
            ))
        })
        .collect()
}

fn default_native_libs() -> &'static str {
    if cfg!(target_os = "macos") {
        "-lSystem -lc -lm"
    } else {
        "-lgcc_s -lutil -lrt -lpthread -lm -ldl -lc"
    }
}

/// Builds the static library; returns its path and the native libraries it
/// needs.
fn cargo_build(
    manifest: &Path,
    lib_name: &str,
    release: bool,
) -> Result<(PathBuf, String), String> {
    let mut cmd = cargo();
    cmd.args([
        "rustc",
        "--lib",
        "--crate-type",
        "staticlib",
        "--message-format",
        "json",
        "--manifest-path",
    ])
    .arg(manifest);
    if release {
        cmd.arg("--release");
    }
    cmd.args(["--", "--print", "native-static-libs"]);
    let out = run_capture(&mut cmd)?;
    let mut staticlib = None;
    let mut native = None;
    for line in out.lines() {
        let Ok(msg) = Json::parse(line) else { continue };
        match str_field(&msg, "reason") {
            Some("compiler-message") => {
                let Some(rendered) = msg.get("message").and_then(|m| str_field(m, "rendered"))
                else {
                    continue;
                };
                if let Some(libs) = rendered.split("native-static-libs:").nth(1) {
                    native = Some(libs.lines().next().unwrap_or_default().trim().to_owned());
                } else if !rendered.contains("link against the following native artifacts") {
                    eprint!("{rendered}");
                }
            }
            Some("compiler-artifact") => {
                let is_lib = msg.get("target").and_then(|t| str_field(t, "name")) == Some(lib_name);
                let a = msg
                    .get("filenames")
                    .and_then(Json::as_array)
                    .and_then(|fs| {
                        fs.iter()
                            .filter_map(Json::as_str)
                            .find(|f| f.ends_with(".a"))
                    });
                if let (true, Some(a)) = (is_lib, a) {
                    staticlib = Some(PathBuf::from(a));
                }
            }
            _ => {}
        }
    }
    let staticlib = staticlib.ok_or("cargo built no static library")?;
    Ok((
        staticlib,
        native.unwrap_or_else(|| default_native_libs().to_owned()),
    ))
}

/// Checks that every scanned function's wrapper made it into the static
/// library. A `#[pg_function]` in a file outside the module tree, or behind a
/// `cfg` that's off, is scanned but never compiled; on Linux the module
/// would still link (undefined symbols are allowed in a shared object) and
/// fail only at `CREATE EXTENSION`.
fn check_wrappers(staticlib: &Path, functions: &[Function]) -> Result<(), String> {
    // Apple's nm exits 1 on the LLVM bitcode in Rust's compiler_builtins
    // while still listing every other member, so the exit status is no
    // signal: an empty listing is.
    let out = Command::new("nm")
        .arg("-g")
        .arg(staticlib)
        .stderr(Stdio::null())
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default();
    if !out.contains("sutegi_pgext_fn_") && !out.contains("rust_begin_unwind") {
        eprintln!(
            "warning: couldn't list the static library with nm; skipping the check that every pg_function was compiled"
        );
        return Ok(());
    }
    let defined: std::collections::HashSet<&str> = out
        .lines()
        .filter_map(|l| {
            // `[address] T symbol`; macOS prefixes C symbols with `_`.
            let mut fields = l.split_whitespace().rev();
            let (symbol, kind) = (fields.next()?, fields.next()?);
            (kind == "T").then(|| symbol.strip_prefix('_').unwrap_or(symbol))
        })
        .collect();
    let missing: Vec<String> = functions
        .iter()
        .filter(|f| !defined.contains(format!("sutegi_pgext_fn_{}", f.rust_name).as_str()))
        .map(|f| format!("  {} ({}:{})", f.rust_name, f.file, f.line))
        .collect();
    if missing.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "these #[pg_function]s were scanned but not compiled — is their module declared, and not cfg'd out?\n{}",
            missing.join("\n")
        ))
    }
}

fn valid_ext_name(name: &str) -> bool {
    name.chars()
        .next()
        .is_some_and(|c| c.is_ascii_lowercase() || c == '_')
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        && name.len() <= 63
}

fn valid_version(v: &str) -> bool {
    !v.is_empty()
        && !v.contains("--")
        && !v.starts_with('-')
        && !v.ends_with('-')
        && v.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '+'))
}

fn make_safe(path: &Path) -> Result<String, String> {
    let s = path
        .to_str()
        .ok_or_else(|| format!("{} is not UTF-8", path.display()))?;
    // PGXS expands these paths into both Make prerequisites and shell words.
    if s.is_empty()
        || s.starts_with('-')
        || !s
            .chars()
            .all(|c| c.is_alphanumeric() || "/._-+@".contains(c))
    {
        return Err(format!(
            "PGXS needs a plain path, got {s:?}: use letters, digits, / . _ - + or @"
        ));
    }
    Ok(s.to_owned())
}

fn write(path: &Path, contents: &str) -> Result<(), String> {
    std::fs::write(path, contents).map_err(|e| format!("{}: {e}", path.display()))
}

/// Builds (and with [`Options::install`], installs) the extension.
///
/// # Errors
///
/// Any failing step, with the failing command or source location.
pub fn build(opts: &Options) -> Result<Built, String> {
    let manifest = opts
        .dir
        .join("Cargo.toml")
        .canonicalize()
        .map_err(|e| format!("{}/Cargo.toml: {e}", opts.dir.display()))?;
    let root = manifest.parent().ok_or("bad manifest path")?.to_path_buf();
    let pkg = package(&manifest)?;
    let ext = pkg.name.replace('-', "_");
    if !valid_ext_name(&ext) {
        return Err(format!(
            "'{ext}' isn't a valid extension name ([a-z_][a-z0-9_]*)"
        ));
    }
    if !valid_version(&pkg.version) {
        return Err(format!(
            "version '{}' can't name an extension script",
            pkg.version
        ));
    }

    let functions = scan_dir(&root, &pkg.src_dir)?;
    let extra = extra_sql(&root)?;
    let (staticlib, native_libs) = cargo_build(&manifest, &pkg.lib_name, opts.release)?;
    check_wrappers(&staticlib, &functions)?;

    let work = pkg.target_dir.join("sutegi-pgext").join(&ext);
    std::fs::create_dir_all(&work).map_err(|e| format!("{}: {e}", work.display()))?;
    make_safe(&work)?;
    let macos = cfg!(target_os = "macos");
    write(&work.join("sutegi_pgext.c"), RUNTIME_C)?;
    write(&work.join("sutegi_pgext.h"), RUNTIME_H)?;
    write(
        &work.join("functions.c"),
        &codegen::functions_c(&ext, &pkg.version, &functions),
    )?;
    write(
        &work.join(if macos { "exports.txt" } else { "exports.map" }),
        &codegen::exports(&functions, macos),
    )?;
    let comment = if pkg.description.is_empty() {
        format!("{ext} (sutegi-pgext)")
    } else {
        pkg.description.clone()
    };
    write(
        &work.join(format!("{ext}.control")),
        &codegen::control(&ext, &pkg.version, &comment),
    )?;
    write(
        &work.join(format!("{ext}--{}.sql", pkg.version)),
        &codegen::sql_script(&ext, &pkg.version, &functions, &extra),
    )?;
    write(
        &work.join("Makefile"),
        &codegen::makefile(&codegen::MakefileInput {
            ext: &ext,
            version: &pkg.version,
            staticlib: &make_safe(&staticlib)?,
            native_libs: &native_libs,
            pg_config: &make_safe(Path::new(&opts.pg_config))?,
            macos,
        }),
    )?;

    // with_llvm=no: skip PGXS's LLVM bitcode for the C runtime — there is no
    // Rust bitcode to inline it into, and it saves needing clang.
    run_inherit(
        Command::new("make")
            .arg("-C")
            .arg(&work)
            .arg("with_llvm=no"),
    )?;
    if opts.install {
        run_inherit(
            Command::new("make")
                .arg("-C")
                .arg(&work)
                .args(["with_llvm=no", "install"]),
        )?;
    }
    Ok(Built {
        name: ext,
        version: pkg.version,
        work_dir: work,
        functions,
    })
}

/// Where a new project gets `sutegi-pgext` from.
#[derive(Debug, Clone)]
pub enum Dependency {
    /// The sutegi repository on GitHub.
    Git,
    /// A local sutegi checkout.
    Path(PathBuf),
}

/// Scaffolds a new extension crate in `dir`, named after its last
/// component; returns the extension name.
///
/// # Errors
///
/// When `dir` exists, its name isn't a valid extension name, or a write
/// fails.
pub fn new_project(dir: &Path, dep: &Dependency) -> Result<String, String> {
    if dir.exists() {
        return Err(format!("'{}' already exists", dir.display()));
    }
    let name = dir
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or("the project needs a name")?
        .replace('-', "_");
    if !valid_ext_name(&name) {
        return Err(format!(
            "'{name}' isn't a valid extension name ([a-z_][a-z0-9_]*)"
        ));
    }
    let dep_line = match dep {
        Dependency::Git => {
            "sutegi-pgext = { git = \"https://github.com/enekos/sutegi\" }".to_owned()
        }
        Dependency::Path(p) => format!(
            "sutegi-pgext = {{ path = \"{}\" }}",
            p.join("crates/sutegi-pgext")
                .display()
                .to_string()
                .replace('\\', "/")
        ),
    };
    std::fs::create_dir_all(dir.join("src")).map_err(|e| format!("{}: {e}", dir.display()))?;
    write(
        &dir.join("Cargo.toml"),
        &NEW_CARGO_TOML
            .replace("{name}", &name)
            .replace("{dep}", &dep_line),
    )?;
    write(&dir.join("src/lib.rs"), NEW_LIB_RS)?;
    write(
        &dir.join("test.sql"),
        &NEW_TEST_SQL.replace("{name}", &name),
    )?;
    write(&dir.join(".gitignore"), "/target\n")?;
    Ok(name)
}

const NEW_CARGO_TOML: &str = r#"[package]
name = "{name}"
version = "0.1.0"
edition = "2024"
description = "The {name} PostgreSQL extension"

# `sutegi pgext build` links the extension from this static library.
[lib]
crate-type = ["staticlib"]
test = false
doctest = false

[dependencies]
{dep}

# Panics must unwind: the runtime turns them into SQL errors.
[profile.release]
panic = "unwind"

[workspace]
"#;

const NEW_LIB_RS: &str = r#"use sutegi_pgext::{Error, Result, SetOf, SqlState, pg_function};

/// Returns a greeting for name.
#[pg_function(immutable, parallel_safe)]
fn hello(name: &str) -> String {
    format!("Hello, {name}!")
}

/// Divides a by b, raising division_by_zero instead of panicking.
#[pg_function(immutable, parallel_safe)]
fn safe_div(a: i64, b: i64) -> Result<i64> {
    if b == 0 {
        return Err(Error::new(SqlState::DIVISION_BY_ZERO, "division by zero"));
    }
    a.checked_div(b)
        .ok_or_else(|| Error::new(SqlState::NUMERIC_VALUE_OUT_OF_RANGE, "bigint out of range"))
}

/// Counts from 1 to n, streaming the rows.
#[pg_function(immutable, parallel_safe, rows = 10)]
fn count_to(n: i64) -> SetOf<i64> {
    SetOf::new(1..=n)
}
"#;

const NEW_TEST_SQL: &str = r#"-- Asserts by raising: run with psql -v ON_ERROR_STOP=1.
CREATE EXTENSION {name};
DO $$
BEGIN
  IF hello('world') IS DISTINCT FROM 'Hello, world!' THEN RAISE 'hello'; END IF;
  IF safe_div(7, 2) IS DISTINCT FROM 3 THEN RAISE 'safe_div'; END IF;
  BEGIN
    PERFORM safe_div('-9223372036854775808'::bigint, -1);
    RAISE 'expected bigint overflow';
  EXCEPTION WHEN numeric_value_out_of_range THEN NULL;
  END;
  IF (SELECT sum(n) FROM count_to(4) AS n) IS DISTINCT FROM 10 THEN RAISE 'count_to'; END IF;
END $$;
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extension_names_follow_sql_identifiers() {
        assert!(valid_ext_name("hello_world2"));
        assert!(!valid_ext_name("Hello"));
        assert!(!valid_ext_name("2x"));
        assert!(!valid_ext_name(""));
    }

    #[test]
    fn versions_must_name_a_script() {
        assert!(valid_version("0.1.0"));
        assert!(valid_version("1.0.0-beta.1"));
        assert!(!valid_version("1--2"));
        assert!(!valid_version("1.0'"));
    }

    #[test]
    fn make_rejects_paths_it_would_split() {
        assert!(make_safe(Path::new("/tmp/a b")).is_err());
        assert!(make_safe(Path::new("/tmp/a$b")).is_err());
        assert!(make_safe(Path::new("/tmp/R&D/postgres")).is_err());
        assert!(make_safe(Path::new("/tmp/build[2]")).is_err());
        assert!(make_safe(Path::new("pg_config")).is_ok());
        assert!(make_safe(Path::new("/tmp/postgresql@17/ok-path_1")).is_ok());
    }

    #[test]
    fn duplicate_sql_signatures_are_refused_at_build_time() {
        let dir = std::env::temp_dir().join(format!("sutegi-pgext-dup-{}", std::process::id()));
        let src = dir.join("src");
        std::fs::create_dir_all(src.join("m")).unwrap();
        std::fs::write(src.join("lib.rs"), "#[pg_function] fn a(x: i32) {}\n").unwrap();
        std::fs::write(
            src.join("m/b.rs"),
            "#[pg_function(name = \"a\")] fn b(y: i32) {}\n",
        )
        .unwrap();
        let err = scan_dir(&dir, &src).unwrap_err();
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(
            err.contains("SQL function a(integer) is already defined by a"),
            "{err}"
        );
    }

    #[test]
    fn the_scaffold_scans_cleanly() {
        let fs = scan("src/lib.rs", NEW_LIB_RS).unwrap();
        let names: Vec<_> = fs.iter().map(|f| f.sql_name.as_str()).collect();
        assert_eq!(names, ["hello", "safe_div", "count_to"]);
    }
}
