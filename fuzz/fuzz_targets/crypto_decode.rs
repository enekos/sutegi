#![no_main]
//! libFuzzer target: the base64/hex decoders must never panic on arbitrary
//! input, and decoding sutegi's own encoder output must round-trip exactly.
//! Run: `cargo +nightly fuzz run crypto_decode`.

use libfuzzer_sys::fuzz_target;
use sutegi_crypto::{base64_decode, base64_encode, from_hex, hex};

fuzz_target!(|data: &[u8]| {
    // Round-trip: our own encodings must decode back to the input.
    assert_eq!(base64_decode(&base64_encode(data)).unwrap(), data);
    assert_eq!(from_hex(&hex(data)).unwrap(), data);

    // Arbitrary input as text must never panic the decoders.
    if let Ok(s) = std::str::from_utf8(data) {
        let _ = base64_decode(s);
        let _ = from_hex(s);
    }
});
