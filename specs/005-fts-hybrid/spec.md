# 005 — full-text + hybrid search

## What

Lexical search over text columns (tsvector ↔ FTS5) plus **hybrid search**
(reciprocal-rank fusion of lexical + vector legs) through one portable API in
`sutegi_orm::search`:

```rust
search::setup(&db, "docs", "id", &["title", "body"])?;   // idempotent DDL
let hits = search::search(&db, "docs", "id", &["title", "body"], r#"rust "job queue" -django"#, 20)?;
let hits = search::hybrid_search(&db, "docs", "id", &["title", "body"], "rust queue", "embedding", &vec, 10)?;
```

## Scope deviation from the plan (deliberate)

The plan sketched `#[model(searchable)]` + migrations-emitted artifacts. That
requires the schema IR to learn *expression indexes* and *virtual tables* and
round-trip them through `introspect` on both engines — a diff-engine
expansion that dwarfs the feature. Instead, search artifacts are
**framework-managed** (the `EventStore::migrate` / `Queue::migrate` pattern
already in the codebase): `_sutegi_`-prefixed and thus **excluded from
introspection on both backends** (verified), so `migrate:drift` never sees
them. Verified: PG expression indexes join-drop out of `introspect_pg`
(indkey attnum 0 has no `pg_attribute` row). `#[model(searchable)]` can layer
on top later without changing the artifacts.

## Design

### One search grammar

`word "a phrase" -negated OR alternative` — implicit AND, `-` negation,
`OR` between groups, quoted phrases. Parsed once into an AST; user input
never touches engine query syntax raw (FTS5 MATCH raises *syntax errors* on
stray operators; tsquery is worse). Rendered per engine and **bound as a
parameter**:

- PG: `to_tsquery('simple', 'a & b <-> c & !d | e')` — `simple` config for
  parity with FTS5's unstemmed unicode61 tokenizer (language knob deferred);
  term text sanitized to word characters at parse time.
- SQLite: `"a" AND "b c" NOT "d" OR "e"` — terms always quoted.

### Artifacts (`setup`, idempotent)

- **PG**: one expression GIN index
  `_sutegi_fts_<table> ON <table> USING GIN (to_tsvector('simple', coalesce(col,'') ‖ ' ' ‖ …))`.
  No column added to the user's table — nothing for drift to see; the search
  query uses the byte-identical expression so the planner uses the index.
- **SQLite**: external-content FTS5 table
  `_sutegi_fts_<table>(cols…, content=<table>, content_rowid=<pk>)` + three
  sync triggers (insert/update/delete) + an initial `rebuild` for
  pre-existing rows. FTS5's shadow tables inherit the `_sutegi_` prefix.

### Query

- PG: `WHERE to_tsvector(…) @@ to_tsquery('simple', ?)` ordered by
  `ts_rank … DESC`.
- SQLite: join the FTS table (`MATCH ?`) back to the base table by
  `content_rowid`, ordered by bm25 `rank`.
- Both return base-table rows plus `_rank`; gated on the `fts` capability.

### Hybrid (RRF in Rust, both backends)

Two legs run independently — lexical (`search`) and vector
(`nearest_pushdown_typed` on PG / portable brute-force `nearest` on SQLite,
both already in `embedding.rs`) — then reciprocal-rank fusion
(`Σ 1/(60+rank)`) merges in Rust and the top-k rows are fetched by pk. One
code path for both engines; a single-SQL PG optimization (CTE + FULL JOIN)
is deferred until a consumer shows the two round-trips matter.

### Surfaces

- Caps: `fts: true` on Pg, Db, and their Tx handles.
- `App::register_search(table, cols)` → a `search` block in `/__introspect`
  (mirrors `register_model`), so an agent can discover which tables are
  searchable and with which columns.

## Acceptance

- Unit: grammar parse/render both engines (phrases, negation, OR, hostile
  input: FTS5 operators, tsquery syntax, quotes, backslashes → no engine
  syntax error reachable); setup DDL idempotent.
- SQLite: setup + insert/update/delete keep FTS in sync via triggers; ranked
  search finds phrase and respects negation; introspect/drift stays clean
  after setup.
- Live PG: same behavior on tsvector; `EXPLAIN` shows the GIN index on the
  search query (index-expression parity); introspect clean after setup.
- Hybrid: seeded docs where lexical-only and vector-only orders differ; RRF
  top hit is the doc strong in both.
- Gate per constitution.
