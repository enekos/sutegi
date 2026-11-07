//! The [`Email`] builder and its RFC 2822 / MIME rendering.
//!
//! An email is text, HTML, or both (both renders as `multipart/alternative`).
//! Bodies are base64 transfer-encoded (safe for any UTF-8 content), subjects
//! and display names use RFC 2047 encoded-words when they need to.

use sutegi_crypto::base64_encode;

/// An address with an optional display name.
#[derive(Clone, Debug, PartialEq)]
pub struct Address {
    pub email: String,
    pub name: String,
}

/// Longest forward-path RFC 5321 §4.5.3.1.3 allows, minus the angle brackets.
const MAX_MAILBOX: usize = 254;
/// RFC 5321 §4.5.3.1.1.
const MAX_LOCAL_PART: usize = 64;

/// Check a bare `local@domain` mailbox against a strict, injection-proof
/// subset of RFC 5321: dot-atom local part, hostname domain. Everything that
/// could split one address into two (`,` `;` whitespace), close the SMTP path
/// (`<` `>`), open a quoted string or comment (`"` `(` `)` `\`), or read as a
/// command-line option (a leading `-`) is refused. Quoted local parts and
/// address literals are not supported. Non-ASCII letters and digits are
/// allowed (SMTPUTF8 / IDN).
///
/// # Errors
/// A description of why `addr` is not an acceptable mailbox.
pub fn check_mailbox(addr: &str) -> Result<(), String> {
    let invalid = || format!("invalid email address: {addr:?}");
    if addr.is_empty() || addr.len() > MAX_MAILBOX || addr.starts_with('-') {
        return Err(invalid());
    }
    let (local, domain) = addr.split_once('@').ok_or_else(invalid)?;
    // RFC 5322 atext, plus '.' between atoms.
    let local_ok = !local.is_empty()
        && local.len() <= MAX_LOCAL_PART
        && !local.starts_with('.')
        && !local.ends_with('.')
        && !local.contains("..")
        && local.chars().all(|c| {
            c.is_ascii_alphanumeric()
                || "!#$%&'*+-/=?^_`{|}~.".contains(c)
                || (!c.is_ascii() && c.is_alphanumeric())
        });
    let domain_ok = !domain.is_empty()
        && !domain.starts_with(['.', '-'])
        && !domain.ends_with('.')
        && !domain.contains("..")
        && domain.chars().all(|c| {
            c.is_ascii_alphanumeric()
                || c == '-'
                || c == '.'
                || (!c.is_ascii() && c.is_alphanumeric())
        });
    if local_ok && domain_ok {
        Ok(())
    } else {
        Err(invalid())
    }
}

impl Address {
    /// `a@b.co` or `Name <a@b.co>`. The mailbox itself must pass
    /// [`check_mailbox`]; the display name may be anything without control
    /// characters (it is quoted or RFC 2047-encoded on render).
    ///
    /// # Errors
    /// The input is not one of those two shapes, or the mailbox is invalid.
    pub fn parse(s: &str) -> Result<Address, String> {
        // Reject control characters that would let an address smuggle a header
        // or SMTP command (CRLF injection). Guarding the raw input keeps both
        // `email` and `name` clean for `render` and the envelope writes.
        if s.contains(['\r', '\n', '\0']) {
            return Err("invalid email address: control character".to_string());
        }
        let s = s.trim();
        let (name, email) = match (s.rfind('<'), s.ends_with('>')) {
            (Some(i), true) => (
                s[..i].trim().trim_matches('"').to_string(),
                s[i + 1..s.len() - 1].trim().to_string(),
            ),
            _ => (String::new(), s.to_string()),
        };
        check_mailbox(&email)?;
        if name.chars().any(char::is_control) {
            return Err("invalid email address: control character".to_string());
        }
        Ok(Address { email, name })
    }

    /// Re-check an `Address` that may not have come from [`Address::parse`]
    /// (the fields are public).
    ///
    /// # Errors
    /// The mailbox is invalid or the name holds control characters.
    pub fn check(&self) -> Result<(), String> {
        check_mailbox(&self.email)?;
        if self.name.chars().any(char::is_control) {
            return Err(format!(
                "invalid display name for {}: control character",
                self.email
            ));
        }
        Ok(())
    }

    fn render(&self) -> String {
        if self.name.is_empty() {
            self.email.clone()
        } else if self.name.is_ascii() && !self.name.contains(['"', '\\', '\r', '\n']) {
            format!("\"{}\" <{}>", self.name, self.email)
        } else {
            format!("{} <{}>", encoded_word(&self.name), self.email)
        }
    }
}

/// A message under construction. Build with the fluent methods, hand to a
/// [`crate::Mailer`] (which fills in the default `From` if you didn't).
#[derive(Clone, Debug, Default)]
pub struct Email {
    pub from: Option<Address>,
    pub to: Vec<Address>,
    pub cc: Vec<Address>,
    pub bcc: Vec<Address>,
    pub reply_to: Option<Address>,
    pub subject: String,
    pub text: Option<String>,
    pub html: Option<String>,
    pub headers: Vec<(String, String)>,
}

impl Email {
    pub fn new() -> Email {
        Email::default()
    }

    /// Add a recipient (`a@b.co` or `Name <a@b.co>`). Chainable; call again
    /// for more recipients. Invalid addresses surface when the mail is sent.
    pub fn to(mut self, addr: &str) -> Email {
        if let Ok(a) = Address::parse(addr) {
            self.to.push(a);
        } else {
            self.headers
                .push(("X-Sutegi-Invalid-To".to_string(), addr.to_string()));
        }
        self
    }

    pub fn cc(mut self, addr: &str) -> Email {
        if let Ok(a) = Address::parse(addr) {
            self.cc.push(a);
        }
        self
    }

    pub fn bcc(mut self, addr: &str) -> Email {
        if let Ok(a) = Address::parse(addr) {
            self.bcc.push(a);
        }
        self
    }

    pub fn from(mut self, addr: &str) -> Email {
        self.from = Address::parse(addr).ok();
        self
    }

    pub fn reply_to(mut self, addr: &str) -> Email {
        self.reply_to = Address::parse(addr).ok();
        self
    }

    pub fn subject(mut self, s: &str) -> Email {
        self.subject = s.to_string();
        self
    }

    /// The plain-text body.
    pub fn text(mut self, body: &str) -> Email {
        self.text = Some(body.to_string());
        self
    }

    /// The HTML body. Set both `text` and `html` for `multipart/alternative`.
    pub fn html(mut self, body: &str) -> Email {
        self.html = Some(body.to_string());
        self
    }

    /// An extra top-level header.
    pub fn header(mut self, name: &str, value: &str) -> Email {
        self.headers.push((name.to_string(), value.to_string()));
        self
    }

    /// Sanity-check before sending: a sender, at least one recipient, a body.
    pub fn validate(&self) -> Result<(), String> {
        if self.from.is_none() {
            return Err("email has no From address".to_string());
        }
        if self.to.is_empty() && self.cc.is_empty() && self.bcc.is_empty() {
            return Err("email has no recipients".to_string());
        }
        if let Some((_, bad)) = self
            .headers
            .iter()
            .find(|(k, _)| k == "X-Sutegi-Invalid-To")
        {
            return Err(format!("invalid email address: {bad}"));
        }
        if self.text.is_none() && self.html.is_none() {
            return Err("email has no body".to_string());
        }
        // Fields are public: an Address built by hand never went through parse.
        let every = self
            .from
            .iter()
            .chain(&self.reply_to)
            .chain(self.recipients());
        for addr in every {
            addr.check()?;
        }
        Ok(())
    }

    /// Every envelope recipient (To + Cc + Bcc) — what SMTP `RCPT TO` needs.
    pub fn recipients(&self) -> Vec<&Address> {
        self.to.iter().chain(&self.cc).chain(&self.bcc).collect()
    }

    /// Render the full RFC 2822 message (headers + body, CRLF line endings).
    /// `Bcc` recipients are on the envelope, never in the headers.
    /// `message_id` becomes the `Message-ID`; `now` stamps the `Date`.
    pub fn render(&self, message_id: &str, now_unix: i64) -> String {
        let mut out = String::new();
        let mut push = |k: &str, v: &str| {
            out.push_str(k);
            out.push_str(": ");
            out.push_str(v);
            out.push_str("\r\n");
        };

        if let Some(from) = &self.from {
            push("From", &from.render());
        }
        if !self.to.is_empty() {
            push("To", &join(&self.to));
        }
        if !self.cc.is_empty() {
            push("Cc", &join(&self.cc));
        }
        if let Some(rt) = &self.reply_to {
            push("Reply-To", &rt.render());
        }
        push("Subject", &encode_subject(&self.subject));
        push("Date", &rfc2822_date(now_unix));
        push("Message-ID", &format!("<{message_id}>"));
        push("MIME-Version", "1.0");
        for (k, v) in &self.headers {
            if !k.starts_with("X-Sutegi-Invalid") {
                // Strip CRLF from the name too — a smuggled newline in the
                // header name would otherwise inject a whole extra header.
                push(&k.replace(['\r', '\n'], " "), &v.replace(['\r', '\n'], " "));
            }
        }

        match (&self.text, &self.html) {
            (Some(text), Some(html)) => {
                let boundary = format!("=_sutegi_{message_id}");
                push(
                    "Content-Type",
                    &format!("multipart/alternative; boundary=\"{boundary}\""),
                );
                out.push_str("\r\n");
                out.push_str(&format!("--{boundary}\r\n"));
                out.push_str(&part("text/plain", text));
                out.push_str(&format!("--{boundary}\r\n"));
                out.push_str(&part("text/html", html));
                out.push_str(&format!("--{boundary}--\r\n"));
            }
            (Some(text), None) => {
                out.push_str(&body_headers("text/plain"));
                out.push_str("\r\n");
                out.push_str(&wrap_base64(text.as_bytes()));
            }
            (None, Some(html)) => {
                out.push_str(&body_headers("text/html"));
                out.push_str("\r\n");
                out.push_str(&wrap_base64(html.as_bytes()));
            }
            (None, None) => out.push_str("\r\n"),
        }
        out
    }
}

fn join(addrs: &[Address]) -> String {
    addrs
        .iter()
        .map(Address::render)
        .collect::<Vec<_>>()
        .join(", ")
}

fn body_headers(mime: &str) -> String {
    format!("Content-Type: {mime}; charset=utf-8\r\nContent-Transfer-Encoding: base64\r\n")
}

fn part(mime: &str, body: &str) -> String {
    format!("{}\r\n{}", body_headers(mime), wrap_base64(body.as_bytes()))
}

/// Base64 with 76-char lines (RFC 2045 §6.8).
fn wrap_base64(data: &[u8]) -> String {
    let b64 = base64_encode(data);
    let mut out = String::with_capacity(b64.len() + b64.len() / 76 * 2 + 2);
    for chunk in b64.as_bytes().chunks(76) {
        out.push_str(std::str::from_utf8(chunk).unwrap_or_default());
        out.push_str("\r\n");
    }
    out
}

/// RFC 2047 encoded-word for non-ASCII header text.
fn encoded_word(s: &str) -> String {
    format!("=?UTF-8?B?{}?=", base64_encode(s.as_bytes()))
}

fn encode_subject(s: &str) -> String {
    if s.is_ascii() && !s.contains(['\r', '\n']) {
        s.to_string()
    } else {
        encoded_word(&s.replace(['\r', '\n'], " "))
    }
}

/// `Tue, 02 Jul 2026 12:34:56 +0000` for a unix timestamp (always UTC).
pub(crate) fn rfc2822_date(unix: i64) -> String {
    const DAYS: [&str; 7] = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"]; // epoch was a Thursday
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let days = unix.div_euclid(86_400);
    let rem = unix.rem_euclid(86_400);
    let weekday = DAYS[days.rem_euclid(7) as usize];
    // Civil-from-days (Howard Hinnant).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = mp + if mp < 10 { 3 } else { -9 };
    let y = y + i64::from(m <= 2);
    format!(
        "{weekday}, {d:02} {} {y:04} {:02}:{:02}:{:02} +0000",
        MONTHS[(m - 1) as usize],
        rem / 3_600,
        rem % 3_600 / 60,
        rem % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn address_parsing() {
        assert_eq!(
            Address::parse("a@b.co").unwrap(),
            Address {
                email: "a@b.co".into(),
                name: String::new()
            }
        );
        let named = Address::parse("Eneko S <e@s.dev>").unwrap();
        assert_eq!(
            (named.name.as_str(), named.email.as_str()),
            ("Eneko S", "e@s.dev")
        );
        assert!(Address::parse("nope").is_err());
        assert!(Address::parse("<@b.co>").is_err());
    }

    #[test]
    fn address_parse_rejects_control_chars() {
        // CRLF injection PoC: a recipient smuggling a header / SMTP command.
        assert!(Address::parse("a@b.co\r\nBcc: attacker@evil.com").is_err());
        assert!(Address::parse("a@b.co\nBcc: attacker@evil.com").is_err());
        assert!(Address::parse("victim@x.co\0").is_err());
        // Injection via the display name is rejected too.
        assert!(Address::parse("N\r\nBcc: e@x.co <a@b.co>").is_err());
        // Valid forms still parse.
        assert!(Address::parse("a@b.co").is_ok());
        assert!(Address::parse("Name <a@b.co>").is_ok());
    }

    #[test]
    fn address_parse_rejects_recipient_injection() {
        for bad in [
            // Two recipients in one To: sendmail -t would deliver to both.
            "me@evil.com, v1@x.com",
            "me@evil.com;v1@x.com",
            // ESMTP parameter smuggled into RCPT TO:<...>.
            "x <a@b.co> NOTIFY=SUCCESS ORCPT=rfc822;spy@evil.com",
            "a@b.co> NOTIFY=SUCCESS",
            "Name <a@b.co, c@d.co>",
            "\"a b\"@x.co",
            "a(comment)@x.co",
            "a\\b@x.co",
            "a b@x.co",
            "a\tb@x.co",
            // Read as a sendmail option.
            "-X/var/www/x.log@a",
            "-oQ/tmp@x",
            "a@@b.co",
            "a@b@c.co",
            ".a@b.co",
            "a..b@c.co",
            "a@-b.co",
            "a@b..co",
        ] {
            assert!(Address::parse(bad).is_err(), "{bad:?} must be rejected");
        }
        assert!(Address::parse(&format!("{}@b.co", "a".repeat(65))).is_err());
        // The accepted forms are unchanged.
        for good in [
            "a@b.co",
            "first.last+tag@sub.example.co.uk",
            "o'brien@x.ie",
            "Name <a@b.co>",
            "\"Last, First\" <a@b.co>",
            "  Name   <  a@b.co  >  ",
            "Kaixo Ñ <app@example.com>",
            "ñandú@ejemplo.es",
        ] {
            assert!(Address::parse(good).is_ok(), "{good:?} must parse");
        }
        let a = Address::parse("\"Last, First\" <a@b.co>").unwrap();
        assert_eq!(
            (a.name.as_str(), a.email.as_str()),
            ("Last, First", "a@b.co")
        );
    }

    #[test]
    fn hand_built_addresses_are_validated() {
        let mut email = Email::new().from("a@b.co").to("t@b.co").text("x");
        email.to.push(Address {
            email: "me@evil.com, v1@x.com".to_string(),
            name: String::new(),
        });
        assert!(email.validate().is_err());
        let mut email = Email::new().from("a@b.co").to("t@b.co").text("x");
        email.from = Some(Address {
            email: "a@b.co".to_string(),
            name: "N\0".to_string(),
        });
        assert!(email.validate().is_err());
    }

    #[test]
    fn render_single_part() {
        let msg = Email::new()
            .from("app@example.com")
            .to("You <you@example.com>")
            .subject("Hello")
            .text("hi there")
            .render("mid-1@test", 1_782_991_791);
        assert!(msg.contains("From: app@example.com\r\n"));
        assert!(msg.contains("To: \"You\" <you@example.com>\r\n"));
        assert!(msg.contains("Subject: Hello\r\n"));
        assert!(msg.contains("Message-ID: <mid-1@test>\r\n"));
        assert!(msg.contains("Content-Type: text/plain; charset=utf-8\r\n"));
        assert!(msg.contains(&base64_encode(b"hi there")));
    }

    #[test]
    fn render_multipart_alternative() {
        let msg = Email::new()
            .from("app@example.com")
            .to("you@example.com")
            .subject("Both")
            .text("plain")
            .html("<b>rich</b>")
            .render("mid-2@test", 0);
        assert!(msg.contains("multipart/alternative; boundary="));
        assert!(msg.contains("Content-Type: text/plain"));
        assert!(msg.contains("Content-Type: text/html"));
        assert!(msg.contains(&base64_encode(b"plain")));
        assert!(msg.contains(&base64_encode(b"<b>rich</b>")));
        assert!(msg.ends_with("--\r\n"));
    }

    #[test]
    fn utf8_subject_and_name_are_encoded_words() {
        let msg = Email::new()
            .from("Kaixo Ñ <app@example.com>")
            .to("you@example.com")
            .subject("¡Kaixo!")
            .text("x")
            .render("mid-3@test", 0);
        assert!(msg.contains("Subject: =?UTF-8?B?"));
        assert!(msg.contains("From: =?UTF-8?B?"));
    }

    #[test]
    fn bcc_is_envelope_only() {
        let email = Email::new()
            .from("a@b.co")
            .to("to@b.co")
            .bcc("hidden@b.co")
            .subject("s")
            .text("x");
        let msg = email.render("mid-4@test", 0);
        assert!(!msg.contains("hidden@b.co"));
        assert_eq!(email.recipients().len(), 2);
    }

    #[test]
    fn header_injection_is_neutralized() {
        let msg = Email::new()
            .from("a@b.co")
            .to("to@b.co")
            .subject("line1\r\nX-Evil: yes")
            .header("X-Custom", "v\r\nX-Evil2: yes")
            .text("x")
            .render("mid-5@test", 0);
        // Neither smuggled name may appear at the start of a header line.
        assert!(!msg.contains("\r\nX-Evil:"));
        assert!(!msg.contains("\r\nX-Evil2:"));
    }

    #[test]
    fn header_name_injection_is_neutralized() {
        let msg = Email::new()
            .from("a@b.co")
            .to("to@b.co")
            .header("X-Evil\r\nBcc: attacker@evil.com", "v")
            .subject("s")
            .text("x")
            .render("mid-6@test", 0);
        // The smuggled name must not open a new header line.
        assert!(!msg.contains("\r\nBcc: attacker@evil.com"));
        assert!(!msg.contains("X-Evil\r\n"));
    }

    #[test]
    fn validate_catches_gaps() {
        assert!(Email::new().validate().is_err()); // no from
        assert!(Email::new().from("a@b.co").validate().is_err()); // no rcpt
        assert!(Email::new().from("a@b.co").to("t@b.co").validate().is_err()); // no body
        assert!(Email::new()
            .from("a@b.co")
            .to("bad")
            .text("x")
            .validate()
            .is_err());
        assert!(Email::new()
            .from("a@b.co")
            .to("t@b.co")
            .text("x")
            .validate()
            .is_ok());
    }

    #[test]
    fn date_format() {
        assert_eq!(rfc2822_date(0), "Thu, 01 Jan 1970 00:00:00 +0000");
        assert_eq!(
            rfc2822_date(1_369_353_600),
            "Fri, 24 May 2013 00:00:00 +0000"
        );
    }

    #[test]
    fn base64_wraps_at_76() {
        let long = "x".repeat(300);
        let wrapped = wrap_base64(long.as_bytes());
        assert!(wrapped.lines().all(|l| l.len() <= 76));
    }
}
