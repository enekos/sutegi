use std::collections::{BTreeMap, BTreeSet};

use sutegi_json::Json;

use crate::ty::{Def, Field, Shape, Type};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Query,
    Mutation,
    Stream,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Query => "query",
            Kind::Mutation => "mutation",
            Kind::Stream => "stream",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Procedure {
    pub name: String,
    pub kind: Kind,
    pub method: String,
    pub path: String,
    pub doc: String,
    pub input: Type,
    pub output: Type,
    pub event: Option<Type>,
    pub path_params: Vec<String>,
    pub bound_params: Vec<String>,
}

impl Procedure {
    pub fn unbound_params(&self) -> impl Iterator<Item = &String> {
        self.path_params
            .iter()
            .filter(|p| !self.bound_params.contains(p))
    }

    pub fn to_json(&self) -> Json {
        let mut out = vec![
            ("name", Json::str(self.name.clone())),
            ("kind", Json::str(self.kind.as_str())),
            ("method", Json::str(self.method.clone())),
            ("path", Json::str(self.path.clone())),
            ("doc", Json::str(self.doc.clone())),
            ("input", self.input.to_json()),
            ("output", self.output.to_json()),
            (
                "path_params",
                Json::arr(self.path_params.iter().cloned().map(Json::Str).collect()),
            ),
        ];
        if let Some(e) = &self.event {
            out.push(("event", e.to_json()));
        }
        Json::obj(out)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Message {
    pub event: String,
    pub doc: String,
    pub payload: Type,
    pub reply: Option<Type>,
}

impl Message {
    fn to_json(&self) -> Json {
        let mut out = vec![
            ("event", Json::str(self.event.clone())),
            ("doc", Json::str(self.doc.clone())),
            ("payload", self.payload.to_json()),
        ];
        if let Some(r) = &self.reply {
            out.push(("reply", r.to_json()));
        }
        Json::obj(out)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ChannelSpec {
    pub pattern: String,
    pub doc: String,
    pub join: Option<Message>,
    pub handles: Vec<Message>,
    pub emits: Vec<Message>,
}

impl ChannelSpec {
    pub fn new(pattern: &str) -> ChannelSpec {
        ChannelSpec {
            pattern: pattern.to_string(),
            doc: String::new(),
            join: None,
            handles: Vec::new(),
            emits: Vec::new(),
        }
    }

    pub fn is_typed(&self) -> bool {
        self.join.is_some() || !self.handles.is_empty() || !self.emits.is_empty()
    }

    pub fn wildcard(&self) -> bool {
        self.pattern.ends_with('*')
    }

    pub fn prefix(&self) -> &str {
        self.pattern.trim_end_matches('*')
    }

    pub fn ident(&self) -> String {
        camel_ident(self.prefix().trim_end_matches(':'))
    }

    pub fn to_json(&self) -> Json {
        let msgs = |ms: &[Message]| Json::arr(ms.iter().map(Message::to_json).collect());
        let mut out = vec![
            ("pattern", Json::str(self.pattern.clone())),
            ("doc", Json::str(self.doc.clone())),
            ("handles", msgs(&self.handles)),
            ("emits", msgs(&self.emits)),
        ];
        if let Some(j) = &self.join {
            out.push(("join", j.to_json()));
        }
        Json::obj(out)
    }
}

pub(crate) fn camel_ident(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut upper = false;
    for ch in raw.chars() {
        if ch.is_ascii_alphanumeric() {
            if upper && !out.is_empty() {
                out.push(ch.to_ascii_uppercase());
            } else {
                out.push(ch);
            }
            upper = false;
        } else {
            upper = true;
        }
    }
    if out.is_empty() || out.starts_with(|c: char| c.is_ascii_digit()) {
        out.insert(0, '_');
    }
    out
}

#[derive(Clone, Debug, Default)]
pub struct Registry {
    defs: BTreeMap<String, Def>,
    building: BTreeSet<String>,
    procedures: Vec<Procedure>,
    channels: Vec<ChannelSpec>,
}

impl Registry {
    pub fn new() -> Registry {
        Registry::default()
    }

    pub fn of<T: crate::Contract>(&mut self) -> Type {
        T::describe(self)
    }

    pub fn define(
        &mut self,
        name: &str,
        rust: &str,
        doc: &str,
        build: impl FnOnce(&mut Registry) -> Shape,
    ) -> Type {
        if let Some(existing) = self.defs.get(name) {
            assert!(
                existing.rust == rust,
                "contract type name {name:?} is used by both {} and {rust}; give one of them #[contract(rename = \"...\")]",
                existing.rust
            );
            return Type::Ref(name.to_string());
        }
        if !self.building.insert(name.to_string()) {
            return Type::Ref(name.to_string());
        }
        let shape = build(self);
        self.building.remove(name);
        self.defs.insert(
            name.to_string(),
            Def {
                name: name.to_string(),
                rust: rust.to_string(),
                doc: doc.to_string(),
                shape,
            },
        );
        Type::Ref(name.to_string())
    }

    pub fn def(&self, name: &str) -> Option<&Def> {
        self.defs.get(name)
    }

    pub fn defs(&self) -> impl Iterator<Item = &Def> {
        self.defs.values()
    }

    pub fn procedures(&self) -> &[Procedure] {
        &self.procedures
    }

    pub fn channels(&self) -> &[ChannelSpec] {
        &self.channels
    }

    pub fn field_names(&self, ty: &Type) -> Option<Vec<String>> {
        self.struct_fields(ty)
            .map(|fs| fs.iter().map(|f| f.name.clone()).collect())
    }

    pub fn struct_fields(&self, ty: &Type) -> Option<&[Field]> {
        let mut seen = BTreeSet::new();
        let mut cur = ty;
        loop {
            let Type::Ref(name) = cur else { return None };
            if !seen.insert(name.as_str()) {
                return None;
            }
            match &self.defs.get(name)?.shape {
                Shape::Struct(fields) => return Some(fields),
                Shape::Alias(inner) => cur = inner,
                _ => return None,
            }
        }
    }

    pub fn add_procedure(&mut self, procedure: Procedure) {
        assert!(
            !procedure.name.is_empty()
                && procedure
                    .name
                    .split('.')
                    .all(|s| !s.is_empty()
                        && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')),
            "procedure name {:?} must be dot-separated identifiers, e.g. \"agents.list\"",
            procedure.name
        );
        let clash = self.procedures.iter().any(|p| {
            p.name == procedure.name
                || p.name.starts_with(&format!("{}.", procedure.name))
                || procedure.name.starts_with(&format!("{}.", p.name))
        });
        assert!(
            !clash,
            "procedure name {:?} collides with an existing procedure or namespace",
            procedure.name
        );
        self.procedures.push(procedure);
    }

    pub fn add_channel(&mut self, channel: ChannelSpec) {
        let ident = channel.ident();
        assert!(
            !self.channels.iter().any(|c| c.ident() == ident),
            "channel pattern {:?} maps to client name {ident:?}, which another channel already uses",
            channel.pattern
        );
        self.channels.push(channel);
    }

    pub fn absorb(&mut self, other: Registry) {
        for (name, def) in other.defs {
            match self.defs.get(&name) {
                Some(existing) => assert!(
                    existing.rust == def.rust,
                    "contract type name {name:?} is used by both {} and {}; give one of them #[contract(rename = \"...\")]",
                    existing.rust,
                    def.rust
                ),
                None => {
                    self.defs.insert(name, def);
                }
            }
        }
        for p in other.procedures {
            self.add_procedure(p);
        }
        for c in other.channels {
            self.add_channel(c);
        }
    }

    pub fn is_empty(&self) -> bool {
        self.procedures.is_empty() && self.channels.is_empty()
    }

    pub fn manifest(&self) -> Json {
        Json::obj(vec![
            (
                "types",
                Json::Obj(
                    self.defs
                        .iter()
                        .map(|(k, d)| (k.clone(), d.to_json()))
                        .collect(),
                ),
            ),
            (
                "procedures",
                Json::arr(self.procedures.iter().map(Procedure::to_json).collect()),
            ),
            (
                "channels",
                Json::arr(self.channels.iter().map(ChannelSpec::to_json).collect()),
            ),
        ])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn camel_ident_from_patterns() {
        assert_eq!(camel_ident("table"), "table");
        assert_eq!(camel_ident("table-rows"), "tableRows");
        assert_eq!(camel_ident("ws:table"), "wsTable");
        assert_eq!(camel_ident("7up"), "_7up");
        assert_eq!(camel_ident(""), "_");
    }

    #[test]
    #[should_panic(expected = "collides")]
    fn namespace_and_leaf_cannot_share_a_name() {
        let mut r = Registry::new();
        let p = |name: &str| Procedure {
            name: name.to_string(),
            kind: Kind::Query,
            method: "GET".into(),
            path: "/".into(),
            doc: String::new(),
            input: Type::Unit,
            output: Type::Unit,
            event: None,
            path_params: vec![],
            bound_params: vec![],
        };
        r.add_procedure(p("agents"));
        r.add_procedure(p("agents.list"));
    }
}
