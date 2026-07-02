//! Password hashing: **PBKDF2-HMAC-SHA256** with a per-password random salt,
//! serialized as a PHC-style string:
//!
//! ```text
//! $pbkdf2-sha256$i=600000$<base64 salt>$<base64 hash>
//! ```
//!
//! PBKDF2 is the NIST-blessed KDF that sutegi can provide with **zero
//! dependencies** — the HMAC chain is [`sutegi_crypto`]'s known-answer-tested
//! implementation, the same one the Postgres driver's SCRAM auth uses. The
//! default work factor is the OWASP recommendation for PBKDF2-SHA256
//! (600 000 iterations, ~a quarter second in release builds). The iteration
//! count is stored in the string, so it can be raised later without breaking
//! existing hashes — [`needs_rehash`] tells you when to upgrade one at login.

use sutegi_crypto::{
    base64_decode, base64_encode, constant_time_eq, pbkdf2_hmac_sha256, random_bytes,
};

/// OWASP-recommended PBKDF2-HMAC-SHA256 work factor.
pub const DEFAULT_ITERATIONS: u32 = 600_000;

const SALT_LEN: usize = 16;

/// Hash a password with the default work factor. Returns a PHC-style string
/// safe to store as-is.
pub fn hash_password(password: &str) -> Result<String, String> {
    hash_password_with(password, DEFAULT_ITERATIONS)
}

/// Hash with an explicit iteration count (tuning, or fast test setups).
pub fn hash_password_with(password: &str, iterations: u32) -> Result<String, String> {
    if iterations == 0 {
        return Err("iterations must be positive".to_string());
    }
    let salt = random_bytes(SALT_LEN)?;
    let dk = pbkdf2_hmac_sha256(password.as_bytes(), &salt, iterations);
    Ok(format!(
        "$pbkdf2-sha256$i={iterations}${}${}",
        base64_encode(&salt),
        base64_encode(&dk)
    ))
}

/// Verify a password against a stored PHC string, in constant time over the
/// derived keys. Any parse failure is simply `false`.
pub fn verify_password(password: &str, stored: &str) -> bool {
    let Some((iterations, salt, expected)) = parse_phc(stored) else {
        return false;
    };
    let dk = pbkdf2_hmac_sha256(password.as_bytes(), &salt, iterations);
    constant_time_eq(&dk, &expected)
}

/// Whether a stored hash uses a weaker work factor than `iterations` (or
/// isn't a recognizable hash at all) — if so, re-hash at next successful
/// login.
pub fn needs_rehash(stored: &str, iterations: u32) -> bool {
    match parse_phc(stored) {
        Some((i, _, _)) => i < iterations,
        None => true,
    }
}

fn parse_phc(stored: &str) -> Option<(u32, Vec<u8>, Vec<u8>)> {
    let mut parts = stored.split('$');
    if !parts.next()?.is_empty() {
        return None; // must start with '$'
    }
    if parts.next()? != "pbkdf2-sha256" {
        return None;
    }
    let iterations: u32 = parts.next()?.strip_prefix("i=")?.parse().ok()?;
    let salt = base64_decode(parts.next()?).ok()?;
    let hash = base64_decode(parts.next()?).ok()?;
    if parts.next().is_some() || salt.is_empty() || hash.len() != 32 || iterations == 0 {
        return None;
    }
    Some((iterations, salt, hash))
}

#[cfg(test)]
mod tests {
    use super::*;

    // Low iteration counts: these tests exercise correctness, not cost.
    const FAST: u32 = 1_000;

    #[test]
    fn roundtrip_and_reject() {
        let phc = hash_password_with("hunter2!", FAST).unwrap();
        assert!(phc.starts_with("$pbkdf2-sha256$i=1000$"));
        assert!(verify_password("hunter2!", &phc));
        assert!(!verify_password("hunter2?", &phc));
        assert!(!verify_password("", &phc));
    }

    #[test]
    fn salts_differ_per_hash() {
        let a = hash_password_with("same", FAST).unwrap();
        let b = hash_password_with("same", FAST).unwrap();
        assert_ne!(a, b);
        assert!(verify_password("same", &a) && verify_password("same", &b));
    }

    #[test]
    fn malformed_hashes_verify_false() {
        for bad in [
            "",
            "plaintext",
            "$pbkdf2-sha256$i=0$AAAA$AAAA",
            "$pbkdf2-sha256$i=x$AAAA$AAAA",
            "$argon2id$i=3$AAAA$AAAA",
            "$pbkdf2-sha256$i=1000$notb64!$AAAA",
            "$pbkdf2-sha256$i=1000$AAAA$dG9vc2hvcnQ=", // hash not 32 bytes
        ] {
            assert!(!verify_password("pw", bad), "{bad:?} must not verify");
        }
    }

    #[test]
    fn rehash_detection() {
        let weak = hash_password_with("pw", FAST).unwrap();
        assert!(needs_rehash(&weak, DEFAULT_ITERATIONS));
        assert!(!needs_rehash(&weak, FAST));
        assert!(needs_rehash("garbage", FAST));
    }

    #[test]
    fn known_answer_pbkdf2_shape() {
        // The underlying PBKDF2 is KAT-tested in sutegi-crypto; here we pin
        // the PHC serialization format itself.
        let phc = hash_password_with("pw", 2).unwrap();
        let mut parts = phc.split('$').skip(1);
        assert_eq!(parts.next(), Some("pbkdf2-sha256"));
        assert_eq!(parts.next(), Some("i=2"));
        assert!(parts.next().is_some()); // salt
        assert_eq!(base64_decode(parts.next().unwrap()).unwrap().len(), 32);
    }
}
