#![no_main]
//! libFuzzer target: the HTTP request parser must never panic and must never
//! buffer without bound (the reader is capped at `max_header_bytes`). Run:
//! `cargo +nightly fuzz run http_parse`.

use libfuzzer_sys::fuzz_target;
use std::io::BufReader;
use sutegi_http::{parse_request, Limits};

fuzz_target!(|data: &[u8]| {
    let limits = Limits::default();
    let mut r = BufReader::new(data);
    let _ = parse_request(&mut r, &limits); // Ok(None)/Ok(Some(..))/Err — never panic
});
