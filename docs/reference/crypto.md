<!-- Generated from landing/src/Docs.svelte. Run: cd landing && pnpm docs:export -->

[← Collections](collections.md) · [Documentation index](../README.md) · [Hexagonal architecture →](hexagonal.md)

# Crypto primitives

`sutegi::crypto` is always compiled — it is what the sessions, auth, SigV4 signing, SCRAM handshake and WebSocket handshake are built from — and it is directly usable.

```rust
use sutegi::crypto;

// Hashing & MACs — the primitives the rest of the framework is built on.
let digest = crypto::sha256(b"payload");
let mut h = crypto::Sha256::new();                   // incremental: constant memory
h.update(&chunk); let digest = h.finalize();          // S3 bodies never live twice
let mac = crypto::hmac_sha256(secret, message);
let dk  = crypto::pbkdf2_hmac_sha256(pw, &salt, 600_000);

// Per-purpose subkeys, so a signing key and an encryption key never alias:
let enc_key = crypto::hkdf_sha256(master, b"", b"session-enc", 32);

// Two-way encryption: ChaCha20-Poly1305 (RFC 8439). seal() prepends a fresh
// random nonce (nonce || ct || tag, +28 bytes), so nonce reuse is impossible
// by construction. ChaCha over AES deliberately: add-rotate-xor is
// constant-time in plain software, where AES table lookups leak via cache.
let sealed = crypto::seal(&key, b"secret")?;
let plain  = crypto::open(&key, &sealed);            // Option<Vec<u8>>

// Utilities: constant_time_eq (piped through black_box), random_bytes (OS
// entropy), hex/from_hex, base64_encode/decode, now_secs/now_millis.
// Known-answer tested against the RFC vectors, with deterministic fuzz
// coverage for round-trip, bit-flip, AAD-mismatch and truncation.
```

The choices are deliberate: ChaCha20-Poly1305 over AES because add-rotate-xor is constant-time in plain software where AES table lookups leak through cache timing; a fresh random nonce prepended by `seal` so nonce reuse is impossible by construction; HKDF so a signing key and an encryption key derived from one master secret never alias; and `constant_time_eq` piped through `black_box` so the optimizer cannot rewrite it into an early exit. The raw stream cipher and one-shot Poly1305 stay private, because those are the pieces that are dangerous to hold directly.

> [!WARNING]
> **Read the posture page first**
>
> This is hand-rolled cryptography, known-answer tested and fuzzed but [not independently audited](honesty.md). Use it inside a trusted boundary; do not make it the only thing between hostile traffic and your secrets.

---

[← Collections](collections.md) · [Documentation index](../README.md) · [Hexagonal architecture →](hexagonal.md)
