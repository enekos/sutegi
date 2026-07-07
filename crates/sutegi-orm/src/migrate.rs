//! First-class, **versioned** migrations over any [`Backend`].
//!
//! Where [`Model::migrate`](crate::Model::migrate) is a one-shot
//! `CREATE TABLE IF NOT EXISTS`, this module tracks *which* migrations have run
//! in a `_sutegi_migrations` history table, applies only the pending ones (in
//! version order), and can roll the last batch back down again — the
//! Rails/Laravel workflow, backend-agnostic.
//!
//! A [`Migration`] is a `version` (a sortable id like `20260701_120000`), a
//! human `name`, an `up` closure, and an optional `down`. The closures receive
//! a [`MigrationOps`] handle — the object-safe subset of [`Backend`] (raw
//! `execute`/`query` plus schema `migrate`) — so a migration can create tables
//! from a [`TableSchema`] *or* run arbitrary DDL/DML.
//!
//! ```ignore
//! use sutegi::orm::migrate::{Migration, Migrator};
//!
//! fn migrations() -> Migrator {
//!     Migrator::new().add(Migration::reversible(
//!         "20260701_000001",
//!         "create_todos",
//!         |db| db.migrate_schema(&Todo::schema()),
//!         |db| { db.execute("DROP TABLE todos", &[])?; Ok(()) },
//!     ))
//! }
//! ```
//!
//! Each migration runs inside its own `BEGIN`/`COMMIT` (rolled back on error),
//! so a failing migration leaves neither a half-applied schema nor a history
//! row behind.

use crate::backend::Backend;
use crate::schema_diff::{apply, diff, render, Plan, SchemaOp};
use crate::value::{Dialect, TableSchema, Value};
use sutegi_json::Json;

/// The history table every migrator maintains. Portable DDL: `TEXT`/`INTEGER`
/// are spelled the same on SQLite and Postgres.
const HISTORY_TABLE: &str = "_sutegi_migrations";

/// The object-safe slice of [`Backend`] a migration body is handed.
///
/// [`Backend`] itself is not object-safe (it has generic `fetch`/`paginate`
/// methods), so migrations take `&dyn MigrationOps` instead — raw parameterized
/// SQL plus schema-driven table creation, which is all a migration needs. Every
/// [`Backend`] implements it via a blanket impl, so you can pass a `&Db`,
/// `&Pg`, or a transaction handle straight through.
pub trait MigrationOps {
    /// Execute a parameterized statement (`?` placeholders); returns rows affected.
    fn execute(&self, sql: &str, params: &[Value]) -> Result<usize, String>;
    /// Run a parameterized query, returning rows as JSON objects.
    fn query(&self, sql: &str, params: &[Value]) -> Result<Vec<Json>, String>;
    /// Create a table from a schema if it does not already exist.
    fn migrate_schema(&self, schema: &TableSchema) -> Result<(), String>;
}

impl<B: Backend + ?Sized> MigrationOps for B {
    fn execute(&self, sql: &str, params: &[Value]) -> Result<usize, String> {
        Backend::execute(self, sql, params)
    }
    fn query(&self, sql: &str, params: &[Value]) -> Result<Vec<Json>, String> {
        Backend::query(self, sql, params)
    }
    fn migrate_schema(&self, schema: &TableSchema) -> Result<(), String> {
        Backend::migrate(self, schema)
    }
}

/// The signature of a migration's `up`/`down` step: it is handed the
/// object-safe [`MigrationOps`] backend handle and reports success or an error.
pub type MigrationFn = fn(&dyn MigrationOps) -> Result<(), String>;

/// A migration's body: either hand-written closures (for backfills and DDL the
/// diff engine doesn't model) or a list of declarative [`SchemaOp`]s (generated
/// from a model diff, serializable to a file, reversible for free).
enum Body {
    Closure {
        up: MigrationFn,
        down: Option<MigrationFn>,
    },
    Ops(Vec<SchemaOp>),
}

/// A single migration — a `version` (a sortable id like `20260701_120000`), a
/// human `name`, and a [`Body`].
pub struct Migration {
    version: String,
    name: String,
    body: Body,
}

impl Migration {
    /// A forward-only closure migration (no `down`; [`Migrator::rollback`] will
    /// refuse it).
    pub fn new(version: impl Into<String>, name: impl Into<String>, up: MigrationFn) -> Migration {
        Migration {
            version: version.into(),
            name: name.into(),
            body: Body::Closure { up, down: None },
        }
    }

    /// A reversible closure migration with both `up` and `down`.
    pub fn reversible(
        version: impl Into<String>,
        name: impl Into<String>,
        up: MigrationFn,
        down: MigrationFn,
    ) -> Migration {
        Migration {
            version: version.into(),
            name: name.into(),
            body: Body::Closure {
                up,
                down: Some(down),
            },
        }
    }

    /// A declarative migration built from schema ops — the shape `migrate gen`
    /// produces. Its `down` is derived automatically by inverting the ops, and
    /// it serializes to a JSON file via [`to_json`](Migration::to_json).
    pub fn ops(
        version: impl Into<String>,
        name: impl Into<String>,
        ops: Vec<SchemaOp>,
    ) -> Migration {
        Migration {
            version: version.into(),
            name: name.into(),
            body: Body::Ops(ops),
        }
    }

    pub fn version(&self) -> &str {
        &self.version
    }
    pub fn name(&self) -> &str {
        &self.name
    }
    /// True if this migration can be rolled back (declarative migrations always
    /// can; closure migrations only if they were given a `down`).
    pub fn reversible_migration(&self) -> bool {
        match &self.body {
            Body::Closure { down, .. } => down.is_some(),
            Body::Ops(_) => true,
        }
    }
    /// The declarative ops, if this is an ops migration.
    pub fn ops_list(&self) -> Option<&[SchemaOp]> {
        match &self.body {
            Body::Ops(ops) => Some(ops),
            Body::Closure { .. } => None,
        }
    }

    /// A content hash over the (declarative) ops, stored in the history table so
    /// a file edited after being applied is detected. Closure migrations have no
    /// stable content to hash, so they return an empty string (unchecked).
    pub fn checksum(&self) -> String {
        match &self.body {
            Body::Ops(ops) => {
                let arr = Json::arr(ops.iter().map(SchemaOp::to_json).collect());
                sutegi_crypto::hex(&sutegi_crypto::sha256(arr.to_string().as_bytes()))
            }
            Body::Closure { .. } => String::new(),
        }
    }

    /// Serialize an ops migration to its on-disk JSON form (`None` for a closure
    /// migration, which can't be represented as data).
    pub fn to_json(&self) -> Option<Json> {
        self.ops_list().map(|ops| {
            Json::obj(vec![
                ("version", Json::str(self.version.clone())),
                ("name", Json::str(self.name.clone())),
                (
                    "ops",
                    Json::arr(ops.iter().map(SchemaOp::to_json).collect()),
                ),
            ])
        })
    }

    /// Parse an ops migration from its [`to_json`](Migration::to_json) form.
    pub fn from_json(j: &Json) -> Result<Migration, String> {
        let version = j
            .get("version")
            .and_then(Json::as_str)
            .ok_or("migration: missing `version`")?
            .to_string();
        let name = j
            .get("name")
            .and_then(Json::as_str)
            .ok_or("migration: missing `name`")?
            .to_string();
        let ops = j
            .get("ops")
            .and_then(Json::as_array)
            .ok_or("migration: missing `ops` array")?
            .iter()
            .map(SchemaOp::from_json)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Migration::ops(version, name, ops))
    }

    /// Run the forward step against `conn`.
    fn run_up<B: Backend>(&self, conn: &B) -> Result<(), String> {
        match &self.body {
            Body::Closure { up, .. } => up(conn),
            Body::Ops(ops) => exec_ops(conn, ops),
        }
    }

    /// Run the reverse step against `conn` (errors for a forward-only closure).
    fn run_down<B: Backend>(&self, conn: &B) -> Result<(), String> {
        match &self.body {
            Body::Closure { down: Some(d), .. } => d(conn),
            Body::Closure { down: None, .. } => Err(format!(
                "cannot roll back {} ({}): migration is forward-only",
                self.version, self.name
            )),
            Body::Ops(ops) => {
                let inverse: Vec<SchemaOp> = ops.iter().rev().map(SchemaOp::inverse).collect();
                exec_ops(conn, &inverse)
            }
        }
    }
}

/// Execute a list of schema ops against `conn`: render each to the backend's
/// dialect and run it, threading a shadow schema forward so SQLite's table
/// rebuilds see the correct pre-op state. The shadow starts from the live
/// database so ops that touch pre-existing tables render correctly.
fn exec_ops<B: Backend>(conn: &B, ops: &[SchemaOp]) -> Result<(), String> {
    let dialect = conn.dialect();
    let mut shadow = conn.introspect()?;
    for op in ops {
        for sql in render(op, dialect, &shadow)? {
            conn.execute(&sql, &[])?;
        }
        apply(&mut shadow, op)?;
    }
    Ok(())
}

/// Where a migration stands relative to the history table.
#[derive(Clone, Debug, PartialEq)]
pub struct MigrationStatus {
    pub version: String,
    pub name: String,
    pub applied: bool,
    /// The batch it was applied in, if applied.
    pub batch: Option<i64>,
    /// True when the history records this version but no code migration defines
    /// it any more (a dropped or renamed migration) — surfaced, never run.
    pub orphan: bool,
}

/// An ordered set of migrations plus the run/rollback/status machinery.
#[derive(Default)]
pub struct Migrator {
    migrations: Vec<Migration>,
}

impl Migrator {
    pub fn new() -> Migrator {
        Migrator {
            migrations: Vec::new(),
        }
    }

    /// Register a migration (builder-style). Order of registration does not
    /// matter; migrations always run sorted by `version`.
    #[allow(clippy::should_implement_trait)] // builder-style `add`, not `Add::add`
    pub fn add(mut self, migration: Migration) -> Migrator {
        self.migrations.push(migration);
        self
    }

    /// Load every `*.json` migration file in `dir` and register it. Files are
    /// parsed via [`Migration::from_json`]; the version/name come from the file
    /// contents, not the filename. Missing directory is not an error (no files
    /// to load); a malformed file is.
    pub fn load_dir(mut self, dir: &str) -> Result<Migrator, String> {
        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(_) => return Ok(self), // no migrations directory yet
        };
        let mut paths: Vec<std::path::PathBuf> = entries
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("json"))
            .collect();
        paths.sort();
        for path in paths {
            let text = std::fs::read_to_string(&path)
                .map_err(|e| format!("reading {}: {e}", path.display()))?;
            let json =
                Json::parse(&text).map_err(|e| format!("parsing {}: {e}", path.display()))?;
            self.migrations.push(Migration::from_json(&json)?);
        }
        Ok(self)
    }

    /// The registered migrations, sorted by version.
    fn sorted(&self) -> Vec<&Migration> {
        let mut v: Vec<&Migration> = self.migrations.iter().collect();
        v.sort_by(|a, b| a.version.cmp(&b.version));
        v
    }

    /// Create the history table if absent. Idempotent; tolerant of a
    /// concurrent pod winning the `IF NOT EXISTS` race.
    fn ensure_history<B: Backend>(&self, conn: &B) -> Result<(), String> {
        conn.execute(
            &format!(
                "CREATE TABLE IF NOT EXISTS {HISTORY_TABLE} (\
                    version TEXT PRIMARY KEY, \
                    name TEXT NOT NULL, \
                    batch INTEGER NOT NULL, \
                    checksum TEXT NOT NULL DEFAULT '', \
                    applied_at INTEGER NOT NULL)"
            ),
            &[],
        )
        .map(|_| ())
    }

    /// One record per applied migration: `(version, name, batch, checksum)`.
    fn applied<B: Backend>(&self, conn: &B) -> Result<Vec<AppliedRow>, String> {
        let rows = conn.query(
            &format!("SELECT version, name, batch, checksum FROM {HISTORY_TABLE}"),
            &[],
        )?;
        let mut out = Vec::with_capacity(rows.len());
        for r in &rows {
            out.push(AppliedRow {
                version: r
                    .get("version")
                    .and_then(Json::as_str)
                    .ok_or("migration row missing version")?
                    .to_string(),
                name: r
                    .get("name")
                    .and_then(Json::as_str)
                    .unwrap_or("")
                    .to_string(),
                batch: r.get("batch").and_then(Json::as_i64).unwrap_or(0),
                checksum: r
                    .get("checksum")
                    .and_then(Json::as_str)
                    .unwrap_or("")
                    .to_string(),
            });
        }
        Ok(out)
    }

    /// Apply every pending migration in version order, each in its own
    /// transaction. Returns the versions applied (empty if already up to date).
    ///
    /// On Postgres a session-level advisory lock serializes concurrent runners
    /// (many pods booting at once) so migrations can't race. An already-applied
    /// migration whose checksum no longer matches its stored one is a hard error
    /// — a file was edited after being applied.
    pub fn run<B: Backend>(&self, conn: &B) -> Result<Vec<String>, String> {
        let _guard = AdvisoryLock::acquire(conn)?;
        self.ensure_history(conn)?;
        let applied = self.applied(conn)?;
        let done: std::collections::BTreeSet<&str> =
            applied.iter().map(|r| r.version.as_str()).collect();
        let next_batch = applied.iter().map(|r| r.batch).max().unwrap_or(0) + 1;
        let now = sutegi_crypto::now_secs();

        // Tamper check: a migration already in history must still hash the same.
        for m in self.sorted() {
            if let Some(row) = applied.iter().find(|r| r.version == m.version) {
                let current = m.checksum();
                if !row.checksum.is_empty() && !current.is_empty() && row.checksum != current {
                    return Err(format!(
                        "migration {} ({}) was modified after being applied \
                         (checksum mismatch) — restore it or run `migrate repair`",
                        m.version, m.name
                    ));
                }
            }
        }

        let mut ran = Vec::new();
        for m in self.sorted() {
            if done.contains(m.version.as_str()) {
                continue;
            }
            in_transaction(conn, || {
                m.run_up(conn)?;
                conn.execute(
                    &format!(
                        "INSERT INTO {HISTORY_TABLE} (version, name, batch, checksum, applied_at) \
                         VALUES (?, ?, ?, ?, ?)"
                    ),
                    &[
                        Value::Text(m.version.clone()),
                        Value::Text(m.name.clone()),
                        Value::Int(next_batch),
                        Value::Text(m.checksum()),
                        Value::Int(now),
                    ],
                )?;
                Ok(())
            })
            .map_err(|e| format!("migration {} ({}) failed: {e}", m.version, m.name))?;
            ran.push(m.version.clone());
        }
        Ok(ran)
    }

    /// Re-stamp the stored checksums to match the current migration files —
    /// the escape hatch after a deliberate edit to an applied migration. Only
    /// touches rows that are both applied and still defined in code.
    pub fn repair<B: Backend>(&self, conn: &B) -> Result<Vec<String>, String> {
        self.ensure_history(conn)?;
        let applied = self.applied(conn)?;
        let mut fixed = Vec::new();
        for m in self.sorted() {
            if let Some(row) = applied.iter().find(|r| r.version == m.version) {
                let current = m.checksum();
                if row.checksum != current {
                    conn.execute(
                        &format!("UPDATE {HISTORY_TABLE} SET checksum = ? WHERE version = ?"),
                        &[Value::Text(current), Value::Text(m.version.clone())],
                    )?;
                    fixed.push(m.version.clone());
                }
            }
        }
        Ok(fixed)
    }

    /// Roll back the most recent `batches` batch(es), newest first. Each
    /// migration's `down` runs in its own transaction; a forward-only migration
    /// aborts the rollback. Returns the versions rolled back.
    pub fn rollback<B: Backend>(&self, conn: &B, batches: usize) -> Result<Vec<String>, String> {
        let _guard = AdvisoryLock::acquire(conn)?;
        self.ensure_history(conn)?;
        let applied = self.applied(conn)?;
        if applied.is_empty() || batches == 0 {
            return Ok(Vec::new());
        }

        // The `batches` highest distinct batch numbers.
        let mut batch_nums: Vec<i64> = applied.iter().map(|r| r.batch).collect();
        batch_nums.sort_unstable();
        batch_nums.dedup();
        let target: std::collections::BTreeSet<i64> =
            batch_nums.into_iter().rev().take(batches).collect();

        // Versions to undo, newest first (version order is the apply order).
        let mut victims: Vec<&AppliedRow> = applied
            .iter()
            .filter(|r| target.contains(&r.batch))
            .collect();
        victims.sort_by(|a, b| b.version.cmp(&a.version));

        let by_version = |v: &str| self.migrations.iter().find(|m| m.version == v);

        let mut rolled = Vec::new();
        for row in victims {
            let migration = by_version(&row.version).ok_or_else(|| {
                format!(
                    "cannot roll back {}: no such migration in code",
                    row.version
                )
            })?;
            in_transaction(conn, || {
                migration.run_down(conn)?;
                conn.execute(
                    &format!("DELETE FROM {HISTORY_TABLE} WHERE version = ?"),
                    &[Value::Text(row.version.clone())],
                )?;
                Ok(())
            })
            .map_err(|e| format!("rollback of {} ({}) failed: {e}", row.version, row.name))?;
            rolled.push(row.version.clone());
        }
        Ok(rolled)
    }

    /// The status of every migration — code-defined and orphaned — sorted by
    /// version.
    pub fn status<B: Backend>(&self, conn: &B) -> Result<Vec<MigrationStatus>, String> {
        self.ensure_history(conn)?;
        let applied = self.applied(conn)?;
        let batch_of = |v: &str| applied.iter().find(|r| r.version == v).map(|r| r.batch);

        let mut out: Vec<MigrationStatus> = self
            .sorted()
            .iter()
            .map(|m| MigrationStatus {
                version: m.version.to_string(),
                name: m.name.to_string(),
                applied: batch_of(&m.version).is_some(),
                batch: batch_of(&m.version),
                orphan: false,
            })
            .collect();

        let defined: std::collections::BTreeSet<&str> =
            self.migrations.iter().map(|m| m.version.as_str()).collect();
        for row in &applied {
            if !defined.contains(row.version.as_str()) {
                out.push(MigrationStatus {
                    version: row.version.clone(),
                    name: row.name.clone(),
                    applied: true,
                    batch: Some(row.batch),
                    orphan: true,
                });
            }
        }
        out.sort_by(|a, b| a.version.cmp(&b.version));
        Ok(out)
    }

    /// A machine-readable description of the registered migrations, for
    /// introspection (no database access).
    pub fn describe(&self) -> Json {
        Json::arr(
            self.sorted()
                .iter()
                .map(|m| {
                    Json::obj(vec![
                        ("version", Json::str(m.version.clone())),
                        ("name", Json::str(m.name.clone())),
                        ("reversible", Json::Bool(m.reversible_migration())),
                        ("declarative", Json::Bool(m.ops_list().is_some())),
                    ])
                })
                .collect(),
        )
    }

    /// The **shadow schema**: fold every registered declarative migration's ops
    /// into the schema they produce, without touching a database. This is the
    /// deterministic baseline `generate` diffs the models against.
    ///
    /// Errors if any registered migration is a closure (its effect can't be
    /// folded symbolically) — call [`shadow_via`](Migrator::shadow_via) with a
    /// scratch backend to replay those instead.
    pub fn shadow(&self) -> Result<Vec<TableSchema>, String> {
        let mut schemas: Vec<TableSchema> = Vec::new();
        for m in self.sorted() {
            match m.ops_list() {
                Some(ops) => apply_all_ops(&mut schemas, ops)?,
                None => {
                    return Err(format!(
                        "migration {} ({}) is a closure — cannot fold it into a shadow schema; \
                         use shadow_via() with a scratch database to replay it",
                        m.version, m.name
                    ))
                }
            }
        }
        Ok(normalize_all(schemas))
    }

    /// Build the shadow schema by **replaying** all migrations against a fresh
    /// scratch backend (e.g. an in-memory SQLite) and introspecting the result.
    /// Handles closure migrations, which [`shadow`](Migrator::shadow) can't fold.
    pub fn shadow_via<B: Backend>(&self, scratch: &B) -> Result<Vec<TableSchema>, String> {
        self.run(scratch)?;
        Ok(normalize_all(scratch.introspect()?))
    }
}

/// Fold ops into a schema set (thin wrapper over the diff engine's `apply`).
fn apply_all_ops(schemas: &mut Vec<TableSchema>, ops: &[SchemaOp]) -> Result<(), String> {
    for op in ops {
        apply(schemas, op)?;
    }
    Ok(())
}

fn normalize_all(mut schemas: Vec<TableSchema>) -> Vec<TableSchema> {
    schemas = schemas.iter().map(|t| t.normalized()).collect();
    schemas.sort_by(|a, b| a.table.cmp(&b.table));
    schemas
}

/// Diff the desired model schemas against the migrator's shadow schema to build
/// the [`Plan`] a new migration would contain — the deterministic core of
/// `migrate gen`. `dialect` selects the storage-type comparison so a change both
/// backends store identically isn't reported.
///
/// Uses the pure [`shadow`](Migrator::shadow); if the migrator has closure
/// migrations, pass a scratch backend to [`generate_via`] instead.
pub fn generate(
    migrator: &Migrator,
    desired: &[TableSchema],
    dialect: Dialect,
) -> Result<Plan, String> {
    let shadow = migrator.shadow()?;
    Ok(diff(&shadow, &normalize_all(desired.to_vec()), dialect))
}

/// Like [`generate`], but replays the migration history (including closures)
/// against `scratch` to obtain the shadow — use when closure migrations exist.
pub fn generate_via<B: Backend>(
    migrator: &Migrator,
    scratch: &B,
    desired: &[TableSchema],
) -> Result<Plan, String> {
    let shadow = migrator.shadow_via(scratch)?;
    Ok(diff(
        &shadow,
        &normalize_all(desired.to_vec()),
        scratch.dialect(),
    ))
}

/// Write a declarative migration to `<dir>/<version>_<name>.json` (creating the
/// directory if needed) and return the path. Errors for a closure migration.
pub fn write_migration_file(dir: &str, migration: &Migration) -> Result<String, String> {
    let json = migration
        .to_json()
        .ok_or("cannot write a closure migration to a file")?;
    std::fs::create_dir_all(dir).map_err(|e| format!("creating {dir}: {e}"))?;
    let path = format!("{dir}/{}_{}.json", migration.version(), migration.name());
    std::fs::write(&path, json.to_pretty()).map_err(|e| format!("writing {path}: {e}"))?;
    Ok(path)
}

/// One row of the migration history table.
struct AppliedRow {
    version: String,
    name: String,
    batch: i64,
    checksum: String,
}

/// A Postgres session advisory lock held for the duration of a run/rollback so
/// concurrent runners serialize. A no-op on SQLite (single-node). Released on
/// drop.
struct AdvisoryLock<'a, B: Backend> {
    conn: Option<&'a B>,
}

/// A fixed key (any constant) identifying the sutegi-migrations lock.
const ADVISORY_LOCK_KEY: i64 = 0x5537_4547_4900; // "SUTEGI" ish

impl<'a, B: Backend> AdvisoryLock<'a, B> {
    fn acquire(conn: &'a B) -> Result<AdvisoryLock<'a, B>, String> {
        if conn.dialect() == Dialect::Postgres {
            conn.query(
                "SELECT pg_advisory_lock(?)",
                &[Value::Int(ADVISORY_LOCK_KEY)],
            )?;
            Ok(AdvisoryLock { conn: Some(conn) })
        } else {
            Ok(AdvisoryLock { conn: None })
        }
    }
}

impl<B: Backend> Drop for AdvisoryLock<'_, B> {
    fn drop(&mut self) {
        if let Some(conn) = self.conn {
            let _ = conn.query(
                "SELECT pg_advisory_unlock(?)",
                &[Value::Int(ADVISORY_LOCK_KEY)],
            );
        }
    }
}

/// Render a status list as JSON (`[{version,name,applied,batch,orphan}]`).
pub fn status_json(statuses: &[MigrationStatus]) -> Json {
    Json::arr(
        statuses
            .iter()
            .map(|s| {
                Json::obj(vec![
                    ("version", Json::str(&s.version)),
                    ("name", Json::str(&s.name)),
                    ("applied", Json::Bool(s.applied)),
                    ("batch", s.batch.map(Json::int).unwrap_or(Json::Null)),
                    ("orphan", Json::Bool(s.orphan)),
                ])
            })
            .collect(),
    )
}

/// Run `body` between `BEGIN` and `COMMIT`, rolling back on error. Uses the
/// backend's own `execute`, so it works identically on SQLite and Postgres
/// (both give transactional DDL).
fn in_transaction<B: Backend>(
    conn: &B,
    body: impl FnOnce() -> Result<(), String>,
) -> Result<(), String> {
    conn.execute("BEGIN", &[])?;
    match body() {
        Ok(()) => {
            conn.execute("COMMIT", &[])?;
            Ok(())
        }
        Err(e) => {
            let _ = conn.execute("ROLLBACK", &[]);
            Err(e)
        }
    }
}

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use super::*;
    use crate::db::Db;
    use crate::QueryBuilder;

    fn migrator() -> Migrator {
        Migrator::new()
            .add(Migration::reversible(
                "0002_add_posts",
                "add_posts",
                |db| {
                    db.execute(
                        "CREATE TABLE posts (id INTEGER PRIMARY KEY, title TEXT NOT NULL)",
                        &[],
                    )
                    .map(|_| ())
                },
                |db| db.execute("DROP TABLE posts", &[]).map(|_| ()),
            ))
            // Registered out of order on purpose — must still run 0001 first.
            .add(Migration::reversible(
                "0001_create_users",
                "create_users",
                |db| {
                    db.execute(
                        "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT NOT NULL)",
                        &[],
                    )
                    .map(|_| ())
                },
                |db| db.execute("DROP TABLE users", &[]).map(|_| ()),
            ))
    }

    #[test]
    fn runs_pending_in_version_order_and_is_idempotent() {
        let db = Db::memory().unwrap();
        let m = migrator();

        let ran = m.run(&db).unwrap();
        assert_eq!(ran, vec!["0001_create_users", "0002_add_posts"]);

        // Both tables exist and are queryable.
        assert_eq!(db.select(&QueryBuilder::table("users")).unwrap().len(), 0);
        assert_eq!(db.select(&QueryBuilder::table("posts")).unwrap().len(), 0);

        // Second run is a no-op — nothing pending.
        assert!(m.run(&db).unwrap().is_empty());

        // Both applied in the same (first) batch.
        let status = m.status(&db).unwrap();
        assert_eq!(status.len(), 2);
        assert!(status.iter().all(|s| s.applied && s.batch == Some(1)));
    }

    #[test]
    fn rollback_undoes_last_batch_newest_first() {
        let db = Db::memory().unwrap();
        let m = migrator();
        m.run(&db).unwrap();

        // One batch → rollback 1 undoes both, newest version first.
        let rolled = m.rollback(&db, 1).unwrap();
        assert_eq!(rolled, vec!["0002_add_posts", "0001_create_users"]);

        // Tables are gone and history is empty.
        assert!(db.select(&QueryBuilder::table("users")).is_err());
        assert!(m.status(&db).unwrap().iter().all(|s| !s.applied));

        // Re-running applies them again in a fresh batch.
        assert_eq!(m.run(&db).unwrap().len(), 2);
    }

    #[test]
    fn separate_runs_get_separate_batches() {
        let db = Db::memory().unwrap();
        let first = Migrator::new().add(Migration::new("0001_a", "a", |db| {
            db.execute("CREATE TABLE a (id INTEGER PRIMARY KEY)", &[])
                .map(|_| ())
        }));
        first.run(&db).unwrap();

        let both = migrator().add(Migration::new("0001_a", "a", |_| Ok(())));
        // 0001_a already applied (batch 1); the two new ones land in batch 2.
        let ran = both.run(&db).unwrap();
        assert_eq!(ran, vec!["0001_create_users", "0002_add_posts"]);
        let status = both.status(&db).unwrap();
        let posts = status
            .iter()
            .find(|s| s.version == "0002_add_posts")
            .unwrap();
        assert_eq!(posts.batch, Some(2));
    }

    #[test]
    fn forward_only_migration_cannot_roll_back() {
        let db = Db::memory().unwrap();
        let m = Migrator::new().add(Migration::new("0001_x", "x", |db| {
            db.execute("CREATE TABLE x (id INTEGER PRIMARY KEY)", &[])
                .map(|_| ())
        }));
        m.run(&db).unwrap();
        let err = m.rollback(&db, 1).unwrap_err();
        assert!(err.contains("forward-only"), "got: {err}");
    }

    #[test]
    fn failing_migration_rolls_back_cleanly() {
        let db = Db::memory().unwrap();
        let m = Migrator::new().add(Migration::new("0001_boom", "boom", |db| {
            db.execute("CREATE TABLE ok (id INTEGER PRIMARY KEY)", &[])?;
            Err("deliberate failure".into())
        }));
        let err = m.run(&db).unwrap_err();
        assert!(err.contains("deliberate failure"));
        // The transaction rolled back: no table, no history row.
        assert!(db.select(&QueryBuilder::table("ok")).is_err());
        assert!(m.status(&db).unwrap().iter().all(|s| !s.applied));
    }

    #[test]
    fn describe_lists_versions_and_reversibility() {
        let j = migrator().describe();
        let arr = j.as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert_eq!(
            arr[0].get("version").and_then(Json::as_str),
            Some("0001_create_users")
        );
        assert_eq!(arr[0].get("reversible").and_then(Json::as_bool), Some(true));
    }

    // ---- P5: declarative ops migrations + generation ----

    use crate::value::{ColType, Column, TableSchema};

    fn todos_v1() -> TableSchema {
        TableSchema::new("todos")
            .column(Column::new("id", ColType::Integer).primary())
            .column(Column::new("title", ColType::Text))
    }

    fn todos_v2() -> TableSchema {
        // v1 + a new field, exactly the "add a field to a migrated model" case.
        TableSchema::new("todos")
            .column(Column::new("id", ColType::Integer).primary())
            .column(Column::new("title", ColType::Text))
            .column(Column::new("done", ColType::Boolean).default(Value::Bool(false)))
    }

    #[test]
    fn ops_migration_applies_and_rolls_back() {
        let db = Db::memory().unwrap();
        let plan = crate::schema_diff::diff(&[], &[todos_v1()], Dialect::Sqlite);
        let m = Migrator::new().add(Migration::ops("0001_todos", "create_todos", plan.ops));

        assert_eq!(m.run(&db).unwrap(), vec!["0001_todos"]);
        // Table exists and matches; insert works.
        assert_eq!(db.introspect().unwrap()[0], todos_v1().normalized());
        Backend::execute(&db, "INSERT INTO todos (title) VALUES ('x')", &[]).unwrap();

        // An ops migration is reversible for free: rollback drops the table.
        assert_eq!(m.rollback(&db, 1).unwrap(), vec!["0001_todos"]);
        assert!(db.introspect().unwrap().is_empty());
    }

    #[test]
    fn migration_json_round_trips() {
        let plan = crate::schema_diff::diff(&[], &[todos_v2()], Dialect::Sqlite);
        let m = Migration::ops("0001_todos", "create_todos", plan.ops);
        let json = m.to_json().unwrap();
        let back = Migration::from_json(&json).unwrap();
        // Same content ⇒ same checksum.
        assert_eq!(m.checksum(), back.checksum());
        assert_eq!(back.version(), "0001_todos");
    }

    #[test]
    fn generate_is_deterministic_and_diffs_the_shadow() {
        // Migration history: v1 already exists as an ops migration.
        let v1 = crate::schema_diff::diff(&[], &[todos_v1()], Dialect::Sqlite);
        let migrator = Migrator::new().add(Migration::ops("0001_todos", "create_todos", v1.ops));

        // The model is now v2 → generate diffs v2 against the shadow (=v1).
        let plan_a = generate(&migrator, &[todos_v2()], Dialect::Sqlite).unwrap();
        let plan_b = generate(&migrator, &[todos_v2()], Dialect::Sqlite).unwrap();
        // Deterministic: same inputs, identical plan.
        assert_eq!(plan_a, plan_b);
        // And it's exactly the one new column.
        assert_eq!(plan_a.ops.len(), 1);
        assert!(matches!(plan_a.ops[0], SchemaOp::AddColumn { .. }));
    }

    #[test]
    fn end_to_end_add_a_field_to_a_migrated_model() {
        // The headline scenario: create model, migrate, add a field, generate,
        // migrate again — and the new field is really usable.
        let db = Db::memory().unwrap();

        let v1 = crate::schema_diff::diff(&[], &[todos_v1()], Dialect::Sqlite);
        let mut migrator =
            Migrator::new().add(Migration::ops("0001_todos", "create_todos", v1.ops));
        migrator.run(&db).unwrap();
        Backend::execute(&db, "INSERT INTO todos (title) VALUES ('first')", &[]).unwrap();

        // Model gained `done`. Generate the delta, register it, migrate.
        let plan = generate(&migrator, &[todos_v2()], Dialect::Sqlite).unwrap();
        migrator = migrator.add(Migration::ops("0002_add_done", "add_done", plan.ops));
        assert_eq!(migrator.run(&db).unwrap(), vec!["0002_add_done"]);

        // The pre-existing row got the default; the new column is writable.
        let rows = db.select(&QueryBuilder::table("todos")).unwrap();
        assert_eq!(rows.len(), 1);
        // SQLite stores the boolean default as 0; the typed layer coerces it.
        assert_eq!(rows[0].get("done").and_then(Json::as_i64), Some(0));
        Backend::execute(
            &db,
            "INSERT INTO todos (title, done) VALUES ('second', 1)",
            &[],
        )
        .unwrap();
        assert_eq!(db.count(&QueryBuilder::table("todos")).unwrap(), 2);

        // The DB now matches the v2 model exactly.
        assert_eq!(db.introspect().unwrap()[0], todos_v2().normalized());
    }

    #[test]
    fn edited_applied_migration_trips_the_checksum_guard() {
        let db = Db::memory().unwrap();
        let v1 = crate::schema_diff::diff(&[], &[todos_v1()], Dialect::Sqlite);
        Migrator::new()
            .add(Migration::ops("0001_todos", "create_todos", v1.ops))
            .run(&db)
            .unwrap();

        // Re-run with the SAME version but different ops (a post-apply edit).
        let tampered = crate::schema_diff::diff(&[], &[todos_v2()], Dialect::Sqlite);
        let m2 = Migrator::new().add(Migration::ops("0001_todos", "create_todos", tampered.ops));
        let err = m2.run(&db).unwrap_err();
        assert!(err.contains("modified after being applied"), "got: {err}");

        // repair re-stamps, and then run is clean again.
        m2.repair(&db).unwrap();
        assert!(m2.run(&db).unwrap().is_empty());
    }

    #[test]
    fn shadow_errors_on_closures_but_replays_via_scratch() {
        let closure = Migrator::new().add(Migration::new("0001_c", "c", |db| {
            db.execute("CREATE TABLE c (id INTEGER PRIMARY KEY)", &[])
                .map(|_| ())
        }));
        // Pure fold refuses a closure...
        assert!(closure.shadow().is_err());
        // ...but replay into a scratch DB works.
        let scratch = Db::memory().unwrap();
        let shadow = closure.shadow_via(&scratch).unwrap();
        assert_eq!(shadow[0].table, "c");
    }

    #[test]
    fn load_dir_reads_migration_files() {
        let dir = std::env::temp_dir().join(format!("sutegi_mig_{}", sutegi_crypto::now_secs()));
        let dir = dir.to_str().unwrap();
        let plan = crate::schema_diff::diff(&[], &[todos_v1()], Dialect::Sqlite);
        let m = Migration::ops("0001_todos", "create_todos", plan.ops);
        let path = write_migration_file(dir, &m).unwrap();
        assert!(path.ends_with("0001_todos_create_todos.json"));

        let db = Db::memory().unwrap();
        let loaded = Migrator::new().load_dir(dir).unwrap();
        assert_eq!(loaded.run(&db).unwrap(), vec!["0001_todos"]);
        assert_eq!(db.introspect().unwrap()[0], todos_v1().normalized());

        let _ = std::fs::remove_dir_all(dir);
    }
}
