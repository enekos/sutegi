//! Helpers to construct JSON Schema fragments for tool `parameters`, so
//! declaring a tool's inputs reads declaratively instead of as nested map
//! building. Used with [`App::tool`](crate::App::tool):
//!
//! ```ignore
//! app.tool("create_todo", "Create a todo",
//!     schema::object(vec![("title", schema::string("the title"))], &["title"]),
//!     |c, args| { /* … */ });
//! ```

use sutegi_json::Json;

/// A `string` property with a description.
pub fn string(description: &str) -> Json {
    Json::obj(vec![
        ("type", Json::str("string")),
        ("description", Json::str(description)),
    ])
}

/// An `integer` property with a description.
pub fn integer(description: &str) -> Json {
    Json::obj(vec![
        ("type", Json::str("integer")),
        ("description", Json::str(description)),
    ])
}

/// A `boolean` property with a description.
pub fn boolean(description: &str) -> Json {
    Json::obj(vec![
        ("type", Json::str("boolean")),
        ("description", Json::str(description)),
    ])
}

/// An object schema from `(field, schema)` pairs and a list of required fields.
pub fn object(properties: Vec<(&str, Json)>, required: &[&str]) -> Json {
    Json::obj(vec![
        ("type", Json::str("object")),
        ("properties", Json::obj(properties)),
        (
            "required",
            Json::arr(required.iter().map(|r| Json::str(*r)).collect()),
        ),
    ])
}
