//! The execution seam. [`Backend`] is the one trait every runnable store
//! implements — bundled SQLite ([`crate::db::Db`], `sqlite` feature), pure-std
//! Postgres ([`crate::pg::Pg`], `postgres` feature), and a Postgres transaction
//! handle ([`crate::pg::Tx`]). [`Model`] is written once against `Backend`, so
//! the same app code runs on any of them — **swap the backend, not the call
//! sites**.
//!
//! The trait is deliberately small: five **required primitives** each backend
//! must provide (they differ by SQL dialect), and a set of **default methods**
//! (`select`/`count`/`exists`/`paginate`/…) implemented once on top of them.
//! That keeps the read/write surface identical across backends with zero
//! per-backend duplication.

use crate::builder::{DeleteBuilder, Page, QueryBuilder, UpdateBuilder};
use crate::value::{TableSchema, Value};
use sutegi_json::Json;

/// How far a coordination guarantee reaches.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CapScope {
    /// Not available on this backend.
    None,
    /// Holds within one OS process (e.g. SQLite named locks).
    Process,
    /// Holds across every pod sharing the database (e.g. Postgres advisory
    /// locks).
    Cluster,
}

impl CapScope {
    /// The stable string form used in `/__introspect`.
    pub fn as_str(self) -> &'static str {
        match self {
            CapScope::None => "none",
            CapScope::Process => "process",
            CapScope::Cluster => "cluster",
        }
    }
}

/// What a [`Backend`] can actually do beyond the core query/execute surface —
/// coordination, DML extras, documents/search, realtime. Callers (and agents,
/// via the `capabilities` block in `/__introspect`) read this **before**
/// reaching for a feature instead of finding out from a dialect SQL error.
///
/// A capability describes the *framework surface*, not the underlying C
/// library: SQLite ships JSON1/FTS5/RETURNING, but the bit stays off until
/// sutegi exposes the feature through the builder/`Backend`.
#[derive(Clone, Debug, PartialEq)]
pub struct BackendCaps {
    /// Which store this is: `"sqlite"`, `"postgres"`, or an impl-defined name.
    pub backend: &'static str,
    // --- coordination ---
    /// Named advisory locks (`lock`/`try_lock`) and how far they reach.
    pub advisory_locks: CapScope,
    /// Row-level locking clauses (`FOR UPDATE` / `FOR SHARE`).
    pub row_locks: bool,
    /// `SKIP LOCKED` / `NOWAIT` on row-locking reads.
    pub skip_locked: bool,
    /// Explicit isolation levels on transactions.
    pub isolation_levels: bool,
    // --- dml ---
    /// `RETURNING` on UPDATE / DELETE builders.
    pub returning_dml: bool,
    /// A native bulk-insert path (e.g. Postgres `COPY FROM STDIN`).
    pub bulk_copy: bool,
    // --- documents & search ---
    /// JSON path queries (`where_json` / `select_json`).
    pub json_path: bool,
    /// JSON containment (`@>`-style `where_json_contains`).
    pub json_contains: bool,
    /// Full-text search (`tsvector` / FTS5) through the builder.
    pub fts: bool,
    // --- realtime & vectors ---
    /// `LISTEN`/`NOTIFY`-style push wakeups.
    pub listen_notify: bool,
    /// Vector (embedding) columns and nearest-neighbor pushdown.
    pub vector: bool,
    /// Watched queries (`watch()`) and how far change delivery reaches.
    pub live_queries: CapScope,
}

impl BackendCaps {
    /// Everything off — the honest default for a backend that has declared
    /// nothing. Named overrides start from this and flip what they support.
    pub fn none(backend: &'static str) -> BackendCaps {
        BackendCaps {
            backend,
            advisory_locks: CapScope::None,
            row_locks: false,
            skip_locked: false,
            isolation_levels: false,
            returning_dml: false,
            bulk_copy: false,
            json_path: false,
            json_contains: false,
            fts: false,
            listen_notify: false,
            vector: false,
            live_queries: CapScope::None,
        }
    }

    /// The stable JSON form served under `capabilities` in `/__introspect`.
    pub fn to_json(&self) -> Json {
        Json::obj(vec![
            ("advisory_locks", Json::str(self.advisory_locks.as_str())),
            ("backend", Json::str(self.backend)),
            ("bulk_copy", Json::Bool(self.bulk_copy)),
            ("fts", Json::Bool(self.fts)),
            ("isolation_levels", Json::Bool(self.isolation_levels)),
            ("json_contains", Json::Bool(self.json_contains)),
            ("json_path", Json::Bool(self.json_path)),
            ("listen_notify", Json::Bool(self.listen_notify)),
            ("live_queries", Json::str(self.live_queries.as_str())),
            ("returning_dml", Json::Bool(self.returning_dml)),
            ("row_locks", Json::Bool(self.row_locks)),
            ("skip_locked", Json::Bool(self.skip_locked)),
            ("vector", Json::Bool(self.vector)),
        ])
    }
}

/// The uniform error for reaching past a backend's capabilities — features
/// gate on [`BackendCaps`] and return this instead of a dialect SQL error.
pub fn unsupported(capability: &str, backend: &str) -> String {
    format!("unsupported: {capability} is not available on the {backend} backend")
}

/// A held advisory lock. Dropping the guard releases the lock — on Postgres by
/// closing the dedicated session that holds it (which is also what makes
/// crash-release automatic), on SQLite by removing the registry entry.
///
/// Exception: a guard from a Postgres *transaction* handle
/// (`pg_advisory_xact_lock`) releases at COMMIT/ROLLBACK, not at drop — see
/// the `Tx` impl.
pub struct LockGuard {
    name: String,
    /// Whatever keeps the lock alive; dropping it releases. A dedicated PG
    /// [`Client`], a registry token, …
    _hold: Box<dyn std::any::Any + Send>,
}

impl LockGuard {
    /// Wrap a backend-specific hold. For backend implementors.
    pub fn new(name: &str, hold: Box<dyn std::any::Any + Send>) -> LockGuard {
        LockGuard {
            name: name.to_string(),
            _hold: hold,
        }
    }

    /// The lock's name as acquired.
    pub fn name(&self) -> &str {
        &self.name
    }
}

impl std::fmt::Debug for LockGuard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LockGuard")
            .field("name", &self.name)
            .finish()
    }
}

/// The 64-bit key a named lock maps to on Postgres: the first 8 bytes of
/// SHA-256(name), big-endian. Public so an operator can inspect or take the
/// same lock from psql: `SELECT pg_try_advisory_lock(<key>)`.
pub fn lock_key(name: &str) -> i64 {
    let digest = sutegi_crypto::sha256(name.as_bytes());
    i64::from_be_bytes(digest[..8].try_into().expect("8 bytes"))
}

/// How often the default polling [`lock`](Backend::lock) re-attempts.
const LOCK_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(50);

/// Render a requested row-lock clause for a backend, or gate it. Backends
/// without row locks treat plain `FOR UPDATE`/`FOR SHARE` as a documented
/// no-op (a SQLite write transaction already holds the whole-database lock —
/// strictly coarser), but `SKIP LOCKED`/`NOWAIT` **error**: their entire point
/// is altered semantics under contention, which such a backend cannot express.
fn row_lock_sql(lock: crate::builder::RowLock, caps: &BackendCaps) -> Result<String, String> {
    use crate::builder::LockWait;
    if !caps.row_locks {
        return match lock.wait {
            LockWait::Wait => Ok(String::new()),
            LockWait::SkipLocked => Err(unsupported("skip_locked", caps.backend)),
            LockWait::NoWait => Err(unsupported("nowait", caps.backend)),
        };
    }
    let mut sql = String::from(if lock.exclusive {
        " FOR UPDATE"
    } else {
        " FOR SHARE"
    });
    match lock.wait {
        LockWait::Wait => {}
        LockWait::SkipLocked if caps.skip_locked => sql.push_str(" SKIP LOCKED"),
        LockWait::NoWait if caps.skip_locked => sql.push_str(" NOWAIT"),
        LockWait::SkipLocked => return Err(unsupported("skip_locked", caps.backend)),
        LockWait::NoWait => return Err(unsupported("nowait", caps.backend)),
    }
    Ok(sql)
}

/// Validate a bulk-insert call's identifiers and row widths — shared by the
/// default [`Backend::insert_many`] and the Postgres COPY override.
pub(crate) fn check_bulk_shape(
    table: &str,
    cols: &[&str],
    rows: &[Vec<Value>],
) -> Result<(), String> {
    if cols.is_empty() {
        return Err("insert_many: no columns".into());
    }
    let named: Vec<(&str, Value)> = cols.iter().map(|c| (*c, Value::Null)).collect();
    crate::builder::validate_write_idents(table, &named, &[])?;
    if let Some((i, row)) = rows.iter().enumerate().find(|(_, r)| r.len() != cols.len()) {
        return Err(format!(
            "insert_many: row {i} has {} values, expected {}",
            row.len(),
            cols.len()
        ));
    }
    Ok(())
}

/// A transaction isolation level for
/// [`Transactional::transact_with`]. Postgres maps directly
/// (`BEGIN ISOLATION LEVEL …`); SQLite is always serializable by nature, so
/// levels map to *when the write lock is taken* (`Serializable` →
/// `BEGIN EXCLUSIVE`, `RepeatableRead` → `BEGIN IMMEDIATE`, `ReadCommitted` →
/// plain `BEGIN`) — running *stronger* than asked is honest, weaker is not.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Isolation {
    ReadCommitted,
    RepeatableRead,
    Serializable,
}

/// A runnable execution backend behind the query builder.
///
/// Implementors provide the five dialect-specific **primitives**
/// (`query`/`execute`/`insert`/`upsert`/`migrate`); everything else is a
/// **default method** built on top, so the full read/write API is available on
/// any backend without re-implementation.
///
/// The query builder emits canonical `?`-placeholder SQL; each backend is
/// responsible for translating to its own placeholder dialect inside `query`
/// and `execute`.
pub trait Backend {
    // --- required primitives (SQL dialect differs per backend) ---

    /// Run an arbitrary parameterized SELECT and return rows as JSON objects.
    fn query(&self, sql: &str, params: &[Value]) -> Result<Vec<Json>, String>;

    /// Execute a parameterized statement; returns rows affected.
    fn execute(&self, sql: &str, params: &[Value]) -> Result<usize, String>;

    /// Insert a row from `(column, value)` pairs; returns the new primary key.
    /// `pk` names the auto-generated key column (e.g. `id`) for backends that
    /// need an explicit `RETURNING`; backends with a native last-insert-id may
    /// ignore it.
    fn insert(&self, table: &str, cols: &[(&str, Value)], pk: &str) -> Result<i64, String>;

    /// Insert, or update on `conflict`-column conflict (`ON CONFLICT … DO
    /// UPDATE`). Non-conflict columns are overwritten. Returns the affected
    /// row's primary key `pk`.
    fn upsert(
        &self,
        table: &str,
        cols: &[(&str, Value)],
        conflict: &str,
        pk: &str,
    ) -> Result<i64, String>;

    /// Create a table from a schema if it does not already exist.
    fn migrate(&self, schema: &TableSchema) -> Result<(), String>;

    /// What this backend can do beyond the core surface — see [`BackendCaps`].
    /// Defaults to all-off (`BackendCaps::none("unknown")`) so a backend never
    /// advertises what it hasn't implemented; the bundled SQLite and Postgres
    /// backends override it. Surface it to agents via
    /// `App::register_capabilities`.
    fn capabilities(&self) -> BackendCaps {
        BackendCaps::none("unknown")
    }

    /// Try to take the named advisory lock. `Ok(Some(guard))` holds it until
    /// the guard drops; `Ok(None)` means someone else holds it. Scope is the
    /// backend's `capabilities().advisory_locks` — cluster-wide on Postgres,
    /// process-wide on SQLite. The default errors: a backend without the
    /// capability says so instead of pretending.
    fn try_lock(&self, name: &str) -> Result<Option<LockGuard>, String> {
        let _ = name;
        Err(unsupported("advisory_locks", self.capabilities().backend))
    }

    /// Take the named lock, waiting up to `timeout`; `Ok(None)` on timeout.
    /// Default: poll [`try_lock`](Backend::try_lock). The Postgres backend
    /// overrides this with a server-side blocking wait.
    fn lock(&self, name: &str, timeout: std::time::Duration) -> Result<Option<LockGuard>, String> {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            if let Some(guard) = self.try_lock(name)? {
                return Ok(Some(guard));
            }
            let now = std::time::Instant::now();
            if now >= deadline {
                return Ok(None);
            }
            std::thread::sleep(LOCK_POLL_INTERVAL.min(deadline - now));
        }
    }

    /// Run `f` while holding the named lock — the singleton-job shape:
    /// `db.with_lock("nightly-report", timeout, || …)`. `Ok(None)` means the
    /// lock was never acquired (someone else ran it); `f`'s error passes
    /// through.
    fn with_lock<T>(
        &self,
        name: &str,
        timeout: std::time::Duration,
        f: impl FnOnce() -> Result<T, String>,
    ) -> Result<Option<T>, String>
    where
        Self: Sized,
    {
        match self.lock(name, timeout)? {
            Some(_guard) => f().map(Some),
            None => Ok(None),
        }
    }

    /// The SQL dialect this backend speaks — the DDL emitter and diff engine
    /// need it to render the right statements. Defaults to SQLite; the Postgres
    /// backend and its transaction handle override it.
    fn dialect(&self) -> crate::value::Dialect {
        crate::value::Dialect::Sqlite
    }

    /// Read the live schema of every user table back out of the database, as
    /// [`TableSchema`]s — the inverse of [`migrate`](Backend::migrate). The
    /// framework's own `_sutegi_migrations` history table is excluded.
    ///
    /// This backs schema diffing and drift detection. The default errors: a
    /// backend that can't reflect its own catalog (e.g. an in-memory test
    /// double) simply doesn't support it. The real SQLite and Postgres backends
    /// override it.
    fn introspect(&self) -> Result<Vec<TableSchema>, String> {
        Err("this backend does not support schema introspection".into())
    }

    // --- default methods (shared, implemented via the primitives) ---

    /// Run a query builder and return rows as JSON objects. A requested
    /// row-lock clause is emitted here — per this backend's dialect and
    /// capabilities — so `build()` stays dialect-blind.
    fn select(&self, qb: &QueryBuilder) -> Result<Vec<Json>, String> {
        let (mut sql, params) = qb.build()?;
        if let Some(lock) = qb.row_lock() {
            sql.push_str(&row_lock_sql(lock, &self.capabilities())?);
        }
        self.query(&sql, &params)
    }

    /// Run an UPDATE with a `RETURNING` clause and get the affected rows back
    /// in the same round-trip. Gated on the `returning_dml` capability — a
    /// backend whose driver would silently discard the rows errors instead.
    fn update_returning(&self, ub: &UpdateBuilder) -> Result<Vec<Json>, String> {
        let caps = self.capabilities();
        if !caps.returning_dml {
            return Err(unsupported("returning_dml", caps.backend));
        }
        let (sql, params) = ub.build()?;
        self.query(&sql, &params)
    }

    /// Run a DELETE with a `RETURNING` clause and get the deleted rows back.
    /// Same gating as [`update_returning`](Backend::update_returning).
    fn delete_returning(&self, db: &DeleteBuilder) -> Result<Vec<Json>, String> {
        let caps = self.capabilities();
        if !caps.returning_dml {
            return Err(unsupported("returning_dml", caps.backend));
        }
        let (sql, params) = db.build()?;
        self.query(&sql, &params)
    }

    /// Insert many rows of the same shape in few statements. The default
    /// batches multi-row `INSERT … VALUES` under the placeholder budget and
    /// works on any backend; Postgres overrides it with wire-native
    /// `COPY FROM STDIN` (`bulk_copy: true` marks the native path).
    fn insert_many(
        &self,
        table: &str,
        cols: &[&str],
        rows: &[Vec<Value>],
    ) -> Result<usize, String> {
        check_bulk_shape(table, cols, rows)?;
        if rows.is_empty() {
            return Ok(0);
        }
        // Rows per statement: SQLite's binding ceiling is the tight one
        // (32766 on the bundled build); stay well under it and cap statement
        // size for Postgres too.
        let per_batch = (30_000 / cols.len().max(1)).clamp(1, 500);
        let row_marks = format!("({})", vec!["?"; cols.len()].join(", "));
        let mut affected = 0;
        for chunk in rows.chunks(per_batch) {
            let sql = format!(
                "INSERT INTO {} ({}) VALUES {}",
                table,
                cols.join(", "),
                vec![row_marks.as_str(); chunk.len()].join(", "),
            );
            let params: Vec<Value> = chunk.iter().flatten().cloned().collect();
            affected += self.execute(&sql, &params)?;
        }
        Ok(affected)
    }

    /// Run a SELECT and return only the first row, if any.
    fn query_one(&self, sql: &str, params: &[Value]) -> Result<Option<Json>, String> {
        Ok(self.query(sql, params)?.into_iter().next())
    }

    /// Count rows matching a query builder (uses its `build_count`).
    fn count(&self, qb: &QueryBuilder) -> Result<i64, String> {
        let (sql, params) = qb.build_count()?;
        Ok(self
            .query_one(&sql, &params)?
            .and_then(|r| r.get("count").and_then(|j| j.as_f64()))
            .map(|f| f as i64)
            .unwrap_or(0))
    }

    /// Whether any row matches.
    fn exists(&self, qb: &QueryBuilder) -> Result<bool, String> {
        Ok(self.count(qb)? > 0)
    }

    /// Run a query builder and hydrate each row into a typed [`FromRow`].
    fn fetch<T: FromRow>(&self, qb: &QueryBuilder) -> Result<Vec<T>, String>
    where
        Self: Sized,
    {
        self.select(qb)?.iter().map(T::from_row).collect()
    }

    /// Fetch and hydrate the first matching row, if any.
    fn fetch_one<T: FromRow>(&self, qb: &QueryBuilder) -> Result<Option<T>, String>
    where
        Self: Sized,
    {
        Ok(self.fetch::<T>(qb)?.into_iter().next())
    }

    /// Run a paginated query (1-based `page`): the page's rows plus the total.
    fn paginate(&self, qb: &QueryBuilder, page: i64, per_page: i64) -> Result<Page<Json>, String> {
        let (page, per_page) = (page.max(1), per_page.max(1));
        let total = self.count(qb)?;
        let items = self.select(&qb.clone().limit(per_page).offset((page - 1) * per_page))?;
        Ok(Page {
            items,
            total,
            page,
            per_page,
        })
    }

    /// Typed variant of [`paginate`](Backend::paginate).
    fn paginate_typed<T: FromRow>(
        &self,
        qb: &QueryBuilder,
        page: i64,
        per_page: i64,
    ) -> Result<Page<T>, String>
    where
        Self: Sized,
    {
        let (page, per_page) = (page.max(1), per_page.max(1));
        let total = self.count(qb)?;
        let items = self.fetch::<T>(&qb.clone().limit(per_page).offset((page - 1) * per_page))?;
        Ok(Page {
            items,
            total,
            page,
            per_page,
        })
    }
}

/// A [`Backend`] that can run a closure inside a transaction — the seam that
/// lets *generic* code (the event store, a repository, a service) be atomic
/// without naming a concrete store. Both [`crate::db::Db`] and [`crate::pg::Pg`]
/// implement it by delegating to their inherent `transaction` methods.
///
/// The closure receives `&dyn Backend` (the trait's generic helpers are
/// `Self: Sized`-gated precisely so it stays object-safe): the full
/// query/execute/builder surface works, typed `fetch` does not — use the
/// concrete `transaction` method when you need typed hydration inside a
/// transaction.
pub trait Transactional: Backend {
    /// Object-safe core: run `f` inside `BEGIN … COMMIT`, rolling back if `f`
    /// returns `Err`. Prefer the [`transact`](Transactional::transact) wrapper,
    /// which carries a return value.
    fn run_in_tx(
        &self,
        f: &mut dyn FnMut(&dyn Backend) -> Result<(), String>,
    ) -> Result<(), String>;

    /// Run `f` inside a transaction and return its value: commit on `Ok`, roll
    /// back on `Err`.
    fn transact<T>(&self, mut f: impl FnMut(&dyn Backend) -> Result<T, String>) -> Result<T, String>
    where
        Self: Sized,
    {
        let mut out: Option<T> = None;
        self.run_in_tx(&mut |tx| {
            out = Some(f(tx)?);
            Ok(())
        })?;
        out.ok_or_else(|| "transaction closure did not run".to_string())
    }

    /// Object-safe core of [`transact_with`](Transactional::transact_with).
    /// The default **errors** (`unsupported("isolation_levels")`) — silently
    /// running at a weaker level than asked would be a lie; the bundled
    /// backends override it.
    fn run_in_tx_with(
        &self,
        isolation: Isolation,
        f: &mut dyn FnMut(&dyn Backend) -> Result<(), String>,
    ) -> Result<(), String> {
        let _ = (isolation, f);
        Err(unsupported("isolation_levels", self.capabilities().backend))
    }

    /// [`transact`](Transactional::transact) at an explicit [`Isolation`]
    /// level: `db.transact_with(Isolation::Serializable, |tx| …)`.
    fn transact_with<T>(
        &self,
        isolation: Isolation,
        mut f: impl FnMut(&dyn Backend) -> Result<T, String>,
    ) -> Result<T, String>
    where
        Self: Sized,
    {
        let mut out: Option<T> = None;
        self.run_in_tx_with(isolation, &mut |tx| {
            out = Some(f(tx)?);
            Ok(())
        })?;
        out.ok_or_else(|| "transaction closure did not run".to_string())
    }
}

/// Anything that maps to a table. Implementors describe their schema; the
/// framework derives migrations, query helpers, and introspection from it.
/// Every helper is generic over [`Backend`], so a model runs unchanged on
/// SQLite, Postgres, or inside a transaction.
pub trait Model {
    fn schema() -> TableSchema;

    /// The table name. `#[derive(Model)]` implements this as a literal; a
    /// hand-written impl must keep it in sync with [`schema`](Model::schema).
    fn table() -> &'static str;

    /// The primary-key column name (defaults to `id`; the derive overrides it
    /// when a different column is marked `#[model(primary)]`).
    fn primary_key() -> &'static str {
        "id"
    }

    /// Start a query builder scoped to this model's table.
    fn query() -> QueryBuilder {
        QueryBuilder::table(Self::table())
    }

    /// Dev-mode schema sync for this model: create the table if missing, and
    /// **add any columns/indexes/foreign keys the model gained** — the fix for
    /// the old create-if-missing behaviour that silently ignored new fields.
    /// Additive and non-destructive; it errors (pointing at `migrate gen`) on a
    /// change that needs a real migration. Use a [`Migrator`](crate::migrate)
    /// for production.
    fn migrate<B: Backend>(conn: &B) -> Result<(), String> {
        crate::migrate::sync_table(conn, &Self::schema())
    }

    /// Active-record-style: fetch every row as a JSON object.
    fn all<B: Backend>(conn: &B) -> Result<Vec<Json>, String> {
        conn.select(&Self::query())
    }

    /// Active-record-style: find one row by primary key.
    fn find<B: Backend>(conn: &B, id: Value) -> Result<Option<Json>, String> {
        let rows = conn.select(&Self::query().filter(Self::primary_key(), "=", id).limit(1))?;
        Ok(rows.into_iter().next())
    }

    /// Active-record-style: insert a row, returning its new primary key.
    fn create<B: Backend>(conn: &B, values: &[(&str, Value)]) -> Result<i64, String> {
        conn.insert(Self::table(), values, Self::primary_key())
    }

    /// Typed variant of [`all`](Model::all): hydrate every row into `Self`.
    fn all_typed<B: Backend>(conn: &B) -> Result<Vec<Self>, String>
    where
        Self: Sized + FromRow,
    {
        conn.fetch::<Self>(&Self::query())
    }

    /// Typed variant of [`find`](Model::find): hydrate the matching row.
    fn find_typed<B: Backend>(conn: &B, id: Value) -> Result<Option<Self>, String>
    where
        Self: Sized + FromRow,
    {
        let rows =
            conn.fetch::<Self>(&Self::query().filter(Self::primary_key(), "=", id).limit(1))?;
        Ok(rows.into_iter().next())
    }

    /// Total row count for this model's table.
    fn count<B: Backend>(conn: &B) -> Result<i64, String> {
        conn.count(&Self::query())
    }

    /// Update columns on the row matching the primary key. Returns rows affected.
    fn update<B: Backend>(conn: &B, id: Value, sets: &[(&str, Value)]) -> Result<usize, String> {
        let mut builder = UpdateBuilder::table(Self::table());
        for (col, value) in sets {
            builder = builder.set(col, value.clone());
        }
        let (sql, params) = builder.filter(Self::primary_key(), "=", id).build()?;
        conn.execute(&sql, &params)
    }

    /// Delete the row matching the primary key. Returns `true` if a row was removed.
    fn delete<B: Backend>(conn: &B, id: Value) -> Result<bool, String> {
        let (sql, params) = DeleteBuilder::table(Self::table())
            .filter(Self::primary_key(), "=", id)
            .build()?;
        Ok(conn.execute(&sql, &params)? > 0)
    }
}

/// Hydration from a JSON row (as produced by any [`Backend`]) into a typed
/// struct. Implemented by `#[derive(Model)]`. Strict: every non-nullable column
/// must be present, because a real database row always has them.
pub trait FromRow: Sized {
    fn from_row(row: &Json) -> Result<Self, String>;
}

/// Hydration from a **partial** JSON object — e.g. a request body or an AI
/// tool's arguments — where columns the caller doesn't supply (a
/// database-assigned `id`, a `done` flag with a natural default) are filled with
/// their type's default instead of erroring. Implemented by `#[derive(Model)]`.
///
/// This is the lenient counterpart to [`FromRow`]: use `from_row` for rows that
/// came out of a `Backend`, and `from_input` for data coming in from a client.
/// It backs [`Ctx::validated`](../../sutegi_web/struct.Ctx.html) and is handy in
/// tool closures: `let todo = Todo::from_input(&args)?;`.
pub trait FromInput: Sized {
    fn from_input(row: &Json) -> Result<Self, String>;
}

/// Column extractors used by generated `FromRow` impls. They tolerate the
/// SQLite quirks (booleans stored as `0`/`1`, integers arriving as floats),
/// which is what makes typed round-tripping clean across backends.
pub mod row {
    pub use super::FromRow;
    use sutegi_json::Json;

    fn col<'a>(row: &'a Json, name: &str) -> Result<&'a Json, String> {
        row.get(name)
            .ok_or_else(|| format!("missing column '{}'", name))
    }

    fn is_absent(row: &Json, name: &str) -> bool {
        matches!(row.get(name), None | Some(Json::Null))
    }

    pub fn get_i64(row: &Json, name: &str) -> Result<i64, String> {
        match col(row, name)? {
            Json::Num(n) => Ok(*n as i64),
            Json::Bool(b) => Ok(*b as i64),
            Json::Str(s) => s
                .trim()
                .parse()
                .map_err(|_| format!("column '{}' is not an integer", name)),
            _ => Err(format!("column '{}' is not an integer", name)),
        }
    }

    pub fn get_f64(row: &Json, name: &str) -> Result<f64, String> {
        match col(row, name)? {
            Json::Num(n) => Ok(*n),
            Json::Str(s) => s
                .trim()
                .parse()
                .map_err(|_| format!("column '{}' is not a number", name)),
            _ => Err(format!("column '{}' is not a number", name)),
        }
    }

    pub fn get_string(row: &Json, name: &str) -> Result<String, String> {
        match col(row, name)? {
            Json::Str(s) => Ok(s.clone()),
            Json::Num(n) => Ok(n.to_string()),
            Json::Bool(b) => Ok(b.to_string()),
            _ => Err(format!("column '{}' is not text", name)),
        }
    }

    pub fn get_bool(row: &Json, name: &str) -> Result<bool, String> {
        match col(row, name)? {
            Json::Bool(b) => Ok(*b),
            Json::Num(n) => Ok(*n != 0.0),
            Json::Str(s) => Ok(matches!(s.trim(), "1" | "true" | "TRUE" | "yes")),
            _ => Err(format!("column '{}' is not a boolean", name)),
        }
    }

    /// A JSON column. Postgres returns structured JSON directly; SQLite returns
    /// the serialized text, which is parsed here — so either way you get a real
    /// [`Json`] value back.
    pub fn get_json(row: &Json, name: &str) -> Result<Json, String> {
        match col(row, name)? {
            Json::Str(s) => {
                Json::parse(s).map_err(|e| format!("column '{}' is not valid JSON: {}", name, e))
            }
            other => Ok(other.clone()),
        }
    }

    /// An embedding vector column, in either backend's representation
    /// (pgvector's `[1,2,3]` text, a SQLite text array, or a JSON array of
    /// numbers) → `Vec<f32>`.
    pub fn get_vector(row: &Json, name: &str) -> Result<Vec<f32>, String> {
        match col(row, name)? {
            Json::Str(s) => crate::value::vector_from_text(s),
            Json::Arr(items) => items
                .iter()
                .map(|v| {
                    v.as_f64().map(|f| f as f32).ok_or_else(|| {
                        format!("column '{}' has a non-numeric vector element", name)
                    })
                })
                .collect(),
            _ => Err(format!("column '{}' is not a vector", name)),
        }
    }

    pub fn opt_i64(row: &Json, name: &str) -> Result<Option<i64>, String> {
        if is_absent(row, name) {
            Ok(None)
        } else {
            get_i64(row, name).map(Some)
        }
    }
    pub fn opt_f64(row: &Json, name: &str) -> Result<Option<f64>, String> {
        if is_absent(row, name) {
            Ok(None)
        } else {
            get_f64(row, name).map(Some)
        }
    }
    pub fn opt_string(row: &Json, name: &str) -> Result<Option<String>, String> {
        if is_absent(row, name) {
            Ok(None)
        } else {
            get_string(row, name).map(Some)
        }
    }
    pub fn opt_bool(row: &Json, name: &str) -> Result<Option<bool>, String> {
        if is_absent(row, name) {
            Ok(None)
        } else {
            get_bool(row, name).map(Some)
        }
    }
    pub fn opt_json(row: &Json, name: &str) -> Result<Option<Json>, String> {
        if is_absent(row, name) {
            Ok(None)
        } else {
            get_json(row, name).map(Some)
        }
    }
    pub fn opt_vector(row: &Json, name: &str) -> Result<Option<Vec<f32>>, String> {
        if is_absent(row, name) {
            Ok(None)
        } else {
            get_vector(row, name).map(Some)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value::{ColType, Column};

    fn todos() -> TableSchema {
        TableSchema::new("todos")
            .column(Column::new("id", ColType::Integer).primary())
            .column(Column::new("title", ColType::Text))
            .column(Column::new("done", ColType::Boolean))
    }

    #[test]
    fn model_default_primary_key_and_query() {
        struct T;
        impl Model for T {
            fn schema() -> TableSchema {
                todos()
            }
            fn table() -> &'static str {
                "todos"
            }
        }
        assert_eq!(T::primary_key(), "id");
        assert_eq!(T::table(), "todos");
        let (sql, _) = T::query().build().unwrap();
        assert_eq!(sql, "SELECT * FROM todos");
    }

    #[test]
    fn row_extractors_tolerate_sqlite_quirks() {
        let row = Json::obj(vec![
            ("n", Json::Num(7.0)),
            ("done", Json::int(1)),
            ("name", Json::str("x")),
            ("ratio", Json::str("2.5")),
            ("flag", Json::str("true")),
        ]);
        assert_eq!(row::get_i64(&row, "n").unwrap(), 7);
        assert!(row::get_bool(&row, "done").unwrap());
        assert_eq!(row::get_string(&row, "name").unwrap(), "x");
        assert_eq!(row::get_f64(&row, "ratio").unwrap(), 2.5);
        assert!(row::get_bool(&row, "flag").unwrap());
        assert!(row::get_i64(&row, "missing").is_err());
        assert_eq!(
            row::opt_i64(&Json::obj(vec![("x", Json::Null)]), "x").unwrap(),
            None
        );
        assert_eq!(row::opt_string(&row, "name").unwrap().as_deref(), Some("x"));
    }

    /// A hand-rolled in-memory `Backend` proving the default methods
    /// (`select`/`count`/`exists`/`paginate`) work through the primitives
    /// alone — no SQL engine required.
    #[test]
    fn default_methods_ride_on_primitives() {
        use std::cell::RefCell;

        #[derive(Default)]
        struct Mem {
            rows: RefCell<Vec<Json>>,
        }
        impl Backend for Mem {
            fn query(&self, sql: &str, _p: &[Value]) -> Result<Vec<Json>, String> {
                // Only two shapes are issued by the default methods we exercise:
                // a COUNT(*) and a plain select.
                if sql.contains("COUNT(*)") {
                    let n = self.rows.borrow().len() as i64;
                    Ok(vec![Json::obj(vec![("count", Json::int(n))])])
                } else {
                    Ok(self.rows.borrow().clone())
                }
            }
            fn execute(&self, _sql: &str, _p: &[Value]) -> Result<usize, String> {
                Ok(0)
            }
            fn insert(&self, _t: &str, cols: &[(&str, Value)], _pk: &str) -> Result<i64, String> {
                let obj = Json::obj(cols.iter().map(|(k, v)| (*k, v.to_json())).collect());
                self.rows.borrow_mut().push(obj);
                Ok(self.rows.borrow().len() as i64)
            }
            fn upsert(
                &self,
                t: &str,
                cols: &[(&str, Value)],
                _c: &str,
                pk: &str,
            ) -> Result<i64, String> {
                self.insert(t, cols, pk)
            }
            fn migrate(&self, _s: &TableSchema) -> Result<(), String> {
                Ok(())
            }
        }

        let mem = Mem::default();
        assert!(!mem.exists(&QueryBuilder::table("t")).unwrap());
        mem.insert("t", &[("id", Value::Int(1))], "id").unwrap();
        mem.insert("t", &[("id", Value::Int(2))], "id").unwrap();
        assert_eq!(mem.count(&QueryBuilder::table("t")).unwrap(), 2);
        assert!(mem.exists(&QueryBuilder::table("t")).unwrap());
        let page = mem.paginate(&QueryBuilder::table("t"), 1, 10).unwrap();
        assert_eq!(page.total, 2);
        assert_eq!(page.items.len(), 2);
    }

    #[test]
    fn capabilities_default_is_all_off() {
        // A backend that declares nothing advertises nothing.
        struct Bare;
        impl Backend for Bare {
            fn query(&self, _: &str, _: &[Value]) -> Result<Vec<Json>, String> {
                Ok(Vec::new())
            }
            fn execute(&self, _: &str, _: &[Value]) -> Result<usize, String> {
                Ok(0)
            }
            fn insert(&self, _: &str, _: &[(&str, Value)], _: &str) -> Result<i64, String> {
                Ok(0)
            }
            fn upsert(
                &self,
                _: &str,
                _: &[(&str, Value)],
                _: &str,
                _: &str,
            ) -> Result<i64, String> {
                Ok(0)
            }
            fn migrate(&self, _: &TableSchema) -> Result<(), String> {
                Ok(())
            }
        }

        let caps = Bare.capabilities();
        assert_eq!(caps, BackendCaps::none("unknown"));
        assert_eq!(caps.advisory_locks, CapScope::None);
        assert!(!caps.listen_notify);
    }

    #[test]
    fn capabilities_json_shape_is_stable() {
        let json = BackendCaps::none("postgres").to_json().to_string();
        // Alphabetical keys; scopes as strings, features as booleans.
        assert!(json.starts_with("{\"advisory_locks\":\"none\",\"backend\":\"postgres\""));
        assert!(json.contains("\"listen_notify\":false"));
        assert!(json.contains("\"live_queries\":\"none\""));
    }

    #[test]
    fn unsupported_error_names_capability_and_backend() {
        assert_eq!(
            unsupported("advisory_locks", "sqlite"),
            "unsupported: advisory_locks is not available on the sqlite backend"
        );
    }

    #[test]
    fn row_lock_sql_emits_or_gates_per_caps() {
        use crate::builder::{LockWait, RowLock};
        let lock = |exclusive, wait| RowLock { exclusive, wait };

        let pg = BackendCaps {
            row_locks: true,
            skip_locked: true,
            ..BackendCaps::none("postgres")
        };
        let sql = |l| row_lock_sql(l, &pg).unwrap();
        assert_eq!(sql(lock(true, LockWait::Wait)), " FOR UPDATE");
        assert_eq!(sql(lock(false, LockWait::Wait)), " FOR SHARE");
        assert_eq!(
            sql(lock(true, LockWait::SkipLocked)),
            " FOR UPDATE SKIP LOCKED"
        );
        assert_eq!(sql(lock(true, LockWait::NoWait)), " FOR UPDATE NOWAIT");

        // No row locks: plain lock is a documented no-op, altered-semantics
        // variants error.
        let lite = BackendCaps::none("sqlite");
        assert_eq!(row_lock_sql(lock(true, LockWait::Wait), &lite).unwrap(), "");
        assert!(row_lock_sql(lock(true, LockWait::SkipLocked), &lite).is_err());
        assert!(row_lock_sql(lock(true, LockWait::NoWait), &lite).is_err());
    }

    #[test]
    fn insert_many_batches_under_the_placeholder_budget() {
        use std::cell::RefCell;

        /// Records each executed statement's placeholder count.
        #[derive(Default)]
        struct Recorder {
            batches: RefCell<Vec<usize>>,
        }
        impl Backend for Recorder {
            fn query(&self, _: &str, _: &[Value]) -> Result<Vec<Json>, String> {
                Ok(Vec::new())
            }
            fn execute(&self, sql: &str, params: &[Value]) -> Result<usize, String> {
                assert!(sql.starts_with("INSERT INTO items (a, b) VALUES (?, ?)"));
                self.batches.borrow_mut().push(params.len());
                Ok(params.len() / 2)
            }
            fn insert(&self, _: &str, _: &[(&str, Value)], _: &str) -> Result<i64, String> {
                Ok(0)
            }
            fn upsert(
                &self,
                _: &str,
                _: &[(&str, Value)],
                _: &str,
                _: &str,
            ) -> Result<i64, String> {
                Ok(0)
            }
            fn migrate(&self, _: &TableSchema) -> Result<(), String> {
                Ok(())
            }
        }

        let rec = Recorder::default();
        let rows: Vec<Vec<Value>> = (0..1201)
            .map(|i| vec![Value::Int(i), Value::Text(format!("r{i}"))])
            .collect();
        let n = rec.insert_many("items", &["a", "b"], &rows).unwrap();
        assert_eq!(n, 1201);
        // 500 rows per statement: 500 + 500 + 201.
        assert_eq!(*rec.batches.borrow(), vec![1000, 1000, 402]);

        // Shape errors are caught before any statement runs.
        assert!(rec
            .insert_many("items", &["a", "b"], &[vec![Value::Int(1)]])
            .is_err());
        assert!(rec.insert_many("items; --", &["a"], &[]).is_err());
        assert_eq!(rec.insert_many("items", &["a", "b"], &[]).unwrap(), 0);
    }
}
