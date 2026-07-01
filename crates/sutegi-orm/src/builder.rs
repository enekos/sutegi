//! The fluent, parameterized query builders — driver-agnostic and
//! injection-safe. Every builder emits canonical `?`-placeholder SQL plus the
//! ordered bound parameters; each backend translates placeholders to its own
//! dialect (SQLite keeps `?`, Postgres rewrites to `$1, $2, …`).

use crate::value::Value;

/// A single `WHERE` predicate, shared by the SELECT/UPDATE/DELETE builders.
/// Predicates are joined with `AND`; use [`Predicate::Or`] for an OR group.
#[derive(Clone, Debug)]
enum Predicate {
    Cmp(String, String, Value),       // col op ?
    In(String, Vec<Value>),           // col IN (?, …)  — empty => "0 = 1"
    IsNull(String, bool),             // true => IS NULL, false => IS NOT NULL
    Or(Vec<(String, String, Value)>), // (a op ? OR b op ? …)
    Raw(String, Vec<Value>),          // an arbitrary parenthesized fragment
}

/// Render a predicate list to a `" WHERE …"` clause (empty string if none) plus
/// the ordered bound parameters. Shared by every builder so AND/OR/NULL/raw
/// behave identically across SELECT, UPDATE, and DELETE.
fn render_predicates(preds: &[Predicate]) -> (String, Vec<Value>) {
    if preds.is_empty() {
        return (String::new(), Vec::new());
    }
    let mut clauses = Vec::new();
    let mut params = Vec::new();
    for p in preds {
        match p {
            Predicate::Cmp(c, op, v) => {
                params.push(v.clone());
                clauses.push(format!("{} {} ?", c, op));
            }
            Predicate::In(c, values) => {
                if values.is_empty() {
                    clauses.push("0 = 1".to_string());
                } else {
                    let marks = vec!["?"; values.len()].join(", ");
                    clauses.push(format!("{} IN ({})", c, marks));
                    params.extend(values.iter().cloned());
                }
            }
            Predicate::IsNull(c, is_null) => {
                clauses.push(format!(
                    "{} IS {}NULL",
                    c,
                    if *is_null { "" } else { "NOT " }
                ));
            }
            Predicate::Or(group) => {
                if group.is_empty() {
                    clauses.push("0 = 1".to_string());
                } else {
                    let parts: Vec<String> = group
                        .iter()
                        .map(|(c, op, v)| {
                            params.push(v.clone());
                            format!("{} {} ?", c, op)
                        })
                        .collect();
                    clauses.push(format!("({})", parts.join(" OR ")));
                }
            }
            Predicate::Raw(frag, ps) => {
                clauses.push(format!("({})", frag));
                params.extend(ps.iter().cloned());
            }
        }
    }
    (format!(" WHERE {}", clauses.join(" AND ")), params)
}

/// A fluent, parameterized SELECT builder with filters, OR groups, joins,
/// grouping, ordering, and paging. Emits `?` placeholders and the matching
/// ordered parameter list — driver-agnostic and injection-safe.
#[derive(Clone, Debug)]
pub struct QueryBuilder {
    table: String,
    distinct: bool,
    columns: Vec<String>,
    joins: Vec<String>,
    preds: Vec<Predicate>,
    group_by: Vec<String>,
    order: Vec<(String, bool)>,
    limit: Option<i64>,
    offset: Option<i64>,
}

impl QueryBuilder {
    pub fn table(table: &str) -> QueryBuilder {
        QueryBuilder {
            table: table.to_string(),
            distinct: false,
            columns: Vec::new(),
            joins: Vec::new(),
            preds: Vec::new(),
            group_by: Vec::new(),
            order: Vec::new(),
            limit: None,
            offset: None,
        }
    }

    pub fn select(mut self, cols: &[&str]) -> QueryBuilder {
        self.columns = cols.iter().map(|c| c.to_string()).collect();
        self
    }

    /// `SELECT DISTINCT …`.
    pub fn distinct(mut self) -> QueryBuilder {
        self.distinct = true;
        self
    }

    /// Add a `WHERE col <op> ?` clause (AND-joined). `op` is e.g. `=`, `>`, `LIKE`.
    pub fn filter(mut self, col: &str, op: &str, value: Value) -> QueryBuilder {
        self.preds
            .push(Predicate::Cmp(col.to_string(), op.to_string(), value));
        self
    }

    /// Add a `WHERE col IN (?, ?, …)` clause. An empty list matches nothing.
    pub fn filter_in(mut self, col: &str, values: Vec<Value>) -> QueryBuilder {
        self.preds.push(Predicate::In(col.to_string(), values));
        self
    }

    /// `WHERE col IS NULL`.
    pub fn where_null(mut self, col: &str) -> QueryBuilder {
        self.preds.push(Predicate::IsNull(col.to_string(), true));
        self
    }

    /// `WHERE col IS NOT NULL`.
    pub fn where_not_null(mut self, col: &str) -> QueryBuilder {
        self.preds.push(Predicate::IsNull(col.to_string(), false));
        self
    }

    /// An OR group, AND-joined with the rest: `AND (a op ? OR b op ? …)`.
    pub fn or_group(mut self, group: &[(&str, &str, Value)]) -> QueryBuilder {
        self.preds.push(Predicate::Or(
            group
                .iter()
                .map(|(c, op, v)| (c.to_string(), op.to_string(), v.clone()))
                .collect(),
        ));
        self
    }

    /// A `col LIKE ?` convenience.
    pub fn like(self, col: &str, pattern: &str) -> QueryBuilder {
        self.filter(col, "LIKE", Value::Text(pattern.to_string()))
    }

    /// An arbitrary parenthesized WHERE fragment with its own bound params —
    /// an escape hatch for SQL the builder doesn't model.
    pub fn where_raw(mut self, fragment: &str, params: Vec<Value>) -> QueryBuilder {
        self.preds
            .push(Predicate::Raw(fragment.to_string(), params));
        self
    }

    /// `INNER JOIN other ON left = right`.
    pub fn join(mut self, other: &str, left: &str, right: &str) -> QueryBuilder {
        self.joins
            .push(format!("JOIN {} ON {} = {}", other, left, right));
        self
    }

    /// `LEFT JOIN other ON left = right`.
    pub fn left_join(mut self, other: &str, left: &str, right: &str) -> QueryBuilder {
        self.joins
            .push(format!("LEFT JOIN {} ON {} = {}", other, left, right));
        self
    }

    /// `GROUP BY …` (call with the grouping columns).
    pub fn group_by(mut self, cols: &[&str]) -> QueryBuilder {
        self.group_by = cols.iter().map(|c| c.to_string()).collect();
        self
    }

    /// Add an `ORDER BY` term. Call multiple times for tie-breaking columns.
    pub fn order_by(mut self, col: &str, descending: bool) -> QueryBuilder {
        self.order.push((col.to_string(), descending));
        self
    }

    pub fn limit(mut self, n: i64) -> QueryBuilder {
        self.limit = Some(n);
        self
    }

    pub fn offset(mut self, n: i64) -> QueryBuilder {
        self.offset = Some(n);
        self
    }

    fn build_from_and_joins(&self) -> String {
        let mut s = self.table.clone();
        for j in &self.joins {
            s.push(' ');
            s.push_str(j);
        }
        s
    }

    /// Build the SELECT SQL and ordered bound parameters.
    pub fn build(&self) -> (String, Vec<Value>) {
        let cols = if self.columns.is_empty() {
            "*".to_string()
        } else {
            self.columns.join(", ")
        };
        let distinct = if self.distinct { "DISTINCT " } else { "" };
        let (where_sql, params) = render_predicates(&self.preds);
        let mut sql = format!(
            "SELECT {}{} FROM {}{}",
            distinct,
            cols,
            self.build_from_and_joins(),
            where_sql
        );

        if !self.group_by.is_empty() {
            sql.push_str(&format!(" GROUP BY {}", self.group_by.join(", ")));
        }
        if !self.order.is_empty() {
            let terms: Vec<String> = self
                .order
                .iter()
                .map(|(c, desc)| format!("{} {}", c, if *desc { "DESC" } else { "ASC" }))
                .collect();
            sql.push_str(&format!(" ORDER BY {}", terms.join(", ")));
        }
        if let Some(n) = self.limit {
            sql.push_str(&format!(" LIMIT {}", n));
        }
        if let Some(n) = self.offset {
            sql.push_str(&format!(" OFFSET {}", n));
        }
        (sql, params)
    }

    /// Build a `SELECT COUNT(*)` over the same table/joins/filters (ignores
    /// columns/order/limit/group), for pagination totals.
    pub fn build_count(&self) -> (String, Vec<Value>) {
        let (where_sql, params) = render_predicates(&self.preds);
        (
            format!(
                "SELECT COUNT(*) AS count FROM {}{}",
                self.build_from_and_joins(),
                where_sql
            ),
            params,
        )
    }
}

/// A parameterized `UPDATE` builder.
#[derive(Clone, Debug)]
pub struct UpdateBuilder {
    table: String,
    sets: Vec<(String, Value)>,
    preds: Vec<Predicate>,
}

impl UpdateBuilder {
    pub fn table(table: &str) -> UpdateBuilder {
        UpdateBuilder {
            table: table.to_string(),
            sets: Vec::new(),
            preds: Vec::new(),
        }
    }
    pub fn set(mut self, col: &str, value: Value) -> UpdateBuilder {
        self.sets.push((col.to_string(), value));
        self
    }
    pub fn filter(mut self, col: &str, op: &str, value: Value) -> UpdateBuilder {
        self.preds
            .push(Predicate::Cmp(col.to_string(), op.to_string(), value));
        self
    }
    pub fn where_null(mut self, col: &str) -> UpdateBuilder {
        self.preds.push(Predicate::IsNull(col.to_string(), true));
        self
    }
    pub fn where_raw(mut self, fragment: &str, params: Vec<Value>) -> UpdateBuilder {
        self.preds
            .push(Predicate::Raw(fragment.to_string(), params));
        self
    }
    /// Returns `(sql, params)`. Params are SET values first, then WHERE values.
    pub fn build(&self) -> (String, Vec<Value>) {
        let mut params = Vec::new();
        let assignments: Vec<String> = self
            .sets
            .iter()
            .map(|(c, v)| {
                params.push(v.clone());
                format!("{} = ?", c)
            })
            .collect();
        let (where_sql, where_params) = render_predicates(&self.preds);
        params.extend(where_params);
        (
            format!(
                "UPDATE {} SET {}{}",
                self.table,
                assignments.join(", "),
                where_sql
            ),
            params,
        )
    }
}

/// A parameterized `DELETE` builder.
#[derive(Clone, Debug)]
pub struct DeleteBuilder {
    table: String,
    preds: Vec<Predicate>,
}

impl DeleteBuilder {
    pub fn table(table: &str) -> DeleteBuilder {
        DeleteBuilder {
            table: table.to_string(),
            preds: Vec::new(),
        }
    }
    pub fn filter(mut self, col: &str, op: &str, value: Value) -> DeleteBuilder {
        self.preds
            .push(Predicate::Cmp(col.to_string(), op.to_string(), value));
        self
    }
    pub fn where_null(mut self, col: &str) -> DeleteBuilder {
        self.preds.push(Predicate::IsNull(col.to_string(), true));
        self
    }
    pub fn where_raw(mut self, fragment: &str, params: Vec<Value>) -> DeleteBuilder {
        self.preds
            .push(Predicate::Raw(fragment.to_string(), params));
        self
    }
    pub fn build(&self) -> (String, Vec<Value>) {
        let (where_sql, params) = render_predicates(&self.preds);
        (format!("DELETE FROM {}{}", self.table, where_sql), params)
    }
}

/// A page of results plus paging metadata — the return of `Backend::paginate`.
#[derive(Clone, Debug)]
pub struct Page<T> {
    pub items: Vec<T>,
    pub total: i64,
    pub page: i64,
    pub per_page: i64,
}

impl<T> Page<T> {
    pub fn total_pages(&self) -> i64 {
        if self.per_page <= 0 {
            0
        } else {
            (self.total + self.per_page - 1) / self.per_page
        }
    }
    pub fn has_next(&self) -> bool {
        self.page < self.total_pages()
    }
    pub fn has_prev(&self) -> bool {
        self.page > 1
    }
}

impl Page<sutegi_json::Json> {
    /// `{ items, total, page, per_page, pages }` — ready to return from a handler.
    pub fn to_json(&self) -> sutegi_json::Json {
        sutegi_json::Json::obj(vec![
            ("items", sutegi_json::Json::arr(self.items.clone())),
            ("total", sutegi_json::Json::int(self.total)),
            ("page", sutegi_json::Json::int(self.page)),
            ("per_page", sutegi_json::Json::int(self.per_page)),
            ("pages", sutegi_json::Json::int(self.total_pages())),
        ])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_parameterized_select() {
        let (sql, params) = QueryBuilder::table("todos")
            .select(&["id", "title"])
            .filter("done", "=", Value::Bool(false))
            .order_by("id", true)
            .limit(10)
            .build();
        assert_eq!(
            sql,
            "SELECT id, title FROM todos WHERE done = ? ORDER BY id DESC LIMIT 10"
        );
        assert_eq!(params, vec![Value::Bool(false)]);
    }

    #[test]
    fn select_with_in_offset_and_multi_order() {
        let (sql, params) = QueryBuilder::table("todos")
            .filter_in("id", vec![Value::Int(1), Value::Int(2)])
            .order_by("done", false)
            .order_by("id", true)
            .limit(10)
            .offset(20)
            .build();
        assert_eq!(
            sql,
            "SELECT * FROM todos WHERE id IN (?, ?) ORDER BY done ASC, id DESC LIMIT 10 OFFSET 20"
        );
        assert_eq!(params.len(), 2);
    }

    #[test]
    fn empty_in_matches_nothing() {
        let (sql, _) = QueryBuilder::table("t").filter_in("id", vec![]).build();
        assert_eq!(sql, "SELECT * FROM t WHERE 0 = 1");
    }

    #[test]
    fn count_ignores_columns() {
        let (sql, _) = QueryBuilder::table("t")
            .select(&["a", "b"])
            .filter("done", "=", Value::Bool(true))
            .build_count();
        assert_eq!(sql, "SELECT COUNT(*) AS count FROM t WHERE done = ?");
    }

    #[test]
    fn or_group_null_like_and_joins() {
        let (sql, params) = QueryBuilder::table("todos")
            .filter("done", "=", Value::Bool(false))
            .or_group(&[
                ("priority", "=", Value::Text("high".into())),
                ("pinned", "=", Value::Bool(true)),
            ])
            .where_not_null("title")
            .like("title", "%sutegi%")
            .build();
        assert_eq!(
            sql,
            "SELECT * FROM todos WHERE done = ? AND (priority = ? OR pinned = ?) AND title IS NOT NULL AND title LIKE ?"
        );
        assert_eq!(params.len(), 4); // done, priority, pinned, like-pattern

        let (jsql, _) = QueryBuilder::table("todos")
            .select(&["todos.id", "users.name"])
            .join("users", "users.id", "todos.user_id")
            .group_by(&["users.name"])
            .build();
        assert_eq!(
            jsql,
            "SELECT todos.id, users.name FROM todos JOIN users ON users.id = todos.user_id GROUP BY users.name"
        );

        let (dsql, _) = QueryBuilder::table("t").distinct().select(&["a"]).build();
        assert_eq!(dsql, "SELECT DISTINCT a FROM t");
    }

    #[test]
    fn where_raw_fragment() {
        let (sql, params) = QueryBuilder::table("t")
            .where_raw(
                "created_at > ? AND created_at < ?",
                vec![Value::Int(1), Value::Int(9)],
            )
            .build();
        assert_eq!(
            sql,
            "SELECT * FROM t WHERE (created_at > ? AND created_at < ?)"
        );
        assert_eq!(params.len(), 2);
    }

    #[test]
    fn update_and_delete_builders() {
        let (sql, params) = UpdateBuilder::table("todos")
            .set("title", Value::Text("new".into()))
            .set("done", Value::Bool(true))
            .filter("id", "=", Value::Int(5))
            .build();
        assert_eq!(sql, "UPDATE todos SET title = ?, done = ? WHERE id = ?");
        assert_eq!(params.len(), 3);

        let (dsql, dparams) = DeleteBuilder::table("todos")
            .filter("id", "=", Value::Int(5))
            .build();
        assert_eq!(dsql, "DELETE FROM todos WHERE id = ?");
        assert_eq!(dparams, vec![Value::Int(5)]);
    }

    #[test]
    fn update_delete_support_null_and_raw() {
        let (usql, _) = UpdateBuilder::table("t")
            .set("done", Value::Bool(true))
            .where_null("deleted_at")
            .build();
        assert_eq!(usql, "UPDATE t SET done = ? WHERE deleted_at IS NULL");

        let (dsql, dparams) = DeleteBuilder::table("t")
            .where_raw("age > ?", vec![Value::Int(65)])
            .build();
        assert_eq!(dsql, "DELETE FROM t WHERE (age > ?)");
        assert_eq!(dparams, vec![Value::Int(65)]);
    }

    #[test]
    fn build_count_keeps_joins_and_filters() {
        let (sql, params) = QueryBuilder::table("todos")
            .join("users", "users.id", "todos.user_id")
            .filter("users.active", "=", Value::Bool(true))
            .build_count();
        assert_eq!(
            sql,
            "SELECT COUNT(*) AS count FROM todos JOIN users ON users.id = todos.user_id WHERE users.active = ?"
        );
        assert_eq!(params.len(), 1);
    }

    #[test]
    fn page_pagination_math() {
        let page = Page {
            items: vec![1, 2, 3],
            total: 10,
            page: 2,
            per_page: 3,
        };
        assert_eq!(page.total_pages(), 4); // ceil(10/3)
        assert!(page.has_next());
        assert!(page.has_prev());
        let first = Page {
            items: vec![1],
            total: 2,
            page: 1,
            per_page: 3,
        };
        assert!(!first.has_prev());
        let degenerate = Page {
            items: Vec::<i32>::new(),
            total: 5,
            page: 1,
            per_page: 0,
        };
        assert_eq!(degenerate.total_pages(), 0);
        let j = Page {
            items: vec![sutegi_json::Json::int(1)],
            total: 5,
            page: 1,
            per_page: 2,
        }
        .to_json();
        assert_eq!(j.get("pages").and_then(sutegi_json::Json::as_i64), Some(3));
        assert_eq!(j.get("total").and_then(sutegi_json::Json::as_i64), Some(5));
    }
}
