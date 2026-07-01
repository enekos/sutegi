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
use crate::value::{TableSchema, Value};
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

/// A single reversible (or forward-only) migration.
pub struct Migration {
    version: &'static str,
    name: &'static str,
    up: MigrationFn,
    down: Option<MigrationFn>,
}

impl Migration {
    /// A forward-only migration (no `down`; [`Migrator::rollback`] will refuse it).
    pub fn new(version: &'static str, name: &'static str, up: MigrationFn) -> Migration {
        Migration {
            version,
            name,
            up,
            down: None,
        }
    }

    /// A reversible migration with both `up` and `down`.
    pub fn reversible(
        version: &'static str,
        name: &'static str,
        up: MigrationFn,
        down: MigrationFn,
    ) -> Migration {
        Migration {
            version,
            name,
            up,
            down: Some(down),
        }
    }

    pub fn version(&self) -> &str {
        self.version
    }
    pub fn name(&self) -> &str {
        self.name
    }
    pub fn reversible_migration(&self) -> bool {
        self.down.is_some()
    }
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

    /// The registered migrations, sorted by version.
    fn sorted(&self) -> Vec<&Migration> {
        let mut v: Vec<&Migration> = self.migrations.iter().collect();
        v.sort_by_key(|m| m.version);
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
                    applied_at INTEGER NOT NULL)"
            ),
            &[],
        )
        .map(|_| ())
    }

    /// `version -> batch` for every applied migration.
    fn applied<B: Backend>(&self, conn: &B) -> Result<Vec<(String, String, i64)>, String> {
        let rows = conn.query(
            &format!("SELECT version, name, batch FROM {HISTORY_TABLE}"),
            &[],
        )?;
        let mut out = Vec::with_capacity(rows.len());
        for r in &rows {
            let version = r
                .get("version")
                .and_then(Json::as_str)
                .ok_or("migration row missing version")?
                .to_string();
            let name = r
                .get("name")
                .and_then(Json::as_str)
                .unwrap_or("")
                .to_string();
            let batch = r.get("batch").and_then(Json::as_i64).unwrap_or(0);
            out.push((version, name, batch));
        }
        Ok(out)
    }

    /// Apply every pending migration in version order, each in its own
    /// transaction. Returns the versions applied (empty if already up to date).
    pub fn run<B: Backend>(&self, conn: &B) -> Result<Vec<String>, String> {
        self.ensure_history(conn)?;
        let applied = self.applied(conn)?;
        let done: std::collections::BTreeSet<&str> =
            applied.iter().map(|(v, _, _)| v.as_str()).collect();
        let next_batch = applied.iter().map(|(_, _, b)| *b).max().unwrap_or(0) + 1;
        let now = now_epoch();

        let mut ran = Vec::new();
        for m in self.sorted() {
            if done.contains(m.version) {
                continue;
            }
            in_transaction(conn, || {
                (m.up)(conn)?;
                conn.execute(
                    &format!(
                        "INSERT INTO {HISTORY_TABLE} (version, name, batch, applied_at) \
                         VALUES (?, ?, ?, ?)"
                    ),
                    &[
                        Value::Text(m.version.to_string()),
                        Value::Text(m.name.to_string()),
                        Value::Int(next_batch),
                        Value::Int(now),
                    ],
                )?;
                Ok(())
            })
            .map_err(|e| format!("migration {} ({}) failed: {e}", m.version, m.name))?;
            ran.push(m.version.to_string());
        }
        Ok(ran)
    }

    /// Roll back the most recent `batches` batch(es), newest first. Each
    /// migration's `down` runs in its own transaction; a forward-only migration
    /// aborts the rollback. Returns the versions rolled back.
    pub fn rollback<B: Backend>(&self, conn: &B, batches: usize) -> Result<Vec<String>, String> {
        self.ensure_history(conn)?;
        let applied = self.applied(conn)?;
        if applied.is_empty() || batches == 0 {
            return Ok(Vec::new());
        }

        // The `batches` highest distinct batch numbers.
        let mut batch_nums: Vec<i64> = applied.iter().map(|(_, _, b)| *b).collect();
        batch_nums.sort_unstable();
        batch_nums.dedup();
        let target: std::collections::BTreeSet<i64> =
            batch_nums.into_iter().rev().take(batches).collect();

        // Versions to undo, newest first (version order is the apply order).
        let mut victims: Vec<&(String, String, i64)> = applied
            .iter()
            .filter(|(_, _, b)| target.contains(b))
            .collect();
        victims.sort_by(|a, b| b.0.cmp(&a.0));

        let by_version = |v: &str| self.migrations.iter().find(|m| m.version == v);

        let mut rolled = Vec::new();
        for (version, name, _) in victims {
            let migration = by_version(version)
                .ok_or_else(|| format!("cannot roll back {version}: no such migration in code"))?;
            let down = migration.down.ok_or_else(|| {
                format!("cannot roll back {version} ({name}): migration is forward-only")
            })?;
            in_transaction(conn, || {
                down(conn)?;
                conn.execute(
                    &format!("DELETE FROM {HISTORY_TABLE} WHERE version = ?"),
                    &[Value::Text(version.clone())],
                )?;
                Ok(())
            })
            .map_err(|e| format!("rollback of {version} ({name}) failed: {e}"))?;
            rolled.push(version.clone());
        }
        Ok(rolled)
    }

    /// The status of every migration — code-defined and orphaned — sorted by
    /// version.
    pub fn status<B: Backend>(&self, conn: &B) -> Result<Vec<MigrationStatus>, String> {
        self.ensure_history(conn)?;
        let applied = self.applied(conn)?;
        let batch_of = |v: &str| {
            applied
                .iter()
                .find(|(ver, _, _)| ver == v)
                .map(|(_, _, b)| *b)
        };

        let mut out: Vec<MigrationStatus> = self
            .sorted()
            .iter()
            .map(|m| MigrationStatus {
                version: m.version.to_string(),
                name: m.name.to_string(),
                applied: batch_of(m.version).is_some(),
                batch: batch_of(m.version),
                orphan: false,
            })
            .collect();

        let defined: std::collections::BTreeSet<&str> =
            self.migrations.iter().map(|m| m.version).collect();
        for (version, name, batch) in &applied {
            if !defined.contains(version.as_str()) {
                out.push(MigrationStatus {
                    version: version.clone(),
                    name: name.clone(),
                    applied: true,
                    batch: Some(*batch),
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
                        ("version", Json::str(m.version)),
                        ("name", Json::str(m.name)),
                        ("reversible", Json::Bool(m.down.is_some())),
                    ])
                })
                .collect(),
        )
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

/// Seconds since the Unix epoch (stored in the history table's `applied_at`).
fn now_epoch() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
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
}
