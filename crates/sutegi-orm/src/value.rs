//! Schema primitives: the scalar [`Value`] type bound to query parameters, the
//! column/table descriptors, and the DDL/JSON emitters derived from them.

use sutegi_json::Json;

/// A SQL value used as a bound parameter.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Null,
    Int(i64),
    Real(f64),
    Text(String),
    Bool(bool),
    /// A structured JSON document, bound to a `json`/`jsonb` (Postgres) or
    /// `TEXT` (SQLite) column.
    Json(Json),
    /// An embedding vector, bound to a `vector` (Postgres/pgvector) or `TEXT`
    /// (SQLite) column. Rendered in pgvector's canonical `[1,2,3]` text form.
    Vector(Vec<f32>),
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
            Value::Json(j) => j.clone(),
            Value::Vector(v) => Json::arr(v.iter().map(|x| Json::Num(*x as f64)).collect()),
        }
    }
}

/// Format a float slice as pgvector's text form: `[1,2,3]`. Shared by both
/// backends so a vector round-trips identically on SQLite and Postgres.
pub fn vector_to_text(v: &[f32]) -> String {
    let mut s = String::with_capacity(v.len() * 8 + 2);
    s.push('[');
    for (i, x) in v.iter().enumerate() {
        if i > 0 {
            s.push(',');
        }
        s.push_str(&x.to_string());
    }
    s.push(']');
    s
}

/// Parse pgvector's `[1,2,3]` text form (or a bare comma list) into floats.
pub fn vector_from_text(s: &str) -> Result<Vec<f32>, String> {
    let inner = s
        .trim()
        .trim_start_matches('[')
        .trim_end_matches(']')
        .trim();
    if inner.is_empty() {
        return Ok(Vec::new());
    }
    inner
        .split(',')
        .map(|p| {
            p.trim()
                .parse::<f32>()
                .map_err(|_| format!("invalid vector component '{}'", p.trim()))
        })
        .collect()
}

/// A column's storage type.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ColType {
    Integer,
    Real,
    Text,
    Boolean,
    /// A JSON document (`jsonb` on Postgres, `TEXT` on SQLite).
    Json,
    /// An embedding vector of an optional fixed dimension (`vector(dim)` on
    /// Postgres via pgvector, `TEXT` on SQLite).
    Vector {
        dim: Option<usize>,
    },
}

impl ColType {
    /// SQL type keyword (SQLite-flavored, the common denominator). JSON and
    /// vectors both fall back to `TEXT` on SQLite (which has no native type for
    /// either); the Postgres emitter maps them to richer types.
    pub fn sql(&self) -> &'static str {
        match self {
            ColType::Integer => "INTEGER",
            ColType::Real => "REAL",
            ColType::Text => "TEXT",
            ColType::Boolean => "BOOLEAN",
            ColType::Json => "TEXT",
            ColType::Vector { .. } => "TEXT",
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            ColType::Integer => "integer",
            ColType::Real => "real",
            ColType::Text => "text",
            ColType::Boolean => "boolean",
            ColType::Json => "json",
            ColType::Vector { .. } => "vector",
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
            let mut fields = vec![
                ("name", Json::str(c.name)),
                ("type", Json::str(c.ty.name())),
                ("nullable", Json::Bool(c.nullable)),
                ("primary", Json::Bool(c.primary)),
            ];
            // Expose the vector dimension so an agent knows the embedding shape.
            if let ColType::Vector { dim: Some(d) } = c.ty {
                fields.push(("dim", Json::int(d as i64)));
            }
            Json::obj(fields)
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
    fn json_and_vector_types_and_values() {
        // Both fall back to TEXT on SQLite; names surface for introspection.
        assert_eq!(ColType::Json.sql(), "TEXT");
        assert_eq!(ColType::Json.name(), "json");
        assert_eq!(ColType::Vector { dim: Some(3) }.sql(), "TEXT");
        assert_eq!(ColType::Vector { dim: None }.name(), "vector");

        // Value introspection: JSON is itself; a vector is a number array.
        let j = Json::obj(vec![("a", Json::int(1))]);
        assert_eq!(Value::Json(j.clone()).to_json(), j);
        assert_eq!(
            Value::Vector(vec![1.0, 2.5]).to_json(),
            Json::arr(vec![Json::Num(1.0), Json::Num(2.5)])
        );
    }

    #[test]
    fn vector_text_roundtrip() {
        assert_eq!(vector_to_text(&[1.0, 2.0, 3.5]), "[1,2,3.5]");
        assert_eq!(vector_to_text(&[]), "[]");
        assert_eq!(vector_from_text("[1,2,3.5]").unwrap(), vec![1.0, 2.0, 3.5]);
        assert_eq!(vector_from_text("  [ 1 , 2 ] ").unwrap(), vec![1.0, 2.0]);
        assert_eq!(vector_from_text("[]").unwrap(), Vec::<f32>::new());
        assert!(vector_from_text("[1,nope]").is_err());
    }

    #[test]
    fn schema_json_exposes_vector_dim() {
        let schema = TableSchema {
            table: "docs",
            columns: vec![Column {
                name: "embedding",
                ty: ColType::Vector { dim: Some(384) },
                nullable: false,
                primary: false,
            }],
        };
        let j = schema_json(&schema);
        let col = &j.get("columns").and_then(Json::as_array).unwrap()[0];
        assert_eq!(col.get("type").and_then(Json::as_str), Some("vector"));
        assert_eq!(col.get("dim").and_then(Json::as_i64), Some(384));
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
