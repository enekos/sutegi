# 004 — JSON path queries

## What

Query *inside* JSON columns through the builder: `where_json(col, path, op,
value)`, `select_json(col, path, alias)`, `where_json_contains(col, json)`.
Makes the already-storable `Value::Json` queryable — document-store mode for
agent-authored data whose schema isn't known up front.

## Design

### One path grammar, compiled per dialect

A `$.a.b[0].c` JSONPath subset — identifier keys and `[n]` array indexes —
parsed once into segments at builder time (errors land in the builder's `err`
slot like every other validation). Deliberately a subset: exotic keys need
quoting rules that differ per engine; identifiers compile everywhere.

- **SQLite (JSON1)**: `json_extract(col, ?)` with the canonical path string
  **bound as a parameter** — never spliced.
- **Postgres**: `col #>> ?` with a `{a,b,0,c}` text-array literal bound as a
  parameter. `#>>` returns text, so comparisons cast by the *value's* type:
  `(col #>> ?)::numeric` for Int/Real, `::boolean` for Bool, plain for text.
  (SQLite's `json_extract` returns typed values natively — no casts.)

### Dialect-aware build

JSON predicates are the first builder feature whose *SQL shape* differs per
engine, so the builder grows `build_for(dialect)` (and `build_count_for`);
`build()` stays the canonical-SQLite form for existing callers. The
`Backend::select`/`count` defaults switch to `build_for(self.dialect())` and
gate: a builder using JSON paths on a backend without `json_path` errors with
`unsupported` before any SQL is sent.

### Containment

`where_json_contains(col, value)` → PG `col @> ?::jsonb` (`json_contains:
true`). SQLite has no containment operator — cap stays false and the
predicate errors there; emulating `@>` semantics with `json_each` walks is a
lie we don't tell.

### Caps

`json_path`: true on Pg + Db (and their Tx handles). `json_contains`: Pg
only.

### Deferred (out of scope, tracked)

- **GIN index emission** for jsonb columns: needs an index *kind* in the
  schema IR + diff + both DDL emitters + introspection round-trip. The query
  surface works without it; index emission joins the FTS milestone's DDL work
  (005) where the IR must learn non-btree indexes anyway.
- Quoted/exotic path keys (`$["a b"]`).

## Acceptance

- Unit: path parser (valid, index, nested; rejects `$..`, quotes, injection
  attempts); per-dialect SQL rendering with params in the right order;
  builder error on malformed paths.
- SQLite: where_json / select_json round-trip over inserted JSON docs (typed
  numeric comparison included); where_json_contains errors as unsupported.
- Live PG: same round-trip on jsonb columns (numeric cast correctness:
  `9 < 10` as numbers, not text); where_json_contains matches subset docs.
- Gate per constitution.
