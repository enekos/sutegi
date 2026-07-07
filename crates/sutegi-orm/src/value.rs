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

/// The SQL dialect a schema is rendered into. Everything schema-shaped —
/// storage types, DDL emission, diffing — is parameterized by this so the same
/// [`TableSchema`] runs on both backends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dialect {
    Sqlite,
    Postgres,
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
    /// The SQL type this column occupies in `dialect`. JSON and vectors both
    /// fall back to `TEXT` on SQLite (which has native types for neither).
    ///
    /// This is also the **comparison key for schema diffing**: two `ColType`s
    /// that share a storage type in a dialect (e.g. `Text` vs `Json` on SQLite)
    /// are indistinguishable in that database and must not diff.
    pub fn storage(&self, dialect: Dialect) -> String {
        match (self, dialect) {
            (ColType::Integer, Dialect::Sqlite) => "INTEGER".into(),
            (ColType::Integer, Dialect::Postgres) => "BIGINT".into(),
            (ColType::Real, Dialect::Sqlite) => "REAL".into(),
            (ColType::Real, Dialect::Postgres) => "DOUBLE PRECISION".into(),
            (ColType::Text, _) => "TEXT".into(),
            (ColType::Boolean, _) => "BOOLEAN".into(),
            (ColType::Json, Dialect::Sqlite) => "TEXT".into(),
            (ColType::Json, Dialect::Postgres) => "JSONB".into(),
            (ColType::Vector { .. }, Dialect::Sqlite) => "TEXT".into(),
            (ColType::Vector { dim: Some(d) }, Dialect::Postgres) => format!("vector({d})"),
            (ColType::Vector { dim: None }, Dialect::Postgres) => "vector".into(),
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

    /// Inverse of [`name`](ColType::name), for deserializing schema files.
    /// A vector's dimension travels separately (see `schema_json`'s `dim`).
    pub fn from_name(name: &str, dim: Option<usize>) -> Option<ColType> {
        match name {
            "integer" => Some(ColType::Integer),
            "real" => Some(ColType::Real),
            "text" => Some(ColType::Text),
            "boolean" => Some(ColType::Boolean),
            "json" => Some(ColType::Json),
            "vector" => Some(ColType::Vector { dim }),
            _ => None,
        }
    }
}

/// A single column definition. Build with [`Column::new`] and chain the
/// modifiers: `Column::new("email", ColType::Text).unique()`.
#[derive(Clone, Debug, PartialEq)]
pub struct Column {
    pub name: String,
    pub ty: ColType,
    pub nullable: bool,
    pub primary: bool,
    pub unique: bool,
    /// Rendered as `DEFAULT <literal>` in DDL — and required before a
    /// `NOT NULL` column can be added to a table that already has rows.
    pub default: Option<Value>,
}

impl Column {
    /// A `NOT NULL`, non-primary, non-unique column with no default.
    pub fn new(name: impl Into<String>, ty: ColType) -> Column {
        Column {
            name: name.into(),
            ty,
            nullable: false,
            primary: false,
            unique: false,
            default: None,
        }
    }

    pub fn primary(mut self) -> Column {
        self.primary = true;
        self
    }

    pub fn nullable(mut self) -> Column {
        self.nullable = true;
        self
    }

    pub fn unique(mut self) -> Column {
        self.unique = true;
        self
    }

    pub fn default(mut self, value: Value) -> Column {
        self.default = Some(value);
        self
    }
}

/// A secondary index over one or more columns.
#[derive(Clone, Debug, PartialEq)]
pub struct Index {
    pub name: String,
    pub columns: Vec<String>,
    pub unique: bool,
}

impl Index {
    /// The conventional index name: `idx_<table>_<col1>_<col2>`.
    pub fn conventional_name(table: &str, columns: &[&str]) -> String {
        format!("idx_{}_{}", table, columns.join("_"))
    }
}

/// What happens to referencing rows when the referenced row is deleted.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FkAction {
    #[default]
    NoAction,
    Cascade,
    SetNull,
    Restrict,
}

impl FkAction {
    pub fn sql(&self) -> &'static str {
        match self {
            FkAction::NoAction => "NO ACTION",
            FkAction::Cascade => "CASCADE",
            FkAction::SetNull => "SET NULL",
            FkAction::Restrict => "RESTRICT",
        }
    }

    pub fn from_sql(s: &str) -> FkAction {
        match s.to_ascii_uppercase().as_str() {
            "CASCADE" => FkAction::Cascade,
            "SET NULL" => FkAction::SetNull,
            "RESTRICT" => FkAction::Restrict,
            _ => FkAction::NoAction,
        }
    }
}

/// A foreign-key constraint: `column` references `ref_table.ref_column`.
#[derive(Clone, Debug, PartialEq)]
pub struct ForeignKey {
    pub column: String,
    pub ref_table: String,
    pub ref_column: String,
    pub on_delete: FkAction,
}

/// A table's full schema. Build with [`TableSchema::new`] and chain
/// `.column(…)`, `.index(…)`, `.foreign_key(…)`.
#[derive(Clone, Debug, PartialEq)]
pub struct TableSchema {
    pub table: String,
    pub columns: Vec<Column>,
    pub indexes: Vec<Index>,
    pub foreign_keys: Vec<ForeignKey>,
}

impl TableSchema {
    pub fn new(table: impl Into<String>) -> TableSchema {
        TableSchema {
            table: table.into(),
            columns: Vec::new(),
            indexes: Vec::new(),
            foreign_keys: Vec::new(),
        }
    }

    pub fn column(mut self, column: Column) -> TableSchema {
        self.columns.push(column);
        self
    }

    /// Add a conventionally-named (`idx_<table>_<cols>`) non-unique index.
    pub fn index(mut self, columns: &[&str]) -> TableSchema {
        self.indexes.push(Index {
            name: Index::conventional_name(&self.table, columns),
            columns: columns.iter().map(|c| c.to_string()).collect(),
            unique: false,
        });
        self
    }

    /// Add a conventionally-named unique index.
    pub fn unique_index(mut self, columns: &[&str]) -> TableSchema {
        self.indexes.push(Index {
            name: Index::conventional_name(&self.table, columns),
            columns: columns.iter().map(|c| c.to_string()).collect(),
            unique: true,
        });
        self
    }

    pub fn foreign_key(
        mut self,
        column: &str,
        ref_table: &str,
        ref_column: &str,
        on_delete: FkAction,
    ) -> TableSchema {
        self.foreign_keys.push(ForeignKey {
            column: column.into(),
            ref_table: ref_table.into(),
            ref_column: ref_column.into(),
            on_delete,
        });
        self
    }

    /// Look a column up by name.
    pub fn col(&self, name: &str) -> Option<&Column> {
        self.columns.iter().find(|c| c.name == name)
    }

    /// A canonical copy for comparison: columns keep their declared order (it is
    /// semantic — it drives row shape), but indexes and foreign keys are sorted
    /// into a stable order, since they're *sets* whose declaration order carries
    /// no meaning. Diffing and round-trip checks compare normalized schemas so a
    /// database that reports its indexes in a different order doesn't false-diff.
    pub fn normalized(&self) -> TableSchema {
        let mut s = self.clone();
        s.indexes.sort_by(|a, b| a.name.cmp(&b.name));
        s.foreign_keys.sort_by(|a, b| {
            (&a.column, &a.ref_table, &a.ref_column).cmp(&(&b.column, &b.ref_table, &b.ref_column))
        });
        s
    }
}

/// Render a [`Value`] as a SQL DDL literal, for `DEFAULT` clauses. Text (and
/// text-stored JSON/vectors) is single-quoted with `'` doubled.
pub fn default_sql(value: &Value) -> String {
    fn quote(s: &str) -> String {
        format!("'{}'", s.replace('\'', "''"))
    }
    match value {
        Value::Null => "NULL".into(),
        Value::Int(i) => i.to_string(),
        Value::Real(r) => r.to_string(),
        Value::Bool(b) => if *b { "TRUE" } else { "FALSE" }.into(),
        Value::Text(s) => quote(s),
        Value::Json(j) => quote(&j.to_string()),
        Value::Vector(v) => quote(&vector_to_text(v)),
    }
}

/// Parse a SQL `DEFAULT` clause literal back into a [`Value`] — the best-effort
/// inverse of [`default_sql`], used when introspecting a live database.
///
/// Handles the literal forms both dialects emit (quoted strings, integers,
/// floats, `TRUE`/`FALSE`) and strips Postgres's trailing `::type` cast. Returns
/// `None` for `NULL` or any expression it can't reduce to a scalar (a computed
/// default like `now()` has no `Value` equivalent; the diff treats it as absent).
pub fn parse_default_literal(raw: &str) -> Option<Value> {
    let mut s = raw.trim();
    // Strip a Postgres cast suffix: `'draft'::text`, `0::bigint`.
    if let Some(idx) = s.rfind("::") {
        let cast = &s[idx + 2..];
        if !cast.is_empty()
            && cast
                .chars()
                .all(|c| c.is_ascii_alphabetic() || c == ' ' || c == '"')
        {
            s = s[..idx].trim();
        }
    }
    if s.eq_ignore_ascii_case("null") || s.is_empty() {
        return None;
    }
    if s.eq_ignore_ascii_case("true") {
        return Some(Value::Bool(true));
    }
    if s.eq_ignore_ascii_case("false") {
        return Some(Value::Bool(false));
    }
    if let Some(inner) = s.strip_prefix('\'').and_then(|r| r.strip_suffix('\'')) {
        return Some(Value::Text(inner.replace("''", "'")));
    }
    if let Ok(i) = s.parse::<i64>() {
        return Some(Value::Int(i));
    }
    if let Ok(r) = s.parse::<f64>() {
        return Some(Value::Real(r));
    }
    None
}

/// Describe a table schema as JSON, for `/__introspect` and schema files.
pub fn schema_json(schema: &TableSchema) -> Json {
    let cols = schema
        .columns
        .iter()
        .map(|c| {
            let mut fields = vec![
                ("name", Json::str(c.name.clone())),
                ("type", Json::str(c.ty.name())),
                ("nullable", Json::Bool(c.nullable)),
                ("primary", Json::Bool(c.primary)),
            ];
            if c.unique {
                fields.push(("unique", Json::Bool(true)));
            }
            if let Some(d) = &c.default {
                fields.push(("default", d.to_json()));
            }
            // Expose the vector dimension so an agent knows the embedding shape.
            if let ColType::Vector { dim: Some(d) } = c.ty {
                fields.push(("dim", Json::int(d as i64)));
            }
            Json::obj(fields)
        })
        .collect();
    let mut fields = vec![
        ("table", Json::str(schema.table.clone())),
        ("columns", Json::arr(cols)),
    ];
    if !schema.indexes.is_empty() {
        fields.push((
            "indexes",
            Json::arr(
                schema
                    .indexes
                    .iter()
                    .map(|i| {
                        Json::obj(vec![
                            ("name", Json::str(i.name.clone())),
                            (
                                "columns",
                                Json::arr(i.columns.iter().map(|c| Json::str(c.clone())).collect()),
                            ),
                            ("unique", Json::Bool(i.unique)),
                        ])
                    })
                    .collect(),
            ),
        ));
    }
    if !schema.foreign_keys.is_empty() {
        fields.push((
            "foreign_keys",
            Json::arr(
                schema
                    .foreign_keys
                    .iter()
                    .map(|f| {
                        Json::obj(vec![
                            ("column", Json::str(f.column.clone())),
                            ("ref_table", Json::str(f.ref_table.clone())),
                            ("ref_column", Json::str(f.ref_column.clone())),
                            ("on_delete", Json::str(f.on_delete.sql())),
                        ])
                    })
                    .collect(),
            ),
        ));
    }
    Json::obj(fields)
}

// ---------------------------------------------------------------------------
// Lossless JSON serialization for migration files (schema_json above is for the
// human/agent-facing /__introspect view; this pair round-trips exactly).
// ---------------------------------------------------------------------------

/// A [`Value`] as a self-describing tagged object, so a default's exact variant
/// survives a round trip (`Int(5)` and `Real(5.0)` are distinguishable, unlike
/// [`Value::to_json`]).
pub fn value_to_json(v: &Value) -> Json {
    let (t, val) = match v {
        Value::Null => ("null", Json::Null),
        Value::Int(i) => ("int", Json::int(*i)),
        Value::Real(r) => ("real", Json::Num(*r)),
        Value::Text(s) => ("text", Json::str(s.clone())),
        Value::Bool(b) => ("bool", Json::Bool(*b)),
        Value::Json(j) => ("json", j.clone()),
        Value::Vector(vec) => (
            "vector",
            Json::arr(vec.iter().map(|x| Json::Num(*x as f64)).collect()),
        ),
    };
    Json::obj(vec![("t", Json::str(t)), ("v", val)])
}

/// Parse a [`value_to_json`] tagged object back into a [`Value`].
pub fn value_from_json(j: &Json) -> Result<Value, String> {
    let t = j
        .get("t")
        .and_then(Json::as_str)
        .ok_or("value: missing tag `t`")?;
    let v = j.get("v").unwrap_or(&Json::Null);
    Ok(match t {
        "null" => Value::Null,
        "int" => Value::Int(v.as_i64().ok_or("value: `int` needs a number")?),
        "real" => Value::Real(v.as_f64().ok_or("value: `real` needs a number")?),
        "text" => Value::Text(
            v.as_str()
                .ok_or("value: `text` needs a string")?
                .to_string(),
        ),
        "bool" => Value::Bool(v.as_bool().ok_or("value: `bool` needs a boolean")?),
        "json" => Value::Json(v.clone()),
        "vector" => {
            let arr = v.as_array().ok_or("value: `vector` needs an array")?;
            Value::Vector(
                arr.iter()
                    .filter_map(|x| x.as_f64().map(|f| f as f32))
                    .collect(),
            )
        }
        other => return Err(format!("value: unknown tag `{other}`")),
    })
}

/// Serialize a [`Column`] losslessly (tagged default), for migration files.
pub fn column_to_json(c: &Column) -> Json {
    let mut f = vec![
        ("name", Json::str(c.name.clone())),
        ("type", Json::str(c.ty.name())),
        ("nullable", Json::Bool(c.nullable)),
        ("primary", Json::Bool(c.primary)),
        ("unique", Json::Bool(c.unique)),
    ];
    if let ColType::Vector { dim: Some(d) } = c.ty {
        f.push(("dim", Json::int(d as i64)));
    }
    if let Some(d) = &c.default {
        f.push(("default", value_to_json(d)));
    }
    Json::obj(f)
}

/// Parse a [`column_to_json`] object back into a [`Column`].
pub fn column_from_json(c: &Json) -> Result<Column, String> {
    let name = c
        .get("name")
        .and_then(Json::as_str)
        .ok_or("column: missing `name`")?;
    let type_name = c
        .get("type")
        .and_then(Json::as_str)
        .ok_or("column: missing `type`")?;
    let dim = c.get("dim").and_then(Json::as_i64).map(|d| d as usize);
    let ty = ColType::from_name(type_name, dim)
        .ok_or_else(|| format!("column: unknown type `{type_name}`"))?;
    let mut col = Column::new(name, ty);
    if c.get("nullable").and_then(Json::as_bool) == Some(true) {
        col = col.nullable();
    }
    if c.get("primary").and_then(Json::as_bool) == Some(true) {
        col = col.primary();
    }
    if c.get("unique").and_then(Json::as_bool) == Some(true) {
        col = col.unique();
    }
    if let Some(d) = c.get("default") {
        if !matches!(d, Json::Null) {
            col = col.default(value_from_json(d)?);
        }
    }
    Ok(col)
}

/// Serialize an [`Index`].
pub fn index_to_json(i: &Index) -> Json {
    Json::obj(vec![
        ("name", Json::str(i.name.clone())),
        (
            "columns",
            Json::arr(i.columns.iter().map(|c| Json::str(c.clone())).collect()),
        ),
        ("unique", Json::Bool(i.unique)),
    ])
}

/// Parse an [`index_to_json`] object.
pub fn index_from_json(i: &Json) -> Result<Index, String> {
    Ok(Index {
        name: i
            .get("name")
            .and_then(Json::as_str)
            .ok_or("index: missing `name`")?
            .to_string(),
        columns: i
            .get("columns")
            .and_then(Json::as_array)
            .map(Vec::as_slice)
            .unwrap_or(&[])
            .iter()
            .filter_map(|c| c.as_str().map(str::to_string))
            .collect(),
        unique: i.get("unique").and_then(Json::as_bool).unwrap_or(false),
    })
}

/// Serialize a [`ForeignKey`].
pub fn fk_to_json(f: &ForeignKey) -> Json {
    Json::obj(vec![
        ("column", Json::str(f.column.clone())),
        ("ref_table", Json::str(f.ref_table.clone())),
        ("ref_column", Json::str(f.ref_column.clone())),
        ("on_delete", Json::str(f.on_delete.sql())),
    ])
}

/// Parse an [`fk_to_json`] object.
pub fn fk_from_json(f: &Json) -> Result<ForeignKey, String> {
    Ok(ForeignKey {
        column: f
            .get("column")
            .and_then(Json::as_str)
            .ok_or("fk: missing `column`")?
            .to_string(),
        ref_table: f
            .get("ref_table")
            .and_then(Json::as_str)
            .ok_or("fk: missing `ref_table`")?
            .to_string(),
        ref_column: f
            .get("ref_column")
            .and_then(Json::as_str)
            .ok_or("fk: missing `ref_column`")?
            .to_string(),
        on_delete: FkAction::from_sql(f.get("on_delete").and_then(Json::as_str).unwrap_or("")),
    })
}

/// Serialize a [`TableSchema`] losslessly, for a migration file.
pub fn schema_to_json(schema: &TableSchema) -> Json {
    Json::obj(vec![
        ("table", Json::str(schema.table.clone())),
        (
            "columns",
            Json::arr(schema.columns.iter().map(column_to_json).collect()),
        ),
        (
            "indexes",
            Json::arr(schema.indexes.iter().map(index_to_json).collect()),
        ),
        (
            "foreign_keys",
            Json::arr(schema.foreign_keys.iter().map(fk_to_json).collect()),
        ),
    ])
}

/// Parse a [`schema_to_json`] object back into a [`TableSchema`].
pub fn schema_from_json(j: &Json) -> Result<TableSchema, String> {
    let table = j
        .get("table")
        .and_then(Json::as_str)
        .ok_or("schema: missing `table`")?
        .to_string();
    let mut schema = TableSchema::new(table);
    for c in j
        .get("columns")
        .and_then(Json::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
    {
        schema.columns.push(column_from_json(c)?);
    }
    for i in j
        .get("indexes")
        .and_then(Json::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
    {
        schema.indexes.push(index_from_json(i)?);
    }
    for f in j
        .get("foreign_keys")
        .and_then(Json::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
    {
        schema.foreign_keys.push(fk_from_json(f)?);
    }
    Ok(schema)
}

/// Render one column definition (no indent), including inline `PRIMARY KEY`,
/// `NOT NULL`, `UNIQUE`, and `DEFAULT`. The Postgres integer-primary-key case
/// becomes an identity column. Shared by `CREATE TABLE` and `ADD COLUMN`.
pub(crate) fn column_sql(c: &Column, dialect: Dialect) -> String {
    if dialect == Dialect::Postgres && c.primary && c.ty == ColType::Integer {
        // BY DEFAULT (not ALWAYS) mirrors SQLite's `INTEGER PRIMARY KEY`:
        // auto-generated when omitted, but explicit values are allowed
        // (needed for upsert-by-id and seeding).
        return format!(
            "{} BIGINT GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY",
            c.name
        );
    }
    let mut def = format!("{} {}", c.name, c.ty.storage(dialect));
    if c.primary {
        def.push_str(" PRIMARY KEY");
    }
    if !c.nullable && !c.primary {
        def.push_str(" NOT NULL");
    }
    if c.unique && !c.primary {
        def.push_str(" UNIQUE");
    }
    if let Some(d) = &c.default {
        def.push_str(" DEFAULT ");
        def.push_str(&default_sql(d));
    }
    def
}

/// Render a table-level `FOREIGN KEY (...) REFERENCES ...` clause (no indent).
pub(crate) fn fk_clause(fk: &ForeignKey) -> String {
    let mut clause = format!(
        "FOREIGN KEY ({}) REFERENCES {} ({})",
        fk.column, fk.ref_table, fk.ref_column
    );
    if fk.on_delete != FkAction::NoAction {
        clause.push_str(" ON DELETE ");
        clause.push_str(fk.on_delete.sql());
    }
    clause
}

/// Render `CREATE [UNIQUE] INDEX IF NOT EXISTS <name> ON <table> (cols)`.
pub(crate) fn create_index_sql(table: &str, index: &Index) -> String {
    format!(
        "CREATE {}INDEX IF NOT EXISTS {} ON {} ({})",
        if index.unique { "UNIQUE " } else { "" },
        index.name,
        table,
        index.columns.join(", ")
    )
}

/// Emit just the `CREATE TABLE IF NOT EXISTS (...)` statement (columns + inline
/// foreign keys), without the secondary-index statements.
pub(crate) fn create_table_only(schema: &TableSchema, dialect: Dialect) -> String {
    let mut cols: Vec<String> = schema
        .columns
        .iter()
        .map(|c| format!("  {}", column_sql(c, dialect)))
        .collect();
    for fk in &schema.foreign_keys {
        cols.push(format!("  {}", fk_clause(fk)));
    }
    format!(
        "CREATE TABLE IF NOT EXISTS {} (\n{}\n)",
        schema.table,
        cols.join(",\n")
    )
}

/// Emit the full `CREATE TABLE` batch for a schema in `dialect`: the table
/// (with inline uniques, defaults, and foreign keys) followed by one
/// `CREATE INDEX` per secondary index, `;`-separated so both backends can run
/// the batch in one call.
pub fn create_table_sql(schema: &TableSchema, dialect: Dialect) -> String {
    let mut sql = create_table_only(schema, dialect);
    sql.push(';');
    for index in &schema.indexes {
        sql.push('\n');
        sql.push_str(&create_index_sql(&schema.table, index));
        sql.push(';');
    }
    sql
}

#[cfg(test)]
mod tests {
    use super::*;

    fn todos() -> TableSchema {
        TableSchema::new("todos")
            .column(Column::new("id", ColType::Integer).primary())
            .column(Column::new("title", ColType::Text))
            .column(Column::new("done", ColType::Boolean))
    }

    #[test]
    fn emits_create_table() {
        let sql = create_table_sql(&todos(), Dialect::Sqlite);
        assert!(sql.contains("CREATE TABLE IF NOT EXISTS todos"));
        assert!(sql.contains("id INTEGER PRIMARY KEY"));
        assert!(sql.contains("title TEXT NOT NULL"));
    }

    #[test]
    fn emits_unique_default_fk_and_indexes() {
        let schema = TableSchema::new("posts")
            .column(Column::new("id", ColType::Integer).primary())
            .column(Column::new("slug", ColType::Text).unique())
            .column(Column::new("views", ColType::Integer).default(Value::Int(0)))
            .column(Column::new("note", ColType::Text).default(Value::Text("it's".into())))
            .column(Column::new("user_id", ColType::Integer))
            .foreign_key("user_id", "users", "id", FkAction::Cascade)
            .index(&["user_id"])
            .unique_index(&["slug", "user_id"]);

        let sql = create_table_sql(&schema, Dialect::Sqlite);
        assert!(sql.contains("slug TEXT NOT NULL UNIQUE"));
        assert!(sql.contains("views INTEGER NOT NULL DEFAULT 0"));
        // Quote-escaped text default.
        assert!(sql.contains("DEFAULT 'it''s'"));
        assert!(sql.contains("FOREIGN KEY (user_id) REFERENCES users (id) ON DELETE CASCADE"));
        assert!(sql.contains("CREATE INDEX IF NOT EXISTS idx_posts_user_id ON posts (user_id);"));
        assert!(sql.contains(
            "CREATE UNIQUE INDEX IF NOT EXISTS idx_posts_slug_user_id ON posts (slug, user_id);"
        ));

        // Postgres flavor: identity pk + BIGINT.
        let pg = create_table_sql(&schema, Dialect::Postgres);
        assert!(pg.contains("id BIGINT GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY"));
        assert!(pg.contains("user_id BIGINT NOT NULL"));
    }

    #[test]
    fn coltype_and_value_mappings() {
        assert_eq!(ColType::Integer.storage(Dialect::Sqlite), "INTEGER");
        assert_eq!(ColType::Integer.storage(Dialect::Postgres), "BIGINT");
        assert_eq!(ColType::Boolean.storage(Dialect::Sqlite), "BOOLEAN");
        assert_eq!(ColType::Real.name(), "real");
        assert_eq!(ColType::from_name("real", None), Some(ColType::Real));
        assert_eq!(
            ColType::from_name("vector", Some(3)),
            Some(ColType::Vector { dim: Some(3) })
        );
        assert_eq!(Value::Bool(true).to_json(), Json::Bool(true));
        assert_eq!(Value::Int(5).to_json(), Json::Num(5.0));
        assert_eq!(Value::Null.to_json(), Json::Null);
        assert_eq!(Value::Text("x".into()).to_json(), Json::str("x"));
    }

    #[test]
    fn json_and_vector_types_and_values() {
        // Both fall back to TEXT on SQLite; names surface for introspection.
        assert_eq!(ColType::Json.storage(Dialect::Sqlite), "TEXT");
        assert_eq!(ColType::Json.storage(Dialect::Postgres), "JSONB");
        assert_eq!(ColType::Json.name(), "json");
        assert_eq!(
            ColType::Vector { dim: Some(3) }.storage(Dialect::Sqlite),
            "TEXT"
        );
        assert_eq!(
            ColType::Vector { dim: Some(3) }.storage(Dialect::Postgres),
            "vector(3)"
        );
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
        let schema = TableSchema::new("docs")
            .column(Column::new("embedding", ColType::Vector { dim: Some(384) }));
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
