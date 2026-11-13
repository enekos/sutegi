<!-- Generated from landing/src/Docs.svelte. Run: cd landing && pnpm docs:export -->

[← JSON path queries](json.md) · [Documentation index](../README.md) · [Embeddings & vectors →](vectors.md)

# Full-text & hybrid search

One grammar and one API over `tsvector` (Postgres) and FTS5 (SQLite), plus RAG-shaped hybrid retrieval. `search::setup` creates the artifacts, `search::search` returns ranked base-table rows, and `search::hybrid_search` fuses a lexical leg with a vector leg.

```rust
use sutegi::orm::search;

search::setup(&db, "docs", "id", &["title", "body"])?;   // idempotent DDL

// Lexical, ranked best-first, with "_rank" attached to each row:
let hits = search::search(&db, "docs", "id", &["title", "body"],
                          "rust \"job queue\" -django", 20)?;

// Hybrid: the lexical leg + a vector leg, fused with reciprocal-rank fusion
// (sum of 1/(60+rank)); "_score" attached. One code path on both engines.
let hits = search::hybrid_search(&db, "docs", "id", &["title", "body"],
                                 "rust queue", "embedding", &query_vec, 10)?;

// Tell agents what is searchable, without source access:
App::new("api").register_search("docs", &["title", "body"])
```

The grammar is `word "a phrase" -negated OR alternative` — implicit AND, `-` negation, `OR` between groups, quoted phrases. Raw input **never touches engine query syntax**: words are sanitized to alphanumerics at parse time, so neither FTS5 operators nor tsquery syntax can be injected — a stray `*` or `NEAR/2` cannot even cause an engine syntax error — and the rendered query is bound as a parameter. Pure-negative queries are rejected.

The artifacts are framework-managed, `_sutegi_`-prefixed, and invisible to schema introspection and `migrate:drift`: on Postgres a single *expression* GIN index (nothing added to your table; the search query uses the byte-identical expression, EXPLAIN-verified to hit the index), on SQLite an external-content FTS5 table plus sync triggers and a one-time rebuild. The `simple`, unstemmed configuration matches FTS5’s tokenizer so semantics agree across engines; a language/stemming knob is deferred until a consumer needs it.

Hybrid search is reciprocal-rank fusion (`Σ 1/(60+rank)`) merged in Rust over two legs — the lexical one above, and pgvector pushdown where `capabilities().vector` is true, portable brute-force cosine otherwise. A document ranked well in *both* legs beats one ranked first in a single leg and absent from the other.

---

[← JSON path queries](json.md) · [Documentation index](../README.md) · [Embeddings & vectors →](vectors.md)
