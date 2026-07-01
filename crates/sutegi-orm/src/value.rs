//! Schema primitives: the scalar [`Value`] type bound to query parameters, the
//! column/table descriptors, and the DDL/JSON emitters derived from them.

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

/// Emit a `CREATE TABLE IF NOT EXISTS` statement from a schema (SQLite dialect;
/// the Postgres backend has its own [`crate::pg::create_table_pg`]).
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

#[cfg(test)]
mod tests {
    use super::*;

    fn todos() -> TableSchema {
        TableSchema {
            table: "todos",
            columns: vec![
                Column {
                    name: "id",
                    ty: ColType::Integer,
                    nullable: false,
                    primary: true,
                },
                Column {
                    name: "title",
                    ty: ColType::Text,
                    nullable: false,
                    primary: false,
                },
                Column {
                    name: "done",
                    ty: ColType::Boolean,
                    nullable: false,
                    primary: false,
                },
            ],
        }
    }

    #[test]
    fn emits_create_table() {
        let sql = create_table_sql(&todos());
        assert!(sql.contains("CREATE TABLE IF NOT EXISTS todos"));
        assert!(sql.contains("id INTEGER PRIMARY KEY"));
        assert!(sql.contains("title TEXT NOT NULL"));
    }

    #[test]
    fn coltype_and_value_mappings() {
        assert_eq!(ColType::Integer.sql(), "INTEGER");
        assert_eq!(ColType::Boolean.sql(), "BOOLEAN");
        assert_eq!(ColType::Real.name(), "real");
        assert_eq!(Value::Bool(true).to_json(), Json::Bool(true));
        assert_eq!(Value::Int(5).to_json(), Json::Num(5.0));
        assert_eq!(Value::Null.to_json(), Json::Null);
        assert_eq!(Value::Text("x".into()).to_json(), Json::str("x"));
    }

    #[test]
    fn schema_json_describes_columns() {
        let j = schema_json(&todos());
        assert_eq!(j.get("table").and_then(Json::as_str), Some("todos"));
        let cols = j.get("columns").and_then(Json::as_array).unwrap();
        assert_eq!(cols.len(), 3);
        let id = &cols[0];
        assert_eq!(id.get("name").and_then(Json::as_str), Some("id"));
        assert_eq!(id.get("primary").and_then(Json::as_bool), Some(true));
    }
}
