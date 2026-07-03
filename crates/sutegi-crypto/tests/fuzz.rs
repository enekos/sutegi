//! Adversarial + differential coverage for the hand-rolled crypto primitives.
//!
//! The unit tests in `lib.rs` are RFC known-answer vectors: they prove the
//! happy path against published constants but say nothing about edge cases
//! (multi-block inputs, key-longer-than-block, malformed decoder input, the
//! iteration loop). This file fills that gap with:
//!
//!   * extra published KAT vectors that exercise the *multi-block* code paths,
//!   * **differential** checks that validate a primitive against its own spec
//!     definition built from a simpler, already-vector-checked primitive
//!     (e.g. PBKDF2's loop re-derived from HMAC), so we don't hand-copy a
//!     fragile constant, and
//!   * a deterministic fuzz loop that asserts the codecs never panic and
//!     always round-trip.
//!
//! Deterministic (fixed-seed splitmix64) so it is reproducible in CI.

use sutegi_crypto::*;

/// splitmix64 — a tiny deterministic PRNG (no third-party dep, seedable).
fn splitmix(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

fn rand_bytes(state: &mut u64, max_len: usize) -> Vec<u8> {
    let len = (splitmix(state) as usize) % (max_len + 1);
    (0..len).map(|_| splitmix(state) as u8).collect()
}

// ---------------------------------------------------------------------------
// Multi-block known-answer vectors (the lib.rs vectors are all single-block).
// ---------------------------------------------------------------------------

#[test]
fn sha256_multi_block_vectors() {
    // NIST two-block (896-bit) vector.
    let two_block = "abcdefghbcdefghicdefghijdefghijkefghijklfghijklmghijklmn\
                     hijklmnoijklmnopjklmnopqklmnopqrlmnopqrsmnopqrstnopqrstu";
    assert_eq!(
        hex(&sha256(two_block.as_bytes())),
        "cf5b16a778af8380036ce59e7b0492370b249b11e8f07a51afac45037afee9d1"
    );
    // The classic one-million-'a' vector — many blocks, exercises length padding.
    let million_a = vec![b'a'; 1_000_000];
    assert_eq!(
        hex(&sha256(&million_a)),
        "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0"
    );
}

#[test]
fn md5_longer_vector() {
    // RFC 1321 suite: input with a trailing period (multi-block boundary).
    assert_eq!(
        hex(&md5(b"The quick brown fox jumps over the lazy dog.")),
        "e4d909c290d0fb1ca068ffaddf22cbd0"
    );
}

#[test]
fn hmac_key_longer_than_block() {
    // RFC 4231 test case 6: 131-byte key forces the "hash the key first" branch.
    let key = vec![0xaau8; 131];
    let mac = hmac_sha256(
        &key,
        b"Test Using Larger Than Block-Size Key - Hash Key First",
    );
    assert_eq!(
        hex(&mac),
        "60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54"
    );
}

// ---------------------------------------------------------------------------
// Differential checks — validate a primitive against its own definition.
// ---------------------------------------------------------------------------

#[test]
fn pbkdf2_loop_matches_its_definition() {
    // PBKDF2 with dkLen == hLen is: U1 = HMAC(P, S || INT(1)); Uk = HMAC(P, Uk-1);
    // DK = U1 xor U2 xor ... xor Uc. Re-derive that from the (RFC-vector-checked)
    // HMAC so we're testing the xor/iteration loop independently of any copied
    // multi-iteration constant.
    for &(pw, salt, iters) in &[
        (&b"passwd"[..], &b"salt"[..], 1u32),
        (b"password", b"NaCl", 2),
        (b"x", b"y", 7),
        (b"correct horse", b"battery staple", 4096),
    ] {
        let mut block = salt.to_vec();
        block.extend_from_slice(&1u32.to_be_bytes());
        let mut u = hmac_sha256(pw, &block);
        let mut expected = u;
        for _ in 1..iters {
            u = hmac_sha256(pw, &u);
            for (e, x) in expected.iter_mut().zip(u.iter()) {
                *e ^= *x;
            }
        }
        assert_eq!(
            pbkdf2_hmac_sha256(pw, salt, iters),
            expected,
            "iters={iters}"
        );
    }
}

#[test]
fn hmac_matches_its_definition() {
    // HMAC(K, m) == H((K' xor opad) || H((K' xor ipad) || m)), differentially
    // re-derived from the vector-checked sha256 for short keys.
    let mut seed = 0x5341_5745_5F31_0000_u64;
    for _ in 0..500 {
        let key = rand_bytes(&mut seed, 63); // <= block, so K' == key padded
        let msg = rand_bytes(&mut seed, 200);
        let mut k = [0u8; 64];
        k[..key.len()].copy_from_slice(&key);
        let mut ipad = [0x36u8; 64];
        let mut opad = [0x5cu8; 64];
        for i in 0..64 {
            ipad[i] ^= k[i];
            opad[i] ^= k[i];
        }
        let mut inner = ipad.to_vec();
        inner.extend_from_slice(&msg);
        let mut outer = opad.to_vec();
        outer.extend_from_slice(&sha256(&inner));
        assert_eq!(hmac_sha256(&key, &msg), sha256(&outer));
    }
}

// ---------------------------------------------------------------------------
// Codec fuzz — never panic, always round-trip.
// ---------------------------------------------------------------------------

#[test]
fn base64_roundtrips_all_lengths() {
    let mut seed = 0x0000_0B64_F000_0000_u64;
    for _ in 0..5_000 {
        let bytes = rand_bytes(&mut seed, 300);
        let encoded = base64_encode(&bytes);
        // Standard alphabet, padded to a multiple of 4.
        assert_eq!(encoded.len() % 4, 0);
        assert_eq!(
            base64_decode(&encoded).expect("own output must decode"),
            bytes
        );
    }
}

#[test]
fn base64_decode_never_panics_on_arbitrary_ascii() {
    let mut seed = 0x0000_0DEA_DB64_0000_u64;
    for _ in 0..10_000 {
        // Arbitrary printable ASCII (incl. non-alphabet chars) — must return
        // Ok/Err, never panic or overflow.
        let len = (splitmix(&mut seed) as usize) % 40;
        let s: String = (0..len)
            .map(|_| (0x20 + (splitmix(&mut seed) as u8 % 0x5f)) as char)
            .collect();
        let _ = base64_decode(&s);
    }
}

#[test]
fn hex_roundtrips_and_decode_never_panics() {
    let mut seed = 0x0000_0FF1_CE00_0000_u64;
    for _ in 0..5_000 {
        let bytes = rand_bytes(&mut seed, 200);
        assert_eq!(from_hex(&hex(&bytes)).unwrap(), bytes);
    }
    for _ in 0..10_000 {
        let len = (splitmix(&mut seed) as usize) % 40;
        let s: String = (0..len)
            .map(|_| (0x20 + (splitmix(&mut seed) as u8 % 0x5f)) as char)
            .collect();
        let _ = from_hex(&s); // must not panic on odd length / non-hex
    }
}

#[test]
fn constant_time_eq_agrees_with_plain_eq() {
    // Differential: the constant-time comparator must agree with `==` on every
    // input (it only hides *timing*, not the result).
    let mut seed = 0x0000_0000_00C0_FFEE_u64;
    for _ in 0..5_000 {
        let a = rand_bytes(&mut seed, 64);
        let mut b = a.clone();
        // Half the time, perturb one byte.
        if splitmix(&mut seed) & 1 == 0 && !b.is_empty() {
            let i = (splitmix(&mut seed) as usize) % b.len();
            b[i] = b[i].wrapping_add(1);
        }
        assert_eq!(constant_time_eq(&a, &b), a == b);
    }
}
