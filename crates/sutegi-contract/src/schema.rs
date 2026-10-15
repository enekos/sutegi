use std::collections::BTreeMap;

use sutegi_json::Json;

use crate::ty::{Field, Rules, Shape, Type, VariantShape};
use crate::Registry;

pub fn json_schema(reg: &Registry, ty: &Type) -> Json {
    let mut stack = Vec::new();
    render(reg, ty, &mut stack)
}

fn obj(pairs: Vec<(&str, Json)>) -> BTreeMap<String, Json> {
    pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect()
}

fn render(reg: &Registry, ty: &Type, stack: &mut Vec<String>) -> Json {
    match ty {
        Type::Unit => Json::obj(vec![("type", Json::str("null"))]),
        Type::Bool => Json::obj(vec![("type", Json::str("boolean"))]),
        Type::Int => Json::obj(vec![("type", Json::str("integer"))]),
        Type::Float => Json::obj(vec![("type", Json::str("number"))]),
        Type::String => Json::obj(vec![("type", Json::str("string"))]),
        Type::Any => Json::obj(vec![]),
        Type::Option(inner) => nullable(render(reg, inner, stack)),
        Type::List(inner) => Json::obj(vec![
            ("type", Json::str("array")),
            ("items", render(reg, inner, stack)),
        ]),
        Type::Map(inner) => Json::obj(vec![
            ("type", Json::str("object")),
            ("additionalProperties", render(reg, inner, stack)),
        ]),
        Type::Ref(name) => render_ref(reg, name, stack),
    }
}

fn nullable(inner: Json) -> Json {
    if let Json::Obj(mut m) = inner {
        match m.get("type") {
            Some(Json::Str(t)) if t != "null" => {
                let t = t.clone();
                m.insert(
                    "type".to_string(),
                    Json::arr(vec![Json::Str(t), Json::str("null")]),
                );
                return Json::Obj(m);
            }
            Some(_) => return Json::Obj(m),
            None if m.is_empty() => return Json::Obj(m),
            None => {
                return Json::obj(vec![(
                    "anyOf",
                    Json::arr(vec![
                        Json::Obj(m),
                        Json::obj(vec![("type", Json::str("null"))]),
                    ]),
                )])
            }
        }
    }
    inner
}

fn render_ref(reg: &Registry, name: &str, stack: &mut Vec<String>) -> Json {
    let Some(def) = reg.def(name) else {
        return Json::obj(vec![]);
    };
    if stack.iter().any(|n| n == name) {
        return Json::obj(vec![(
            "description",
            Json::str(format!("{name} (recursive)")),
        )]);
    }
    stack.push(name.to_string());
    let mut out = match &def.shape {
        Shape::Struct(fields) => object(reg, fields, None, stack),
        Shape::Alias(inner) => match render(reg, inner, stack) {
            Json::Obj(m) => m,
            _ => BTreeMap::new(),
        },
        Shape::Enum(variants) => obj(vec![
            ("type", Json::str("string")),
            (
                "enum",
                Json::arr(variants.iter().map(|v| Json::str(v.name.clone())).collect()),
            ),
        ]),
        Shape::Union { tag, variants } => {
            let arms = variants
                .iter()
                .map(|v| {
                    let tag_field = (tag.as_str(), v.name.as_str());
                    let mut arm = match &v.shape {
                        VariantShape::Unit => object(reg, &[], Some(tag_field), stack),
                        VariantShape::Struct(fields) => object(reg, fields, Some(tag_field), stack),
                        VariantShape::Newtype(inner) => {
                            let fields = reg.struct_fields(inner).unwrap_or(&[]);
                            object(reg, fields, Some(tag_field), stack)
                        }
                    };
                    if !v.doc.is_empty() {
                        arm.insert("description".to_string(), Json::str(v.doc.clone()));
                    }
                    Json::Obj(arm)
                })
                .collect();
            obj(vec![("oneOf", Json::arr(arms))])
        }
    };
    stack.pop();
    if !def.doc.is_empty() {
        out.insert("description".to_string(), Json::str(def.doc.clone()));
    }
    Json::Obj(out)
}

fn object(
    reg: &Registry,
    fields: &[Field],
    tag: Option<(&str, &str)>,
    stack: &mut Vec<String>,
) -> BTreeMap<String, Json> {
    let mut props = BTreeMap::new();
    let mut required = Vec::new();
    if let Some((key, value)) = tag {
        props.insert(
            key.to_string(),
            Json::obj(vec![
                ("type", Json::str("string")),
                ("enum", Json::arr(vec![Json::str(value)])),
            ]),
        );
        required.push(Json::str(key));
    }
    for f in fields {
        let mut schema = render(reg, &f.ty, stack);
        if let Json::Obj(m) = &mut schema {
            apply_rules(m, &f.ty, &f.rules);
            if !f.doc.is_empty() {
                m.insert("description".to_string(), Json::str(f.doc.clone()));
            }
        }
        props.insert(f.name.clone(), schema);
        if f.required {
            required.push(Json::str(f.name.clone()));
        }
    }
    obj(vec![
        ("type", Json::str("object")),
        ("properties", Json::Obj(props)),
        ("required", Json::Arr(required)),
    ])
}

fn base(ty: &Type) -> &Type {
    match ty {
        Type::Option(inner) => base(inner),
        other => other,
    }
}

fn apply_rules(m: &mut BTreeMap<String, Json>, ty: &Type, rules: &Rules) {
    let (min_key, max_key) = match base(ty) {
        Type::List(_) => ("minItems", "maxItems"),
        _ => ("minLength", "maxLength"),
    };
    if let Some(n) = rules.min_len {
        m.insert(min_key.to_string(), Json::Num(n as f64));
    }
    if let Some(n) = rules.max_len {
        m.insert(max_key.to_string(), Json::Num(n as f64));
    }
    if let Some(n) = rules.min {
        m.insert("minimum".to_string(), Json::Num(n));
    }
    if let Some(n) = rules.max {
        m.insert("maximum".to_string(), Json::Num(n));
    }
}
