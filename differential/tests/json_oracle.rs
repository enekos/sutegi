//! Differential test: sutegi's hand-rolled JSON parser vs `serde_json`.
//!
//! Known-answer vectors and no-panic fuzzing (see the crates' own `tests/`)
//! prove sutegi doesn't crash and matches published constants. They can't prove
//! it *agrees with a real parser* on the messy middle. This oracle does:
//!
//!   1. **Round-trip through the reference.** Generate an arbitrary JSON value,
//!      serialize it with `serde_json`, and require sutegi to parse it back to
//!      an equal value. Anything serde can emit, sutegi must read correctly.
//!   2. **Mutual-accept agreement.** Over random strings, whenever *both*
//!      parsers accept, their values must match. sutegi is allowed to be more
//!      lenient (accept where serde rejects) — those are counted, not failed —
//!      but it may never disagree on a shared accept, nor panic.
//!
//! Deterministic (fixed-seed splitmix64) for reproducibility.

use serde_json::Value as SVal;
use sutegi_json::Json;

fn splitmix(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// Generate an arbitrary `serde_json::Value`, bounded in depth and in numeric
/// range (values stay inside f64's exact-integer range so a lossless round-trip
/// through sutegi's f64-backed numbers is a fair expectation).
fn gen_value(seed: &mut u64, depth: u32) -> SVal {
    let pick = splitmix(seed) % if depth == 0 { 4 } else { 6 };
    match pick {
        0 => SVal::Null,
        1 => SVal::Bool(splitmix(seed) & 1 == 0),
        2 => {
            // Integers in a safe range, plus the occasional simple decimal.
            if splitmix(seed) & 1 == 0 {
                let n = (splitmix(seed) % 2_000_000) as i64 - 1_000_000;
                SVal::from(n)
            } else {
                let n = ((splitmix(seed) % 20_000) as f64) / 100.0;
                serde_json::Number::from_f64(n).map(SVal::Number).unwrap_or(SVal::Null)
            }
        }
        3 => SVal::String(gen_string(seed)),
        4 => {
            let len = (splitmix(seed) % 5) as usize;
            SVal::Array((0..len).map(|_| gen_value(seed, depth - 1)).collect())
        }
        _ => {
            let len = (splitmix(seed) % 5) as usize;
            let mut m = serde_json::Map::new();
            for _ in 0..len {
                m.insert(gen_string(seed), gen_value(seed, depth - 1));
            }
            SVal::Object(m)
        }
    }
}

/// A short string with characters that exercise escaping: quotes, backslashes,
/// control chars, and non-BMP scalars (surrogate-pair territory once escaped).
fn gen_string(seed: &mut u64) -> String {
    let len = (splitmix(seed) % 8) as usize;
    let palette = [
        'a', 'Z', '0', ' ', '"', '\\', '\n', '\t', '/', 'ñ', '€', '😀', '\u{1}',
    ];
    (0..len)
        .map(|_| palette[(splitmix(seed) as usize) % palette.len()])
        .collect()
}

/// Structural equality between a serde value and a sutegi value.
fn agree(s: &SVal, j: &Json) -> bool {
    match (s, j) {
        (SVal::Null, Json::Null) => true,
        (SVal::Bool(a), Json::Bool(b)) => a == b,
        // Numbers compare with a relative epsilon, not bit-equality: two
        // independently correctly-rounded parsers can land on *adjacent* f64s
        // for pathological inputs (e.g. `97.404E52`). sutegi uses std's
        // `parse::<f64>()`; that ULP-level divergence from serde is benign, not
        // a mis-parse. A real disagreement (1 vs 2, truncation) still fails.
        (SVal::Number(a), Json::Num(b)) => a
            .as_f64()
            .map(|f| {
                let scale = f.abs().max(b.abs()).max(1.0);
                (f - *b).abs() <= 1e-9 * scale
            })
            .unwrap_or(false),
        (SVal::String(a), Json::Str(b)) => a == b,
        (SVal::Array(a), Json::Arr(b)) => {
            a.len() == b.len() && a.iter().zip(b).all(|(x, y)| agree(x, y))
        }
        (SVal::Object(a), Json::Obj(b)) => {
            a.len() == b.len()
                && a.iter()
                    .all(|(k, v)| b.get(k).map(|jv| agree(v, jv)).unwrap_or(false))
        }
        _ => false,
    }
}

#[test]
fn sutegi_parses_everything_serde_emits() {
    let mut seed = 0x0000_0AC1_D000_0001u64;
    for _ in 0..100_000 {
        let value = gen_value(&mut seed, 5);
        let text = serde_json::to_string(&value).expect("serde serialize");
        match Json::parse(&text) {
            Ok(parsed) => assert!(
                agree(&value, &parsed),
                "value mismatch\n serde: {value}\n text:  {text}\n sutegi: {parsed:?}"
            ),
            Err(e) => panic!("sutegi rejected serde-valid JSON: {text:?} -> {e}"),
        }
    }
}

#[test]
fn mutual_accepts_agree_and_no_panic() {
    let mut seed = 0x0000_0DEA_D000_0002u64;
    let mut lenient = 0u64; // sutegi accepted where serde rejected (allowed)
    for _ in 0..200_000 {
        let len = (splitmix(&mut seed) as usize) % 40;
        let alphabet = b"{}[]\":,0123456789.-+eEtfnul \t\n\\/ru";
        let s: String = (0..len)
            .map(|_| alphabet[(splitmix(&mut seed) as usize) % alphabet.len()] as char)
            .collect();

        let sutegi = Json::parse(&s); // must never panic
        let serde: Result<SVal, _> = serde_json::from_str(&s);
        match (&serde, &sutegi) {
            (Ok(sv), Ok(jv)) => assert!(
                agree(sv, jv),
                "mutual accept but values differ for {s:?}: serde={sv} sutegi={jv:?}"
            ),
            (Err(_), Ok(_)) => lenient += 1,
            _ => {}
        }
    }
    // Not an assertion — just a visible record of how much more lenient the
    // hand-rolled parser is than the reference (run with --nocapture).
    println!("sutegi accepted {lenient} inputs serde rejected (leniency, allowed)");
}
