//! Full-text + hybrid search over any [`Backend`] — `tsvector` on Postgres,
//! FTS5 on SQLite, one grammar and one API on top.
//!
//! ```ignore
//! search::setup(&db, "docs", "id", &["title", "body"])?;      // idempotent DDL
//! let hits = search::search(&db, "docs", "id", &["title", "body"],
//!                           r#"rust "job queue" -django"#, 20)?;
//! ```
//!
//! Search artifacts are **framework-managed** (the `EventStore::migrate`
//! pattern): `_sutegi_`-prefixed, so schema introspection and `migrate:drift`
//! never see them. On Postgres the artifact is an *expression* GIN index —
//! no column is added to the user's table.

use crate::backend::{unsupported, Backend};
use crate::builder::{valid_identifier, QueryBuilder};
use crate::embedding::{self, Metric};
use crate::value::{Dialect, Value};
use sutegi_json::Json;

/// One search term: a word or a quoted phrase, possibly negated.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Term {
    negated: bool,
    /// One word, or several for a phrase. Words are sanitized to alphanumeric
    /// + `_` at parse time, so no engine operator can survive into a query.
    words: Vec<String>,
}

/// A parsed search query: OR-separated groups of AND-ed terms.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchQuery {
    groups: Vec<Vec<Term>>,
}

/// Strip a raw token down to letters/digits/`_` words. Anything else — FTS5
/// operators, tsquery syntax, quotes, parens — dies here, which is what makes
/// the rendered query strings safe to bind.
fn words_of(raw: &str) -> Vec<String> {
    raw.split(|c: char| !c.is_alphanumeric() && c != '_')
        .filter(|w| !w.is_empty())
        .map(|w| w.to_lowercase())
        .collect()
}

/// Parse the search grammar: `word "a phrase" -negated OR alternative`.
/// Implicit AND within a group, `OR` between groups, `-` negation, quoted
/// phrases. Every group needs at least one positive term (pure negation
/// matches "everything except" — never what a search box means, and FTS5
/// can't express it).
pub fn parse_query(input: &str) -> Result<SearchQuery, String> {
    let mut groups: Vec<Vec<Term>> = vec![Vec::new()];
    let mut rest = input.trim();
    while !rest.is_empty() {
        // OR starts a new group.
        if let Some(r) = rest
            .strip_prefix("OR ")
            .or_else(|| (rest == "OR").then_some(""))
        {
            if !groups.last().unwrap().is_empty() {
                groups.push(Vec::new());
            }
            rest = r.trim_start();
            continue;
        }
        let negated = rest.starts_with('-');
        if negated {
            rest = &rest[1..];
        }
        let words = if let Some(r) = rest.strip_prefix('"') {
            // Quoted phrase: up to the closing quote (or end of input).
            let end = r.find('"').unwrap_or(r.len());
            let words = words_of(&r[..end]);
            rest = r[end..].strip_prefix('"').unwrap_or("").trim_start();
            words
        } else {
            let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
            let words = words_of(&rest[..end]);
            rest = rest[end..].trim_start();
            words
        };
        if !words.is_empty() {
            groups.last_mut().unwrap().push(Term { negated, words });
        }
    }
    groups.retain(|g| !g.is_empty());
    if groups.is_empty() {
        return Err("empty search query".into());
    }
    for g in &groups {
        if g.iter().all(|t| t.negated) {
            return Err("search query needs at least one positive term per OR group".into());
        }
    }
    Ok(SearchQuery { groups })
}

impl SearchQuery {
    /// Render for Postgres `to_tsquery('simple', …)`: `&`/`|`/`!`, phrases as
    /// `a <-> b`. Sanitized words only — no tsquery syntax can be injected.
    pub fn to_tsquery(&self) -> String {
        let term = |t: &Term| {
            let body = t.words.join(" <-> ");
            let body = if t.words.len() > 1 {
                format!("({body})")
            } else {
                body
            };
            if t.negated {
                format!("!{body}")
            } else {
                body
            }
        };
        self.groups
            .iter()
            .map(|g| {
                let s = g.iter().map(term).collect::<Vec<_>>().join(" & ");
                if self.groups.len() > 1 && g.len() > 1 {
                    format!("({s})")
                } else {
                    s
                }
            })
            .collect::<Vec<_>>()
            .join(" | ")
    }

    /// Render for SQLite FTS5 `MATCH`: quoted terms, `AND`/`OR`/`NOT`.
    /// Negations render as trailing `NOT` clauses so a group is
    /// `(a AND "b c") NOT d`.
    pub fn to_fts5(&self) -> String {
        let quoted = |t: &Term| format!("\"{}\"", t.words.join(" "));
        self.groups
            .iter()
            .map(|g| {
                let pos = g
                    .iter()
                    .filter(|t| !t.negated)
                    .map(quoted)
                    .collect::<Vec<_>>()
                    .join(" AND ");
                let mut s = pos;
                for t in g.iter().filter(|t| t.negated) {
                    s = format!("{s} NOT {}", quoted(t));
                }
                if self.groups.len() > 1 && g.len() > 1 {
                    format!("({s})")
                } else {
                    s
                }
            })
            .collect::<Vec<_>>()
            .join(" OR ")
    }
}

/// Validate the identifiers a search call interpolates (identifiers can't be
/// bound as parameters — same rule as the query builder).
fn check_idents(table: &str, pk: &str, cols: &[&str]) -> Result<(), String> {
    if cols.is_empty() {
        return Err("search: no columns".into());
    }
    for ident in [table, pk].iter().chain(cols.iter()) {
        if !valid_identifier(ident) {
            return Err(format!("invalid identifier: {ident:?}"));
        }
    }
    Ok(())
}

/// The tsvector input expression — **byte-identical** between `setup` and
/// `search` so the planner matches the expression GIN index.
fn tsvector_expr(cols: &[&str]) -> String {
    cols.iter()
        .map(|c| format!("coalesce({c}, '')"))
        .collect::<Vec<_>>()
        .join(" || ' ' || ")
}

fn fts_table(table: &str) -> String {
    format!("_sutegi_fts_{table}")
}

/// Create the search artifacts for `table` over `cols` (idempotent).
///
/// - **Postgres**: an expression GIN index — invisible to schema
///   introspection, nothing added to the user's table.
/// - **SQLite**: an external-content FTS5 table + insert/update/delete sync
///   triggers, plus a one-time `rebuild` to index pre-existing rows.
pub fn setup<B: Backend>(db: &B, table: &str, pk: &str, cols: &[&str]) -> Result<(), String> {
    check_idents(table, pk, cols)?;
    let fts = fts_table(table);
    match db.dialect() {
        Dialect::Postgres => {
            db.execute(
                &format!(
                    "CREATE INDEX IF NOT EXISTS {fts} ON {table} \
                     USING GIN (to_tsvector('simple', {expr}))",
                    expr = tsvector_expr(cols),
                ),
                &[],
            )?;
        }
        Dialect::Sqlite => {
            // Only rebuild when the virtual table is first created.
            let exists = db
                .query_one(
                    "SELECT name FROM sqlite_master WHERE type = 'table' AND name = ?",
                    &[Value::Text(fts.clone())],
                )?
                .is_some();
            if exists {
                return Ok(());
            }
            let col_list = cols.join(", ");
            db.execute(
                &format!(
                    "CREATE VIRTUAL TABLE {fts} USING fts5({col_list}, \
                     content={table}, content_rowid={pk})"
                ),
                &[],
            )?;
            let new_vals: Vec<String> = cols.iter().map(|c| format!("new.{c}")).collect();
            let old_vals: Vec<String> = cols.iter().map(|c| format!("old.{c}")).collect();
            let (new_vals, old_vals) = (new_vals.join(", "), old_vals.join(", "));
            db.execute(
                &format!(
                    "CREATE TRIGGER {fts}_ai AFTER INSERT ON {table} BEGIN \
                     INSERT INTO {fts}(rowid, {col_list}) VALUES (new.{pk}, {new_vals}); END"
                ),
                &[],
            )?;
            db.execute(
                &format!(
                    "CREATE TRIGGER {fts}_ad AFTER DELETE ON {table} BEGIN \
                     INSERT INTO {fts}({fts}, rowid, {col_list}) \
                     VALUES ('delete', old.{pk}, {old_vals}); END"
                ),
                &[],
            )?;
            db.execute(
                &format!(
                    "CREATE TRIGGER {fts}_au AFTER UPDATE ON {table} BEGIN \
                     INSERT INTO {fts}({fts}, rowid, {col_list}) \
                     VALUES ('delete', old.{pk}, {old_vals}); \
                     INSERT INTO {fts}(rowid, {col_list}) VALUES (new.{pk}, {new_vals}); END"
                ),
                &[],
            )?;
            db.execute(&format!("INSERT INTO {fts}({fts}) VALUES ('rebuild')"), &[])?;
        }
    }
    Ok(())
}

/// Ranked full-text search: base-table rows (plus a `_rank` column) matching
/// the query, best first. Gated on the `fts` capability.
pub fn search<B: Backend>(
    db: &B,
    table: &str,
    pk: &str,
    cols: &[&str],
    query: &str,
    limit: usize,
) -> Result<Vec<Json>, String> {
    let caps = db.capabilities();
    if !caps.fts {
        return Err(unsupported("fts", caps.backend));
    }
    check_idents(table, pk, cols)?;
    let q = parse_query(query)?;
    match db.dialect() {
        Dialect::Postgres => {
            let expr = tsvector_expr(cols);
            let sql = format!(
                "SELECT *, ts_rank(to_tsvector('simple', {expr}), to_tsquery('simple', ?)) AS _rank \
                 FROM {table} \
                 WHERE to_tsvector('simple', {expr}) @@ to_tsquery('simple', ?) \
                 ORDER BY _rank DESC LIMIT {limit}"
            );
            let ts = Value::Text(q.to_tsquery());
            db.query(&sql, &[ts.clone(), ts])
        }
        Dialect::Sqlite => {
            let fts = fts_table(table);
            // bm25 rank: more negative = better match, so ascending order.
            let sql = format!(
                "SELECT t.*, f.rank AS _rank FROM {fts} f \
                 JOIN {table} t ON t.{pk} = f.rowid \
                 WHERE f.{fts} MATCH ? ORDER BY f.rank LIMIT {limit}"
            );
            db.query(&sql, &[Value::Text(q.to_fts5())])
        }
    }
}

/// How many candidates each hybrid leg contributes before fusion.
fn leg_size(k: usize) -> usize {
    (k * 4).max(50)
}

/// Hybrid search: reciprocal-rank fusion of a lexical leg ([`search`]) and a
/// vector leg (pgvector pushdown on Postgres, portable brute force on
/// SQLite). Returns base-table rows with a `_score` column, best first —
/// the modern RAG retrieval shape, one call.
#[allow(clippy::too_many_arguments)] // a search call site reads better flat than through a params struct
pub fn hybrid_search<B: Backend>(
    db: &B,
    table: &str,
    pk: &str,
    cols: &[&str],
    query: &str,
    vector_col: &str,
    target: &[f32],
    k: usize,
) -> Result<Vec<Json>, String> {
    check_idents(table, pk, cols)?;
    if !valid_identifier(vector_col) {
        return Err(format!("invalid identifier: {vector_col:?}"));
    }
    let n = leg_size(k);
    let pk_of = |row: &Json| row.get(pk).and_then(Json::as_i64);

    // Lexical leg — pks in rank order.
    let lexical: Vec<i64> = search(db, table, pk, cols, query, n)?
        .iter()
        .filter_map(pk_of)
        .collect();

    // Vector leg — pushdown where the database can, brute force where not.
    let caps = db.capabilities();
    let vector: Vec<i64> = if caps.vector {
        db.query(
            &format!("SELECT {pk} FROM {table} ORDER BY {vector_col} <=> ? LIMIT {n}"),
            &[Value::Vector(target.to_vec())],
        )?
        .iter()
        .filter_map(pk_of)
        .collect()
    } else {
        embedding::nearest(
            db,
            &QueryBuilder::table(table),
            vector_col,
            target,
            n,
            Metric::Cosine,
        )?
        .iter()
        .filter_map(|(row, _)| pk_of(row))
        .collect()
    };

    // Reciprocal-rank fusion: score = Σ 1/(60 + rank). The constant damps
    // the head of each list so a doc ranked well in *both* legs beats a doc
    // ranked first in one and absent from the other.
    let mut scores: std::collections::HashMap<i64, f64> = std::collections::HashMap::new();
    for (rank, id) in lexical.iter().enumerate() {
        *scores.entry(*id).or_default() += 1.0 / (60.0 + rank as f64 + 1.0);
    }
    for (rank, id) in vector.iter().enumerate() {
        *scores.entry(*id).or_default() += 1.0 / (60.0 + rank as f64 + 1.0);
    }
    let mut ranked: Vec<(i64, f64)> = scores.into_iter().collect();
    ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    ranked.truncate(k);

    // Fetch the winners and re-order to the fused ranking, attaching _score.
    let ids: Vec<Value> = ranked.iter().map(|(id, _)| Value::Int(*id)).collect();
    let rows = db.select(&QueryBuilder::table(table).filter_in(pk, ids))?;
    let mut by_id: std::collections::HashMap<i64, Json> = rows
        .into_iter()
        .filter_map(|r| pk_of(&r).map(|id| (id, r)))
        .collect();
    Ok(ranked
        .into_iter()
        .filter_map(|(id, score)| {
            by_id.remove(&id).map(|row| match row {
                Json::Obj(mut pairs) => {
                    pairs.insert("_score".into(), Json::Num(score));
                    Json::Obj(pairs)
                }
                other => other,
            })
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grammar_parses_and_renders_both_engines() {
        let q = parse_query(r#"rust "job queue" -django OR phoenix"#).unwrap();
        assert_eq!(
            q.to_tsquery(),
            "(rust & (job <-> queue) & !django) | phoenix"
        );
        assert_eq!(
            q.to_fts5(),
            "(\"rust\" AND \"job queue\" NOT \"django\") OR \"phoenix\""
        );

        // Single group: no wrapping parens.
        let q = parse_query("alpha beta").unwrap();
        assert_eq!(q.to_tsquery(), "alpha & beta");
        assert_eq!(q.to_fts5(), "\"alpha\" AND \"beta\"");
    }

    #[test]
    fn grammar_neutralizes_hostile_input() {
        // Engine operators, quotes, parens — sanitized into plain words, so
        // neither rendering can produce engine syntax errors or injection.
        let q = parse_query(r#"a:b (NEAR/2) *" OR 'x'&|!"#).unwrap();
        for rendered in [q.to_tsquery(), q.to_fts5()] {
            for forbidden in ["(NEAR", "*", "&|", "'x'"] {
                assert!(!rendered.contains(forbidden), "{rendered}");
            }
        }
        // Unbalanced quote: treated as phrase-to-end, still safe.
        let q = parse_query("\"unclosed phrase").unwrap();
        assert_eq!(q.to_fts5(), "\"unclosed phrase\"");

        // Rejections: empty and pure-negative queries.
        assert!(parse_query("").is_err());
        assert!(parse_query("   ").is_err());
        assert!(parse_query("-only -negative").is_err());
        assert!(parse_query("ok OR -bad").is_err());
    }

    #[test]
    fn identifier_validation_gates_setup_and_search() {
        #[cfg(feature = "sqlite")]
        {
            let db = crate::db::Db::memory().unwrap();
            assert!(setup(&db, "t; DROP TABLE x", "id", &["a"]).is_err());
            assert!(setup(&db, "t", "id", &[]).is_err());
            assert!(search(&db, "t", "id", &["a; --"], "q", 5).is_err());
        }
    }

    #[cfg(feature = "sqlite")]
    #[test]
    fn sqlite_fts5_end_to_end_with_trigger_sync() {
        use crate::backend::Backend;
        use crate::value::{ColType, Column, TableSchema};

        let db = crate::db::Db::memory().unwrap();
        db.migrate(
            &TableSchema::new("docs")
                .column(Column::new("id", ColType::Integer).primary())
                .column(Column::new("title", ColType::Text))
                .column(Column::new("body", ColType::Text).nullable()),
        )
        .unwrap();
        // A pre-existing row: the setup rebuild must index it.
        db.insert(
            "docs",
            &[
                ("title", Value::Text("rust job queue".into())),
                ("body", Value::Text("durable workers".into())),
            ],
            "id",
        )
        .unwrap();
        setup(&db, "docs", "id", &["title", "body"]).unwrap();
        setup(&db, "docs", "id", &["title", "body"]).unwrap(); // idempotent

        // Trigger sync: rows inserted after setup are searchable...
        let id2 = db
            .insert(
                "docs",
                &[
                    ("title", Value::Text("phoenix channels".into())),
                    ("body", Value::Null),
                ],
                "id",
            )
            .unwrap();
        let hits = search(&db, "docs", "id", &["title", "body"], "phoenix", 10).unwrap();
        assert_eq!(hits.len(), 1);

        // Phrase + negation semantics.
        let hits = search(
            &db,
            "docs",
            "id",
            &["title", "body"],
            "\"job queue\" -phoenix",
            10,
        )
        .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(
            hits[0].get("title").and_then(Json::as_str),
            Some("rust job queue")
        );

        // ...updates re-index...
        db.execute(
            "UPDATE docs SET title = ? WHERE id = ?",
            &[Value::Text("elixir channels".into()), Value::Int(id2)],
        )
        .unwrap();
        assert!(search(&db, "docs", "id", &["title", "body"], "phoenix", 10)
            .unwrap()
            .is_empty());
        assert_eq!(
            search(&db, "docs", "id", &["title", "body"], "elixir", 10)
                .unwrap()
                .len(),
            1
        );

        // ...and deletes drop out of the index.
        db.execute("DELETE FROM docs WHERE id = ?", &[Value::Int(id2)])
            .unwrap();
        assert!(search(&db, "docs", "id", &["title", "body"], "elixir", 10)
            .unwrap()
            .is_empty());

        // The FTS artifacts stay out of schema introspection (drift-clean).
        let tables: Vec<String> = db
            .introspect()
            .unwrap()
            .into_iter()
            .map(|t| t.table)
            .collect();
        assert_eq!(tables, vec!["docs".to_string()]);
    }

    #[cfg(feature = "sqlite")]
    #[test]
    fn hybrid_fuses_lexical_and_vector_legs() {
        use crate::backend::Backend;
        use crate::value::{ColType, Column, TableSchema};

        let db = crate::db::Db::memory().unwrap();
        db.migrate(
            &TableSchema::new("hy_docs")
                .column(Column::new("id", ColType::Integer).primary())
                .column(Column::new("title", ColType::Text))
                .column(Column::new("emb", ColType::Vector { dim: Some(2) })),
        )
        .unwrap();
        setup(&db, "hy_docs", "id", &["title"]).unwrap();
        let insert = |title: &str, v: [f32; 2]| {
            db.insert(
                "hy_docs",
                &[
                    ("title", Value::Text(title.into())),
                    ("emb", Value::Vector(v.to_vec())),
                ],
                "id",
            )
            .unwrap()
        };
        // Doc A: lexical rank 1 but vector rank 3. Doc B: vector rank 2,
        // no lexical match. Doc C: lexical rank 2 + vector rank 1 — RRF
        // (1/62 + 1/63) fuses it past A (1/62 + 1/64) and B (1/63 alone).
        insert("rust rust rust", [0.0, 1.0]);
        insert("unrelated title", [0.9, 0.1]);
        let c = insert("rust queue", [1.0, 0.0]);

        let hits = hybrid_search(
            &db,
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
}
