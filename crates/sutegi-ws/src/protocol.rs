//! RFC 6455 wire protocol: the handshake accept key and a strict,
//! incremental frame codec.
//!
//! The decoder is server-side and adversarial-input-first: every byte comes
//! from the network, so it never panics, never allocates an attacker-chosen
//! buffer past `max_payload`, requires client masking, rejects reserved
//! bits/opcodes, enforces control-frame rules, and insists on minimal length
//! encodings. Violations map to the RFC close codes the peer will see.

use sutegi_crypto::{base64_encode, sha1};

/// The protocol GUID every `Sec-WebSocket-Accept` is derived with (RFC 6455 §1.3).
pub const WS_GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

/// Compute `Sec-WebSocket-Accept` for a client's `Sec-WebSocket-Key`.
pub fn accept_key(client_key: &str) -> String {
    let mut input = client_key.trim().as_bytes().to_vec();
    input.extend_from_slice(WS_GUID.as_bytes());
    base64_encode(&sha1(&input))
}

/// Frame opcodes (RFC 6455 §5.2). Reserved opcodes are decode errors.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Opcode {
    Continuation,
    Text,
    Binary,
    Close,
    Ping,
    Pong,
}

impl Opcode {
    fn from_bits(b: u8) -> Option<Opcode> {
        match b {
            0x0 => Some(Opcode::Continuation),
            0x1 => Some(Opcode::Text),
            0x2 => Some(Opcode::Binary),
            0x8 => Some(Opcode::Close),
            0x9 => Some(Opcode::Ping),
            0xA => Some(Opcode::Pong),
            _ => None,
        }
    }

    fn bits(self) -> u8 {
        match self {
            Opcode::Continuation => 0x0,
            Opcode::Text => 0x1,
            Opcode::Binary => 0x2,
            Opcode::Close => 0x8,
            Opcode::Ping => 0x9,
            Opcode::Pong => 0xA,
        }
    }

    pub fn is_control(self) -> bool {
        matches!(self, Opcode::Close | Opcode::Ping | Opcode::Pong)
    }
}

/// One decoded (unmasked) frame.
#[derive(Debug, PartialEq, Eq)]
pub struct Frame {
    pub fin: bool,
    pub opcode: Opcode,
    pub payload: Vec<u8>,
}

/// A protocol violation, carrying the RFC 6455 close code to hang up with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProtocolError {
    /// RSV1-3 set without a negotiated extension (we negotiate none) → 1002.
    ReservedBits,
    /// Unknown opcode → 1002.
    BadOpcode,
    /// Client-to-server frames MUST be masked → 1002.
    Unmasked,
    /// Control frame fragmented or with payload > 125 → 1002.
    BadControlFrame,
    /// 16/64-bit length used where a shorter encoding fits → 1002.
    NonMinimalLength,
    /// Payload past `max_payload` (or a 64-bit length past usize) → 1009.
    TooBig,
    /// Continuation without a started message, or a new data frame while a
    /// fragmented message is in flight → 1002.
    BadFragmentation,
    /// Text message (or close reason) that is not valid UTF-8 → 1007.
    BadUtf8,
    /// Close frame with a 1-byte payload or a reserved/invalid code → 1002.
    BadCloseFrame,
}

impl ProtocolError {
    /// The close code to send the peer for this violation.
    pub fn close_code(self) -> u16 {
        match self {
            ProtocolError::TooBig => 1009,
            ProtocolError::BadUtf8 => 1007,
            _ => 1002,
        }
    }
}

/// Try to decode one frame from `buf`. `Ok(None)` means "need more bytes";
/// `Ok(Some((frame, consumed)))` yields the unmasked frame and how many bytes
/// it occupied. Decoding never consumes on error, so the caller can hang up
/// with the frame still in the buffer.
pub fn decode_frame(
    buf: &[u8],
    max_payload: usize,
) -> Result<Option<(Frame, usize)>, ProtocolError> {
    if buf.len() < 2 {
        return Ok(None);
    }
    let b0 = buf[0];
    let b1 = buf[1];
    if b0 & 0x70 != 0 {
        return Err(ProtocolError::ReservedBits);
    }
    let fin = b0 & 0x80 != 0;
    let opcode = Opcode::from_bits(b0 & 0x0F).ok_or(ProtocolError::BadOpcode)?;
    if b1 & 0x80 == 0 {
        return Err(ProtocolError::Unmasked);
    }
    if opcode.is_control() && (!fin || (b1 & 0x7F) > 125) {
        return Err(ProtocolError::BadControlFrame);
    }

    let (len, mut idx) = match b1 & 0x7F {
        126 => {
            if buf.len() < 4 {
                return Ok(None);
            }
            let l = u16::from_be_bytes([buf[2], buf[3]]) as usize;
            if l < 126 {
                return Err(ProtocolError::NonMinimalLength);
            }
            (l, 4)
        }
        127 => {
            if buf.len() < 10 {
                return Ok(None);
            }
            let l = u64::from_be_bytes([
                buf[2], buf[3], buf[4], buf[5], buf[6], buf[7], buf[8], buf[9],
            ]);
            if l < 65536 {
                return Err(ProtocolError::NonMinimalLength);
            }
            if l > usize::MAX as u64 {
                return Err(ProtocolError::TooBig);
            }
            (l as usize, 10)
        }
        n => (n as usize, 2),
    };
    // Refuse before buffering: the length field is attacker-controlled.
    if len > max_payload {
        return Err(ProtocolError::TooBig);
    }
    if buf.len() < idx + 4 {
        return Ok(None);
    }
    let mask = [buf[idx], buf[idx + 1], buf[idx + 2], buf[idx + 3]];
    idx += 4;
    if buf.len() - idx < len {
        return Ok(None);
    }
    let mut payload = buf[idx..idx + len].to_vec();
    unmask(&mut payload, mask);
    Ok(Some((
        Frame {
            fin,
            opcode,
            payload,
        },
        idx + len,
    )))
}

/// XOR-unmask in place, 8 bytes at a time. Masking is the per-message hot
/// loop at high connection counts, so this runs at memory speed instead of
/// byte-at-a-time.
fn unmask(payload: &mut [u8], mask: [u8; 4]) {
    let m64 = u64::from_ne_bytes([
        mask[0], mask[1], mask[2], mask[3], mask[0], mask[1], mask[2], mask[3],
    ]);
    let mut chunks = payload.chunks_exact_mut(8);
    for chunk in &mut chunks {
        let v = u64::from_ne_bytes(chunk.try_into().unwrap()) ^ m64;
        chunk.copy_from_slice(&v.to_ne_bytes());
    }
    for (i, b) in chunks.into_remainder().iter_mut().enumerate() {
        // The remainder starts at an offset divisible by 8, so the mask
        // rotation is still aligned to `i & 3`.
        *b ^= mask[i & 3];
    }
}

/// Encode a server-to-client frame (never masked, per RFC 6455 §5.1).
pub fn encode_frame(opcode: Opcode, payload: &[u8], fin: bool) -> Vec<u8> {
    let mut out = Vec::with_capacity(payload.len() + 10);
    out.push(if fin { 0x80 } else { 0x00 } | opcode.bits());
    match payload.len() {
        n if n <= 125 => out.push(n as u8),
        n if n <= 65535 => {
            out.push(126);
            out.extend_from_slice(&(n as u16).to_be_bytes());
        }
        n => {
            out.push(127);
            out.extend_from_slice(&(n as u64).to_be_bytes());
        }
    }
    out.extend_from_slice(payload);
    out
}

/// Encode a close frame with a code and (truncated-to-fit) reason.
pub fn encode_close(code: u16, reason: &str) -> Vec<u8> {
    let mut payload = Vec::with_capacity(2 + reason.len().min(123));
    payload.extend_from_slice(&code.to_be_bytes());
    // Control payloads cap at 125 bytes; never split a UTF-8 sequence.
    let mut cut = reason.len().min(123);
    while !reason.is_char_boundary(cut) {
        cut -= 1;
    }
    payload.extend_from_slice(&reason.as_bytes()[..cut]);
    encode_frame(Opcode::Close, &payload, true)
}

/// Validate a received close-frame payload → `(code, reason)`. The reason
/// borrows from the payload — no allocation for the common discard-it case.
/// Empty payload means "no code" and is treated as 1000 (RFC 6455 §7.1.5).
pub fn parse_close(payload: &[u8]) -> Result<(u16, &str), ProtocolError> {
    match payload.len() {
        0 => Ok((1000, "")),
        1 => Err(ProtocolError::BadCloseFrame),
        _ => {
            let code = u16::from_be_bytes([payload[0], payload[1]]);
            if !valid_close_code(code) {
                return Err(ProtocolError::BadCloseFrame);
            }
            let reason = std::str::from_utf8(&payload[2..]).map_err(|_| ProtocolError::BadUtf8)?;
            Ok((code, reason))
        }
    }
}

/// Close codes a peer may legitimately send (RFC 6455 §7.4).
fn valid_close_code(code: u16) -> bool {
    matches!(code, 1000..=1003 | 1007..=1011 | 3000..=4999)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Client-side masking, for tests: what a browser puts on the wire.
    pub fn encode_masked(opcode: Opcode, payload: &[u8], fin: bool, mask: [u8; 4]) -> Vec<u8> {
        let mut out = Vec::with_capacity(payload.len() + 14);
        out.push(if fin { 0x80 } else { 0x00 } | opcode.bits());
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
        out.extend(payload.iter().enumerate().map(|(i, b)| b ^ mask[i & 3]));
        out
    }

    #[test]
    fn accept_key_rfc6455_vector() {
        assert_eq!(
            accept_key("dGhlIHNhbXBsZSBub25jZQ=="),
            "s3pPLMBiTxaQ9kYGzzhZRbK+xOo="
        );
    }

    #[test]
    fn masked_roundtrip_all_sizes() {
        // Cover the 7-bit, 16-bit, and 64-bit length encodings.
        for len in [0usize, 1, 5, 125, 126, 65535, 65536, 70_000] {
            let payload: Vec<u8> = (0..len).map(|i| (i * 7) as u8).collect();
            let wire = encode_masked(Opcode::Binary, &payload, true, [0xA1, 0x02, 0xC3, 0x54]);
            let (frame, consumed) = decode_frame(&wire, 1 << 20).unwrap().unwrap();
            assert_eq!(consumed, wire.len());
            assert!(frame.fin);
            assert_eq!(frame.opcode, Opcode::Binary);
            assert_eq!(frame.payload, payload, "len {len}");
        }
    }

    #[test]
    fn partial_input_asks_for_more() {
        let wire = encode_masked(Opcode::Text, b"hello world", true, [1, 2, 3, 4]);
        for cut in 0..wire.len() {
            assert!(
                decode_frame(&wire[..cut], 1 << 20).unwrap().is_none(),
                "cut {cut} should be incomplete"
            );
        }
        assert!(decode_frame(&wire, 1 << 20).unwrap().is_some());
    }

    #[test]
    fn unmasked_client_frame_rejected() {
        let wire = encode_frame(Opcode::Text, b"hi", true);
        assert_eq!(decode_frame(&wire, 1 << 20), Err(ProtocolError::Unmasked));
    }

    #[test]
    fn reserved_bits_and_opcodes_rejected() {
        let mut wire = encode_masked(Opcode::Text, b"x", true, [0; 4]);
        wire[0] |= 0x40; // RSV1
        assert_eq!(decode_frame(&wire, 1024), Err(ProtocolError::ReservedBits));

        for bad in [0x3u8, 0x4, 0x5, 0x6, 0x7, 0xB, 0xC, 0xD, 0xE, 0xF] {
            let mut wire = encode_masked(Opcode::Text, b"x", true, [0; 4]);
            wire[0] = 0x80 | bad;
            assert_eq!(
                decode_frame(&wire, 1024),
                Err(ProtocolError::BadOpcode),
                "opcode {bad:#x}"
            );
        }
    }

    #[test]
    fn control_frame_rules() {
        // Fragmented ping.
        let wire = encode_masked(Opcode::Ping, b"x", false, [0; 4]);
        assert_eq!(
            decode_frame(&wire, 1024),
            Err(ProtocolError::BadControlFrame)
        );
        // Oversized close (>125): encode_masked emits the 16-bit form, whose
        // header alone already violates the control rule.
        let wire = encode_masked(Opcode::Close, &[0u8; 126], true, [0; 4]);
        assert_eq!(
            decode_frame(&wire, 1024),
            Err(ProtocolError::BadControlFrame)
        );
    }

    #[test]
    fn non_minimal_lengths_rejected() {
        // 16-bit length encoding a value < 126.
        let mut wire = vec![0x81, 0x80 | 126];
        wire.extend_from_slice(&5u16.to_be_bytes());
        wire.extend_from_slice(&[0; 4]); // mask
        wire.extend_from_slice(&[0; 5]);
        assert_eq!(
            decode_frame(&wire, 1024),
            Err(ProtocolError::NonMinimalLength)
        );

        // 64-bit length encoding a value < 65536.
        let mut wire = vec![0x81, 0x80 | 127];
        wire.extend_from_slice(&300u64.to_be_bytes());
        wire.extend_from_slice(&[0; 4]);
        assert_eq!(
            decode_frame(&wire, 1 << 20),
            Err(ProtocolError::NonMinimalLength)
        );
    }

    #[test]
    fn oversized_payload_refused_before_buffering() {
        // Only the header arrives; the decoder must refuse from the length
        // field alone, without waiting for (or allocating) the payload.
        let mut wire = vec![0x82, 0x80 | 127];
        wire.extend_from_slice(&(1u64 << 40).to_be_bytes());
        wire.extend_from_slice(&[0; 4]);
        assert_eq!(decode_frame(&wire, 1 << 20), Err(ProtocolError::TooBig));
    }

    #[test]
    fn close_frame_payloads() {
        assert_eq!(parse_close(&[]), Ok((1000, "")));
        assert_eq!(parse_close(&[0x03]), Err(ProtocolError::BadCloseFrame));
        assert_eq!(parse_close(&1000u16.to_be_bytes()), Ok((1000, "")));
        let mut p = 1001u16.to_be_bytes().to_vec();
        p.extend_from_slice("adiós".as_bytes());
        assert_eq!(parse_close(&p), Ok((1001, "adiós")));
        // Reserved codes a peer must not send.
        for code in [0u16, 999, 1004, 1005, 1006, 1012, 2999] {
            assert_eq!(
                parse_close(&code.to_be_bytes()),
                Err(ProtocolError::BadCloseFrame),
                "code {code}"
            );
        }
        // Invalid UTF-8 reason.
        let mut p = 1000u16.to_be_bytes().to_vec();
        p.push(0xFF);
        assert_eq!(parse_close(&p), Err(ProtocolError::BadUtf8));
    }

    #[test]
    fn encode_close_truncates_on_char_boundary() {
        let long = "é".repeat(100); // 200 bytes of 2-byte chars
        let frame = encode_close(1000, &long);
        // header (2) + code (2) + reason ≤ 123, ending on a boundary → 122.
        assert_eq!(frame[1] as usize, 2 + 122);
        std::str::from_utf8(&frame[4..]).expect("reason stays valid UTF-8");
    }

    #[test]
    fn unmask_alignment_torture() {
        // Every (length, offset-pattern) unmasks correctly through the u64
        // fast path + remainder.
        for len in 0..64usize {
            let mask = [0xDE, 0xAD, 0xBE, 0xEF];
            let plain: Vec<u8> = (0..len).map(|i| i as u8).collect();
            let mut masked: Vec<u8> = plain
                .iter()
                .enumerate()
                .map(|(i, b)| b ^ mask[i & 3])
                .collect();
            unmask(&mut masked, mask);
            assert_eq!(masked, plain, "len {len}");
        }
    }
}
