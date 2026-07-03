//! The fluent, parameterized query builders — driver-agnostic and
//! injection-safe. Every builder emits canonical `?`-placeholder SQL plus the
//! ordered bound parameters; each backend translates placeholders to its own
//! dialect (SQLite keeps `?`, Postgres rewrites to `$1, $2, …`).

use crate::value::Value;

/// Validate a SQL **identifier** (table or column name) that the builder
/// interpolates directly into SQL. Identifiers cannot be bound as parameters,
/// so an unchecked one sourced from user or LLM input is a SQL-injection
/// vector — the single most likely real-world exploit path for an agent-driven
/// app, where a tool argument can reach a column or `ORDER BY` slot.
///
/// Accepts a plain or singly-qualified name of ASCII word characters —
/// `col`, `table.col`, `table.*`, or `*`. Everything else (spaces, quotes,
/// parentheses, semicolons, operators, comment markers) is rejected. For
/// expressions the builder doesn't model (function calls, casts, aliases), use
/// the `where_raw` escape hatch, which is explicitly the caller's responsibility.
pub fn valid_identifier(s: &str) -> bool {
    // Allocation-free and byte-level (all valid identifier characters are
    // ASCII, so we never need UTF-8 decoding): this runs on every builder
    // setter, so it must be cheap. At most one `.` qualifier is allowed
    // (`table.col` / `table.*`).
    if s == "*" {
        return true;
    }
    let b = s.as_bytes();
    match b.iter().position(|&c| c == b'.') {
        Some(dot) => {
            let (qual, name) = (&b[..dot], &b[dot + 1..]);
            // The tail may be `*` (for `table.*`) but must not contain a
            // further `.` (that would be a third segment).
            is_plain_ident(qual)
                && (name == b"*" || (!name.contains(&b'.') && is_plain_ident(name)))
        }
        None => is_plain_ident(b),
    }
}

/// One unqualified identifier segment: a non-empty ASCII word starting with a
/// letter or `_`. Byte-level; non-ASCII bytes fail the class checks.
fn is_plain_ident(seg: &[u8]) -> bool {
    match seg.first() {
        Some(&c) if c.is_ascii_alphabetic() || c == b'_' => {}
        _ => return false,
    }
    seg[1..]
        .iter()
        .all(|&c| c.is_ascii_alphanumeric() || c == b'_')
}

/// Validate a comparison **operator** that is interpolated into a `WHERE`
/// clause. Only a fixed allowlist is permitted; anything else (e.g.
/// `"= 1 OR 1=1 --"`) would inject. `IN`/`IS NULL` have their own dedicated
/// builder methods and are not spelled as operators here. Allocation-free
/// (case-insensitive compare, no `to_uppercase`) since it runs per setter.
pub fn valid_operator(op: &str) -> bool {
    const OPS: [&str; 11] = [
        "=",
        "!=",
        "<>",
        "<",
        "<=",
        ">",
        ">=",
        "LIKE",
        "NOT LIKE",
        "ILIKE",
        "NOT ILIKE",
    ];
    OPS.iter().any(|o| op.eq_ignore_ascii_case(o))
}

/// Validate every identifier on an INSERT/UPSERT write path (`table`, each
/// column name, and any extra identifiers like the conflict/pk column). Called
/// by the `Backend::insert`/`upsert` primitives, whose column names are
/// interpolated into SQL just like the query builder's. Returns `Err` naming
/// the first offender.
pub fn validate_write_idents(
    table: &str,
    cols: &[(&str, Value)],
    extra: &[&str],
) -> Result<(), String> {
    if !valid_identifier(table) {
        return Err(format!("invalid table identifier: {table:?}"));
    }
    for (name, _) in cols {
        if !valid_identifier(name) {
            return Err(format!("invalid column identifier: {name:?}"));
        }
    }
    for name in extra {
        if !valid_identifier(name) {
            return Err(format!("invalid column identifier: {name:?}"));
        }
    }
    Ok(())
}

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
    /// First identifier/operator validation error, surfaced by [`build`]. Kept
    /// as a field so the setters stay fluent (`-> Self`) rather than `Result`.
    err: Option<String>,
}

/// Record the first invalid-identifier error into a builder's `err` slot.
fn check_ident(err: &mut Option<String>, kind: &str, s: &str) {
    if err.is_none() && !valid_identifier(s) {
        *err = Some(format!("invalid {kind} identifier: {s:?}"));
    }
}

/// Record the first invalid-operator error into a builder's `err` slot.
fn check_op(err: &mut Option<String>, op: &str) {
    if err.is_none() && !valid_operator(op) {
        *err = Some(format!("invalid comparison operator: {op:?}"));
    }
}

impl QueryBuilder {
    pub fn table(table: &str) -> QueryBuilder {
        let mut err = None;
        check_ident(&mut err, "table", table);
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
            err,
        }
    }

    pub fn select(mut self, cols: &[&str]) -> QueryBuilder {
        for c in cols {
            check_ident(&mut self.err, "column", c);
        }
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
        check_ident(&mut self.err, "column", col);
        check_op(&mut self.err, op);
        self.preds
            .push(Predicate::Cmp(col.to_string(), op.to_string(), value));
        self
    }

    /// Add a `WHERE col IN (?, ?, …)` clause. An empty list matches nothing.
    pub fn filter_in(mut self, col: &str, values: Vec<Value>) -> QueryBuilder {
        check_ident(&mut self.err, "column", col);
        self.preds.push(Predicate::In(col.to_string(), values));
        self
    }

    /// `WHERE col IS NULL`.
    pub fn where_null(mut self, col: &str) -> QueryBuilder {
        check_ident(&mut self.err, "column", col);
        self.preds.push(Predicate::IsNull(col.to_string(), true));
        self
    }

    /// `WHERE col IS NOT NULL`.
    pub fn where_not_null(mut self, col: &str) -> QueryBuilder {
        check_ident(&mut self.err, "column", col);
        self.preds.push(Predicate::IsNull(col.to_string(), false));
        self
    }

    /// An OR group, AND-joined with the rest: `AND (a op ? OR b op ? …)`.
    pub fn or_group(mut self, group: &[(&str, &str, Value)]) -> QueryBuilder {
        for (c, op, _) in group {
            check_ident(&mut self.err, "column", c);
            check_op(&mut self.err, op);
        }
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
        check_ident(&mut self.err, "join table", other);
        check_ident(&mut self.err, "join column", left);
        check_ident(&mut self.err, "join column", right);
        self.joins
            .push(format!("JOIN {} ON {} = {}", other, left, right));
        self
    }

    /// `LEFT JOIN other ON left = right`.
    pub fn left_join(mut self, other: &str, left: &str, right: &str) -> QueryBuilder {
        check_ident(&mut self.err, "join table", other);
        check_ident(&mut self.err, "join column", left);
        check_ident(&mut self.err, "join column", right);
        self.joins
            .push(format!("LEFT JOIN {} ON {} = {}", other, left, right));
        self
    }

    /// `GROUP BY …` (call with the grouping columns).
    pub fn group_by(mut self, cols: &[&str]) -> QueryBuilder {
        for c in cols {
            check_ident(&mut self.err, "group-by column", c);
        }
        self.group_by = cols.iter().map(|c| c.to_string()).collect();
        self
    }

    /// Add an `ORDER BY` term. Call multiple times for tie-breaking columns.
    pub fn order_by(mut self, col: &str, descending: bool) -> QueryBuilder {
        check_ident(&mut self.err, "order-by column", col);
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

    /// Build the SELECT SQL and ordered bound parameters. Returns `Err` if any
    /// identifier or operator supplied to a setter failed validation — an
    /// injection attempt (or a typo) surfaces here rather than reaching the DB.
    pub fn build(&self) -> Result<(String, Vec<Value>), String> {
        if let Some(e) = &self.err {
            return Err(e.clone());
        }
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
        Ok((sql, params))
    }

    /// Build a `SELECT COUNT(*)` over the same table/joins/filters (ignores
    /// columns/order/limit/group), for pagination totals.
    pub fn build_count(&self) -> Result<(String, Vec<Value>), String> {
        if let Some(e) = &self.err {
            return Err(e.clone());
        }
        let (where_sql, params) = render_predicates(&self.preds);
        Ok((
            format!(
                "SELECT COUNT(*) AS count FROM {}{}",
                self.build_from_and_joins(),
                where_sql
            ),
            params,
        ))
    }
}

/// A parameterized `UPDATE` builder.
#[derive(Clone, Debug)]
pub struct UpdateBuilder {
    table: String,
    sets: Vec<(String, Value)>,
    preds: Vec<Predicate>,
    err: Option<String>,
}

impl UpdateBuilder {
    pub fn table(table: &str) -> UpdateBuilder {
        let mut err = None;
        check_ident(&mut err, "table", table);
        UpdateBuilder {
            table: table.to_string(),
            sets: Vec::new(),
            preds: Vec::new(),
            err,
        }
    }
    pub fn set(mut self, col: &str, value: Value) -> UpdateBuilder {
        check_ident(&mut self.err, "column", col);
        self.sets.push((col.to_string(), value));
        self
    }
    pub fn filter(mut self, col: &str, op: &str, value: Value) -> UpdateBuilder {
        check_ident(&mut self.err, "column", col);
        check_op(&mut self.err, op);
        self.preds
            .push(Predicate::Cmp(col.to_string(), op.to_string(), value));
        self
    }
    pub fn where_null(mut self, col: &str) -> UpdateBuilder {
        check_ident(&mut self.err, "column", col);
        self.preds.push(Predicate::IsNull(col.to_string(), true));
        self
    }
    pub fn where_raw(mut self, fragment: &str, params: Vec<Value>) -> UpdateBuilder {
        self.preds
            .push(Predicate::Raw(fragment.to_string(), params));
        self
    }
    /// Returns `(sql, params)`. Params are SET values first, then WHERE values.
    /// `Err` if any identifier/operator failed validation.
    pub fn build(&self) -> Result<(String, Vec<Value>), String> {
        if let Some(e) = &self.err {
            return Err(e.clone());
        }
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
        Ok((
            format!(
                "UPDATE {} SET {}{}",
                self.table,
                assignments.join(", "),
                where_sql
            ),
            params,
        ))
    }
}

/// A parameterized `DELETE` builder.
#[derive(Clone, Debug)]
pub struct DeleteBuilder {
    table: String,
    preds: Vec<Predicate>,
    err: Option<String>,
}

impl DeleteBuilder {
    pub fn table(table: &str) -> DeleteBuilder {
        let mut err = None;
        check_ident(&mut err, "table", table);
        DeleteBuilder {
            table: table.to_string(),
            preds: Vec::new(),
            err,
        }
    }
    pub fn filter(mut self, col: &str, op: &str, value: Value) -> DeleteBuilder {
        check_ident(&mut self.err, "column", col);
        check_op(&mut self.err, op);
        self.preds
            .push(Predicate::Cmp(col.to_string(), op.to_string(), value));
        self
    }
    pub fn where_null(mut self, col: &str) -> DeleteBuilder {
        check_ident(&mut self.err, "column", col);
        self.preds.push(Predicate::IsNull(col.to_string(), true));
        self
    }
    pub fn where_raw(mut self, fragment: &str, params: Vec<Value>) -> DeleteBuilder {
        self.preds
            .push(Predicate::Raw(fragment.to_string(), params));
        self
    }
    /// `Err` if any identifier/operator failed validation.
    pub fn build(&self) -> Result<(String, Vec<Value>), String> {
        if let Some(e) = &self.err {
            return Err(e.clone());
        }
        let (where_sql, params) = render_predicates(&self.preds);
        Ok((format!("DELETE FROM {}{}", self.table, where_sql), params))
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
    fn identifier_validation_accepts_and_rejects() {
        // Legitimate names, qualified names, and wildcards.
        for ok in ["id", "user_id", "_x", "users.name", "users.*", "*", "a1"] {
            assert!(valid_identifier(ok), "should accept {ok:?}");
        }
        // Injection payloads and anything with SQL-significant characters.
        for bad in [
            "id; DROP TABLE users",
            "id) OR 1=1 --",
            "a.b.c",      // too many qualifiers
            "count(*)",   // parens
            "col name",   // space
            "\"quoted\"", // quote
            "",           // empty
            "1col",       // leading digit
            "a,b",        // comma
            "a-b",        // dash
        ] {
            assert!(!valid_identifier(bad), "should reject {bad:?}");
        }
    }

    #[test]
    fn operator_allowlist() {
        for ok in [
            "=", "!=", "<>", "<", "<=", ">", ">=", "like", "LIKE", "ILIKE", "not like",
        ] {
            assert!(valid_operator(ok), "should accept {ok:?}");
        }
        for bad in ["= 1 OR 1=1 --", "==", "; DROP", "GLOB", "BETWEEN", ""] {
            assert!(!valid_operator(bad), "should reject {bad:?}");
        }
    }

    #[test]
    fn injection_in_identifier_slots_is_rejected_at_build() {
        // Column, table, order-by, and operator slots each reject an injection
        // attempt with an error rather than emitting the payload as SQL.
        assert!(QueryBuilder::table("todos")
            .filter("id; DROP TABLE todos", "=", Value::Int(1))
            .build()
            .is_err());
        assert!(QueryBuilder::table("todos; DROP TABLE users")
            .build()
            .is_err());
        assert!(QueryBuilder::table("t")
            .order_by("name); DROP TABLE t --", false)
            .build()
            .is_err());
        assert!(QueryBuilder::table("t")
            .filter("id", "= 1 OR 1=1 --", Value::Int(1))
            .build()
            .is_err());
        // The write path (INSERT/UPSERT identifiers) is guarded too.
        assert!(validate_write_idents("t", &[("a); DROP --", Value::Int(1))], &[]).is_err());
        assert!(validate_write_idents("t", &[("a", Value::Int(1))], &["id"]).is_ok());
        // where_raw stays the explicit, caller-owned escape hatch (not validated).
        assert!(QueryBuilder::table("t")
            .where_raw("anything the caller wants", vec![])
            .build()
            .is_ok());
    }

    #[test]
    fn builds_parameterized_select() {
        let (sql, params) = QueryBuilder::table("todos")
            .select(&["id", "title"])
            .filter("done", "=", Value::Bool(false))
            .order_by("id", true)
            .limit(10)
            .build()
            .unwrap();
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
            .build()
            .unwrap();
        assert_eq!(
            sql,
            "SELECT * FROM todos WHERE id IN (?, ?) ORDER BY done ASC, id DESC LIMIT 10 OFFSET 20"
        );
        assert_eq!(params.len(), 2);
    }

    #[test]
    fn empty_in_matches_nothing() {
        let (sql, _) = QueryBuilder::table("t")
            .filter_in("id", vec![])
            .build()
            .unwrap();
        assert_eq!(sql, "SELECT * FROM t WHERE 0 = 1");
    }

    #[test]
    fn count_ignores_columns() {
        let (sql, _) = QueryBuilder::table("t")
            .select(&["a", "b"])
            .filter("done", "=", Value::Bool(true))
            .build_count()
            .unwrap();
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
            .build()
            .unwrap();
        assert_eq!(
            sql,
            "SELECT * FROM todos WHERE done = ? AND (priority = ? OR pinned = ?) AND title IS NOT NULL AND title LIKE ?"
        );
        assert_eq!(params.len(), 4); // done, priority, pinned, like-pattern

        let (jsql, _) = QueryBuilder::table("todos")
            .select(&["todos.id", "users.name"])
            .join("users", "users.id", "todos.user_id")
            .group_by(&["users.name"])
            .build()
            .unwrap();
        assert_eq!(
            jsql,
            "SELECT todos.id, users.name FROM todos JOIN users ON users.id = todos.user_id GROUP BY users.name"
        );

        let (dsql, _) = QueryBuilder::table("t")
            .distinct()
            .select(&["a"])
            .build()
            .unwrap();
        assert_eq!(dsql, "SELECT DISTINCT a FROM t");
    }

    #[test]
    fn where_raw_fragment() {
        let (sql, params) = QueryBuilder::table("t")
            .where_raw(
                "created_at > ? AND created_at < ?",
                vec![Value::Int(1), Value::Int(9)],
            )
            .build()
            .unwrap();
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
            .build()
            .unwrap();
        assert_eq!(sql, "UPDATE todos SET title = ?, done = ? WHERE id = ?");
        assert_eq!(params.len(), 3);

        let (dsql, dparams) = DeleteBuilder::table("todos")
            .filter("id", "=", Value::Int(5))
            .build()
            .unwrap();
        assert_eq!(dsql, "DELETE FROM todos WHERE id = ?");
        assert_eq!(dparams, vec![Value::Int(5)]);
    }

    #[test]
    fn update_delete_support_null_and_raw() {
        let (usql, _) = UpdateBuilder::table("t")
            .set("done", Value::Bool(true))
            .where_null("deleted_at")
            .build()
            .unwrap();
        assert_eq!(usql, "UPDATE t SET done = ? WHERE deleted_at IS NULL");

        let (dsql, dparams) = DeleteBuilder::table("t")
            .where_raw("age > ?", vec![Value::Int(65)])
            .build()
            .unwrap();
        assert_eq!(dsql, "DELETE FROM t WHERE (age > ?)");
        assert_eq!(dparams, vec![Value::Int(65)]);
    }

    #[test]
    fn build_count_keeps_joins_and_filters() {
        let (sql, params) = QueryBuilder::table("todos")
            .join("users", "users.id", "todos.user_id")
            .filter("users.active", "=", Value::Bool(true))
            .build_count()
            .unwrap();
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
