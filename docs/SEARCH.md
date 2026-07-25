# Full-text + hybrid search

One grammar and one API over `tsvector` (Postgres) and FTS5 (SQLite), plus
reciprocal-rank-fusion hybrid search with the embedding layer.

```rust
use sutegi_orm::search;

search::setup(&db, "docs", "id", &["title", "body"])?;   // idempotent DDL

// Lexical, ranked (best first, `_rank` attached):
let hits = search::search(&db, "docs", "id", &["title", "body"],
                          r#"rust "job queue" -django"#, 20)?;

// Hybrid: lexical + vector legs fused with RRF (`_score` attached):
let hits = search::hybrid_search(&db, "docs", "id", &["title", "body"],
                                 "rust queue", "embedding", &query_vec, 10)?;
```

## The grammar

`word "a phrase" -negated OR alternative` — implicit AND, `-` negation, `OR`
between groups, quoted phrases. Parsed once; raw user input **never touches
engine query syntax** (words are sanitized to alphanumerics at parse time, so
neither FTS5 operators nor tsquery syntax can be injected — a stray `*` or
`NEAR/2` can't even cause an engine syntax error). Pure-negative queries are
rejected. Rendered per engine and bound as a parameter:

| grammar | Postgres (`to_tsquery('simple', …)`) | SQLite (FTS5 `MATCH`) |
|---|---|---|
| `a b` | `a & b` | `"a" AND "b"` |
| `"a b"` | `(a <-> b)` | `"a b"` |
| `-x` | `!x` | `… NOT "x"` |
| `a OR b` | `a \| b` | `"a" OR "b"` |

The `simple` config (no stemming) matches FTS5's unstemmed unicode61
tokenizer, so semantics agree across engines. A language/stemming knob is
deferred until a consumer needs it.

## The artifacts (`setup`)

Framework-managed, `_sutegi_`-prefixed, and **invisible to schema
introspection / `migrate:drift`** on both engines (tested):

- **Postgres**: one *expression* GIN index over
  `to_tsvector('simple', coalesce(col,'') || ' ' || …)` — nothing added to
  the user's table. `search()` uses the byte-identical expression, so the
  planner uses the index (asserted via EXPLAIN in the live-PG test).
- **SQLite**: an external-content FTS5 table + three sync triggers
  (insert/update/delete) + a one-time `rebuild` for pre-existing rows.

## Hybrid = RRF over two legs

The lexical leg is `search()`; the vector leg is pgvector pushdown
(`ORDER BY col <=> ?`) where `capabilities().vector` is true, portable
brute-force cosine otherwise. Reciprocal-rank fusion (`Σ 1/(60+rank)`) merges
in Rust — one code path on both engines — and the top-k rows come back in
fused order. A doc ranked well in *both* legs beats a doc ranked first in one
and absent from the other.

## Agent surface

- `capabilities().fts` — true on both bundled backends.
- `App::register_search("docs", &["title", "body"])` → a `search` block in
  `/__introspect`, so an agent discovers what's searchable without source
  access.
