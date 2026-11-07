//! Expiring, purpose-scoped **signed tokens** for email links (verification,
//! password reset). Stateless: `purpose | uid | exp | bind` is hex-encoded
//! and HMAC-SHA256-signed; nothing is stored. The `bind` slot ties a token to
//! a piece of current state (e.g. a fingerprint of the password hash) so the
//! token dies when that state changes.

use sutegi_crypto::{constant_time_eq, from_hex, hex, hmac_sha256};

/// A signer/verifier over one secret. Cheap to clone.
#[derive(Clone)]
pub struct Links {
    secret: Vec<u8>,
}

impl Links {
    /// Use a long, random, configured secret (it can be the session secret,
    /// but a dedicated one is cleaner to rotate).
    ///
    /// # Panics
    ///
    /// If `secret` is shorter than [`sutegi_session::MIN_SECRET_LEN`] bytes.
    pub fn new(secret: &[u8]) -> Links {
        assert!(
            secret.len() >= sutegi_session::MIN_SECRET_LEN,
            "Links::new: secret is {} bytes, need at least {} \
             (generate one with `openssl rand -hex 32`)",
            secret.len(),
            sutegi_session::MIN_SECRET_LEN
        );
        Links {
            secret: secret.to_vec(),
        }
    }

    /// Mint a token for `purpose`/`uid` expiring at `exp` (unix seconds).
    /// `bind` is any state fingerprint the token must stay consistent with
    /// (empty for none).
    pub fn mint(&self, purpose: &str, uid: i64, exp: i64, bind: &str) -> String {
        let payload = format!("{purpose}|{uid}|{exp}|{bind}");
        let sig = hex(&hmac_sha256(&self.secret, payload.as_bytes()));
        format!("{}.{sig}", hex(payload.as_bytes()))
    }

    /// Verify a token for `purpose` as of `now`; returns `(uid, bind)` when
    /// the signature holds, the purpose matches, and it hasn't expired.
    pub fn verify(&self, purpose: &str, token: &str, now: i64) -> Option<(i64, String)> {
        let (payload_hex, sig) = token.split_once('.')?;
        let payload = from_hex(payload_hex)?;
        let expected = hex(&hmac_sha256(&self.secret, &payload));
        if !constant_time_eq(expected.as_bytes(), sig.as_bytes()) {
            return None;
        }
        let payload = String::from_utf8(payload).ok()?;
        let mut parts = payload.splitn(4, '|');
        if parts.next()? != purpose {
            return None;
        }
        let uid: i64 = parts.next()?.parse().ok()?;
        let exp: i64 = parts.next()?.parse().ok()?;
        let bind = parts.next()?.to_string();
        if exp < now {
            return None;
        }
        Some((uid, bind))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &[u8] = b"links-test-secret-0123456789abcdef";

    #[test]
    #[should_panic(expected = "need at least 32")]
    fn short_secret_is_rejected() {
        let _ = Links::new(b"secret");
    }

    #[test]
    fn roundtrip_and_scoping() {
        let links = Links::new(KEY);
        let t = links.mint("verify", 42, 1_000, "");
        assert_eq!(links.verify("verify", &t, 999), Some((42, String::new())));
        assert_eq!(links.verify("reset", &t, 999), None); // wrong purpose
        assert_eq!(links.verify("verify", &t, 1_001), None); // expired
        assert_eq!(
            Links::new(b"other-secret-0123456789abcdef01234").verify("verify", &t, 0),
            None
        ); // wrong key
    }

    #[test]
    fn bind_travels_and_tampering_fails() {
        let links = Links::new(KEY);
        let t = links.mint("reset", 7, i64::MAX, "abc123");
        assert_eq!(links.verify("reset", &t, 0), Some((7, "abc123".into())));

        let tampered = t.replacen('.', "61.", 1); // splice into payload
        assert_eq!(links.verify("reset", &tampered, 0), None);
        assert_eq!(links.verify("reset", "garbage", 0), None);
        assert_eq!(links.verify("reset", "", 0), None);
    }
}
