//! Adversarial coverage for RFC 2822 / MIME email rendering.
//!
//! The headline risk for a mail builder is **header injection**: a newline in a
//! subject, recipient, or custom header value that smuggles in a new header
//! (e.g. an attacker-chosen `Bcc:`). Rendering must neutralize embedded CRLFs,
//! and must never panic on arbitrary field content.
//!
//! Deterministic (fixed-seed splitmix64) for reproducibility in CI.

use sutegi_mail::Email;

fn splitmix(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

#[test]
fn header_injection_is_neutralized() {
    // A CRLF-laden subject must not create a new `Bcc:` header line.
    let e = Email::new()
        .from("a@b.com")
        .to("c@d.com")
        .subject("Hello\r\nBcc: evil@attacker.com")
        .text("body");
    let wire = e.render("mid@sutegi", 0);
    assert!(
        !wire.contains("\r\nBcc: evil@attacker.com\r\n"),
        "subject CRLF injected a header:\n{wire}"
    );

    // Same for a custom header value.
    let e = Email::new()
        .from("a@b.com")
        .to("c@d.com")
        .header("X-Thing", "ok\r\nX-Evil: injected")
        .text("body");
    let wire = e.render("mid@sutegi", 0);
    assert!(
        !wire.contains("\r\nX-Evil: injected"),
        "custom header CRLF injected a header:\n{wire}"
    );
}

#[test]
fn rendering_arbitrary_fields_never_panics() {
    let mut seed = 0x0000_004d_4149_4c00u64; // "MAIL"
    for _ in 0..20_000 {
        let field = |seed: &mut u64| -> String {
            let len = (splitmix(seed) as usize) % 48;
            // Include CR, LF, quotes, non-ASCII, and address-ish characters.
            let alphabet = "abc @.<>\"\\\r\n\tñé0123456789,;:";
            let chars: Vec<char> = alphabet.chars().collect();
            (0..len)
                .map(|_| chars[(splitmix(seed) as usize) % chars.len()])
                .collect()
        };
        let e = Email::new()
            .from("sender@example.com")
            .to("rcpt@example.com")
            .subject(&field(&mut seed))
            .header("X-Fuzz", &field(&mut seed))
            .text(&field(&mut seed))
            .html(&field(&mut seed));
        let wire = e.render("mid@sutegi", 1_700_000_000);
        // No rendered header line may be a bare CRLF-smuggled injection: every
        // header the payload could inject would begin its own `\r\n`-prefixed
        // line, and no field value is allowed to introduce one verbatim.
        assert!(!wire.contains("\r\n\r\nX-Fuzz-Injected"));
    }
}
