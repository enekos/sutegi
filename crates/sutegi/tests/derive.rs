//! Integration coverage for `#[derive(Model)]` (the `sutegi-macros` crate).
//!
//! A proc-macro can only be exercised from a crate that depends on it, so this
//! lives in the facade's test target. It pins the macro's contract: schema
//! generation, `FromRow` hydration (including SQLite's int-as-bool quirk),
//! `to_values()`/`to_json()`, `#[model(skip)]`, column renaming, and the
//! default snake-case-pluralized table name.
//!
//! Requires the `derive` (⇒ `orm`) feature; inert in a minimal build.
#![cfg(feature = "derive")]

use sutegi::prelude::*;

#[derive(Model, Default)]
#[model(table = "todos")]
struct Todo {
    #[model(primary)]
    id: i64,
    title: String,
    done: bool,
    #[model(column = "note_text")]
    note: Option<String>,
    // Not a column: omitted from schema/persistence, default-initialized on load.
    #[model(skip)]
    cached: u32,
}

// No `#[model(table=...)]` → table name defaults to snake_case + pluralize.
// Fields are exercised only through the generated impls, not read directly.
#[derive(Model, Default)]
#[allow(dead_code)]
struct Category {
    #[model(primary)]
    id: i64,
    name: String,
}

#[test]
fn schema_reflects_attributes() {
    let schema = Todo::schema();
    assert_eq!(schema.table, "todos");
    // The skipped field is not a column.
    let names: Vec<&str> = schema.columns.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, vec!["id", "title", "done", "note_text"]);
    // The primary key is detected from the attribute.
    assert_eq!(Todo::primary_key(), "id");
    // The renamed Option column is nullable; required columns are not.
    let note = schema
        .columns
        .iter()
        .find(|c| c.name == "note_text")
        .unwrap();
    assert!(note.nullable);
    let title = schema.columns.iter().find(|c| c.name == "title").unwrap();
    assert!(!title.nullable);
    // Column SQL types follow the Rust field types.
    assert_eq!(title.ty, ColType::Text);
    assert_eq!(
        schema.columns.iter().find(|c| c.name == "done").unwrap().ty,
        ColType::Boolean
    );
}

#[test]
fn default_table_name_is_snake_pluralized() {
    assert_eq!(Category::schema().table, "categories");
    assert_eq!(Category::table(), "categories");
}

// Exercises the P1 schema-IR attributes: unique, index, default, and a
// belongs_to relation projected into a foreign key with an ON DELETE action.
#[derive(Model, Default)]
#[allow(dead_code)]
struct Article {
    #[model(primary)]
    id: i64,
    #[model(unique)]
    slug: String,
    #[model(index)]
    author_id: i64,
    #[model(default = 0)]
    views: i64,
    #[model(default = "draft")]
    status: String,
    #[model(default = true)]
    public: bool,
    #[model(belongs_to(Category, foreign_key = "author_id", on_delete = "cascade"))]
    author: Option<Category>,
}

#[test]
fn schema_reflects_unique_default_index_and_fk() {
    let schema = Article::schema();

    let slug = schema.col("slug").unwrap();
    assert!(slug.unique);

    assert_eq!(schema.col("views").unwrap().default, Some(Value::Int(0)));
    assert_eq!(
        schema.col("status").unwrap().default,
        Some(Value::Text("draft".into()))
    );
    assert_eq!(
        schema.col("public").unwrap().default,
        Some(Value::Bool(true))
    );

    // #[model(index)] emits a conventional single-column index.
    let idx = schema
        .indexes
        .iter()
        .find(|i| i.columns == vec!["author_id".to_string()])
        .unwrap();
    assert!(!idx.unique);
    assert_eq!(idx.name, "idx_articles_author_id");

    // belongs_to → a foreign key on this table's column, resolving the related
    // model's table + primary key, carrying the ON DELETE action.
    let fk = &schema.foreign_keys[0];
    assert_eq!(fk.column, "author_id");
    assert_eq!(fk.ref_table, "categories");
    assert_eq!(fk.ref_column, "id");
    assert_eq!(fk.on_delete, FkAction::Cascade);
}

#[test]
fn from_row_hydrates_and_tolerates_sqlite_quirks() {
    // SQLite returns booleans as 0/1; the generated FromRow must coerce them.
    let row = Json::obj(vec![
        ("id", Json::int(7)),
        ("title", Json::str("ship sutegi")),
        ("done", Json::int(1)),
        ("note_text", Json::str("remember")),
    ]);
    let todo = Todo::from_row(&row).unwrap();
    assert_eq!(todo.id, 7);
    assert_eq!(todo.title, "ship sutegi");
    assert!(todo.done);
    assert_eq!(todo.note.as_deref(), Some("remember"));
    // The skipped field is default-initialized, never read from the row.
    assert_eq!(todo.cached, 0);

    // An absent nullable column hydrates to None.
    let sparse = Json::obj(vec![
        ("id", Json::int(1)),
        ("title", Json::str("x")),
        ("done", Json::int(0)),
        ("note_text", Json::Null),
    ]);
    assert_eq!(Todo::from_row(&sparse).unwrap().note, None);
}

#[test]
fn to_values_skips_marked_fields_and_nulls_options() {
    let todo = Todo {
        id: 1,
        title: "a".into(),
        done: true,
        note: None,
        cached: 99,
    };
    let values = todo.to_values();
    let cols: Vec<&str> = values.iter().map(|(c, _)| *c).collect();
    // `cached` is excluded; the renamed column is used.
    assert_eq!(cols, vec!["id", "title", "done", "note_text"]);
    // None Option → SQL NULL.
    let note = values.iter().find(|(c, _)| *c == "note_text").unwrap();
    assert_eq!(note.1, Value::Null);
    // bool maps to Value::Bool.
    let done = values.iter().find(|(c, _)| *c == "done").unwrap();
    assert_eq!(done.1, Value::Bool(true));
}

#[test]
fn to_json_renders_real_booleans_and_omits_skipped() {
    let todo = Todo {
        id: 1,
        title: "a".into(),
        done: false,
        note: Some("n".into()),
        cached: 5,
    };
    let j = todo.to_json();
    // Booleans serialize as real JSON booleans, not 0/1.
    assert_eq!(j.get("done"), Some(&Json::Bool(false)));
    assert_eq!(j.get("note_text").and_then(Json::as_str), Some("n"));
    // The skipped field never appears.
    assert!(j.get("cached").is_none());
}

#[test]
fn from_input_defaults_absent_columns() {
    // A partial client payload (only `title`) hydrates: `id`/`done` default,
    // while `from_row` on the same object would error on the missing `id`.
    let partial = Json::obj(vec![("title", Json::str("via agent"))]);
    assert!(Todo::from_row(&partial).is_err());
    let todo = Todo::from_input(&partial).unwrap();
    assert_eq!(todo.id, 0);
    assert_eq!(todo.title, "via agent");
    assert!(!todo.done);
    // A nullable column left absent stays None (not defaulted to a value).
    assert_eq!(todo.note, None);
}

#[cfg(feature = "sqlite")]
#[test]
fn save_inserts_and_lets_the_backend_assign_the_id() {
    let db = Db::memory().unwrap();
    Todo::migrate(&db).unwrap();
    // The `id` on the struct is ignored — the backend assigns it.
    let first = Todo {
        id: 0,
        title: "ship".into(),
        done: false,
        note: None,
        cached: 0,
    };
    assert_eq!(first.save(&db).unwrap(), 1);
    let second = Todo {
        id: 999,
        title: "again".into(),
        done: true,
        note: Some("n".into()),
        cached: 0,
    };
    assert_eq!(second.save(&db).unwrap(), 2);
    let all = Todo::all_typed(&db).unwrap();
    assert_eq!(all.len(), 2);
    assert_eq!(all[0].title, "ship");
}

#[cfg(feature = "validate")]
#[derive(Validate)]
#[allow(dead_code)]
struct Signup {
    #[validate(required, str, min_len = 3, max_len = 20)]
    username: String,
    #[validate(required, email)]
    email: String,
    #[validate(min = 18)]
    age: i64,
}

#[cfg(feature = "validate")]
#[test]
fn derive_validate_builds_a_working_ruleset() {
    let rules = Signup::rules();
    // A well-formed payload passes.
    let ok = Json::obj(vec![
        ("username", Json::str("eneko")),
        ("email", Json::str("e@example.com")),
        ("age", Json::int(30)),
    ]);
    assert!(rules.validate(&ok).is_ok());
    // Each violated field is reported.
    let bad = Json::obj(vec![
        ("username", Json::str("x")),
        ("email", Json::str("nope")),
        ("age", Json::int(12)),
    ]);
    let errs = rules.validate(&bad).unwrap_err().to_json();
    assert!(errs.get("username").is_some());
    assert!(errs.get("email").is_some());
    assert!(errs.get("age").is_some());
}
