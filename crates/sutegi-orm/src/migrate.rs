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
//! `execute`/`query` plus schema `migrate` and the SQL [`Dialect`]) — so a
//! migration can create tables from a [`TableSchema`] *or* run arbitrary
//! DDL/DML.
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
//! ## The reliability contract
//!
//! Running migrations is the one moment an app rewrites its own foundations,
//! so [`Migrator::run`] and [`Migrator::rollback`] are built so that **no
//! outcome leaves the database in a state the migrator cannot account for**:
//!
//! - **Each migration is atomic.** Its body and its history row commit in one
//!   *real* transaction pinned to a single connection
//!   ([`Transactional::run_in_tx`]) — never `BEGIN`/`COMMIT` strings sprayed
//!   across a connection pool, where each statement can land on a different
//!   connection and "rollback" rolls back nothing. A failing migration leaves
//!   neither a half-applied schema nor a history row.
//! - **Concurrent runners serialize.** The run holds the backend's named
//!   advisory lock (`sutegi:migrations`) — cluster-wide on Postgres via a
//!   dedicated session that auto-releases on crash, process-wide on SQLite.
//!   Where the lock cannot reach (two OS processes on one SQLite file), each
//!   migration *re-checks the history table inside its own write transaction*
//!   before running, so a lost race means a skip, never a double-apply.
//! - **Nothing runs before the plan is validated.** Duplicate or malformed
//!   versions, an out-of-order pending migration (older than something already
//!   applied — the merged-stale-branch hazard), an applied migration whose
//!   file was edited (checksum) or renamed, and a rollback batch containing a
//!   forward-only or code-deleted migration are all rejected **up front**,
//!   before the database is touched at all.
//!
//! A migration that *must not* run in a transaction (Postgres
//! `CREATE INDEX CONCURRENTLY`) can opt out with
//! [`Migration::no_transaction`]; it trades atomicity for that capability and
//! must be written idempotently.

use crate::backend::{Backend, CapScope, Isolation, LockGuard, Transactional};
use crate::schema_diff::{apply, diff, render, Plan, SchemaOp};
use crate::value::{Dialect, TableSchema, Value};
use sutegi_json::Json;

/// The history table every migrator maintains. Portable DDL: `TEXT`/`INTEGER`
/// are spelled the same on SQLite and Postgres.
const HISTORY_TABLE: &str = "_sutegi_migrations";

/// The advisory-lock name serializing concurrent migration runners.
const MIGRATION_LOCK: &str = "sutegi:migrations";

/// How long [`Migrator::run`]/[`rollback`](Migrator::rollback) wait for the
/// migration lock before giving up (override with
/// [`Migrator::lock_timeout`]).
const DEFAULT_LOCK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);

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
    /// The SQL dialect on the other side — for a closure migration that must
    /// write dialect-specific DDL by hand.
    fn dialect(&self) -> Dialect;
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
    fn dialect(&self) -> Dialect {
        Backend::dialect(self)
    }
}

/// [`MigrationOps`] over a `&dyn Backend` — the sized adapter that lets a
/// migration body run against the transaction handle `run_in_tx` provides
/// (an unsized `dyn Backend` can't coerce to `dyn MigrationOps` directly).
struct DynOps<'a>(&'a dyn Backend);

impl MigrationOps for DynOps<'_> {
    fn execute(&self, sql: &str, params: &[Value]) -> Result<usize, String> {
        self.0.execute(sql, params)
    }
    fn query(&self, sql: &str, params: &[Value]) -> Result<Vec<Json>, String> {
        self.0.query(sql, params)
    }
    fn migrate_schema(&self, schema: &TableSchema) -> Result<(), String> {
        self.0.migrate(schema)
    }
    fn dialect(&self) -> Dialect {
        self.0.dialect()
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
    transactional: bool,
}

impl Migration {
    /// A forward-only closure migration (no `down`; [`Migrator::rollback`] will
    /// refuse it).
    pub fn new(version: impl Into<String>, name: impl Into<String>, up: MigrationFn) -> Migration {
        Migration {
            version: version.into(),
            name: name.into(),
            body: Body::Closure { up, down: None },
            transactional: true,
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
            transactional: true,
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
            transactional: true,
        }
    }

    /// Opt this migration out of the per-migration transaction — for DDL that
    /// refuses to run inside one, like Postgres `CREATE INDEX CONCURRENTLY`.
    ///
    /// The trade is explicit: if the process dies between the body finishing
    /// and the history row committing, the next run executes the body
    /// **again** — so a non-transactional migration must be idempotent
    /// (`IF NOT EXISTS` its DDL, key its backfills).
    pub fn no_transaction(mut self) -> Migration {
        self.transactional = false;
        self
    }

    pub fn version(&self) -> &str {
        &self.version
    }
    pub fn name(&self) -> &str {
        &self.name
    }
    /// True unless [`no_transaction`](Migration::no_transaction) opted out.
    pub fn runs_in_transaction(&self) -> bool {
        self.transactional
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
    fn run_up(&self, conn: &dyn Backend) -> Result<(), String> {
        match &self.body {
            Body::Closure { up, .. } => up(&DynOps(conn)),
            Body::Ops(ops) => exec_ops(conn, ops),
        }
    }

    /// Run the reverse step against `conn` (errors for a forward-only closure).
    fn run_down(&self, conn: &dyn Backend) -> Result<(), String> {
        match &self.body {
            Body::Closure { down: Some(d), .. } => d(&DynOps(conn)),
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

/// True for the version/name strings the migrator accepts: non-empty ASCII
/// letters, digits, `_`, `-`, `.`. One path component by construction, so a
/// version can never traverse out of the migrations directory, and `<` on the
/// string is a sane apply order.
fn valid_ident(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
}

/// Execute a list of schema ops against `conn`: render each to the backend's
/// dialect and run it, threading a shadow schema forward so SQLite's table
/// rebuilds see the correct pre-op state. The shadow starts from the live
/// database so ops that touch pre-existing tables render correctly.
fn exec_ops(conn: &dyn Backend, ops: &[SchemaOp]) -> Result<(), String> {
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

/// What [`Migrator::plan_run`] would do for one pending migration, without
/// doing it.
#[derive(Clone, Debug, PartialEq)]
pub struct PlannedMigration {
    pub version: String,
    pub name: String,
    /// The SQL a declarative migration would execute, rendered for the
    /// backend's dialect against the current schema. `None` for a closure
    /// migration — its statements only exist at run time. (Rendering after a
    /// closure is best-effort: the closure's schema effects are unknowable
    /// without running it.)
    pub statements: Option<Vec<String>>,
}

/// An ordered set of migrations plus the run/rollback/status machinery.
pub struct Migrator {
    migrations: Vec<Migration>,
    allow_out_of_order: bool,
    lock_timeout: std::time::Duration,
}

impl Default for Migrator {
    fn default() -> Migrator {
        Migrator::new()
    }
}

impl Migrator {
    pub fn new() -> Migrator {
        Migrator {
            migrations: Vec::new(),
            allow_out_of_order: false,
            lock_timeout: DEFAULT_LOCK_TIMEOUT,
        }
    }

    /// Register a migration (builder-style). Order of registration does not
    /// matter; migrations always run sorted by `version`.
    #[allow(clippy::should_implement_trait)] // builder-style `add`, not `Add::add`
    pub fn add(mut self, migration: Migration) -> Migrator {
        self.migrations.push(migration);
        self
    }

    /// Accept a pending migration whose version sorts *before* one already
    /// applied. Off by default: an out-of-order pending migration usually
    /// means a stale branch was merged, and applying it changes a schema that
    /// later migrations already built on — [`run`](Migrator::run) errors and
    /// names the versions instead. Opt in when parallel teams genuinely ship
    /// interleaved versions.
    pub fn allow_out_of_order(mut self) -> Migrator {
        self.allow_out_of_order = true;
        self
    }

    /// How long [`run`](Migrator::run)/[`rollback`](Migrator::rollback) wait
    /// for the `sutegi:migrations` advisory lock before erroring (default
    /// 300 s). Raise it when a fleet's slowest migration outlives it.
    pub fn lock_timeout(mut self, timeout: std::time::Duration) -> Migrator {
        self.lock_timeout = timeout;
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

    /// Reject a malformed migration set before anything touches the database:
    /// empty or non-portable version/name strings, and duplicate versions
    /// (two files, a file shadowing a coded migration, a copy-paste slip) —
    /// running under a duplicate would apply one body but record a version
    /// that ambiguously names two.
    fn validate(&self) -> Result<(), String> {
        let mut seen = std::collections::BTreeMap::new();
        for m in &self.migrations {
            if !valid_ident(&m.version) {
                return Err(format!(
                    "invalid migration version {:?} ({}): use ASCII letters, digits, `_`, `-`, `.`",
                    m.version, m.name
                ));
            }
            if !valid_ident(&m.name) {
                return Err(format!(
                    "invalid migration name {:?} ({}): use ASCII letters, digits, `_`, `-`, `.`",
                    m.name, m.version
                ));
            }
            if let Some(other) = seen.insert(m.version.as_str(), m.name.as_str()) {
                return Err(format!(
                    "duplicate migration version {}: registered as both {:?} and {:?}",
                    m.version, other, m.name
                ));
            }
        }
        Ok(())
    }

    /// Hold the backend's migration lock for the duration of the returned
    /// guard. Backends without advisory locks (`CapScope::None`) proceed
    /// without one — the in-transaction history re-check still prevents
    /// double-apply there.
    ///
    /// Waits by **polling `try_lock`**, never by a server-side blocking wait:
    /// on Postgres a session parked inside `pg_advisory_lock()` holds a
    /// snapshot for the whole wait, and the holder running a
    /// [`no_transaction`](Migration::no_transaction) `CREATE INDEX
    /// CONCURRENTLY` must wait for every such snapshot — a deadlock between
    /// the waiters and the very migration they're waiting on. A polling
    /// waiter is snapshot-free between attempts, so the holder always
    /// finishes.
    fn acquire_lock<B: Backend>(&self, conn: &B) -> Result<Option<LockGuard>, String> {
        if conn.capabilities().advisory_locks == CapScope::None {
            return Ok(None);
        }
        let poll = std::time::Duration::from_millis(200);
        let deadline = std::time::Instant::now() + self.lock_timeout;
        loop {
            if let Some(guard) = conn.try_lock(MIGRATION_LOCK)? {
                return Ok(Some(guard));
            }
            let now = std::time::Instant::now();
            if now >= deadline {
                return Err(format!(
                    "could not acquire the migration lock within {:?} — another \
                     migration runner appears to be active; retry once it finishes, \
                     or raise Migrator::lock_timeout",
                    self.lock_timeout
                ));
            }
            std::thread::sleep(poll.min(deadline - now));
        }
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

    /// The tamper checks run before anything is applied: an already-applied
    /// migration must still hash to its stored checksum (a file edited after
    /// apply) and must still carry its recorded name (a rename after apply).
    /// Both are fixed by restoring the file or, if the change was deliberate,
    /// by [`repair`](Migrator::repair).
    fn check_integrity(&self, applied: &[AppliedRow]) -> Result<(), String> {
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
                if !row.name.is_empty() && row.name != m.name {
                    return Err(format!(
                        "migration {} was renamed after being applied \
                         ({:?} in the history, {:?} in code) — restore the name \
                         or run `migrate repair`",
                        m.version, row.name, m.name
                    ));
                }
            }
        }
        Ok(())
    }

    /// The out-of-order guard: a pending migration sorting before the newest
    /// applied version usually means a stale branch merged late, and its DDL
    /// would run against a schema that later migrations already reshaped.
    /// Rejected unless [`allow_out_of_order`](Migrator::allow_out_of_order).
    ///
    /// Only versions **this migrator defines** anchor the comparison — history
    /// rows from another app sharing the database (or from migrations since
    /// deleted from code) don't make every new migration read as stale.
    fn check_order(
        &self,
        applied: &[AppliedRow],
        done: &std::collections::BTreeSet<&str>,
    ) -> Result<(), String> {
        if self.allow_out_of_order {
            return Ok(());
        }
        let defined: std::collections::BTreeSet<&str> =
            self.migrations.iter().map(|m| m.version.as_str()).collect();
        let newest_applied = match applied
            .iter()
            .map(|r| r.version.as_str())
            .filter(|v| defined.contains(v))
            .max()
        {
            Some(v) => v,
            None => return Ok(()),
        };
        let stale: Vec<&str> = self
            .sorted()
            .iter()
            .filter(|m| !done.contains(m.version.as_str()) && m.version.as_str() < newest_applied)
            .map(|m| m.version.as_str())
            .collect();
        if stale.is_empty() {
            Ok(())
        } else {
            Err(format!(
                "out-of-order migration(s) [{}] sort before the newest applied \
                 version ({newest_applied}) — a stale branch was probably merged; \
                 renumber them, or opt in with Migrator::allow_out_of_order()",
                stale.join(", ")
            ))
        }
    }

    /// True if `version` has a history row — the in-transaction re-check that
    /// makes a lost race a skip instead of a double-apply.
    fn is_applied(&self, conn: &dyn Backend, version: &str) -> Result<bool, String> {
        Ok(!conn
            .query(
                &format!("SELECT version FROM {HISTORY_TABLE} WHERE version = ?"),
                &[Value::Text(version.to_string())],
            )?
            .is_empty())
    }

    /// Apply one pending migration atomically. Returns `false` if a concurrent
    /// runner got there first (seen by the re-check inside the transaction).
    fn apply_one<B: Backend + Transactional>(
        &self,
        conn: &B,
        m: &Migration,
        batch: i64,
        now: i64,
    ) -> Result<bool, String> {
        let record = |ops: &dyn Backend| {
            ops.execute(
                &format!(
                    "INSERT INTO {HISTORY_TABLE} (version, name, batch, checksum, applied_at) \
                     VALUES (?, ?, ?, ?, ?)"
                ),
                &[
                    Value::Text(m.version.clone()),
                    Value::Text(m.name.clone()),
                    Value::Int(batch),
                    Value::Text(m.checksum()),
                    Value::Int(now),
                ],
            )
            .map(|_| ())
        };

        if !m.transactional {
            if self.is_applied(conn, &m.version)? {
                return Ok(false);
            }
            m.run_up(conn)?;
            record(conn)?;
            return Ok(true);
        }

        let mut applied_now = false;
        run_write_tx(conn, &mut |tx| {
            if self.is_applied(tx, &m.version)? {
                return Ok(());
            }
            m.run_up(tx)?;
            record(tx)?;
            applied_now = true;
            Ok(())
        })?;
        Ok(applied_now)
    }

    /// Apply every pending migration in version order, each atomically (its
    /// body and history row in one single-connection transaction). Returns the
    /// versions applied (empty if already up to date).
    ///
    /// Before anything runs, the whole plan is validated — duplicate/malformed
    /// versions, checksum and rename tampering, out-of-order pending
    /// migrations — and the backend's `sutegi:migrations` advisory lock is
    /// held for the duration so concurrent runners (many pods booting at once)
    /// serialize. See the module docs for the full reliability contract.
    pub fn run<B: Backend + Transactional>(&self, conn: &B) -> Result<Vec<String>, String> {
        self.validate()?;
        let _guard = self.acquire_lock(conn)?;
        self.ensure_history(conn)?;
        let applied = self.applied(conn)?;
        self.check_integrity(&applied)?;
        let done: std::collections::BTreeSet<&str> =
            applied.iter().map(|r| r.version.as_str()).collect();
        self.check_order(&applied, &done)?;
        let next_batch = applied.iter().map(|r| r.batch).max().unwrap_or(0) + 1;
        let now = sutegi_crypto::now_secs();

        let mut ran = Vec::new();
        for m in self.sorted() {
            if done.contains(m.version.as_str()) {
                continue;
            }
            let applied_now = self
                .apply_one(conn, m, next_batch, now)
                .map_err(|e| format!("migration {} ({}) failed: {e}", m.version, m.name))?;
            if applied_now {
                ran.push(m.version.clone());
            }
        }
        Ok(ran)
    }

    /// What [`run`](Migrator::run) would do, without doing it: the pending
    /// migrations in apply order, each with the SQL it would execute (rendered
    /// against the live schema for declarative migrations; `None` for closure
    /// bodies, which only exist at run time). Read-only apart from creating
    /// the (empty) history table on a fresh database.
    pub fn plan_run<B: Backend>(&self, conn: &B) -> Result<Vec<PlannedMigration>, String> {
        self.validate()?;
        self.ensure_history(conn)?;
        let applied = self.applied(conn)?;
        let done: std::collections::BTreeSet<&str> =
            applied.iter().map(|r| r.version.as_str()).collect();

        let dialect = conn.dialect();
        let mut shadow = conn.introspect()?;
        let mut out = Vec::new();
        for m in self.sorted() {
            if done.contains(m.version.as_str()) {
                continue;
            }
            let statements = match m.ops_list() {
                Some(ops) => {
                    let mut stmts = Vec::new();
                    for op in ops {
                        stmts.extend(render(op, dialect, &shadow)?);
                        apply(&mut shadow, op)?;
                    }
                    Some(stmts)
                }
                None => None,
            };
            out.push(PlannedMigration {
                version: m.version.clone(),
                name: m.name.clone(),
                statements,
            });
        }
        Ok(out)
    }

    /// Re-stamp the stored checksums and names to match the current migration
    /// files — the escape hatch after a deliberate edit or rename of an
    /// applied migration. Only touches rows that are both applied and still
    /// defined in code.
    pub fn repair<B: Backend>(&self, conn: &B) -> Result<Vec<String>, String> {
        self.validate()?;
        self.ensure_history(conn)?;
        let applied = self.applied(conn)?;
        let mut fixed = Vec::new();
        for m in self.sorted() {
            if let Some(row) = applied.iter().find(|r| r.version == m.version) {
                let current = m.checksum();
                if row.checksum != current || row.name != m.name {
                    conn.execute(
                        &format!(
                            "UPDATE {HISTORY_TABLE} SET checksum = ?, name = ? WHERE version = ?"
                        ),
                        &[
                            Value::Text(current),
                            Value::Text(m.name.clone()),
                            Value::Text(m.version.clone()),
                        ],
                    )?;
                    fixed.push(m.version.clone());
                }
            }
        }
        Ok(fixed)
    }

    /// Roll back the most recent `batches` batch(es), newest first, each
    /// migration atomically (its `down` and its history delete in one
    /// transaction). Returns the versions rolled back.
    ///
    /// Only batches containing at least one version **this migrator defines**
    /// are candidates — another app sharing the database (and the history
    /// table) can't have *its* newest batch picked as this app's rollback
    /// target. The whole batch is then **preflighted before anything is
    /// undone**: if any victim is forward-only or no longer defined in code,
    /// the rollback errors with the database untouched — never a
    /// half-rolled-back batch.
    pub fn rollback<B: Backend + Transactional>(
        &self,
        conn: &B,
        batches: usize,
    ) -> Result<Vec<String>, String> {
        self.validate()?;
        let _guard = self.acquire_lock(conn)?;
        self.ensure_history(conn)?;
        let applied = self.applied(conn)?;
        if applied.is_empty() || batches == 0 {
            return Ok(Vec::new());
        }

        // The `batches` highest distinct batch numbers among batches that
        // contain at least one version defined here (a batch is written by a
        // single run, so this keeps another app's batches out of scope while
        // still surfacing a code-deleted migration inside our own batch).
        let defined: std::collections::BTreeSet<&str> =
            self.migrations.iter().map(|m| m.version.as_str()).collect();
        let mut batch_nums: Vec<i64> = applied
            .iter()
            .filter(|r| defined.contains(r.version.as_str()))
            .map(|r| r.batch)
            .collect();
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

        // Preflight the whole batch before touching anything.
        let mut plan: Vec<(&AppliedRow, &Migration)> = Vec::with_capacity(victims.len());
        for row in victims {
            let migration = self
                .migrations
                .iter()
                .find(|m| m.version == row.version)
                .ok_or_else(|| {
                    format!(
                        "cannot roll back {}: no such migration in code — nothing was rolled back",
                        row.version
                    )
                })?;
            if !migration.reversible_migration() {
                return Err(format!(
                    "cannot roll back {} ({}): migration is forward-only — nothing was rolled back",
                    row.version, row.name
                ));
            }
            plan.push((row, migration));
        }

        let mut rolled = Vec::new();
        for (row, migration) in plan {
            self.rollback_one(conn, migration)
                .map_err(|e| format!("rollback of {} ({}) failed: {e}", row.version, row.name))?;
            rolled.push(row.version.clone());
        }
        Ok(rolled)
    }

    /// Undo one migration atomically, skipping if a concurrent runner already
    /// removed its history row.
    fn rollback_one<B: Backend + Transactional>(
        &self,
        conn: &B,
        m: &Migration,
    ) -> Result<(), String> {
        let erase = |ops: &dyn Backend| {
            ops.execute(
                &format!("DELETE FROM {HISTORY_TABLE} WHERE version = ?"),
                &[Value::Text(m.version.clone())],
            )
            .map(|_| ())
        };

        if !m.transactional {
            if !self.is_applied(conn, &m.version)? {
                return Ok(());
            }
            m.run_down(conn)?;
            return erase(conn);
        }

        run_write_tx(conn, &mut |tx| {
            if !self.is_applied(tx, &m.version)? {
                return Ok(());
            }
            m.run_down(tx)?;
            erase(tx)
        })
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
                        ("transactional", Json::Bool(m.transactional)),
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
    pub fn shadow_via<B: Backend + Transactional>(
        &self,
        scratch: &B,
    ) -> Result<Vec<TableSchema>, String> {
        self.run(scratch)?;
        Ok(normalize_all(scratch.introspect()?))
    }
}

/// Run `f` in a real single-connection transaction, taking the write lock up
/// front where the backend can express it (`BEGIN IMMEDIATE` on SQLite via
/// `Isolation::RepeatableRead`) so a cross-process racer serializes at BEGIN
/// instead of deadlocking on a mid-transaction lock upgrade. Backends without
/// isolation levels get a plain transaction.
fn run_write_tx<B: Backend + Transactional>(
    conn: &B,
    f: &mut dyn FnMut(&dyn Backend) -> Result<(), String>,
) -> Result<(), String> {
    if conn.capabilities().isolation_levels {
        conn.run_in_tx_with(Isolation::RepeatableRead, f)
    } else {
        conn.run_in_tx(f)
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
pub fn generate_via<B: Backend + Transactional>(
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

/// A [`SchemaOp`] is a drop — the ops dev-mode [`sync`] refuses to run so it can
/// never destroy data (extra columns/tables in the database are left alone).
fn is_drop(op: &SchemaOp) -> bool {
    matches!(
        op,
        SchemaOp::DropTable(_)
            | SchemaOp::DropColumn { .. }
            | SchemaOp::DropIndex { .. }
            | SchemaOp::DropForeignKey { .. }
    )
}

/// **Dev-mode schema sync**: bring the database's `desired` tables up to date
/// with *additive, non-destructive* changes only — create missing tables, add
/// columns/indexes/foreign keys, apply safe column widenings. Returns the
/// summaries of what it applied.
///
/// The whole sync — introspection, planning, and every op — runs inside one
/// single-connection transaction, so a failure (or a crash mid-way through a
/// SQLite table rebuild) leaves the database exactly as it was.
///
/// It never drops anything (extra columns and tables are left untouched), and it
/// refuses — with an error pointing at `migrate gen` — any change that could
/// lose data or fail on existing rows (a `NOT NULL` column with no default, a
/// lossy type change). This is the honest replacement for the old
/// create-if-missing [`Model::migrate`](crate::Model::migrate): a convenience
/// for local iteration, not a substitute for reviewed migrations in production.
pub fn sync<B: Backend + Transactional>(
    conn: &B,
    desired: &[TableSchema],
) -> Result<Vec<String>, String> {
    conn.transact(|tx| sync_in_tx(tx, desired))
}

fn sync_in_tx(conn: &dyn Backend, desired: &[TableSchema]) -> Result<Vec<String>, String> {
    let dialect = conn.dialect();
    let live = conn.introspect()?;

    // Only diff the tables the caller cares about — never propose dropping a
    // table that simply isn't in this model set.
    let wanted: std::collections::BTreeSet<&str> =
        desired.iter().map(|t| t.table.as_str()).collect();
    let current: Vec<TableSchema> = live
        .iter()
        .filter(|t| wanted.contains(t.table.as_str()))
        .cloned()
        .collect();

    let plan = diff(&current, &normalize_all(desired.to_vec()), dialect);

    // Anything unsafe that isn't a (skipped) drop blocks the sync.
    let blocked: Vec<String> = plan
        .ops
        .iter()
        .filter(|op| !is_drop(op) && op.safety() != crate::schema_diff::Safety::Safe)
        .map(|op| format!("  - {}", op.summary()))
        .collect();
    if !blocked.is_empty() {
        return Err(format!(
            "sync can only apply safe additive changes; these need a real migration \
             (run `migrate gen`):\n{}",
            blocked.join("\n")
        ));
    }

    // Apply the safe, non-drop ops, threading the live schema as rebuild context.
    let mut shadow = live;
    let mut applied = Vec::new();
    for op in &plan.ops {
        if is_drop(op) || op.safety() != crate::schema_diff::Safety::Safe {
            continue;
        }
        for sql in render(op, dialect, &shadow)? {
            conn.execute(&sql, &[])?;
        }
        apply(&mut shadow, op)?;
        applied.push(op.summary());
    }
    Ok(applied)
}

/// Single-table [`sync`] — the engine behind the reimplemented
/// [`Model::migrate`](crate::Model::migrate).
pub fn sync_table<B: Backend + Transactional>(
    conn: &B,
    schema: &TableSchema,
) -> Result<(), String> {
    sync(conn, std::slice::from_ref(schema)).map(|_| ())
}

/// A three-way drift report comparing the models, the migration history's
/// shadow schema, and the live database.
#[derive(Clone, Debug)]
pub struct DriftReport {
    /// The DB diverges from what the migrations say it should be (a hand-edit,
    /// or migrations not fully applied). Empty ⇒ in sync.
    pub db_vs_migrations: Plan,
    /// The models have changes the migrations don't capture yet — you need to
    /// `migrate gen`. Empty ⇒ no pending model changes.
    pub models_vs_migrations: Plan,
}

impl DriftReport {
    /// True when everything agrees: DB matches migrations and models match too.
    pub fn is_clean(&self) -> bool {
        self.db_vs_migrations.is_empty() && self.models_vs_migrations.is_empty()
    }

    /// A machine-readable summary, for `/__migrations` and `migrate drift`.
    pub fn to_json(&self) -> Json {
        let ops = |p: &Plan| Json::arr(p.ops.iter().map(|o| Json::str(o.summary())).collect());
        Json::obj(vec![
            ("clean", Json::Bool(self.is_clean())),
            ("db_vs_migrations", ops(&self.db_vs_migrations)),
            ("models_vs_migrations", ops(&self.models_vs_migrations)),
        ])
    }
}

/// Compute drift: diff the shadow schema (migration history) against both the
/// live database and the models. The shadow is folded purely from ops
/// migrations; pass a scratch backend via [`Migrator::shadow_via`] first if you
/// have closures (then call [`drift_with_shadow`]).
pub fn drift<B: Backend>(
    conn: &B,
    migrator: &Migrator,
    models: &[TableSchema],
) -> Result<DriftReport, String> {
    let shadow = migrator.shadow()?;
    drift_with_shadow(conn, &shadow, models)
}

/// Drift against a precomputed shadow (so callers with closure migrations can
/// supply a replayed one).
pub fn drift_with_shadow<B: Backend>(
    conn: &B,
    shadow: &[TableSchema],
    models: &[TableSchema],
) -> Result<DriftReport, String> {
    let dialect = conn.dialect();
    let live = normalize_all(conn.introspect()?);
    // Compare only the tables the migrations manage, so unrelated tables (a KV
    // store, the events log) don't read as drift.
    let managed: std::collections::BTreeSet<&str> =
        shadow.iter().map(|t| t.table.as_str()).collect();
    let live_managed: Vec<TableSchema> = live
        .iter()
        .filter(|t| managed.contains(t.table.as_str()))
        .cloned()
        .collect();
    Ok(DriftReport {
        db_vs_migrations: diff(shadow, &live_managed, dialect),
        models_vs_migrations: diff(shadow, &normalize_all(models.to_vec()), dialect),
    })
}

/// Write a declarative migration to `<dir>/<version>_<name>.json` (creating the
/// directory if needed) and return the path. Errors for a closure migration,
/// and for a version/name that isn't a plain identifier (which could otherwise
/// escape `dir`).
pub fn write_migration_file(dir: &str, migration: &Migration) -> Result<String, String> {
    if !valid_ident(migration.version()) || !valid_ident(migration.name()) {
        return Err(format!(
            "cannot write migration {:?} ({:?}): version and name must be ASCII \
             letters, digits, `_`, `-`, `.`",
            migration.version(),
            migration.name()
        ));
    }
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
        assert_eq!(
            arr[0].get("transactional").and_then(Json::as_bool),
            Some(true)
        );
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

    // ---- P6: dev-mode sync + drift ----

    #[test]
    fn sync_creates_then_adds_columns_additively() {
        let db = Db::memory().unwrap();

        // First sync creates the table.
        let applied = sync(&db, &[todos_v1()]).unwrap();
        assert_eq!(applied.len(), 1);
        Backend::execute(&db, "INSERT INTO todos (title) VALUES ('x')", &[]).unwrap();

        // Second sync adds the new (defaulted) column — the old silent-noop bug.
        let applied = sync(&db, &[todos_v2()]).unwrap();
        assert_eq!(applied.len(), 1);
        assert!(applied[0].contains("add column todos.done"));
        assert_eq!(db.introspect().unwrap()[0], todos_v2().normalized());
        // Idempotent: a third sync does nothing.
        assert!(sync(&db, &[todos_v2()]).unwrap().is_empty());
    }

    #[test]
    fn sync_refuses_a_not_null_column_without_default() {
        let db = Db::memory().unwrap();
        sync(&db, &[todos_v1()]).unwrap();
        Backend::execute(&db, "INSERT INTO todos (title) VALUES ('x')", &[]).unwrap();

        // A required column with no default can't be synced onto a populated table.
        let needs_migration = TableSchema::new("todos")
            .column(Column::new("id", ColType::Integer).primary())
            .column(Column::new("title", ColType::Text))
            .column(Column::new("owner", ColType::Text));
        let err = sync(&db, &[needs_migration]).unwrap_err();
        assert!(err.contains("migrate gen"), "got: {err}");
    }

    #[test]
    fn sync_leaves_extra_db_columns_alone() {
        let db = Db::memory().unwrap();
        sync(&db, &[todos_v2()]).unwrap();
        // The model is now a subset (no `done`) — sync must not drop the column.
        let applied = sync(&db, &[todos_v1()]).unwrap();
        assert!(applied.is_empty());
        assert!(db.introspect().unwrap()[0].col("done").is_some());
    }

    #[test]
    fn drift_flags_pending_model_changes_and_hand_edits() {
        let db = Db::memory().unwrap();
        let v1 = crate::schema_diff::diff(&[], &[todos_v1()], Dialect::Sqlite);
        let migrator = Migrator::new().add(Migration::ops("0001_todos", "create_todos", v1.ops));
        migrator.run(&db).unwrap();

        // DB matches migrations, models match migrations → clean.
        let report = drift(&db, &migrator, &[todos_v1()]).unwrap();
        assert!(report.is_clean(), "{:?}", report);

        // Model gained a field but no migration generated → models-vs-migrations drift.
        let report = drift(&db, &migrator, &[todos_v2()]).unwrap();
        assert!(report.db_vs_migrations.is_empty());
        assert!(!report.models_vs_migrations.is_empty());

        // Someone hand-edits the DB → db-vs-migrations drift.
        Backend::execute(&db, "ALTER TABLE todos ADD COLUMN sneaky TEXT", &[]).unwrap();
        let report = drift(&db, &migrator, &[todos_v1()]).unwrap();
        assert!(!report.db_vs_migrations.is_empty());
    }

    // ---- reliability guard rails ----

    #[test]
    fn duplicate_version_is_rejected_before_running() {
        let db = Db::memory().unwrap();
        let m = Migrator::new()
            .add(Migration::new("0001_x", "first", |_| Ok(())))
            .add(Migration::new("0001_x", "second", |_| Ok(())));
        let err = m.run(&db).unwrap_err();
        assert!(err.contains("duplicate migration version"), "got: {err}");
        // Nothing ran, not even the history table row.
        assert!(m.status(&db).is_ok());
    }

    #[test]
    fn malformed_version_is_rejected() {
        let db = Db::memory().unwrap();
        for bad in ["", "0001/evil", "0001 x", "0001;drop"] {
            let m = Migrator::new().add(Migration::new(bad, "n", |_| Ok(())));
            let err = m.run(&db).unwrap_err();
            assert!(err.contains("invalid migration version"), "{bad}: {err}");
        }
    }

    #[test]
    fn write_migration_file_rejects_path_escapes() {
        let plan = crate::schema_diff::diff(&[], &[todos_v1()], Dialect::Sqlite);
        let m = Migration::ops("../../0001", "create_todos", plan.ops);
        let err = write_migration_file("/tmp/nowhere", &m).unwrap_err();
        assert!(err.contains("version and name"), "got: {err}");
    }

    #[test]
    fn out_of_order_pending_is_rejected_unless_opted_in() {
        let db = Db::memory().unwrap();
        Migrator::new()
            .add(Migration::new("0002_later", "later", |db| {
                db.execute("CREATE TABLE later (id INTEGER PRIMARY KEY)", &[])
                    .map(|_| ())
            }))
            .run(&db)
            .unwrap();

        // A stale branch lands 0001 after 0002 is already applied.
        let stale = Migration::new("0001_stale", "stale", |db| {
            db.execute("CREATE TABLE stale (id INTEGER PRIMARY KEY)", &[])
                .map(|_| ())
        });
        let m = Migrator::new()
            .add(Migration::new("0002_later", "later", |_| Ok(())))
            .add(stale);
        let err = m.run(&db).unwrap_err();
        assert!(err.contains("out-of-order"), "got: {err}");
        assert!(err.contains("0001_stale"), "got: {err}");
        // The stale migration did not run.
        assert!(db.select(&QueryBuilder::table("stale")).is_err());

        // Explicit opt-in applies it.
        let m = Migrator::new()
            .add(Migration::new("0002_later", "later", |_| Ok(())))
            .add(Migration::new("0001_stale", "stale", |db| {
                db.execute("CREATE TABLE stale (id INTEGER PRIMARY KEY)", &[])
                    .map(|_| ())
            }))
            .allow_out_of_order();
        assert_eq!(m.run(&db).unwrap(), vec!["0001_stale"]);
    }

    #[test]
    fn renamed_applied_migration_trips_the_guard_and_repair_fixes_it() {
        let db = Db::memory().unwrap();
        Migrator::new()
            .add(Migration::new("0001_x", "old_name", |_| Ok(())))
            .run(&db)
            .unwrap();

        let renamed = Migrator::new().add(Migration::new("0001_x", "new_name", |_| Ok(())));
        let err = renamed.run(&db).unwrap_err();
        assert!(err.contains("renamed after being applied"), "got: {err}");

        assert_eq!(renamed.repair(&db).unwrap(), vec!["0001_x"]);
        assert!(renamed.run(&db).unwrap().is_empty());
        let status = renamed.status(&db).unwrap();
        assert_eq!(status[0].name, "new_name");
        assert!(!status[0].orphan);
    }

    #[test]
    fn rollback_preflights_the_whole_batch_before_undoing_anything() {
        let db = Db::memory().unwrap();
        // One batch: a forward-only migration below a reversible one.
        let m = Migrator::new()
            .add(Migration::new("0001_forward", "forward", |db| {
                db.execute("CREATE TABLE fwd (id INTEGER PRIMARY KEY)", &[])
                    .map(|_| ())
            }))
            .add(Migration::reversible(
                "0002_rev",
                "rev",
                |db| {
                    db.execute("CREATE TABLE rev (id INTEGER PRIMARY KEY)", &[])
                        .map(|_| ())
                },
                |db| db.execute("DROP TABLE rev", &[]).map(|_| ()),
            ));
        m.run(&db).unwrap();

        // 0002 would be undone first — but 0001 is forward-only, so the
        // preflight must refuse with NOTHING rolled back (0002 still applied).
        let err = m.rollback(&db, 1).unwrap_err();
        assert!(err.contains("forward-only"), "got: {err}");
        assert!(db.select(&QueryBuilder::table("rev")).is_ok());
        let status = m.status(&db).unwrap();
        assert!(status.iter().all(|s| s.applied), "{status:?}");
    }

    #[test]
    fn rollback_refuses_a_code_deleted_migration_without_undoing_others() {
        let db = Db::memory().unwrap();
        migrator().run(&db).unwrap();

        // Code lost 0001_create_users; its history row is now an orphan.
        let partial = Migrator::new().add(Migration::reversible(
            "0002_add_posts",
            "add_posts",
            |_| Ok(()),
            |db| db.execute("DROP TABLE posts", &[]).map(|_| ()),
        ));
        let err = partial.rollback(&db, 1).unwrap_err();
        assert!(err.contains("no such migration in code"), "got: {err}");
        // 0002 was NOT rolled back on the way to discovering the orphan.
        assert!(db.select(&QueryBuilder::table("posts")).is_ok());
    }

    #[test]
    fn rollback_targets_only_this_migrators_batches() {
        // Two apps share one database (and one history table). App B's
        // rollback must pick B's newest batch, not A's globally-newest one.
        let db = Db::memory().unwrap();
        let app_a = Migrator::new().add(Migration::reversible(
            "a_0001",
            "a1",
            |db| {
                db.execute("CREATE TABLE a1 (id INTEGER PRIMARY KEY)", &[])
                    .map(|_| ())
            },
            |db| db.execute("DROP TABLE a1", &[]).map(|_| ()),
        ));
        let app_b = Migrator::new().add(Migration::reversible(
            "b_0001",
            "b1",
            |db| {
                db.execute("CREATE TABLE b1 (id INTEGER PRIMARY KEY)", &[])
                    .map(|_| ())
            },
            |db| db.execute("DROP TABLE b1", &[]).map(|_| ()),
        ));
        app_b.run(&db).unwrap();
        app_a.run(&db).unwrap(); // batch 2 — the globally newest

        // B rolls back: undoes b_0001 (batch 1), leaves A's batch 2 alone.
        assert_eq!(app_b.rollback(&db, 1).unwrap(), vec!["b_0001"]);
        assert!(db.select(&QueryBuilder::table("a1")).is_ok());
        assert!(db.select(&QueryBuilder::table("b1")).is_err());
        let a_status = app_a.status(&db).unwrap();
        let a1 = a_status.iter().find(|s| s.version == "a_0001").unwrap();
        assert!(a1.applied);
    }

    #[test]
    fn plan_run_previews_sql_without_executing() {
        let db = Db::memory().unwrap();
        let v1 = crate::schema_diff::diff(&[], &[todos_v1()], Dialect::Sqlite);
        let m = Migrator::new()
            .add(Migration::ops("0001_todos", "create_todos", v1.ops))
            .add(Migration::new("0002_backfill", "backfill", |_| Ok(())));

        let plan = m.plan_run(&db).unwrap();
        assert_eq!(plan.len(), 2);
        assert_eq!(plan[0].version, "0001_todos");
        let stmts = plan[0].statements.as_ref().unwrap();
        assert!(
            stmts
                .iter()
                .any(|s| s.contains("CREATE TABLE") && s.contains("todos")),
            "{stmts:?}"
        );
        // Closure bodies have no renderable SQL.
        assert!(plan[1].statements.is_none());

        // Nothing executed: the table does not exist, nothing is applied.
        assert!(db.select(&QueryBuilder::table("todos")).is_err());
        assert!(m.status(&db).unwrap().iter().all(|s| !s.applied));

        // After running, the plan is empty.
        m.run(&db).unwrap();
        assert!(m.plan_run(&db).unwrap().is_empty());
    }

    #[test]
    fn no_transaction_migration_applies_and_reruns_after_partial_failure() {
        let db = Db::memory().unwrap();
        let ok = Migrator::new().add(
            Migration::new("0001_idem", "idem", |db| {
                db.execute(
                    "CREATE TABLE IF NOT EXISTS idem (id INTEGER PRIMARY KEY)",
                    &[],
                )
                .map(|_| ())
            })
            .no_transaction(),
        );
        assert_eq!(ok.run(&db).unwrap(), vec!["0001_idem"]);
        assert!(ok.run(&db).unwrap().is_empty());

        // A failing non-transactional migration keeps its side effects (the
        // documented trade) but records nothing, so the fixed body re-runs.
        let boom = Migrator::new().add(
            Migration::new("0002_boom", "boom", |db| {
                db.execute(
                    "CREATE TABLE IF NOT EXISTS half (id INTEGER PRIMARY KEY)",
                    &[],
                )?;
                Err("deliberate".into())
            })
            .no_transaction(),
        );
        assert!(boom.run(&db).is_err());
        assert!(db.select(&QueryBuilder::table("half")).is_ok());
        let fixed = Migrator::new().add(
            Migration::new("0002_boom", "boom", |db| {
                db.execute(
                    "CREATE TABLE IF NOT EXISTS half (id INTEGER PRIMARY KEY)",
                    &[],
                )
                .map(|_| ())
            })
            .no_transaction(),
        );
        assert_eq!(fixed.run(&db).unwrap(), vec!["0002_boom"]);
    }

    #[test]
    fn migration_ops_exposes_the_dialect() {
        let db = Db::memory().unwrap();
        let m = Migrator::new().add(Migration::new("0001_d", "dialect_probe", |ops| {
            if ops.dialect() != Dialect::Sqlite {
                return Err("expected sqlite".into());
            }
            Ok(())
        }));
        m.run(&db).unwrap();
    }
}
