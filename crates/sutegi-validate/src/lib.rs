//! Validation, two ways — both producing the same structured, agent-readable
//! error shape (`{ field: [messages] }`).
//!
//! * [`Ruleset`] — a Laravel-`Validator`-style fluent rule builder for request
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
        self.0.entry(field.to_string()).or_default().push(message.into());
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

/// A single Laravel-flavored validation rule.
#[derive(Clone, Debug)]
pub enum Rule {
    Required,
    Str,
    Integer,
    Number,
    Bool,
    Email,
    /// Minimum numeric value.
    Min(f64),
    /// Maximum numeric value.
    Max(f64),
    /// Minimum length (string chars or array items).
    MinLen(usize),
    /// Maximum length (string chars or array items).
    MaxLen(usize),
    /// Value (as a string) must be one of these.
    In(Vec<String>),
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
                check_rule(name, rule, value, &mut errors);
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
                errors.add(field, format!("The {} must be a valid email address.", field));
            }
        }
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
                    errors.add(field, format!("The {} must not be greater than {}.", field, max));
                }
            }
        }
        Rule::MinLen(min) => {
            if let Some(len) = length_of(value) {
                if len < *min {
                    errors.add(field, format!("The {} must be at least {} long.", field, min));
                }
            }
        }
        Rule::MaxLen(max) => {
            if let Some(len) = length_of(value) {
                if len > *max {
                    errors.add(field, format!("The {} must not be longer than {}.", field, max));
                }
            }
        }
        Rule::In(allowed) => {
            let as_str = match value {
                Json::Str(s) => Some(s.clone()),
                Json::Num(_) | Json::Bool(_) => Some(value.to_string()),
                _ => None,
            };
            let ok = as_str.map(|s| allowed.iter().any(|a| a == &s)).unwrap_or(false);
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
    fn schema_validates_types_and_required() {
        let schema = Json::obj(vec![
            ("type", Json::str("object")),
            (
                "properties",
                Json::obj(vec![
                    ("title", Json::obj(vec![("type", Json::str("string")), ("minLength", Json::num(1))])),
                    ("count", Json::obj(vec![("type", Json::str("integer")), ("minimum", Json::num(0))])),
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
}
