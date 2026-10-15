use std::collections::BTreeMap;

use sutegi_contract::{decode, decode_lenient, schema, ts, type_of, Contract, Json};
use sutegi_macros::Contract;

#[derive(Contract, Debug)]
#[contract(crate = "::sutegi_contract")]
struct Tree {
    label: String,
    #[contract(default, max_len = 64)]
    children: Vec<Tree>,
    weight: Option<f32>,
}

#[derive(Contract, Debug)]
#[contract(crate = "::sutegi_contract", tag = "t")]
enum Shape {
    Circle { r: f64 },
    Many(Bag),
    Empty,
}

#[derive(Contract, Debug)]
#[contract(crate = "::sutegi_contract")]
struct Bag {
    #[contract(min_len = 1, max_len = 3)]
    name: String,
    counts: BTreeMap<String, u16>,
    small: Vec<i8>,
}

fn splitmix(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

const KEYS: &[&str] = &[
    "label", "children", "weight", "t", "r", "name", "counts", "small", "x", "",
];
const STRS: &[&str] = &[
    "circle",
    "many",
    "empty",
    "",
    "[1,2]",
    "{\"name\":\"a\"}",
    "1e400",
    "-0",
    "NaN",
    "ñ\u{1F600}",
];
const NUMS: &[f64] = &[
    0.0,
    -1.0,
    1.5,
    255.0,
    256.0,
    -129.0,
    65535.0,
    9.0e15,
    1.0e300,
    f64::MIN_POSITIVE,
];

fn random_json(state: &mut u64, depth: u32) -> Json {
    let pick = |s: &mut u64, n: usize| (splitmix(s) % n as u64) as usize;
    match pick(state, if depth == 0 { 4 } else { 6 }) {
        0 => Json::Null,
        1 => Json::Bool(splitmix(state) & 1 == 0),
        2 => Json::Num(NUMS[pick(state, NUMS.len())]),
        3 => Json::str(STRS[pick(state, STRS.len())]),
        4 => Json::Arr(
            (0..pick(state, 4))
                .map(|_| random_json(state, depth - 1))
                .collect(),
        ),
        _ => Json::Obj(
            (0..pick(state, 5))
                .map(|_| {
                    (
                        KEYS[pick(state, KEYS.len())].to_string(),
                        random_json(state, depth - 1),
                    )
                })
                .collect(),
        ),
    }
}

fn exercise(v: &Json) {
    let _ = decode::<Tree>(v);
    let _ = decode_lenient::<Tree>(v);
    let _ = decode::<Shape>(v);
    let _ = decode_lenient::<Shape>(v);
    let _ = decode::<Bag>(v);
    let _ = decode::<Vec<Option<u8>>>(v);
    let _ = decode_lenient::<BTreeMap<String, Vec<i64>>>(v);
    let _ = decode_lenient::<u64>(v);
    let _ = decode::<f32>(v);
    if let Ok(shape) = decode::<Shape>(v) {
        let again = decode::<Shape>(&shape.encode());
        assert!(again.is_ok(), "re-decode of {v} failed: {again:?}");
    }
}

#[test]
fn random_documents_never_panic_and_valid_ones_round_trip() {
    let mut state = 0x5EED_u64;
    for _ in 0..20_000 {
        let v = random_json(&mut state, 5);
        exercise(&v);
        exercise(&Json::Str(v.to_string()));
    }
}

#[test]
fn random_bytes_through_the_parser_never_panic() {
    let mut state = 0xBAD_u64;
    let alphabet = b"{}[]\":,0123456789.-+eEtrufalsn \\u\"abc";
    for _ in 0..20_000 {
        let len = (splitmix(&mut state) % 64) as usize;
        let s: String = (0..len)
            .map(|_| alphabet[(splitmix(&mut state) % alphabet.len() as u64) as usize] as char)
            .collect();
        if let Ok(v) = Json::parse(&s) {
            exercise(&v);
        }
    }
}

#[test]
fn deepest_parseable_input_decodes_without_overflow() {
    let mut src = String::new();
    for _ in 0..63 {
        src.push_str(r#"{"label":"n","children":["#);
    }
    src.push_str(r#"{"label":"leaf"}"#);
    for _ in 0..63 {
        src.push_str("]}");
    }
    let v = Json::parse(&src).expect("within the parser's depth limit");
    let tree = decode::<Tree>(&v).unwrap();
    let mut depth = 0;
    let mut cur = &tree;
    while let Some(next) = cur.children.first() {
        depth += 1;
        cur = next;
    }
    assert_eq!(depth, 63);
    let nested_string = Json::Str(src);
    let _ = decode_lenient::<Tree>(&Json::obj(vec![
        ("label", Json::str("x")),
        ("children", Json::arr(vec![nested_string])),
    ]));
}

#[test]
fn error_count_is_bounded() {
    let big = Json::Arr((0..10_000).map(|_| Json::str("x")).collect());
    let errs = decode::<Vec<u8>>(&big).unwrap_err();
    assert!(errs.iter().count() <= 64);
}

#[test]
fn schema_and_typescript_render_for_recursive_types() {
    let (reg, ty) = type_of::<Tree>();
    assert!(schema::json_schema(&reg, &ty).to_string().len() < 10_000);
    let (reg, _) = type_of::<Shape>();
    let out = ts::render(&reg);
    assert!(out.contains("| ({ t: \"many\" } & Bag)"), "{out}");
}
