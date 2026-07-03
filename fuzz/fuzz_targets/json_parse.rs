#![no_main]
//! libFuzzer target: the JSON parser must never panic or overflow, and any
//! value it accepts must re-serialize and re-parse identically (idempotent
//! round-trip). Run: `cargo +nightly fuzz run json_parse`.

use libfuzzer_sys::fuzz_target;
use sutegi_json::Json;

fuzz_target!(|data: &[u8]| {
    if let Ok(s) = std::str::from_utf8(data) {
        if let Ok(v) = Json::parse(s) {
            let once = v.to_string();
            let reparsed = Json::parse(&once).expect("own serialization must re-parse");
            assert_eq!(once, reparsed.to_string(), "serialization not idempotent");
        }
    }
});
