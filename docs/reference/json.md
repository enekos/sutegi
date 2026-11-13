<!-- Generated from landing/src/Docs.svelte. Run: cd landing && pnpm docs:export -->

[← Advisory locks](advisory.md) · [Documentation index](../README.md) · [Full-text & hybrid search →](search.md)

# JSON path queries

Query *inside* JSON columns through the builder — document-store mode for data whose schema is not known up front, which is exactly the shape agent-authored payloads arrive in.

```rust
// WHERE inside the document, typed comparison:
let hot = db.select(
    &QueryBuilder::table("docs")
        .where_json("meta", "$.stats.views", ">", 50.into()),
)?;

// Project a path out as a column:
let rows = db.select(
    &QueryBuilder::table("docs")
        .select(&["id"])
        .select_json("meta", "$.author.name", "author"),
)?;

// Containment — Postgres only (capabilities().json_contains):
let posts = db.select(
    &QueryBuilder::table("docs")
        .where_json_contains("meta", Json::obj(vec![("kind", Json::str("post"))])),
)?;

// The grammar is $.key.nested[0].deeper — identifier keys and [n] indexes.
// A malformed path is a builder error, and the compiled path is always
// BOUND as a parameter: SQLite json_extract(col, ?), Postgres col #>> ?.
```

The path grammar is a deliberate subset — identifier keys and `[n]` indexes — because exotic keys need per-engine quoting rules while identifiers compile everywhere. This is the first builder feature whose SQL *shape* differs per engine, so the builder stores parsed segments and the executing backend compiles them: `json_extract(col, ?)` with typed values on SQLite, `col #>> ?` with a bound `{a,b,0}` text array and value-driven casts on Postgres (`::numeric` so `9 < 10` compares as numbers). Containment is Postgres only; SQLite errors honestly rather than emulating subset semantics with `json_each` walks.

> [!NOTE]
> **Indexing**
>
> A `jsonb` GIN index for containment-heavy tables is not emitted by migrations yet — the schema IR learns non-btree index kinds alongside the search work. Add one by hand if `@>` gets hot: `CREATE INDEX ON docs USING GIN (meta)`.

---

[← Advisory locks](advisory.md) · [Documentation index](../README.md) · [Full-text & hybrid search →](search.md)
