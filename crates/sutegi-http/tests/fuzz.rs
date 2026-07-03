//! Adversarial coverage for the hand-rolled HTTP/1.1 request parser.
//!
//! `parse_request` runs on every connection against fully attacker-controlled
//! bytes, so it must (a) never panic and (b) never buffer without bound. The
//! unit tests cover well-formed requests and the two size limits; this file
//! adds garbage-input fuzzing and an explicit newline-less memory-DoS probe.
//!
//! Deterministic (fixed-seed splitmix64) for reproducibility in CI.

use std::io::BufReader;
use sutegi_http::{parse_request, Incoming, Limits};

fn splitmix(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

#[test]
fn garbage_input_never_panics() {
    let limits = Limits::default();
    let mut seed = 0x0000_8877_7076_u64;
    for _ in 0..30_000 {
        let len = (splitmix(&mut seed) as usize) % 256;
        // Bias toward HTTP-structural bytes: CR, LF, colon, space, digits, and
        // header-ish words — so we reach real parser states.
        let alphabet = b"GET POST /HTTP1.\r\n:Content-Length 0123456789abcXYZ?&=;";
        let bytes: Vec<u8> = (0..len)
            .map(|_| alphabet[(splitmix(&mut seed) as usize) % alphabet.len()])
            .collect();
        let mut r = BufReader::new(&bytes[..]);
        // Must return Ok(None)/Ok(Some(..))/Err — never panic.
        let _ = parse_request(&mut r, &limits);
    }
}

#[test]
fn newline_less_stream_is_capped_not_buffered() {
    // A request line (and any header line) that never sends `\n` must be
    // refused once it exceeds the header budget, not buffered without bound.
    let limits = Limits {
        max_header_bytes: 8 * 1024,
        ..Limits::default()
    };
    // 1 MiB of a single line with no terminator — far past the 8 KiB budget.
    let flood = vec![b'A'; 1024 * 1024];
    let mut r = BufReader::new(&flood[..]);
    match parse_request(&mut r, &limits).expect("no io error") {
        Some(Incoming::TooLarge) => {}
        other => panic!("expected TooLarge, got {:?}", other.is_some()),
    }

    // Same attack in a header line after a valid request line.
    let mut msg = b"GET / HTTP/1.1\r\n".to_vec();
    msg.extend(std::iter::repeat_n(b'B', 1024 * 1024)); // no CRLF
    let mut r = BufReader::new(&msg[..]);
    match parse_request(&mut r, &limits).expect("no io error") {
        Some(Incoming::TooLarge) => {}
        other => panic!(
            "expected TooLarge on header flood, got {:?}",
            other.is_some()
        ),
    }
}

#[test]
fn well_formed_request_still_parses_after_hardening() {
    let limits = Limits::default();
    let raw = b"POST /api/x?q=1 HTTP/1.1\r\nHost: localhost\r\nContent-Length: 5\r\n\r\nhello";
    let mut r = BufReader::new(&raw[..]);
    match parse_request(&mut r, &limits).unwrap() {
        Some(Incoming::Request(req)) => {
            assert_eq!(req.path, "/api/x");
            assert_eq!(req.query, "q=1");
            assert_eq!(req.body, b"hello");
            assert_eq!(req.header("host"), Some("localhost"));
        }
        _ => panic!("expected a parsed request"),
    }
}
