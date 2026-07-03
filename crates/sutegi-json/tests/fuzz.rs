//! Adversarial coverage for the hand-rolled JSON parser.
//!
//! Every request body and every AI tool argument flows through `Json::parse`,
//! so a single parser panic or unbounded recursion is a remote crash. The
//! unit tests cover well-formed inputs and a few named errors; this file hunts
//! the *edge* cases:
//!
//!   * deeply-nested input must be rejected, never overflow the stack
//!     (a Rust stack overflow aborts the whole process),
//!   * arbitrary/garbage input must return `Ok`/`Err`, never panic, and
//!   * anything that parses must re-serialize and re-parse identically
//!     (idempotent round-trip).
//!
//! Deterministic (fixed-seed splitmix64) for reproducibility in CI.

use sutegi_json::Json;

fn splitmix(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

#[test]
fn deep_nesting_is_rejected_not_overflowed() {
    // Arrays and objects, far past any legitimate depth. Before the depth guard
    // this recursed once per byte and aborted the process with a stack overflow.
    for open in ["[", "{\"a\":"] {
        let deep = open.repeat(50_000);
        let err = Json::parse(&deep).unwrap_err();
        assert!(
            err.contains("depth") || err.contains("nesting"),
            "expected a depth error, got: {err}"
        );
    }
    // A legitimately-nested document (well under the limit) still parses.
    let ok = format!("{}1{}", "[".repeat(100), "]".repeat(100));
    assert!(Json::parse(&ok).is_ok());
}

#[test]
fn arbitrary_input_never_panics() {
    let mut seed = 0x1234_5678_u64;
    for _ in 0..50_000 {
        let len = (splitmix(&mut seed) as usize) % 64;
        // Bias the byte alphabet toward JSON-structural characters so we hit
        // real parser states, not just "unexpected character" rejections.
        let alphabet = b"{}[]\":,0123456789.-+eEtfn \t\n\\uabcdef/";
        let s: String = (0..len)
            .map(|_| {
                let b = splitmix(&mut seed) as usize;
                alphabet[b % alphabet.len()] as char
            })
            .collect();
        let _ = Json::parse(&s); // must be Ok or Err, never a panic
    }
}

#[test]
fn parse_serialize_roundtrip_is_idempotent() {
    // Anything that parses must serialize to something that re-parses to an
    // equal value, and re-serializes byte-identically (a serializer/parser
    // agreement check).
    let mut seed = 0x0000_0000_0000_C0DE_u64;
    let mut parsed = 0u32;
    while parsed < 2_000 {
        let len = (splitmix(&mut seed) as usize) % 96;
        let alphabet = b"{}[]\":,0123456789.- tfnaeul";
        let s: String = (0..len)
            .map(|_| alphabet[(splitmix(&mut seed) as usize) % alphabet.len()] as char)
            .collect();
        if let Ok(v) = Json::parse(&s) {
            let once = v.to_string();
            let reparsed = Json::parse(&once).expect("own serialization must re-parse");
            assert_eq!(v, reparsed, "value changed across round-trip: {once}");
            assert_eq!(once, reparsed.to_string(), "serialization not idempotent");
            parsed += 1;
        }
    }
}

#[test]
fn surrogate_pairs_decode_to_astral_scalars() {
    // Regression: escaped non-BMP characters must reassemble from their UTF-16
    // surrogate pair, not collapse to U+FFFD. U+1F600 😀 -> D83D DE00,
    // U+1D11E 𝄞 -> D834 DD1E.
    assert_eq!(
        Json::parse(r#""\uD83D\uDE00""#).unwrap().as_str(),
        Some("😀")
    );
    assert_eq!(
        Json::parse(r#""\uD834\uDD1E""#).unwrap().as_str(),
        Some("𝄞")
    );
    // A BMP escape is unchanged, and a lone/broken surrogate degrades to U+FFFD
    // instead of panicking or producing invalid UTF-8.
    assert_eq!(Json::parse(r#""A""#).unwrap().as_str(), Some("A"));
    assert_eq!(
        Json::parse(r#""\uD83D""#).unwrap().as_str(),
        Some("\u{FFFD}")
    );
}
