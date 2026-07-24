//! sutegi's shared pure-`std` cryptographic primitives: SHA-256 (one-shot and
//! streaming [`Sha256`]), HMAC-SHA-256, PBKDF2-HMAC-SHA-256, HKDF-SHA-256,
//! ChaCha20-Poly1305 AEAD ([`seal`]/[`open`]),
//! MD5 (legacy Postgres auth only), hex, Base64, constant-time comparison,
//! OS randomness — plus the epoch clock helpers ([`now_secs`]/[`now_millis`])
//! every row-stamping crate shares.
//!
//! Born inside the Postgres driver (SCRAM-SHA-256 needs the full HMAC/PBKDF2
//! chain) and extracted once it grew more consumers: `sutegi-pg` (SCRAM),
//! `sutegi-storage` (S3 SigV4 presigning), `sutegi-session` (signed cookies),
//! and `sutegi-auth` (password hashing, API tokens). One audited copy, **zero
//! third-party crates** — small, self-contained, and covered by published
//! known-answer test vectors at the bottom of the file.
//!
//! The AEAD is ChaCha20-Poly1305 (RFC 8439), deliberately not AES: ChaCha20 is
//! add-rotate-xor only, so a plain-software implementation is constant-time by
//! construction, while software AES needs table lookups that leak key material
//! through cache timing. Only the misuse-resistant [`seal`]/[`open`] pair and
//! the explicit-nonce [`chacha20_poly1305_seal`]/[`chacha20_poly1305_open`]
//! are exposed — the raw stream cipher stays private so unauthenticated
//! encryption can't be reached for by accident.

// ---------------------------------------------------------------------------
// SHA-256 (FIPS 180-4)
// ---------------------------------------------------------------------------

const SHA256_K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

const SHA256_INIT: [u32; 8] = [
    0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
];

fn sha256_compress(h: &mut [u32; 8], block: &[u8; 64]) {
    let mut w = [0u32; 64];
    for (i, word) in w.iter_mut().enumerate().take(16) {
        let j = i * 4;
        *word = u32::from_be_bytes([block[j], block[j + 1], block[j + 2], block[j + 3]]);
    }
    for i in 16..64 {
        let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
        let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
        w[i] = w[i - 16]
            .wrapping_add(s0)
            .wrapping_add(w[i - 7])
            .wrapping_add(s1);
    }

    let mut v = *h;
    for i in 0..64 {
        let s1 = v[4].rotate_right(6) ^ v[4].rotate_right(11) ^ v[4].rotate_right(25);
        let ch = (v[4] & v[5]) ^ ((!v[4]) & v[6]);
        let t1 = v[7]
            .wrapping_add(s1)
            .wrapping_add(ch)
            .wrapping_add(SHA256_K[i])
            .wrapping_add(w[i]);
        let s0 = v[0].rotate_right(2) ^ v[0].rotate_right(13) ^ v[0].rotate_right(22);
        let maj = (v[0] & v[1]) ^ (v[0] & v[2]) ^ (v[1] & v[2]);
        let t2 = s0.wrapping_add(maj);
        v[7] = v[6];
        v[6] = v[5];
        v[5] = v[4];
        v[4] = v[3].wrapping_add(t1);
        v[3] = v[2];
        v[2] = v[1];
        v[1] = v[0];
        v[0] = t1.wrapping_add(t2);
    }
    for (hi, vi) in h.iter_mut().zip(v.iter()) {
        *hi = hi.wrapping_add(*vi);
    }
}

/// Incremental SHA-256: [`update`](Sha256::update) in chunks of any size,
/// then [`finalize`](Sha256::finalize) — constant memory no matter how large
/// the input, unlike the one-shot [`sha256`] (which wraps this and buffers
/// nothing extra either, but takes the whole input as one slice). Use it to
/// hash streams: S3 upload bodies, file reads, wire frames.
pub struct Sha256 {
    h: [u32; 8],
    buf: [u8; 64],
    buf_len: usize,
    total_len: u64,
}

impl Sha256 {
    pub fn new() -> Self {
        Sha256 {
            h: SHA256_INIT,
            buf: [0u8; 64],
            buf_len: 0,
            total_len: 0,
        }
    }

    pub fn update(&mut self, mut data: &[u8]) {
        self.total_len = self.total_len.wrapping_add(data.len() as u64);

        // Top up a partially filled buffer first.
        if self.buf_len > 0 {
            let take = (64 - self.buf_len).min(data.len());
            self.buf[self.buf_len..self.buf_len + take].copy_from_slice(&data[..take]);
            self.buf_len += take;
            if self.buf_len < 64 {
                return; // data exhausted before the buffer filled
            }
            let block = self.buf;
            sha256_compress(&mut self.h, &block);
            self.buf_len = 0;
            data = &data[take..];
        }

        // Whole blocks straight from the input, no copy.
        let mut chunks = data.chunks_exact(64);
        for block in &mut chunks {
            sha256_compress(&mut self.h, block.try_into().unwrap());
        }

        let rest = chunks.remainder();
        self.buf[..rest.len()].copy_from_slice(rest);
        self.buf_len = rest.len();
    }

    /// Consume the hasher and return the 32-byte digest.
    pub fn finalize(mut self) -> [u8; 32] {
        // Pad: 0x80, zeros to 56 mod 64, then the 64-bit big-endian bit length.
        let bit_len = self.total_len.wrapping_mul(8);
        self.update(&[0x80]);
        while self.buf_len != 56 {
            self.update(&[0]);
        }
        self.update(&bit_len.to_be_bytes());
        debug_assert_eq!(self.buf_len, 0);

        let mut out = [0u8; 32];
        for (i, word) in self.h.iter().enumerate() {
            out[i * 4..i * 4 + 4].copy_from_slice(&word.to_be_bytes());
        }
        out
    }
}

impl Default for Sha256 {
    fn default() -> Self {
        Self::new()
    }
}

/// SHA-256 digest of `data` (32 bytes). One-shot form of [`Sha256`].
pub fn sha256(data: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hasher.finalize()
}

/// HMAC-SHA-256(key, msg) → 32 bytes (RFC 2104).
pub fn hmac_sha256(key: &[u8], msg: &[u8]) -> [u8; 32] {
    const BLOCK: usize = 64;
    let mut k = [0u8; BLOCK];
    if key.len() > BLOCK {
        k[..32].copy_from_slice(&sha256(key));
    } else {
        k[..key.len()].copy_from_slice(key);
    }
    let mut ipad = [0x36u8; BLOCK];
    let mut opad = [0x5cu8; BLOCK];
    for i in 0..BLOCK {
        ipad[i] ^= k[i];
        opad[i] ^= k[i];
    }
    let mut inner = Sha256::new();
    inner.update(&ipad);
    inner.update(msg);
    let inner_hash = inner.finalize();

    let mut outer = Sha256::new();
    outer.update(&opad);
    outer.update(&inner_hash);
    outer.finalize()
}

/// PBKDF2-HMAC-SHA-256 with a 32-byte derived key — the `Hi()` function from
/// the SCRAM spec (RFC 5802 §2.2).
pub fn pbkdf2_hmac_sha256(password: &[u8], salt: &[u8], iterations: u32) -> [u8; 32] {
    // dkLen == hLen == 32, so there is exactly one block (INT(1) = 0x00000001).
    let mut block = salt.to_vec();
    block.extend_from_slice(&1u32.to_be_bytes());
    let mut u = hmac_sha256(password, &block);
    let mut result = u;
    for _ in 1..iterations {
        u = hmac_sha256(password, &u);
        for (r, x) in result.iter_mut().zip(u.iter()) {
            *r ^= *x;
        }
    }
    result
}

// ---------------------------------------------------------------------------
// HKDF-SHA-256 (RFC 5869) — derive per-purpose subkeys from one master secret
// so signing and encryption never share key material.
// ---------------------------------------------------------------------------

/// HKDF-SHA-256 extract-then-expand: `len` bytes derived from `ikm` (the
/// master secret). `salt` may be empty (RFC 5869 treats that as 32 zero
/// bytes); `info` names the purpose and is what keeps derived keys apart —
/// e.g. `hkdf_sha256(master, b"", b"session-enc", 32)` vs `b"kv-enc"`.
///
/// `len` must be ≤ 8160 (255 × 32, the RFC ceiling); larger asks panic.
pub fn hkdf_sha256(ikm: &[u8], salt: &[u8], info: &[u8], len: usize) -> Vec<u8> {
    assert!(len <= 255 * 32, "hkdf_sha256: len exceeds 255*32 bytes");
    let prk = hmac_sha256(salt, ikm);
    let mut okm = Vec::with_capacity(len.div_ceil(32) * 32);
    let mut prev: Option<[u8; 32]> = None;
    let mut i = 1u8;
    while okm.len() < len {
        let mut data = Vec::with_capacity(32 + info.len() + 1);
        if let Some(p) = prev {
            data.extend_from_slice(&p);
        }
        data.extend_from_slice(info);
        data.push(i);
        let block = hmac_sha256(&prk, &data);
        okm.extend_from_slice(&block);
        prev = Some(block);
        i += 1;
    }
    okm.truncate(len);
    okm
}

// ---------------------------------------------------------------------------
// SHA-1 (RFC 3174) — only for the RFC 6455 WebSocket handshake, whose
// `Sec-WebSocket-Accept` is pinned to SHA-1 by the spec. Not collision-safe;
// never use it for signatures or password hashing.
// ---------------------------------------------------------------------------

/// SHA-1 digest of `data` (20 bytes). WebSocket handshake only — see above.
pub fn sha1(data: &[u8]) -> [u8; 20] {
    let mut h: [u32; 5] = [0x67452301, 0xefcdab89, 0x98badcfe, 0x10325476, 0xc3d2e1f0];

    let bit_len = (data.len() as u64).wrapping_mul(8);
    let mut msg = data.to_vec();
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_be_bytes());

    for block in msg.chunks_exact(64) {
        let mut w = [0u32; 80];
        for (i, word) in w.iter_mut().enumerate().take(16) {
            let j = i * 4;
            *word = u32::from_be_bytes([block[j], block[j + 1], block[j + 2], block[j + 3]]);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }

        let (mut a, mut b, mut c, mut d, mut e) = (h[0], h[1], h[2], h[3], h[4]);
        for (i, wi) in w.iter().enumerate() {
            let (f, k) = match i {
                0..=19 => ((b & c) | ((!b) & d), 0x5a827999u32),
                20..=39 => (b ^ c ^ d, 0x6ed9eba1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8f1bbcdc),
                _ => (b ^ c ^ d, 0xca62c1d6),
            };
            let t = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(*wi);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = t;
        }
        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
    }

    let mut out = [0u8; 20];
    for (i, word) in h.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&word.to_be_bytes());
    }
    out
}

// ---------------------------------------------------------------------------
// MD5 (RFC 1321) — only for PostgreSQL's legacy `md5` auth method.
// ---------------------------------------------------------------------------

#[rustfmt::skip]
const MD5_S: [u32; 64] = [
    7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22,
    5,  9, 14, 20, 5,  9, 14, 20, 5,  9, 14, 20, 5,  9, 14, 20,
    4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23,
    6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
];

#[rustfmt::skip]
const MD5_K: [u32; 64] = [
    0xd76aa478, 0xe8c7b756, 0x242070db, 0xc1bdceee, 0xf57c0faf, 0x4787c62a, 0xa8304613, 0xfd469501,
    0x698098d8, 0x8b44f7af, 0xffff5bb1, 0x895cd7be, 0x6b901122, 0xfd987193, 0xa679438e, 0x49b40821,
    0xf61e2562, 0xc040b340, 0x265e5a51, 0xe9b6c7aa, 0xd62f105d, 0x02441453, 0xd8a1e681, 0xe7d3fbc8,
    0x21e1cde6, 0xc33707d6, 0xf4d50d87, 0x455a14ed, 0xa9e3e905, 0xfcefa3f8, 0x676f02d9, 0x8d2a4c8a,
    0xfffa3942, 0x8771f681, 0x6d9d6122, 0xfde5380c, 0xa4beea44, 0x4bdecfa9, 0xf6bb4b60, 0xbebfbc70,
    0x289b7ec6, 0xeaa127fa, 0xd4ef3085, 0x04881d05, 0xd9d4d039, 0xe6db99e5, 0x1fa27cf8, 0xc4ac5665,
    0xf4292244, 0x432aff97, 0xab9423a7, 0xfc93a039, 0x655b59c3, 0x8f0ccc92, 0xffeff47d, 0x85845dd1,
    0x6fa87e4f, 0xfe2ce6e0, 0xa3014314, 0x4e0811a1, 0xf7537e82, 0xbd3af235, 0x2ad7d2bb, 0xeb86d391,
];

/// MD5 digest of `data` (16 bytes).
pub fn md5(data: &[u8]) -> [u8; 16] {
    let (mut a0, mut b0, mut c0, mut d0) =
        (0x67452301u32, 0xefcdab89u32, 0x98badcfeu32, 0x10325476u32);

    let bit_len = (data.len() as u64).wrapping_mul(8);
    let mut msg = data.to_vec();
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_le_bytes());

    for block in msg.chunks_exact(64) {
        let mut m = [0u32; 16];
        for (i, word) in m.iter_mut().enumerate() {
            let j = i * 4;
            *word = u32::from_le_bytes([block[j], block[j + 1], block[j + 2], block[j + 3]]);
        }
        let (mut a, mut b, mut c, mut d) = (a0, b0, c0, d0);
        for i in 0..64 {
            let (f, g) = match i {
                0..=15 => ((b & c) | ((!b) & d), i),
                16..=31 => ((d & b) | ((!d) & c), (5 * i + 1) % 16),
                32..=47 => (b ^ c ^ d, (3 * i + 5) % 16),
                _ => (c ^ (b | (!d)), (7 * i) % 16),
            };
            let f = f.wrapping_add(a).wrapping_add(MD5_K[i]).wrapping_add(m[g]);
            a = d;
            d = c;
            c = b;
            b = b.wrapping_add(f.rotate_left(MD5_S[i]));
        }
        a0 = a0.wrapping_add(a);
        b0 = b0.wrapping_add(b);
        c0 = c0.wrapping_add(c);
        d0 = d0.wrapping_add(d);
    }

    let mut out = [0u8; 16];
    out[0..4].copy_from_slice(&a0.to_le_bytes());
    out[4..8].copy_from_slice(&b0.to_le_bytes());
    out[8..12].copy_from_slice(&c0.to_le_bytes());
    out[12..16].copy_from_slice(&d0.to_le_bytes());
    out
}

/// Lowercase hex encoding.
pub fn hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(HEX[(b >> 4) as usize] as char);
        s.push(HEX[(b & 0xf) as usize] as char);
    }
    s
}

/// Lowercase hex decoding (the inverse of [`hex`]). `None` on odd length or a
/// non-hex character.
pub fn from_hex(s: &str) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok())
        .collect()
}

/// Constant-time byte-slice equality, for comparing MACs, signatures, and
/// token hashes without leaking a mismatch position through timing. Lengths
/// are compared first (length is not secret in those uses).
pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for i in 0..a.len() {
        diff |= a[i] ^ b[i];
    }
    // black_box keeps the optimizer from proving anything about `diff` and
    // rewriting the accumulation into an early-exit compare.
    std::hint::black_box(diff) == 0
}

/// `n` bytes of OS randomness from `/dev/urandom`, **secret-grade** (password
/// salts, API tokens, signing keys). Errors instead of degrading — callers
/// that can live with a weaker uniqueness-only fallback (e.g. a SCRAM nonce)
/// should implement it themselves.
pub fn random_bytes(n: usize) -> Result<Vec<u8>, String> {
    use std::io::Read;
    let mut f =
        std::fs::File::open("/dev/urandom").map_err(|e| format!("open /dev/urandom: {e}"))?;
    let mut buf = vec![0u8; n];
    f.read_exact(&mut buf)
        .map_err(|e| format!("read /dev/urandom: {e}"))?;
    Ok(buf)
}

// ---------------------------------------------------------------------------
// Clock — the one epoch-timestamp helper, shared by every crate that stamps
// rows (auth token expiry, migration history, KV/event `created_at`).
// ---------------------------------------------------------------------------

/// Seconds since the Unix epoch (0 if the system clock is before it).
pub fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Milliseconds since the Unix epoch (0 if the system clock is before it).
pub fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Base64 (standard alphabet, with padding) — for SCRAM salt/proof transport.
// ---------------------------------------------------------------------------

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Standard Base64 encode (with `=` padding).
pub fn base64_encode(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | (b[2] as u32);
        out.push(B64[((n >> 18) & 63) as usize] as char);
        out.push(B64[((n >> 12) & 63) as usize] as char);
        out.push(if chunk.len() > 1 {
            B64[((n >> 6) & 63) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            B64[(n & 63) as usize] as char
        } else {
            '='
        });
    }
    out
}

/// Standard Base64 decode. Ignores `=` padding; errors on invalid characters.
pub fn base64_decode(s: &str) -> Result<Vec<u8>, String> {
    fn val(c: u8) -> Result<u32, String> {
        match c {
            b'A'..=b'Z' => Ok((c - b'A') as u32),
            b'a'..=b'z' => Ok((c - b'a' + 26) as u32),
            b'0'..=b'9' => Ok((c - b'0' + 52) as u32),
            b'+' => Ok(62),
            b'/' => Ok(63),
            _ => Err(format!("invalid base64 char: {:?}", c as char)),
        }
    }
    let bytes: Vec<u8> = s.bytes().filter(|&c| c != b'=').collect();
    let mut out = Vec::with_capacity(bytes.len() * 3 / 4);
    for chunk in bytes.chunks(4) {
        let mut n = 0u32;
        let mut bits = 0;
        for &c in chunk {
            n = (n << 6) | val(c)?;
            bits += 6;
        }
        // Drop the low padding bits that don't form whole bytes.
        n <<= 24 - bits;
        let nbytes = bits / 8;
        for i in 0..nbytes {
            out.push((n >> (16 - i * 8)) as u8);
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// ChaCha20-Poly1305 AEAD (RFC 8439) — the crate's two-way encryption.
// ChaCha20 and Poly1305 themselves stay private: an unauthenticated stream
// cipher and a raw one-shot MAC are exactly the primitives callers misuse.
// ---------------------------------------------------------------------------

fn chacha20_quarter(s: &mut [u32; 16], a: usize, b: usize, c: usize, d: usize) {
    s[a] = s[a].wrapping_add(s[b]);
    s[d] = (s[d] ^ s[a]).rotate_left(16);
    s[c] = s[c].wrapping_add(s[d]);
    s[b] = (s[b] ^ s[c]).rotate_left(12);
    s[a] = s[a].wrapping_add(s[b]);
    s[d] = (s[d] ^ s[a]).rotate_left(8);
    s[c] = s[c].wrapping_add(s[d]);
    s[b] = (s[b] ^ s[c]).rotate_left(7);
}

/// One 64-byte ChaCha20 keystream block (RFC 8439 §2.3).
fn chacha20_block(key: &[u8; 32], counter: u32, nonce: &[u8; 12]) -> [u8; 64] {
    let mut state = [0u32; 16];
    state[0] = 0x6170_7865; // "expand 32-byte k"
    state[1] = 0x3320_646e;
    state[2] = 0x7962_2d32;
    state[3] = 0x6b20_6574;
    for i in 0..8 {
        state[4 + i] = u32::from_le_bytes(key[i * 4..i * 4 + 4].try_into().unwrap());
    }
    state[12] = counter;
    for i in 0..3 {
        state[13 + i] = u32::from_le_bytes(nonce[i * 4..i * 4 + 4].try_into().unwrap());
    }

    let mut w = state;
    for _ in 0..10 {
        chacha20_quarter(&mut w, 0, 4, 8, 12);
        chacha20_quarter(&mut w, 1, 5, 9, 13);
        chacha20_quarter(&mut w, 2, 6, 10, 14);
        chacha20_quarter(&mut w, 3, 7, 11, 15);
        chacha20_quarter(&mut w, 0, 5, 10, 15);
        chacha20_quarter(&mut w, 1, 6, 11, 12);
        chacha20_quarter(&mut w, 2, 7, 8, 13);
        chacha20_quarter(&mut w, 3, 4, 9, 14);
    }

    let mut out = [0u8; 64];
    for (i, (wi, si)) in w.iter().zip(state.iter()).enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&wi.wrapping_add(*si).to_le_bytes());
    }
    out
}

/// XOR `data` with the ChaCha20 keystream starting at `counter`.
fn chacha20_xor(key: &[u8; 32], nonce: &[u8; 12], counter: u32, data: &mut [u8]) {
    for (i, chunk) in data.chunks_mut(64).enumerate() {
        let ks = chacha20_block(key, counter.wrapping_add(i as u32), nonce);
        for (b, k) in chunk.iter_mut().zip(ks.iter()) {
            *b ^= *k;
        }
    }
}

/// Poly1305 one-shot MAC (RFC 8439 §2.5), 26-bit-limb arithmetic.
fn poly1305(key: &[u8; 32], msg: &[u8]) -> [u8; 16] {
    fn le32(b: &[u8]) -> u32 {
        u32::from_le_bytes([b[0], b[1], b[2], b[3]])
    }

    // r, clamped per spec, spread over five 26-bit limbs (masks fold the
    // clamp 0x0ffffffc0ffffffc0ffffffc0fffffff into the limb split).
    let r0 = le32(&key[0..4]) & 0x03ff_ffff;
    let r1 = (le32(&key[3..7]) >> 2) & 0x03ff_ff03;
    let r2 = (le32(&key[6..10]) >> 4) & 0x03ff_c0ff;
    let r3 = (le32(&key[9..13]) >> 6) & 0x03f0_3fff;
    let r4 = (le32(&key[12..16]) >> 8) & 0x000f_ffff;
    let (s1, s2, s3, s4) = (r1 * 5, r2 * 5, r3 * 5, r4 * 5);

    let (mut h0, mut h1, mut h2, mut h3, mut h4) = (0u32, 0u32, 0u32, 0u32, 0u32);

    for chunk in msg.chunks(16) {
        // Full blocks get the 2^128 marker bit; the final partial block is
        // instead terminated with a 0x01 byte and zero-padded.
        let (m, hibit) = if chunk.len() == 16 {
            let mut buf = [0u8; 16];
            buf.copy_from_slice(chunk);
            (buf, 1u32 << 24)
        } else {
            let mut buf = [0u8; 16];
            buf[..chunk.len()].copy_from_slice(chunk);
            buf[chunk.len()] = 1;
            (buf, 0)
        };

        h0 += le32(&m[0..4]) & 0x03ff_ffff;
        h1 += (le32(&m[3..7]) >> 2) & 0x03ff_ffff;
        h2 += (le32(&m[6..10]) >> 4) & 0x03ff_ffff;
        h3 += (le32(&m[9..13]) >> 6) & 0x03ff_ffff;
        h4 += (le32(&m[12..16]) >> 8) | hibit;

        // h = (h + m) * r mod 2^130-5, schoolbook with the 5x wraparound.
        let (u0, u1, u2, u3, u4) = (h0 as u64, h1 as u64, h2 as u64, h3 as u64, h4 as u64);
        let (v0, v1, v2, v3, v4) = (r0 as u64, r1 as u64, r2 as u64, r3 as u64, r4 as u64);
        let (w1, w2, w3, w4) = (s1 as u64, s2 as u64, s3 as u64, s4 as u64);

        let d0 = u0 * v0 + u1 * w4 + u2 * w3 + u3 * w2 + u4 * w1;
        let mut d1 = u0 * v1 + u1 * v0 + u2 * w4 + u3 * w3 + u4 * w2;
        let mut d2 = u0 * v2 + u1 * v1 + u2 * v0 + u3 * w4 + u4 * w3;
        let mut d3 = u0 * v3 + u1 * v2 + u2 * v1 + u3 * v0 + u4 * w4;
        let mut d4 = u0 * v4 + u1 * v3 + u2 * v2 + u3 * v1 + u4 * v0;

        let mut c = d0 >> 26;
        h0 = (d0 & 0x03ff_ffff) as u32;
        d1 += c;
        c = d1 >> 26;
        h1 = (d1 & 0x03ff_ffff) as u32;
        d2 += c;
        c = d2 >> 26;
        h2 = (d2 & 0x03ff_ffff) as u32;
        d3 += c;
        c = d3 >> 26;
        h3 = (d3 & 0x03ff_ffff) as u32;
        d4 += c;
        c = d4 >> 26;
        h4 = (d4 & 0x03ff_ffff) as u32;
        let t = h0 as u64 + c * 5;
        h0 = (t & 0x03ff_ffff) as u32;
        h1 += (t >> 26) as u32;
    }

    // Final carry chain, then reduce fully mod 2^130-5.
    let mut c = h1 >> 26;
    h1 &= 0x03ff_ffff;
    h2 += c;
    c = h2 >> 26;
    h2 &= 0x03ff_ffff;
    h3 += c;
    c = h3 >> 26;
    h3 &= 0x03ff_ffff;
    h4 += c;
    c = h4 >> 26;
    h4 &= 0x03ff_ffff;
    h0 += c * 5;
    c = h0 >> 26;
    h0 &= 0x03ff_ffff;
    h1 += c;

    // g = h + 5 - 2^130; pick g when it didn't underflow (h >= p), else h.
    let mut g0 = h0.wrapping_add(5);
    c = g0 >> 26;
    g0 &= 0x03ff_ffff;
    let mut g1 = h1.wrapping_add(c);
    c = g1 >> 26;
    g1 &= 0x03ff_ffff;
    let mut g2 = h2.wrapping_add(c);
    c = g2 >> 26;
    g2 &= 0x03ff_ffff;
    let mut g3 = h3.wrapping_add(c);
    c = g3 >> 26;
    g3 &= 0x03ff_ffff;
    let g4 = h4.wrapping_add(c).wrapping_sub(1 << 26);

    let mask = (g4 >> 31).wrapping_sub(1); // all-ones iff no underflow
    h0 = (h0 & !mask) | (g0 & mask);
    h1 = (h1 & !mask) | (g1 & mask);
    h2 = (h2 & !mask) | (g2 & mask);
    h3 = (h3 & !mask) | (g3 & mask);
    h4 = (h4 & !mask) | (g4 & mask);

    // tag = (h + s) mod 2^128, s being the second key half.
    let hh0 = h0 | (h1 << 26);
    let hh1 = (h1 >> 6) | (h2 << 20);
    let hh2 = (h2 >> 12) | (h3 << 14);
    let hh3 = (h3 >> 18) | (h4 << 8);

    let mut f = hh0 as u64 + le32(&key[16..20]) as u64;
    let t0 = f as u32;
    f = hh1 as u64 + le32(&key[20..24]) as u64 + (f >> 32);
    let t1 = f as u32;
    f = hh2 as u64 + le32(&key[24..28]) as u64 + (f >> 32);
    let t2 = f as u32;
    f = hh3 as u64 + le32(&key[28..32]) as u64 + (f >> 32);
    let t3 = f as u32;

    let mut out = [0u8; 16];
    out[0..4].copy_from_slice(&t0.to_le_bytes());
    out[4..8].copy_from_slice(&t1.to_le_bytes());
    out[8..12].copy_from_slice(&t2.to_le_bytes());
    out[12..16].copy_from_slice(&t3.to_le_bytes());
    out
}

/// RFC 8439 §2.8 MAC input: aad ‖ pad16 ‖ ciphertext ‖ pad16 ‖ le64 lengths.
fn aead_tag(poly_key: &[u8; 32], aad: &[u8], ct: &[u8]) -> [u8; 16] {
    fn pad16(n: usize) -> usize {
        (16 - n % 16) % 16
    }
    let mut mac_data = Vec::with_capacity(aad.len() + ct.len() + 48);
    mac_data.extend_from_slice(aad);
    mac_data.resize(mac_data.len() + pad16(aad.len()), 0);
    mac_data.extend_from_slice(ct);
    mac_data.resize(mac_data.len() + pad16(ct.len()), 0);
    mac_data.extend_from_slice(&(aad.len() as u64).to_le_bytes());
    mac_data.extend_from_slice(&(ct.len() as u64).to_le_bytes());
    poly1305(poly_key, &mac_data)
}

/// ChaCha20-Poly1305 encrypt-and-authenticate (RFC 8439 §2.8): returns
/// `ciphertext ‖ 16-byte tag`. `aad` is authenticated but not encrypted.
///
/// A `(key, nonce)` pair must **never** encrypt two different plaintexts —
/// reuse forfeits both confidentiality and authenticity. Unless you have a
/// counter or other unique-nonce scheme, use [`seal`], which draws the nonce
/// from OS randomness for you.
pub fn chacha20_poly1305_seal(
    key: &[u8; 32],
    nonce: &[u8; 12],
    aad: &[u8],
    plaintext: &[u8],
) -> Vec<u8> {
    let poly_key: [u8; 32] = chacha20_block(key, 0, nonce)[..32].try_into().unwrap();
    let mut out = plaintext.to_vec();
    chacha20_xor(key, nonce, 1, &mut out);
    let tag = aead_tag(&poly_key, aad, &out);
    out.extend_from_slice(&tag);
    out
}

/// ChaCha20-Poly1305 verify-then-decrypt: the inverse of
/// [`chacha20_poly1305_seal`]. `None` on any tampering — wrong key, nonce,
/// aad, truncation, or a flipped bit anywhere. The tag is checked (in
/// constant time) **before** anything is decrypted.
pub fn chacha20_poly1305_open(
    key: &[u8; 32],
    nonce: &[u8; 12],
    aad: &[u8],
    sealed: &[u8],
) -> Option<Vec<u8>> {
    if sealed.len() < 16 {
        return None;
    }
    let (ct, tag) = sealed.split_at(sealed.len() - 16);
    let poly_key: [u8; 32] = chacha20_block(key, 0, nonce)[..32].try_into().unwrap();
    if !constant_time_eq(&aead_tag(&poly_key, aad, ct), tag) {
        return None;
    }
    let mut pt = ct.to_vec();
    chacha20_xor(key, nonce, 1, &mut pt);
    Some(pt)
}

/// Encrypt `plaintext` under `key` with a fresh random nonce: returns
/// `nonce(12) ‖ ciphertext ‖ tag(16)`, +28 bytes over the plaintext. This is
/// the API consumers should reach for — nonce reuse, the one catastrophic
/// AEAD failure mode, is impossible by construction. Errors only if OS
/// randomness is unavailable.
///
/// Derive `key` from a master secret with [`hkdf_sha256`], e.g.
/// `hkdf_sha256(master, b"", b"kv-enc", 32).try_into().unwrap()` — never
/// reuse a signing key as an encryption key.
pub fn seal(key: &[u8; 32], plaintext: &[u8]) -> Result<Vec<u8>, String> {
    let nonce: [u8; 12] = random_bytes(12)?.try_into().expect("12 bytes requested");
    let mut out = Vec::with_capacity(12 + plaintext.len() + 16);
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&chacha20_poly1305_seal(key, &nonce, &[], plaintext));
    Ok(out)
}

/// Decrypt-and-verify the inverse of [`seal`]. `None` on any tampering.
pub fn open(key: &[u8; 32], sealed: &[u8]) -> Option<Vec<u8>> {
    if sealed.len() < 12 + 16 {
        return None;
    }
    let nonce: [u8; 12] = sealed[..12].try_into().unwrap();
    chacha20_poly1305_open(key, &nonce, &[], &sealed[12..])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_known_vectors() {
        // NIST / RFC test vectors.
        assert_eq!(
            hex(&sha256(b"")),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            hex(&sha256(b"abc")),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            hex(&sha256(
                b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"
            )),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
    }

    #[test]
    fn sha256_streaming_matches_one_shot() {
        // Same NIST vector, fed in awkward chunk sizes — including splits that
        // straddle the 64-byte block boundary and empty updates.
        let msg = vec![b'x'; 200];
        let expected = sha256(&msg);
        for chunk in [1, 7, 63, 64, 65, 200] {
            let mut h = Sha256::new();
            h.update(&[]);
            for part in msg.chunks(chunk) {
                h.update(part);
            }
            assert_eq!(h.finalize(), expected, "chunk={chunk}");
        }
        assert_eq!(
            hex(&Sha256::new().finalize()),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn hmac_sha256_rfc4231() {
        // RFC 4231 test case 2.
        let mac = hmac_sha256(b"Jefe", b"what do ya want for nothing?");
        assert_eq!(
            hex(&mac),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
    }

    #[test]
    fn pbkdf2_sha256_rfc7914() {
        // RFC 7914 §11: PBKDF2-HMAC-SHA-256 ("passwd", "salt", 1) first 32 bytes.
        let dk = pbkdf2_hmac_sha256(b"passwd", b"salt", 1);
        assert_eq!(
            hex(&dk),
            "55ac046e56e3089fec1691c22544b605f94185216dde0465e68b9d57c20dacbc"
        );
    }

    #[test]
    fn sha1_known_vectors() {
        // FIPS 180-1 / RFC 3174 test vectors.
        assert_eq!(hex(&sha1(b"")), "da39a3ee5e6b4b0d3255bfef95601890afd80709");
        assert_eq!(
            hex(&sha1(b"abc")),
            "a9993e364706816aba3e25717850c26c9cd0d89d"
        );
        assert_eq!(
            hex(&sha1(
                b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"
            )),
            "84983e441c3bd26ebaae4aa1f95129e5e54670f1"
        );
        // Multi-block (>64 bytes) coverage.
        assert_eq!(
            hex(&sha1(&[b'a'; 200])),
            "e61cfffe0d9195a525fc6cf06ca2d77119c24a40"
        );
    }

    #[test]
    fn sha1_websocket_handshake_vector() {
        // RFC 6455 §1.3: the worked Sec-WebSocket-Accept example.
        let mut input = b"dGhlIHNhbXBsZSBub25jZQ==".to_vec();
        input.extend_from_slice(b"258EAFA5-E914-47DA-95CA-C5AB0DC85B11");
        assert_eq!(base64_encode(&sha1(&input)), "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=");
    }

    #[test]
    fn md5_known_vectors() {
        assert_eq!(hex(&md5(b"")), "d41d8cd98f00b204e9800998ecf8427e");
        assert_eq!(hex(&md5(b"abc")), "900150983cd24fb0d6963f7d28e17f72");
        assert_eq!(
            hex(&md5(b"The quick brown fox jumps over the lazy dog")),
            "9e107d9d372bb6826bd81d3542a419d6"
        );
    }

    #[test]
    fn hex_roundtrip_and_rejects() {
        assert_eq!(from_hex(&hex(b"sutegi")).unwrap(), b"sutegi");
        assert_eq!(from_hex("00ff"), Some(vec![0, 255]));
        assert!(from_hex("abc").is_none()); // odd length
        assert!(from_hex("zz").is_none()); // non-hex
    }

    #[test]
    fn constant_time_eq_basics() {
        assert!(constant_time_eq(b"same", b"same"));
        assert!(!constant_time_eq(b"same", b"sam0"));
        assert!(!constant_time_eq(b"short", b"longer"));
        assert!(constant_time_eq(b"", b""));
    }

    #[test]
    fn random_bytes_sized_and_varied() {
        let a = random_bytes(32).unwrap();
        let b = random_bytes(32).unwrap();
        assert_eq!(a.len(), 32);
        assert_ne!(a, b); // astronomically unlikely to collide
        assert!(a.iter().any(|&x| x != 0));
    }

    #[test]
    fn hkdf_sha256_rfc5869() {
        // RFC 5869 test case 1.
        let okm = hkdf_sha256(
            &[0x0b; 22],
            &from_hex("000102030405060708090a0b0c").unwrap(),
            &from_hex("f0f1f2f3f4f5f6f7f8f9").unwrap(),
            42,
        );
        assert_eq!(
            hex(&okm),
            "3cb25f25faacd57a90434f64d0362f2a2d2d0a90cf1a5a4c5db02d56ecc4c5bf34007208d5b887185865"
        );
        // RFC 5869 test case 3: empty salt and info.
        let okm = hkdf_sha256(&[0x0b; 22], &[], &[], 42);
        assert_eq!(
            hex(&okm),
            "8da4e775a563c18f715f802a063c5a31b8a11f5c5ee1879ec3454e5f3c738d2d9d201395faa4b61a96c8"
        );
    }

    /// RFC 8439 §2.4.2 sunscreen plaintext, shared by two vectors below.
    const SUNSCREEN: &[u8] = b"Ladies and Gentlemen of the class of '99: If I could offer you \
        only one tip for the future, sunscreen would be it.";

    #[test]
    fn chacha20_rfc8439_encryption_vector() {
        // RFC 8439 §2.4.2.
        let key: [u8; 32] = (0u8..32).collect::<Vec<u8>>().try_into().unwrap();
        let nonce: [u8; 12] = from_hex("000000000000004a00000000")
            .unwrap()
            .try_into()
            .unwrap();
        let mut data = SUNSCREEN.to_vec();
        chacha20_xor(&key, &nonce, 1, &mut data);
        assert_eq!(
            hex(&data),
            "6e2e359a2568f98041ba0728dd0d6981e97e7aec1d4360c20a27afccfd9fae0b\
             f91b65c5524733ab8f593dabcd62b3571639d624e65152ab8f530c359f0861d8\
             07ca0dbf500d6a6156a38e088a22b65e52bc514d16ccf806818ce91ab7793736\
             5af90bbf74a35be6b40b8eedf2785e42874d"
        );
        // Decrypt is the same XOR.
        chacha20_xor(&key, &nonce, 1, &mut data);
        assert_eq!(data, SUNSCREEN);
    }

    #[test]
    fn poly1305_rfc8439_vector() {
        // RFC 8439 §2.5.2.
        let key: [u8; 32] =
            from_hex("85d6be7857556d337f4452fe42d506a80103808afb0db2fd4abff6af4149f51b")
                .unwrap()
                .try_into()
                .unwrap();
        let tag = poly1305(&key, b"Cryptographic Forum Research Group");
        assert_eq!(hex(&tag), "a8061dc1305136c6c22b8baf0c0127a9");
    }

    #[test]
    fn chacha20_poly1305_rfc8439_aead_vector() {
        // RFC 8439 §2.8.2.
        let key: [u8; 32] = (0x80u8..0xa0).collect::<Vec<u8>>().try_into().unwrap();
        let nonce: [u8; 12] = from_hex("070000004041424344454647")
            .unwrap()
            .try_into()
            .unwrap();
        let aad = from_hex("50515253c0c1c2c3c4c5c6c7").unwrap();

        let sealed = chacha20_poly1305_seal(&key, &nonce, &aad, SUNSCREEN);
        assert_eq!(
            hex(&sealed),
            "d31a8d34648e60db7b86afbc53ef7ec2a4aded51296e08fea9e2b5a736ee62d6\
             3dbea45e8ca9671282fafb69da92728b1a71de0a9e060b2905d6a5b67ecd3b36\
             92ddbd7f2d778b8c9803aee328091b58fab324e4fad675945585808b4831d7bc\
             3ff4def08e4b7a9de576d26586cec64b6116\
             1ae10b594f09e26a7e902ecbd0600691"
        );
        assert_eq!(
            chacha20_poly1305_open(&key, &nonce, &aad, &sealed).unwrap(),
            SUNSCREEN
        );

        // Any single flipped bit — in ciphertext, tag, or aad — must reject.
        for i in [0, sealed.len() / 2, sealed.len() - 1] {
            let mut bad = sealed.clone();
            bad[i] ^= 1;
            assert!(chacha20_poly1305_open(&key, &nonce, &aad, &bad).is_none());
        }
        assert!(chacha20_poly1305_open(&key, &nonce, b"wrong aad", &sealed).is_none());
        assert!(chacha20_poly1305_open(&key, &nonce, &aad, &sealed[..15]).is_none());
    }

    #[test]
    fn seal_open_roundtrip_and_rejects() {
        let key: [u8; 32] = hkdf_sha256(b"master secret", &[], b"test-enc", 32)
            .try_into()
            .unwrap();
        for pt in [&b""[..], b"x", b"hello world", &[0u8; 1000]] {
            let sealed = seal(&key, pt).unwrap();
            assert_eq!(sealed.len(), pt.len() + 28);
            assert_eq!(open(&key, &sealed).unwrap(), pt);
        }

        let sealed = seal(&key, b"attack at dawn").unwrap();
        // Fresh nonce every call: same plaintext, different ciphertext.
        assert_ne!(sealed, seal(&key, b"attack at dawn").unwrap());
        // Wrong key, tampering, truncation all reject.
        let mut other_key = key;
        other_key[0] ^= 1;
        assert!(open(&other_key, &sealed).is_none());
        let mut bad = sealed.clone();
        bad[13] ^= 1;
        assert!(open(&key, &bad).is_none());
        assert!(open(&key, &sealed[..27]).is_none());
    }

    #[test]
    fn base64_roundtrip_and_vectors() {
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
        for s in [&b""[..], b"f", b"fo", b"foo", b"foob", b"hello world!!"] {
            assert_eq!(base64_decode(&base64_encode(s)).unwrap(), s);
        }
    }
}
