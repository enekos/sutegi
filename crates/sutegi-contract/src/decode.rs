use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write;

use sutegi_json::Json;
use sutegi_validate::{Rule, ValidationErrors};

use crate::ty::Rules;
use crate::Contract;

const MAX_ERRORS: usize = 64;

#[derive(Debug)]
enum Seg {
    Key(String),
    Index(usize),
}

#[derive(Debug, Default)]
pub struct Cx {
    path: Vec<Seg>,
    errors: ValidationErrors,
    count: usize,
    lenient_all: bool,
    lenient_top: BTreeSet<String>,
}

impl Cx {
    pub fn new() -> Cx {
        Cx::default()
    }

    pub fn lenient() -> Cx {
        Cx {
            lenient_all: true,
            ..Cx::default()
        }
    }

    pub fn lenient_keys(keys: impl IntoIterator<Item = String>) -> Cx {
        Cx {
            lenient_top: keys.into_iter().collect(),
            ..Cx::default()
        }
    }

    pub fn is_lenient(&self) -> bool {
        if self.lenient_all {
            return true;
        }
        match self.path.as_slice() {
            [Seg::Key(k)] => self.lenient_top.contains(k),
            _ => false,
        }
    }

    pub fn path(&self) -> String {
        let mut out = String::new();
        for seg in &self.path {
            match seg {
                Seg::Key(k) => {
                    if !out.is_empty() {
                        out.push('.');
                    }
                    out.push_str(k);
                }
                Seg::Index(i) => {
                    if out.is_empty() {
                        out.push_str("value");
                    }
                    let _ = write!(out, "[{i}]");
                }
            }
        }
        if out.is_empty() {
            out.push_str("value");
        }
        out
    }

    pub fn error(&mut self, message: impl Into<String>) {
        self.count = self.count.saturating_add(1);
        if self.count > MAX_ERRORS {
            return;
        }
        let path = self.path();
        self.errors.add(&path, message);
    }

    pub fn expected<T>(&mut self, what: &str) -> Option<T> {
        self.error(format!("expected {what}"));
        None
    }

    pub fn has_errors(&self) -> bool {
        self.count > 0
    }

    pub fn finish<T>(self, value: Option<T>) -> Result<T, ValidationErrors> {
        match value {
            Some(v) if self.count == 0 => Ok(v),
            _ => {
                let mut errors = self.errors;
                if errors.is_empty() {
                    errors.add("value", "is invalid");
                }
                Err(errors)
            }
        }
    }

    pub fn at_key<T>(&mut self, key: &str, f: impl FnOnce(&mut Cx) -> T) -> T {
        self.path.push(Seg::Key(key.to_string()));
        let out = f(self);
        self.path.pop();
        out
    }

    pub fn at_index<T>(&mut self, index: usize, f: impl FnOnce(&mut Cx) -> T) -> T {
        self.path.push(Seg::Index(index));
        let out = f(self);
        self.path.pop();
        out
    }

    pub fn object<'v>(&mut self, value: &'v Json) -> Option<&'v BTreeMap<String, Json>> {
        match value {
            Json::Obj(m) => Some(m),
            _ => self.expected("an object"),
        }
    }

    pub fn array<'v>(&mut self, value: &'v Json) -> Option<&'v [Json]> {
        match value {
            Json::Arr(a) => Some(a),
            _ => self.expected("an array"),
        }
    }

    pub fn reparse(&self, value: &Json) -> Option<Json> {
        match value {
            Json::Str(s) if self.is_lenient() && self.path.len() <= 1 => {
                let t = s.trim_start();
                if t.starts_with('{') || t.starts_with('[') {
                    Json::parse(s).ok()
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    pub fn field<T: Contract>(
        &mut self,
        obj: &BTreeMap<String, Json>,
        key: &str,
        rules: &Rules,
    ) -> Option<T> {
        self.at_key(key, |cx| match obj.get(key) {
            None => match T::missing() {
                Some(v) => Some(v),
                None => cx.expected("a value: field is required"),
            },
            Some(v) => cx.checked_in(v, rules, Some(obj)),
        })
    }

    pub fn field_or<T: Contract>(
        &mut self,
        obj: &BTreeMap<String, Json>,
        key: &str,
        rules: &Rules,
        default: impl FnOnce() -> T,
    ) -> Option<T> {
        self.at_key(key, |cx| match obj.get(key) {
            None => Some(default()),
            Some(v) => cx.checked_in(v, rules, Some(obj)),
        })
    }

    pub fn checked<T: Contract>(&mut self, value: &Json, rules: &Rules) -> Option<T> {
        self.checked_in(value, rules, None)
    }

    fn checked_in<T: Contract>(
        &mut self,
        value: &Json,
        rules: &Rules,
        siblings: Option<&BTreeMap<String, Json>>,
    ) -> Option<T> {
        let decoded = T::decode(value, self);
        if decoded.is_some() && !rules.is_empty() {
            self.check_rules(value, rules, siblings);
        }
        decoded
    }

    fn check_rules(
        &mut self,
        value: &Json,
        rules: &Rules,
        siblings: Option<&BTreeMap<String, Json>>,
    ) {
        let coerced = match value {
            Json::Str(s) if self.is_lenient() => s.trim().parse::<f64>().ok().map(Json::Num),
            _ => None,
        };
        let numeric = coerced.as_ref().unwrap_or(value);
        let label = match self.path.last() {
            Some(Seg::Key(k)) => k.clone(),
            _ => "value".to_string(),
        };
        for rule in rules.to_validate() {
            let subject = match rule {
                Rule::Min(_) | Rule::Max(_) => numeric,
                _ => value,
            };
            if let Some(message) = sutegi_validate::check(&label, &rule, subject, siblings) {
                self.error(message);
            }
        }
    }
}

pub fn decode<T: Contract>(value: &Json) -> Result<T, ValidationErrors> {
    let mut cx = Cx::new();
    let out = T::decode(value, &mut cx);
    cx.finish(out)
}

pub fn decode_lenient<T: Contract>(value: &Json) -> Result<T, ValidationErrors> {
    let mut cx = Cx::lenient();
    let out = T::decode(value, &mut cx);
    cx.finish(out)
}

pub fn decode_with<T: Contract>(value: &Json, mut cx: Cx) -> Result<T, ValidationErrors> {
    let out = T::decode(value, &mut cx);
    cx.finish(out)
}
