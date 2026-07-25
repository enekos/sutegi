//! Live integration tests for JSON path queries on Postgres jsonb columns.
//! Runs only when `SUTEGI_PG_TEST_URL` is set.

#![cfg(feature = "postgres")]

use sutegi_json::Json;
use sutegi_orm::pg::Pg;
use sutegi_orm::{Backend, QueryBuilder, Value};

fn db() -> Option<Pg> {
    let url = std::env::var("SUTEGI_PG_TEST_URL").ok()?;
    Some(Pg::connect(&url, 4).unwrap())
}

fn seed(pg: &Pg, table: &str) {
    // Per-test table: the tests in this binary run in parallel, and two
    // sessions racing DROP/CREATE on one name deadlock.
    pg.pool()
        .batch(&format!(
            "DROP TABLE IF EXISTS {table}; \
             CREATE TABLE {table} (id BIGSERIAL PRIMARY KEY, meta JSONB NOT NULL)"
        ))
        .unwrap();
    let doc = |views: i64, kind: &str, tags: Vec<&str>| {
        Value::Json(Json::obj(vec![
            ("kind", Json::str(kind)),
            ("stats", Json::obj(vec![("views", Json::int(views))])),
            ("tags", Json::arr(tags.into_iter().map(Json::str).collect())),
        ]))
    };
    for (views, kind, tags) in [
        (9, "post", vec!["rust", "db"]),
        (100, "post", vec!["rust"]),
        (3, "page", vec![]),
    ] {
        pg.insert(table, &[("meta", doc(views, kind, tags))], "id")
            .unwrap();
    }
}

#[test]
fn json_path_round_trips_on_jsonb() {
    let Some(pg) = db() else {
        eprintln!("skipping: SUTEGI_PG_TEST_URL not set");
        return;
    };
    seed(&pg, "json_path_docs");

    // Numeric cast correctness: 9 vs 100 as numbers, not text ('9' > '100').
    let hot = pg
        .select(&QueryBuilder::table("json_path_docs").where_json(
            "meta",
            "$.stats.views",
            ">",
            Value::Int(50),
        ))
        .unwrap();
    assert_eq!(hot.len(), 1);
    let cold = pg
        .select(&QueryBuilder::table("json_path_docs").where_json(
            "meta",
            "$.stats.views",
            "<",
            Value::Int(10),
        ))
        .unwrap();
    assert_eq!(cold.len(), 2);

    // Projection with an array index in the path.
    let projected = pg
        .select(
            &QueryBuilder::table("json_path_docs")
                .select(&["id"])
                .select_json("meta", "$.tags[0]", "first_tag")
                .order_by("id", false),
        )
        .unwrap();
    assert_eq!(
        projected[0].get("first_tag").and_then(Json::as_str),
        Some("rust")
    );
    assert!(projected[2]
        .get("first_tag")
        .map(|j| j.is_null())
        .unwrap_or(false));

    // count() rides the same dialect-aware path.
    assert_eq!(
        pg.count(&QueryBuilder::table("json_path_docs").where_json(
            "meta",
            "$.kind",
            "=",
            Value::Text("post".into()),
        ))
        .unwrap(),
        2
    );
}

#[test]
fn json_containment_matches_subset_docs() {
    let Some(pg) = db() else {
        eprintln!("skipping: SUTEGI_PG_TEST_URL not set");
        return;
    };
    seed(&pg, "json_contain_docs");

    // @> subset semantics: object key/value…
    let posts = pg
        .select(
            &QueryBuilder::table("json_contain_docs")
                .where_json_contains("meta", Json::obj(vec![("kind", Json::str("post"))])),
        )
        .unwrap();
    assert_eq!(posts.len(), 2);

    // …and array containment inside the document.
    let tagged_db = pg
        .select(
            &QueryBuilder::table("json_contain_docs").where_json_contains(
                "meta",
                Json::obj(vec![("tags", Json::arr(vec![Json::str("db")]))]),
            ),
        )
        .unwrap();
    assert_eq!(tagged_db.len(), 1);
}
