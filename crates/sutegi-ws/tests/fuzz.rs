//! Adversarial coverage for the RFC 6455 frame decoder.
//!
//! Every frame byte comes off the network from an untrusted peer, so the
//! decoder must never panic, never allocate past `max_payload`, and must
//! yield identical results however the bytes are chunked. Deterministic
//! (fixed-seed splitmix64) for reproducibility in CI, matching the harness
//! style used across the other crates.

use sutegi_ws::protocol::{decode_frame, encode_frame, parse_close, Opcode};

fn splitmix(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

fn mask_payload(payload: &[u8], mask: [u8; 4]) -> Vec<u8> {
    payload
        .iter()
        .enumerate()
        .map(|(i, b)| b ^ mask[i & 3])
        .collect()
}

/// Hand-build a masked client frame with an arbitrary header.
fn masked_frame(opcode: u8, fin: bool, payload: &[u8], mask: [u8; 4]) -> Vec<u8> {
    let mut out = vec![if fin { 0x80 } else { 0 } | opcode];
    match payload.len() {
        n if n <= 125 => out.push(0x80 | n as u8),
        n if n <= 65535 => {
            out.push(0x80 | 126);
            out.extend_from_slice(&(n as u16).to_be_bytes());
        }
        n => {
            out.push(0x80 | 127);
            out.extend_from_slice(&(n as u64).to_be_bytes());
        }
    }
    out.extend_from_slice(&mask);
    out.extend_from_slice(&mask_payload(payload, mask));
    out
}

#[test]
fn garbage_bytes_never_panic() {
    let mut seed = 0x5715_31AB_u64;
    for _ in 0..50_000 {
        let len = (splitmix(&mut seed) as usize) % 64;
        let bytes: Vec<u8> = (0..len).map(|_| splitmix(&mut seed) as u8).collect();
        // Ok(None) / Ok(Some) / Err are all acceptable; panics are not.
        let _ = decode_frame(&bytes, 1 << 16);
        let _ = parse_close(&bytes);
    }
}

#[test]
fn structured_garbage_headers_never_panic() {
    // Bias toward valid-looking headers with hostile length fields so the
    // deep decoder states get exercised, not just the first byte checks.
    let mut seed = 0xC0FF_EE00_u64;
    for _ in 0..50_000 {
        let mut bytes = Vec::with_capacity(24);
        let opcode = [0x0u8, 0x1, 0x2, 0x8, 0x9, 0xA, 0x3, 0xF][(splitmix(&mut seed) % 8) as usize];
        let fin = splitmix(&mut seed) & 1 == 0;
        let rsv = ((splitmix(&mut seed) % 8) as u8) << 4;
        bytes.push(if fin { 0x80 } else { 0 } | rsv | opcode);
        let masked = splitmix(&mut seed) & 1 == 0;
        let len_kind = splitmix(&mut seed) % 3;
        match len_kind {
            0 => bytes.push(if masked { 0x80 } else { 0 } | (splitmix(&mut seed) % 128) as u8),
            1 => {
                bytes.push(if masked { 0x80 } else { 0 } | 126);
                bytes.extend_from_slice(&(splitmix(&mut seed) as u16).to_be_bytes());
            }
            _ => {
                bytes.push(if masked { 0x80 } else { 0 } | 127);
                bytes.extend_from_slice(&splitmix(&mut seed).to_be_bytes());
            }
        }
        // Random amount of trailing bytes (mask + partial payload).
        let extra = (splitmix(&mut seed) as usize) % 16;
        for _ in 0..extra {
            bytes.push(splitmix(&mut seed) as u8);
        }
        let _ = decode_frame(&bytes, 1 << 16);
    }
}

#[test]
fn oversized_length_fields_never_allocate() {
    // 64-bit lengths up to u64::MAX must be refused from the header alone —
    // if the decoder tried to allocate, these would abort the process.
    let mut seed = 0xDEAD_BEEF_u64;
    for _ in 0..10_000 {
        let huge = splitmix(&mut seed) | (1 << 62);
        let mut bytes = vec![0x82, 0x80 | 127];
        bytes.extend_from_slice(&huge.to_be_bytes());
        bytes.extend_from_slice(&[0; 4]);
        let r = decode_frame(&bytes, 1 << 20);
        assert!(r.is_err(), "length {huge:#x} must be refused");
    }
}

#[test]
fn random_valid_frames_roundtrip() {
    // Differential check: client-encode (masked) → decode must reproduce the
    // payload exactly, for every opcode and length class.
    let mut seed = 0x0BAD_F00D_u64;
    for _ in 0..2_000 {
        let opcode = match splitmix(&mut seed) % 3 {
            0 => Opcode::Text,
            1 => Opcode::Binary,
            _ => Opcode::Continuation,
        };
        let len = match splitmix(&mut seed) % 4 {
            0 => (splitmix(&mut seed) % 126) as usize,
            1 => 126 + (splitmix(&mut seed) % 1000) as usize,
            2 => 65536 + (splitmix(&mut seed) % 1000) as usize,
            _ => (splitmix(&mut seed) % 200_000) as usize,
        };
        let payload: Vec<u8> = (0..len).map(|_| splitmix(&mut seed) as u8).collect();
        let mask = (splitmix(&mut seed) as u32).to_be_bytes();
        let fin = splitmix(&mut seed) & 1 == 0;
        let opbits = match opcode {
            Opcode::Text => 0x1,
            Opcode::Binary => 0x2,
            _ => 0x0,
        };
        let wire = masked_frame(opbits, fin, &payload, mask);
        let (frame, consumed) = decode_frame(&wire, 1 << 20)
            .expect("valid frame")
            .expect("complete frame");
        assert_eq!(consumed, wire.len());
        assert_eq!(frame.fin, fin);
        assert_eq!(frame.opcode, opcode);
        assert_eq!(frame.payload, payload);
    }
}

#[test]
fn chunked_delivery_is_equivalent() {
    // Split every frame at random points: decode must report "incomplete"
    // for each strict prefix and produce the identical frame at the end.
    let mut seed = 0x00C1_0511_u64;
    for _ in 0..500 {
        let len = (splitmix(&mut seed) % 300) as usize;
        let payload: Vec<u8> = (0..len).map(|_| splitmix(&mut seed) as u8).collect();
        let wire = masked_frame(0x2, true, &payload, [9, 8, 7, 6]);
        for _ in 0..4 {
            let cut = (splitmix(&mut seed) as usize) % wire.len();
            assert!(
                decode_frame(&wire[..cut], 1 << 20).unwrap().is_none(),
                "prefix of {cut}/{} must be incomplete",
                wire.len()
            );
        }
        let (frame, _) = decode_frame(&wire, 1 << 20).unwrap().unwrap();
        assert_eq!(frame.payload, payload);
    }
}

#[test]
fn server_encode_is_client_decodable_shape() {
    // Sanity on the encoder's length fields: decode with the mask
    // requirement satisfied by re-masking the encoded frame.
    let mut seed = 0x7E57_u64;
    for _ in 0..2_000 {
        let len = (splitmix(&mut seed) % 70_000) as usize;
        let payload: Vec<u8> = (0..len).map(|_| splitmix(&mut seed) as u8).collect();
        let server_wire = encode_frame(Opcode::Binary, &payload, true);
        // Reconstruct as a masked frame from the same header info.
        let header_len = server_wire.len() - len;
        let mut masked = server_wire[..header_len].to_vec();
        masked[1] |= 0x80;
        let mask = [0xAA, 0xBB, 0xCC, 0xDD];
        masked.extend_from_slice(&mask);
        masked.extend_from_slice(&mask_payload(&payload, mask));
        let (frame, consumed) = decode_frame(&masked, 1 << 20).unwrap().unwrap();
        assert_eq!(consumed, masked.len());
        assert_eq!(frame.payload, payload);
    }
}
