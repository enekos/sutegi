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
    let names: Vec<&str> = schema.columns.iter().map(|c| c.name).collect();
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
