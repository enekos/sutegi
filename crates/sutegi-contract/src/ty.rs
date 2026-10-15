use sutegi_json::Json;

#[derive(Clone, Debug, PartialEq)]
pub enum Type {
    Unit,
    Bool,
    Int,
    Float,
    String,
    Any,
    Option(Box<Type>),
    List(Box<Type>),
    Map(Box<Type>),
    Ref(String),
}

impl Type {
    pub fn option(inner: Type) -> Type {
        Type::Option(Box::new(inner))
    }

    pub fn list(inner: Type) -> Type {
        Type::List(Box::new(inner))
    }

    pub fn map(inner: Type) -> Type {
        Type::Map(Box::new(inner))
    }

    pub fn label(&self) -> String {
        match self {
            Type::Unit => "Unit".to_string(),
            Type::Bool => "Bool".to_string(),
            Type::Int => "Int".to_string(),
            Type::Float => "Float".to_string(),
            Type::String => "String".to_string(),
            Type::Any => "Json".to_string(),
            Type::Option(t) => format!("Option{}", t.label()),
            Type::List(t) => format!("List{}", t.label()),
            Type::Map(t) => format!("Map{}", t.label()),
            Type::Ref(name) => name.clone(),
        }
    }

    pub fn to_json(&self) -> Json {
        match self {
            Type::Unit => Json::str("unit"),
            Type::Bool => Json::str("bool"),
            Type::Int => Json::str("int"),
            Type::Float => Json::str("float"),
            Type::String => Json::str("string"),
            Type::Any => Json::str("any"),
            Type::Option(t) => Json::obj(vec![("option", t.to_json())]),
            Type::List(t) => Json::obj(vec![("list", t.to_json())]),
            Type::Map(t) => Json::obj(vec![("map", t.to_json())]),
            Type::Ref(name) => Json::obj(vec![("ref", Json::str(name.clone()))]),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rules {
    pub min_len: Option<u64>,
    pub max_len: Option<u64>,
    pub min: Option<f64>,
    pub max: Option<f64>,
}

impl Rules {
    pub const NONE: Rules = Rules {
        min_len: None,
        max_len: None,
        min: None,
        max: None,
    };

    pub fn is_empty(&self) -> bool {
        *self == Rules::NONE
    }

    pub fn to_json(&self) -> Json {
        let mut out = Vec::new();
        if let Some(n) = self.min_len {
            out.push(("min_len", Json::Num(n as f64)));
        }
        if let Some(n) = self.max_len {
            out.push(("max_len", Json::Num(n as f64)));
        }
        if let Some(n) = self.min {
            out.push(("min", Json::Num(n)));
        }
        if let Some(n) = self.max {
            out.push(("max", Json::Num(n)));
        }
        Json::obj(out)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Field {
    pub name: String,
    pub doc: String,
    pub ty: Type,
    pub required: bool,
    pub rules: Rules,
}

impl Field {
    pub fn to_json(&self) -> Json {
        let mut out = vec![
            ("name", Json::str(self.name.clone())),
            ("type", self.ty.to_json()),
            ("required", Json::Bool(self.required)),
        ];
        if !self.doc.is_empty() {
            out.push(("doc", Json::str(self.doc.clone())));
        }
        if !self.rules.is_empty() {
            out.push(("rules", self.rules.to_json()));
        }
        Json::obj(out)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum VariantShape {
    Unit,
    Struct(Vec<Field>),
    Newtype(Type),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Variant {
    pub name: String,
    pub doc: String,
    pub shape: VariantShape,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Shape {
    Struct(Vec<Field>),
    Alias(Type),
    Enum(Vec<Variant>),
    Union { tag: String, variants: Vec<Variant> },
}

#[derive(Clone, Debug, PartialEq)]
pub struct Def {
    pub name: String,
    pub rust: String,
    pub doc: String,
    pub shape: Shape,
}

impl Def {
    pub fn to_json(&self) -> Json {
        let fields = |fs: &[Field]| Json::arr(fs.iter().map(Field::to_json).collect());
        let variant = |v: &Variant| {
            let mut out = vec![("name", Json::str(v.name.clone()))];
            if !v.doc.is_empty() {
                out.push(("doc", Json::str(v.doc.clone())));
            }
            match &v.shape {
                VariantShape::Unit => {}
                VariantShape::Struct(fs) => out.push(("fields", fields(fs))),
                VariantShape::Newtype(t) => out.push(("newtype", t.to_json())),
            }
            Json::obj(out)
        };
        let mut out = vec![("name", Json::str(self.name.clone()))];
        if !self.doc.is_empty() {
            out.push(("doc", Json::str(self.doc.clone())));
        }
        match &self.shape {
            Shape::Struct(fs) => {
                out.push(("kind", Json::str("struct")));
                out.push(("fields", fields(fs)));
            }
            Shape::Alias(t) => {
                out.push(("kind", Json::str("alias")));
                out.push(("type", t.to_json()));
            }
            Shape::Enum(vs) => {
                out.push(("kind", Json::str("enum")));
                out.push(("variants", Json::arr(vs.iter().map(variant).collect())));
            }
            Shape::Union { tag, variants } => {
                out.push(("kind", Json::str("union")));
                out.push(("tag", Json::str(tag.clone())));
                out.push((
                    "variants",
                    Json::arr(variants.iter().map(variant).collect()),
                ));
            }
        }
        Json::obj(out)
    }
}
