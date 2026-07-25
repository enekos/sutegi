//! Live integration tests for full-text + hybrid search on Postgres.
//! Runs only when `SUTEGI_PG_TEST_URL` is set; the hybrid test additionally
//! skips if the pgvector extension isn't installable.

#![cfg(feature = "postgres")]

use sutegi_json::Json;
use sutegi_orm::pg::Pg;
use sutegi_orm::{search, Backend, Value};

fn db() -> Option<Pg> {
    let url = std::env::var("SUTEGI_PG_TEST_URL").ok()?;
    Some(Pg::connect(&url, 4).unwrap())
}

#[test]
fn tsvector_search_uses_the_gin_index_and_ranks() {
    let Some(pg) = db() else {
        eprintln!("skipping: SUTEGI_PG_TEST_URL not set");
        return;
    };
    pg.pool()
        .batch(
            "DROP TABLE IF EXISTS fts_docs; \
             CREATE TABLE fts_docs (id BIGSERIAL PRIMARY KEY, title TEXT, body TEXT)",
        )
        .unwrap();
    for (title, body) in [
        ("rust job queue", "durable workers with skip locked"),
        ("phoenix channels", "realtime topics"),
        ("rust rust rust", "rust everywhere"),
    ] {
        pg.insert(
            "fts_docs",
            &[
                ("title", Value::Text(title.into())),
                ("body", Value::Text(body.into())),
            ],
            "id",
        )
        .unwrap();
    }
    search::setup(&pg, "fts_docs", "id", &["title", "body"]).unwrap();
    search::setup(&pg, "fts_docs", "id", &["title", "body"]).unwrap(); // idempotent

    // Phrase + negation, ranked: the tf-heavy doc outranks the single mention.
    let hits = search::search(&pg, "fts_docs", "id", &["title", "body"], "rust", 10).unwrap();
    assert_eq!(hits.len(), 2);
    assert_eq!(
        hits[0].get("title").and_then(Json::as_str),
        Some("rust rust rust")
    );
    let hits = search::search(
        &pg,
        "fts_docs",
        "id",
        &["title", "body"],
        "\"job queue\" -phoenix",
        10,
    )
    .unwrap();
    assert_eq!(hits.len(), 1);

    // The expression GIN index matches the search expression. (Force the
    // planner's hand: with 3 rows a seqscan always wins on cost.)
    pg.pool().batch("SET enable_seqscan = off").unwrap();
    let plan = pg
        .query(
            "EXPLAIN SELECT * FROM fts_docs \
             WHERE to_tsvector('simple', coalesce(title, '') || ' ' || coalesce(body, '')) \
             @@ to_tsquery('simple', 'rust')",
            &[],
        )
        .unwrap();
    let plan_text = plan
        .iter()
        .filter_map(|r| r.get("QUERY PLAN").and_then(Json::as_str).map(String::from))
        .collect::<Vec<_>>()
        .join("\n");
    pg.pool().batch("SET enable_seqscan = on").unwrap();
    assert!(
        plan_text.contains("_sutegi_fts_fts_docs"),
        "expected the GIN index in the plan:\n{plan_text}"
    );

    // Artifacts stay out of introspection (drift-clean).
    let tables: Vec<String> = pg
        .introspect()
        .unwrap()
        .into_iter()
        .map(|t| t.table)
        .collect();
    assert!(tables.contains(&"fts_docs".to_string()));
    assert!(!tables.iter().any(|t| t.starts_with("_sutegi_fts")));
}

#[test]
fn hybrid_search_fuses_on_pgvector() {
    let Some(pg) = db() else {
        eprintln!("skipping: SUTEGI_PG_TEST_URL not set");
        return;
    };
    if pg
        .pool()
        .batch("CREATE EXTENSION IF NOT EXISTS vector")
        .is_err()
    {
        eprintln!("skipping: pgvector not available");
        return;
    }
    pg.pool()
        .batch(
            "DROP TABLE IF EXISTS hy_docs; \
             CREATE TABLE hy_docs (id BIGSERIAL PRIMARY KEY, title TEXT, emb vector(2))",
        )
        .unwrap();
    search::setup(&pg, "hy_docs", "id", &["title"]).unwrap();
    let insert = |title: &str, v: [f32; 2]| {
        pg.insert(
            "hy_docs",
            &[
                ("title", Value::Text(title.into())),
                ("emb", Value::Vector(v.to_vec())),
            ],
            "id",
        )
        .unwrap()
    };
    // Same shape as the SQLite unit test: C is decent in both legs.
    insert("rust rust rust", [0.0, 1.0]);
    insert("unrelated title", [0.9, 0.1]);
    let c = insert("rust queue", [1.0, 0.0]);

    let hits = search::hybrid_search(
        &pg,
        "hy_docs",
        "id",
        &["title"],
        "rust",
        "emb",
        &[1.0, 0.0],
        2,
    )
    .unwrap();
    assert_eq!(hits[0].get("id").and_then(Json::as_i64), Some(c));
    assert!(hits[0].get("_score").and_then(Json::as_f64).unwrap() > 0.0);
}
