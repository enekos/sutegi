//! Adversarial coverage for signed-cookie tokens.
//!
//! `token`/`verify_token` are the HMAC sign/verify pair behind login cookies
//! and CSRF-ish tokens. Verification runs on fully attacker-controlled input,
//! so it must never panic, must round-trip its own output, and must reject any
//! tampering (payload or signature).
//!
//! Deterministic (fixed-seed splitmix64) for reproducibility in CI.

use sutegi_session::Sessions;

fn splitmix(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

#[test]
fn token_round_trips_and_verify_never_panics() {
    let s = Sessions::new(b"super-secret-key");
    let mut seed = 0x0000_0053_4553_0000u64; // "SES"
    for _ in 0..10_000 {
        // Round-trip: a freshly-signed token verifies back to its value.
        let len = (splitmix(&mut seed) as usize) % 64;
        let value: String = (0..len)
            .map(|_| {
                // arbitrary bytes incl. the `.` separator and non-ASCII
                let c = (splitmix(&mut seed) % 0x110) as u32;
                char::from_u32(c).unwrap_or('x')
            })
            .collect();
        let tok = s.token(&value);
        assert_eq!(s.verify_token(&tok).as_deref(), Some(value.as_str()));

        // Arbitrary garbage must never panic and never verify.
        let glen = (splitmix(&mut seed) as usize) % 80;
        let garbage: String = (0..glen)
            .map(|_| {
                let alphabet = b"0123456789abcdef.ABCDEF+/=xyz";
                alphabet[(splitmix(&mut seed) as usize) % alphabet.len()] as char
            })
            .collect();
        let _ = s.verify_token(&garbage); // Option, never panic
    }
}

#[test]
fn tampering_is_rejected() {
    let s = Sessions::new(b"key");
    let tok = s.token("user=42;role=admin");

    // Flip a character in the payload half → must fail.
    let (hex, sig) = tok.split_once('.').unwrap();
    let mut bad_hex: Vec<char> = hex.chars().collect();
    if let Some(c) = bad_hex.first_mut() {
        *c = if *c == 'a' { 'b' } else { 'a' };
    }
    let tampered = format!("{}.{}", bad_hex.into_iter().collect::<String>(), sig);
    assert_eq!(s.verify_token(&tampered), None);

    // Truncated signature → must fail.
    assert_eq!(s.verify_token(&format!("{hex}.deadbeef")), None);

    // A token minted with a different key must not verify here.
    let other = Sessions::new(b"different-key");
    assert_eq!(s.verify_token(&other.token("x")), None);
}
