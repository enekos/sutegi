//! Validation, two ways — both producing the same structured, agent-readable
//! error shape (`{ field: [messages] }`).
//!
//! * [`Ruleset`] — a fluent, `Validator`-style rule builder for request
//!   bodies: `Ruleset::new().field("email", &[Rule::Required, Rule::Email])`.
//! * [`validate_schema`] — interprets a useful subset of JSON Schema, so an AI
//!   tool's declared `input_schema` doubles as its validator with no extra code.
//!
//! Zero dependencies beyond `sutegi-json`.

use std::collections::BTreeMap;
use sutegi_json::Json;

/// Accumulated validation failures, keyed by (dotted) field path.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct ValidationErrors(BTreeMap<String, Vec<String>>);

impl ValidationErrors {
    pub fn new() -> ValidationErrors {
        ValidationErrors(BTreeMap::new())
    }

    pub fn add(&mut self, field: &str, message: impl Into<String>) {
        self.0
            .entry(field.to_string())
            .or_default()
            .push(message.into());
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Collapse into a `Result`, so callers can `?` on validation.
    pub fn into_result(self) -> Result<(), ValidationErrors> {
        if self.is_empty() {
            Ok(())
        } else {
            Err(self)
        }
    }

    /// `{ "field": ["message", ...], ... }` — ready to return to an agent.
    pub fn to_json(&self) -> Json {
        let pairs: Vec<(&str, Json)> = self
            .0
            .iter()
            .map(|(k, msgs)| {
                (
                    k.as_str(),
                    Json::arr(msgs.iter().map(|m| Json::str(m.clone())).collect()),
                )
            })
            .collect();
        Json::obj(pairs)
    }
}

/// A single validation rule.
#[derive(Clone, Debug)]
pub enum Rule {
    Required,
    Str,
    Integer,
    Number,
    Bool,
    Email,
    /// Letters only.
    Alpha,
    /// Letters and digits only.
    AlphaNum,
    /// An `http(s)://host…` URL.
    Url,
    /// Minimum numeric value.
    Min(f64),
    /// Maximum numeric value.
    Max(f64),
    /// Inclusive numeric range `[min, max]`.
    Between(f64, f64),
    /// Minimum length (string chars or array items).
    MinLen(usize),
    /// Maximum length (string chars or array items).
    MaxLen(usize),
    /// Value (as a string) must be one of these.
    In(Vec<String>),
    /// Must equal the value of another field (e.g. password confirmation).
    Same(String),
}

/// A set of `field -> rules` mappings, validated against a JSON object.
#[derive(Default)]
pub struct Ruleset {
    fields: Vec<(String, Vec<Rule>)>,
}

impl Ruleset {
    pub fn new() -> Ruleset {
        Ruleset { fields: Vec::new() }
    }

    pub fn field(mut self, name: &str, rules: &[Rule]) -> Ruleset {
        self.fields.push((name.to_string(), rules.to_vec()));
        self
    }

    /// Validate `data` (expected to be a JSON object).
    pub fn validate(&self, data: &Json) -> Result<(), ValidationErrors> {
        let mut errors = ValidationErrors::new();
        for (name, rules) in &self.fields {
            let value = data.get(name);
            let present = !matches!(value, None | Some(Json::Null));
            let required = rules.iter().any(|r| matches!(r, Rule::Required));

            if !present {
                if required {
                    errors.add(name, format!("The {} field is required.", name));
                }
                // Absent + optional → nothing else to check.
                continue;
            }

            let value = value.unwrap();
            for rule in rules {
                // `Same` needs sibling-field access, so it's handled here.
                if let Rule::Same(other) = rule {
                    if data.get(other) != Some(value) {
                        errors.add(name, format!("The {} must match {}.", name, other));
                    }
                } else {
                    check_rule(name, rule, value, &mut errors);
                }
            }
        }
        errors.into_result()
    }
}

fn check_rule(field: &str, rule: &Rule, value: &Json, errors: &mut ValidationErrors) {
    match rule {
        Rule::Required => {} // presence handled before per-rule checks
        Rule::Str => {
            if value.as_str().is_none() {
                errors.add(field, format!("The {} must be a string.", field));
            }
        }
        Rule::Integer => {
            let ok = matches!(value, Json::Num(n) if n.fract() == 0.0);
            if !ok {
                errors.add(field, format!("The {} must be an integer.", field));
            }
        }
        Rule::Number => {
            if value.as_f64().is_none() {
                errors.add(field, format!("The {} must be a number.", field));
            }
        }
        Rule::Bool => {
            if value.as_bool().is_none() {
                errors.add(field, format!("The {} must be true or false.", field));
            }
        }
        Rule::Email => {
            let ok = value.as_str().map(is_email).unwrap_or(false);
            if !ok {
                errors.add(
                    field,
                    format!("The {} must be a valid email address.", field),
                );
            }
        }
        Rule::Alpha => {
            let ok = value
                .as_str()
                .map(|s| !s.is_empty() && s.chars().all(|c| c.is_alphabetic()))
                .unwrap_or(false);
            if !ok {
                errors.add(field, format!("The {} may only contain letters.", field));
            }
        }
        Rule::AlphaNum => {
            let ok = value
                .as_str()
                .map(|s| !s.is_empty() && s.chars().all(|c| c.is_alphanumeric()))
                .unwrap_or(false);
            if !ok {
                errors.add(
                    field,
                    format!("The {} may only contain letters and numbers.", field),
                );
            }
        }
        Rule::Url => {
            let ok = value.as_str().map(is_url).unwrap_or(false);
            if !ok {
                errors.add(field, format!("The {} must be a valid URL.", field));
            }
        }
        Rule::Between(min, max) => {
            if let Some(n) = value.as_f64() {
                if n < *min || n > *max {
                    errors.add(
                        field,
                        format!("The {} must be between {} and {}.", field, min, max),
                    );
                }
            }
        }
        // Handled in `Ruleset::validate` (needs sibling-field access).
        Rule::Same(_) => {}
        Rule::Min(min) => {
            if let Some(n) = value.as_f64() {
                if n < *min {
                    errors.add(field, format!("The {} must be at least {}.", field, min));
                }
            }
        }
        Rule::Max(max) => {
            if let Some(n) = value.as_f64() {
                if n > *max {
                    errors.add(
                        field,
                        format!("The {} must not be greater than {}.", field, max),
                    );
                }
            }
        }
        Rule::MinLen(min) => {
            if let Some(len) = length_of(value) {
                if len < *min {
                    errors.add(
                        field,
                        format!("The {} must be at least {} long.", field, min),
                    );
                }
            }
        }
        Rule::MaxLen(max) => {
            if let Some(len) = length_of(value) {
                if len > *max {
                    errors.add(
                        field,
                        format!("The {} must not be longer than {}.", field, max),
                    );
                }
            }
        }
        Rule::In(allowed) => {
            let as_str = match value {
                Json::Str(s) => Some(s.clone()),
                Json::Num(_) | Json::Bool(_) => Some(value.to_string()),
                _ => None,
            };
            let ok = as_str
                .map(|s| allowed.iter().any(|a| a == &s))
                .unwrap_or(false);
            if !ok {
                errors.add(field, format!("The selected {} is invalid.", field));
            }
        }
    }
}

fn length_of(value: &Json) -> Option<usize> {
    match value {
        Json::Str(s) => Some(s.chars().count()),
        Json::Arr(a) => Some(a.len()),
        _ => None,
    }
}

/// A deliberately small, dependency-free email check: one `@`, non-empty
/// local part, and a dotted domain with no spaces.
fn is_email(s: &str) -> bool {
    let mut parts = s.split('@');
    let (local, domain) = match (parts.next(), parts.next(), parts.next()) {
        (Some(l), Some(d), None) => (l, d),
        _ => return false,
    };
    !local.is_empty()
        && domain.contains('.')
        && !domain.starts_with('.')
        && !domain.ends_with('.')
        && !s.contains(char::is_whitespace)
}

/// A minimal URL check: `http(s)://` followed by a non-empty, space-free host.
fn is_url(s: &str) -> bool {
    let rest = match s
        .strip_prefix("https://")
        .or_else(|| s.strip_prefix("http://"))
    {
        Some(r) => r,
        None => return false,
    };
    let host = rest.split(['/', '?', '#']).next().unwrap_or("");
    !host.is_empty() && !host.contains(char::is_whitespace)
}

// ---- JSON Schema subset validator -----------------------------------------

/// Validate a value against a JSON Schema subset. Supported keywords:
/// `type` (object/array/string/integer/number/boolean), `required`,
/// `properties`, `enum`, `minimum`, `maximum`, `minLength`, `maxLength`,
/// and `items`. Errors are keyed by dotted path.
pub fn validate_schema(schema: &Json, value: &Json) -> Result<(), ValidationErrors> {
    let mut errors = ValidationErrors::new();
    validate_node(schema, value, "", &mut errors);
    errors.into_result()
}

fn path_label(path: &str) -> &str {
    if path.is_empty() {
        "value"
    } else {
        path
    }
}

fn validate_node(schema: &Json, value: &Json, path: &str, errors: &mut ValidationErrors) {
    let label = path_label(path);

    if let Some(Json::Str(ty)) = schema.get("type") {
        let type_ok = match ty.as_str() {
            "object" => matches!(value, Json::Obj(_)),
            "array" => matches!(value, Json::Arr(_)),
            "string" => matches!(value, Json::Str(_)),
            "integer" => matches!(value, Json::Num(n) if n.fract() == 0.0),
            "number" => matches!(value, Json::Num(_)),
            "boolean" => matches!(value, Json::Bool(_)),
            "null" => matches!(value, Json::Null),
            _ => true,
        };
        if !type_ok {
            errors.add(label, format!("expected type '{}'", ty));
            // Type is wrong: deeper checks would just add noise.
            return;
        }
    }

    if let Some(Json::Arr(allowed)) = schema.get("enum") {
        if !allowed.iter().any(|a| a == value) {
            let opts: Vec<String> = allowed.iter().map(|a| a.to_string()).collect();
            errors.add(label, format!("must be one of: {}", opts.join(", ")));
        }
    }

    match value {
        Json::Num(n) => {
            if let Some(min) = schema.get("minimum").and_then(|j| j.as_f64()) {
                if *n < min {
                    errors.add(label, format!("must be >= {}", min));
                }
            }
            if let Some(max) = schema.get("maximum").and_then(|j| j.as_f64()) {
                if *n > max {
                    errors.add(label, format!("must be <= {}", max));
                }
            }
        }
        Json::Str(s) => {
            let len = s.chars().count() as f64;
            if let Some(min) = schema.get("minLength").and_then(|j| j.as_f64()) {
                if len < min {
                    errors.add(label, format!("must be at least {} characters", min));
                }
            }
            if let Some(max) = schema.get("maxLength").and_then(|j| j.as_f64()) {
                if len > max {
                    errors.add(label, format!("must be at most {} characters", max));
                }
            }
        }
        Json::Obj(_) => {
            // required
            if let Some(Json::Arr(required)) = schema.get("required") {
                for field in required {
                    if let Json::Str(name) = field {
                        if value.get(name).is_none() {
                            let child = join_path(path, name);
                            errors.add(&child, "is required");
                        }
                    }
                }
            }
            // properties (only validate keys that are present)
            if let Some(Json::Obj(props)) = schema.get("properties") {
                for (name, subschema) in props {
                    if let Some(child_value) = value.get(name) {
                        validate_node(subschema, child_value, &join_path(path, name), errors);
                    }
                }
            }
        }
        Json::Arr(items) => {
            if let Some(item_schema) = schema.get("items") {
                for (i, item) in items.iter().enumerate() {
                    validate_node(item_schema, item, &format!("{}[{}]", label, i), errors);
                }
            }
        }
        _ => {}
    }
}

fn join_path(parent: &str, child: &str) -> String {
    if parent.is_empty() {
        child.to_string()
    } else {
        format!("{}.{}", parent, child)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ruleset_reports_per_field_errors() {
        let rules = Ruleset::new()
            .field("email", &[Rule::Required, Rule::Email])
            .field("age", &[Rule::Integer, Rule::Min(18.0)])
            .field("name", &[Rule::Required, Rule::Str, Rule::MaxLen(3)]);

        let data = Json::obj(vec![
            ("email", Json::str("not-an-email")),
            ("age", Json::num(15)),
            ("name", Json::str("toolong")),
        ]);

        let errs = rules.validate(&data).unwrap_err();
        let j = errs.to_json();
        assert!(j.get("email").is_some());
        assert!(j.get("age").is_some());
        assert!(j.get("name").is_some());
    }

    #[test]
    fn ruleset_passes_valid_input() {
        let rules = Ruleset::new()
            .field("email", &[Rule::Required, Rule::Email])
            .field("role", &[Rule::In(vec!["admin".into(), "user".into()])]);
        let data = Json::obj(vec![
            ("email", Json::str("a@b.com")),
            ("role", Json::str("admin")),
        ]);
        assert!(rules.validate(&data).is_ok());
    }

    #[test]
    fn new_rules() {
        let rules = Ruleset::new()
            .field("slug", &[Rule::AlphaNum])
            .field("site", &[Rule::Url])
            .field("age", &[Rule::Between(0.0, 120.0)])
            .field("password", &[Rule::Required])
            .field("password_confirmation", &[Rule::Same("password".into())]);

        let bad = Json::obj(vec![
            ("slug", Json::str("no spaces!")),
            ("site", Json::str("ftp://x")),
            ("age", Json::num(200)),
            ("password", Json::str("secret")),
            ("password_confirmation", Json::str("typo")),
        ]);
        let j = rules.validate(&bad).unwrap_err().to_json();
        assert!(j.get("slug").is_some());
        assert!(j.get("site").is_some());
        assert!(j.get("age").is_some());
        assert!(j.get("password_confirmation").is_some());

        let good = Json::obj(vec![
            ("slug", Json::str("hello123")),
            ("site", Json::str("https://join.com/path")),
            ("age", Json::num(30)),
            ("password", Json::str("secret")),
            ("password_confirmation", Json::str("secret")),
        ]);
        assert!(rules.validate(&good).is_ok());
    }

    #[test]
    fn schema_validates_types_and_required() {
        let schema = Json::obj(vec![
            ("type", Json::str("object")),
            (
                "properties",
                Json::obj(vec![
                    (
                        "title",
                        Json::obj(vec![
                            ("type", Json::str("string")),
                            ("minLength", Json::num(1)),
                        ]),
                    ),
                    (
                        "count",
                        Json::obj(vec![
                            ("type", Json::str("integer")),
                            ("minimum", Json::num(0)),
                        ]),
                    ),
                ]),
            ),
            ("required", Json::arr(vec![Json::str("title")])),
        ]);

        // missing required + wrong type + below minimum
        let bad = Json::obj(vec![("count", Json::num(-1))]);
        let errs = validate_schema(&schema, &bad).unwrap_err();
        assert!(errs.to_json().get("title").is_some());
        assert!(errs.to_json().get("count").is_some());

        let good = Json::obj(vec![("title", Json::str("ok")), ("count", Json::num(3))]);
        assert!(validate_schema(&schema, &good).is_ok());
    }

    #[test]
    fn scalar_type_rules() {
        let rules = Ruleset::new()
            .field("n", &[Rule::Number])
            .field("i", &[Rule::Integer])
            .field("b", &[Rule::Bool])
            .field("s", &[Rule::Str])
            .field("a", &[Rule::Alpha]);
        // All wrong types.
        let bad = Json::obj(vec![
            ("n", Json::str("x")),
            ("i", Json::num(1.5)),
            ("b", Json::str("x")),
            ("s", Json::num(1)),
            ("a", Json::str("a1")),
        ]);
        let j = bad.clone();
        let errs = rules.validate(&j).unwrap_err().to_json();
        for f in ["n", "i", "b", "s", "a"] {
            assert!(errs.get(f).is_some(), "{f} should fail");
        }
        // All correct types.
        let good = Json::obj(vec![
            ("n", Json::num(1.5)),
            ("i", Json::num(7)),
            ("b", Json::Bool(true)),
            ("s", Json::str("hi")),
            ("a", Json::str("abc")),
        ]);
        assert!(rules.validate(&good).is_ok());
    }

    #[test]
    fn numeric_and_length_bounds() {
        let rules = Ruleset::new()
            .field("score", &[Rule::Min(0.0), Rule::Max(100.0)])
            .field("tags", &[Rule::MinLen(1), Rule::MaxLen(2)]);
        let bad = Json::obj(vec![
            ("score", Json::num(150)),
            (
                "tags",
                Json::arr(vec![Json::str("a"), Json::str("b"), Json::str("c")]),
            ),
        ]);
        let errs = rules.validate(&bad).unwrap_err().to_json();
        assert!(errs.get("score").is_some());
        assert!(errs.get("tags").is_some()); // MaxLen counts array items
                                             // In-bounds passes; MinLen also counts array length.
        let good = Json::obj(vec![
            ("score", Json::num(50)),
            ("tags", Json::arr(vec![Json::str("a")])),
        ]);
        assert!(rules.validate(&good).is_ok());
    }

    #[test]
    fn absent_optional_field_passes_but_required_fails() {
        let rules = Ruleset::new()
            .field("opt", &[Rule::Str, Rule::MaxLen(3)])
            .field("req", &[Rule::Required]);
        // `opt` absent + optional → fine; `req` absent → error.
        let errs = rules.validate(&Json::obj(vec![])).unwrap_err().to_json();
        assert!(errs.get("opt").is_none());
        assert!(errs.get("req").is_some());
        // Explicit null is treated as absent.
        let null_req = Json::obj(vec![("req", Json::Null)]);
        assert!(rules.validate(&null_req).is_err());
    }

    #[test]
    fn in_rule_accepts_numbers_and_strings() {
        let rules = Ruleset::new().field(
            "level",
            &[Rule::In(vec!["1".into(), "2".into(), "3".into()])],
        );
        // A numeric value stringifies and matches.
        assert!(rules
            .validate(&Json::obj(vec![("level", Json::num(2))]))
            .is_ok());
        assert!(rules
            .validate(&Json::obj(vec![("level", Json::num(9))]))
            .is_err());
    }

    #[test]
    fn email_and_url_edge_cases() {
        assert!(is_email("a@b.co"));
        assert!(!is_email("a@b")); // no dotted domain
        assert!(!is_email("@b.co")); // empty local
        assert!(!is_email("a b@c.co")); // whitespace
        assert!(!is_email("a@@b.co")); // two @
        assert!(is_url("https://join.com/x?y#z"));
        assert!(is_url("http://localhost"));
        assert!(!is_url("ftp://x")); // wrong scheme
        assert!(!is_url("http:// host")); // space in host
    }

    #[test]
    fn schema_enum_nested_arrays_and_string_length() {
        let schema = Json::obj(vec![
            ("type", Json::str("object")),
            (
                "properties",
                Json::obj(vec![
                    (
                        "role",
                        Json::obj(vec![(
                            "enum",
                            Json::arr(vec![Json::str("admin"), Json::str("user")]),
                        )]),
                    ),
                    (
                        "name",
                        Json::obj(vec![
                            ("type", Json::str("string")),
                            ("maxLength", Json::num(3)),
                        ]),
                    ),
                    (
                        "tags",
                        Json::obj(vec![
                            ("type", Json::str("array")),
                            ("items", Json::obj(vec![("type", Json::str("integer"))])),
                        ]),
                    ),
                ]),
            ),
        ]);
        let bad = Json::obj(vec![
            ("role", Json::str("root")),                             // not in enum
            ("name", Json::str("toolong")),                          // > maxLength
            ("tags", Json::arr(vec![Json::num(1), Json::str("x")])), // items[1] wrong type
        ]);
        let errs = validate_schema(&schema, &bad).unwrap_err().to_json();
        assert!(errs.get("role").is_some());
        assert!(errs.get("name").is_some());
        assert!(errs.get("tags[1]").is_some(), "nested item path expected");

        let good = Json::obj(vec![
            ("role", Json::str("user")),
            ("name", Json::str("ok")),
            ("tags", Json::arr(vec![Json::num(1), Json::num(2)])),
        ]);
        assert!(validate_schema(&schema, &good).is_ok());
    }

    #[test]
    fn validation_errors_helpers() {
        let mut e = ValidationErrors::new();
        assert!(e.is_empty());
        e.add("x", "bad");
        e.add("x", "also bad");
        assert!(!e.is_empty());
        // Two messages collected under the same field.
        let arr = e.to_json();
        assert_eq!(arr.get("x").and_then(Json::as_array).map(Vec::len), Some(2));
        assert!(e.into_result().is_err());
    }
}
