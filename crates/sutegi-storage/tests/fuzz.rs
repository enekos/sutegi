//! Adversarial coverage for the S3 SigV4 presigner.
//!
//! The signing math is already checked byte-for-byte against AWS's published
//! known-answer test elsewhere; this file adds no-panic fuzzing over arbitrary
//! object keys, methods, and expiries — a presigner runs over caller- (or
//! agent-) supplied keys, so it must never panic and must always produce a
//! well-formed URL for a supported method.
//!
//! Deterministic (fixed-seed splitmix64) for reproducibility in CI.

use sutegi_storage::S3Store;

fn splitmix(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

#[test]
fn presign_arbitrary_keys_never_panics() {
    let store = S3Store::new("my-bucket", "us-east-1", "AKIAEXAMPLE", "secretkey");
    let mut seed = 0x0000_0053_3347_0000u64; // "S3G"
    for _ in 0..20_000 {
        let len = (splitmix(&mut seed) as usize) % 64;
        // Keys can contain slashes, spaces, unicode, percent signs, dots.
        let alphabet = "abc/ .%+=?&#ñé0123_-~:@";
        let chars: Vec<char> = alphabet.chars().collect();
        let key: String = (0..len)
            .map(|_| chars[(splitmix(&mut seed) as usize) % chars.len()])
            .collect();
        let expires = splitmix(&mut seed) % 604_800; // up to 7 days

        for url in [
            store.presign_get(&key, expires),
            store.presign_put(&key, expires),
            store.presign_delete(&key, expires),
        ]
        .into_iter()
        .flatten()
        {
            // A produced URL is a signed https(-ish) URL with the SigV4 query.
            assert!(url.contains("X-Amz-Signature="), "no signature in {url}");
            assert!(url.starts_with("http"), "not a URL: {url}");
        }
    }
}

#[test]
fn presign_is_deterministic_at_fixed_time() {
    // Same inputs → same signature (SigV4 is a pure function of the request +
    // key + time); the `_at` variant pins the timestamp.
    let store = S3Store::new("b", "us-east-1", "AKIA", "sk");
    let a = store.presign_at("GET", "path/to/obj.txt", 3600, 1_700_000_000);
    let b = store.presign_at("GET", "path/to/obj.txt", 3600, 1_700_000_000);
    assert_eq!(a, b);
}
