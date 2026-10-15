use std::collections::{BTreeMap, HashMap};

use sutegi_json::Json;

use crate::{Contract, Cx, Registry, Type};

const SAFE_INT: f64 = 9_007_199_254_740_992.0;

impl Contract for () {
    fn describe(_: &mut Registry) -> Type {
        Type::Unit
    }
    fn decode(value: &Json, cx: &mut Cx) -> Option<Self> {
        match value {
            Json::Null | Json::Obj(_) => Some(()),
            _ => cx.expected("null"),
        }
    }
    fn encode(&self) -> Json {
        Json::Null
    }
}

impl Contract for bool {
    fn describe(_: &mut Registry) -> Type {
        Type::Bool
    }
    fn decode(value: &Json, cx: &mut Cx) -> Option<Self> {
        match value {
            Json::Bool(b) => Some(*b),
            Json::Str(s) if cx.is_lenient() => match s.as_str() {
                "true" | "1" => Some(true),
                "false" | "0" => Some(false),
                _ => cx.expected("a boolean"),
            },
            _ => cx.expected("a boolean"),
        }
    }
    fn encode(&self) -> Json {
        Json::Bool(*self)
    }
}

impl Contract for String {
    fn describe(_: &mut Registry) -> Type {
        Type::String
    }
    fn decode(value: &Json, cx: &mut Cx) -> Option<Self> {
        match value {
            Json::Str(s) => Some(s.clone()),
            _ => cx.expected("a string"),
        }
    }
    fn encode(&self) -> Json {
        Json::Str(self.clone())
    }
}

fn number(value: &Json, cx: &mut Cx) -> Option<f64> {
    match value {
        Json::Num(n) => Some(*n),
        Json::Str(s) if cx.is_lenient() => match s.trim().parse::<f64>() {
            Ok(n) if n.is_finite() => Some(n),
            _ => cx.expected("a number"),
        },
        _ => cx.expected("a number"),
    }
}

fn integer(value: &Json, cx: &mut Cx) -> Option<i64> {
    let n = number(value, cx)?;
    if !n.is_finite() || n.fract() != 0.0 {
        return cx.expected("an integer");
    }
    if n.abs() > SAFE_INT {
        cx.error("is outside the safely representable integer range");
        return None;
    }
    Some(n as i64)
}

macro_rules! int_contract {
    ($($t:ty),*) => {$(
        impl Contract for $t {
            fn describe(_: &mut Registry) -> Type {
                Type::Int
            }
            fn decode(value: &Json, cx: &mut Cx) -> Option<Self> {
                let n = integer(value, cx)?;
                match <$t>::try_from(n) {
                    Ok(v) => Some(v),
                    Err(_) => {
                        cx.error(format!(
                            "must be between {} and {}",
                            <$t>::MIN,
                            <$t>::MAX
                        ));
                        None
                    }
                }
            }
            fn encode(&self) -> Json {
                Json::Num(*self as f64)
            }
        }
    )*};
}

int_contract!(i8, i16, i32, i64, isize, u8, u16, u32, u64, usize);

impl Contract for f64 {
    fn describe(_: &mut Registry) -> Type {
        Type::Float
    }
    fn decode(value: &Json, cx: &mut Cx) -> Option<Self> {
        number(value, cx)
    }
    fn encode(&self) -> Json {
        Json::Num(*self)
    }
}

impl Contract for f32 {
    fn describe(_: &mut Registry) -> Type {
        Type::Float
    }
    fn decode(value: &Json, cx: &mut Cx) -> Option<Self> {
        let n = number(value, cx)?;
        if n.abs() > f64::from(f32::MAX) {
            cx.error("is out of range for a 32-bit float");
            return None;
        }
        Some(n as f32)
    }
    fn encode(&self) -> Json {
        Json::Num(f64::from(*self))
    }
}

impl Contract for Json {
    fn describe(_: &mut Registry) -> Type {
        Type::Any
    }
    fn decode(value: &Json, _: &mut Cx) -> Option<Self> {
        Some(value.clone())
    }
    fn encode(&self) -> Json {
        self.clone()
    }
}

impl<T: Contract> Contract for Option<T> {
    fn describe(reg: &mut Registry) -> Type {
        Type::option(T::describe(reg))
    }
    fn decode(value: &Json, cx: &mut Cx) -> Option<Self> {
        match value {
            Json::Null => Some(None),
            v => T::decode(v, cx).map(Some),
        }
    }
    fn encode(&self) -> Json {
        match self {
            Some(v) => v.encode(),
            None => Json::Null,
        }
    }
    fn missing() -> Option<Self> {
        Some(None)
    }
}

impl<T: Contract> Contract for Box<T> {
    fn describe(reg: &mut Registry) -> Type {
        T::describe(reg)
    }
    fn decode(value: &Json, cx: &mut Cx) -> Option<Self> {
        T::decode(value, cx).map(Box::new)
    }
    fn encode(&self) -> Json {
        (**self).encode()
    }
    fn missing() -> Option<Self> {
        T::missing().map(Box::new)
    }
}

impl<T: Contract> Contract for Vec<T> {
    fn describe(reg: &mut Registry) -> Type {
        Type::list(T::describe(reg))
    }
    fn decode(value: &Json, cx: &mut Cx) -> Option<Self> {
        if let Some(parsed) = cx.reparse(value) {
            return Self::decode(&parsed, cx);
        }
        let items = cx.array(value)?;
        let mut out = Vec::with_capacity(items.len());
        let mut ok = true;
        for (i, item) in items.iter().enumerate() {
            match cx.at_index(i, |cx| T::decode(item, cx)) {
                Some(v) if ok => out.push(v),
                Some(_) => {}
                None => ok = false,
            }
        }
        ok.then_some(out)
    }
    fn encode(&self) -> Json {
        Json::Arr(self.iter().map(Contract::encode).collect())
    }
}

fn decode_entries<T: Contract>(value: &Json, cx: &mut Cx) -> Option<Vec<(String, T)>> {
    if let Some(parsed) = cx.reparse(value) {
        return decode_entries(&parsed, cx);
    }
    let obj = cx.object(value)?;
    let mut out = Vec::with_capacity(obj.len());
    let mut ok = true;
    for (k, v) in obj {
        match cx.at_key(k, |cx| T::decode(v, cx)) {
            Some(x) if ok => out.push((k.clone(), x)),
            Some(_) => {}
            None => ok = false,
        }
    }
    ok.then_some(out)
}

impl<T: Contract> Contract for BTreeMap<String, T> {
    fn describe(reg: &mut Registry) -> Type {
        Type::map(T::describe(reg))
    }
    fn decode(value: &Json, cx: &mut Cx) -> Option<Self> {
        decode_entries(value, cx).map(|v| v.into_iter().collect())
    }
    fn encode(&self) -> Json {
        Json::Obj(self.iter().map(|(k, v)| (k.clone(), v.encode())).collect())
    }
}

impl<T: Contract, S: std::hash::BuildHasher + Default> Contract for HashMap<String, T, S> {
    fn describe(reg: &mut Registry) -> Type {
        Type::map(T::describe(reg))
    }
    fn decode(value: &Json, cx: &mut Cx) -> Option<Self> {
        decode_entries(value, cx).map(|v| v.into_iter().collect())
    }
    fn encode(&self) -> Json {
        Json::Obj(self.iter().map(|(k, v)| (k.clone(), v.encode())).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{decode, decode_lenient};

    #[test]
    fn integers_refuse_fractions_and_out_of_range() {
        assert_eq!(decode::<u8>(&Json::Num(7.0)), Ok(7));
        assert!(decode::<u8>(&Json::Num(7.5)).is_err());
        assert!(decode::<u8>(&Json::Num(256.0)).is_err());
        assert!(decode::<i64>(&Json::Num(1e300)).is_err());
        assert!(decode::<i64>(&Json::Num(f64::NAN)).is_err());
        assert!(decode::<u32>(&Json::Num(-1.0)).is_err());
    }

    #[test]
    fn strict_mode_refuses_strings_for_numbers() {
        assert!(decode::<i64>(&Json::str("5")).is_err());
        assert_eq!(decode_lenient::<i64>(&Json::str("5")), Ok(5));
        assert_eq!(decode_lenient::<bool>(&Json::str("true")), Ok(true));
        assert!(decode_lenient::<f64>(&Json::str("inf")).is_err());
    }

    #[test]
    fn list_errors_carry_indices() {
        let v = Json::parse("[1, \"x\", 3, true]").unwrap();
        let errs = decode::<Vec<i32>>(&v).unwrap_err();
        assert_eq!(errs.get("value[1]").unwrap(), ["expected a number"]);
        assert_eq!(errs.get("value[3]").unwrap(), ["expected a number"]);
    }

    #[test]
    fn lenient_lists_accept_a_json_string() {
        assert_eq!(
            decode_lenient::<Vec<u8>>(&Json::str("[1,2]")),
            Ok(vec![1, 2])
        );
        assert!(decode::<Vec<u8>>(&Json::str("[1,2]")).is_err());
    }

    #[test]
    fn option_takes_null_and_maps_round_trip() {
        assert_eq!(decode::<Option<String>>(&Json::Null), Ok(None));
        let mut m = BTreeMap::new();
        m.insert("a".to_string(), 1u8);
        let j = m.encode();
        assert_eq!(decode::<BTreeMap<String, u8>>(&j), Ok(m));
    }
}
