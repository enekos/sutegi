//! A small, driver-agnostic data layer: a typed schema, a fluent query
//! builder that emits parameterized SQL, and a migration emitter.
//!
//! There is intentionally **no bundled database driver** — that would pull in
//! a C library and inflate the binary. The builder produces `(sql, params)`
//! that you hand to whatever driver you opt into (e.g. enable a `rusqlite`
//! feature later). What ships here is pure std, and every model can describe
//! itself as JSON so an agent can discover the data model at runtime.

use sutegi_json::Json;

/// A SQL scalar value used as a bound parameter.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Null,
    Int(i64),
    Real(f64),
    Text(String),
    Bool(bool),
}

impl Value {
    /// Render for display/introspection (NOT for SQL interpolation — use
    /// placeholders for that).
    pub fn to_json(&self) -> Json {
        match self {
            Value::Null => Json::Null,
            Value::Int(i) => Json::Num(*i as f64),
            Value::Real(r) => Json::Num(*r),
            Value::Text(s) => Json::str(s.clone()),
            Value::Bool(b) => Json::Bool(*b),
        }
    }
}

/// A column's storage type.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ColType {
    Integer,
    Real,
    Text,
    Boolean,
}

impl ColType {
    /// SQL type keyword (SQLite-flavored, the common denominator).
    pub fn sql(&self) -> &'static str {
        match self {
            ColType::Integer => "INTEGER",
            ColType::Real => "REAL",
            ColType::Text => "TEXT",
            ColType::Boolean => "BOOLEAN",
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            ColType::Integer => "integer",
            ColType::Real => "real",
            ColType::Text => "text",
            ColType::Boolean => "boolean",
        }
    }
}

/// A single column definition.
#[derive(Clone, Debug)]
pub struct Column {
    pub name: &'static str,
    pub ty: ColType,
    pub nullable: bool,
    pub primary: bool,
}

/// A table's full schema.
#[derive(Clone, Debug)]
pub struct TableSchema {
    pub table: &'static str,
    pub columns: Vec<Column>,
}

/// Anything that maps to a table. Implementors describe their schema; the
/// framework derives migrations, query helpers, and introspection from it.
pub trait Model {
    fn schema() -> TableSchema;

    fn table() -> &'static str {
        Self::schema().table
    }

    /// Start a query builder scoped to this model's table.
    fn query() -> QueryBuilder {
        QueryBuilder::table(Self::schema().table)
    }
}

/// Describe a table schema as JSON, for `/__introspect`.
pub fn schema_json(schema: &TableSchema) -> Json {
    let cols = schema
        .columns
        .iter()
        .map(|c| {
            Json::obj(vec![
                ("name", Json::str(c.name)),
                ("type", Json::str(c.ty.name())),
                ("nullable", Json::Bool(c.nullable)),
                ("primary", Json::Bool(c.primary)),
            ])
        })
        .collect();
    Json::obj(vec![
        ("table", Json::str(schema.table)),
        ("columns", Json::arr(cols)),
    ])
}

/// Emit a `CREATE TABLE IF NOT EXISTS` statement from a schema.
pub fn create_table_sql(schema: &TableSchema) -> String {
    let mut cols = Vec::new();
    for c in &schema.columns {
        let mut def = format!("  {} {}", c.name, c.ty.sql());
        if c.primary {
            def.push_str(" PRIMARY KEY");
        }
        if !c.nullable && !c.primary {
            def.push_str(" NOT NULL");
        }
        cols.push(def);
    }
    format!(
        "CREATE TABLE IF NOT EXISTS {} (\n{}\n);",
        schema.table,
        cols.join(",\n")
    )
}

/// A fluent, parameterized SELECT builder. Emits `?` placeholders and the
/// matching ordered parameter list — driver-agnostic and injection-safe.
#[derive(Clone, Debug)]
pub struct QueryBuilder {
    table: String,
    columns: Vec<String>,
    wheres: Vec<(String, String, Value)>,
    order: Option<(String, bool)>,
    limit: Option<i64>,
}

impl QueryBuilder {
    pub fn table(table: &str) -> QueryBuilder {
        QueryBuilder {
            table: table.to_string(),
            columns: Vec::new(),
            wheres: Vec::new(),
            order: None,
            limit: None,
        }
    }

    pub fn select(mut self, cols: &[&str]) -> QueryBuilder {
        self.columns = cols.iter().map(|c| c.to_string()).collect();
        self
    }

    /// Add a `WHERE col <op> ?` clause. `op` is e.g. `=`, `>`, `LIKE`.
    pub fn filter(mut self, col: &str, op: &str, value: Value) -> QueryBuilder {
        self.wheres.push((col.to_string(), op.to_string(), value));
        self
    }

    pub fn order_by(mut self, col: &str, descending: bool) -> QueryBuilder {
        self.order = Some((col.to_string(), descending));
        self
    }

    pub fn limit(mut self, n: i64) -> QueryBuilder {
        self.limit = Some(n);
        self
    }

    /// Build the SQL string and the ordered list of bound parameters.
    pub fn build(&self) -> (String, Vec<Value>) {
        let cols = if self.columns.is_empty() {
            "*".to_string()
        } else {
            self.columns.join(", ")
        };
        let mut sql = format!("SELECT {} FROM {}", cols, self.table);
        let mut params = Vec::new();

        if !self.wheres.is_empty() {
            let clauses: Vec<String> = self
                .wheres
                .iter()
                .map(|(c, op, v)| {
                    params.push(v.clone());
                    format!("{} {} ?", c, op)
                })
                .collect();
            sql.push_str(" WHERE ");
            sql.push_str(&clauses.join(" AND "));
        }

        if let Some((col, desc)) = &self.order {
            sql.push_str(&format!(
                " ORDER BY {} {}",
                col,
                if *desc { "DESC" } else { "ASC" }
            ));
        }

        if let Some(n) = self.limit {
            sql.push_str(&format!(" LIMIT {}", n));
        }

        (sql, params)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn todos() -> TableSchema {
        TableSchema {
            table: "todos",
            columns: vec![
                Column { name: "id", ty: ColType::Integer, nullable: false, primary: true },
                Column { name: "title", ty: ColType::Text, nullable: false, primary: false },
                Column { name: "done", ty: ColType::Boolean, nullable: false, primary: false },
            ],
        }
    }

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
    fn emits_create_table() {
        let sql = create_table_sql(&todos());
        assert!(sql.contains("CREATE TABLE IF NOT EXISTS todos"));
        assert!(sql.contains("id INTEGER PRIMARY KEY"));
        assert!(sql.contains("title TEXT NOT NULL"));
    }
}
