//! A blocking, pure-`std` **SMTP client** over `std::net::TcpStream` —
//! EHLO, optional `AUTH PLAIN`/`LOGIN`, `MAIL FROM`/`RCPT TO`/`DATA` with
//! dot-stuffing. No TLS (the same stance as the Postgres driver): point it at
//! an in-cluster relay, a host-local Postfix, or a dev catcher like Mailpit /
//! MailHog. For TLS-only providers, terminate at a local relay or implement
//! [`crate::Transport`] over your own HTTP client.
//!
//! Because there is no TLS, **credentials are only sent to a loopback peer**
//! (a host-local relay) unless the app opts in with
//! [`Smtp::allow_plaintext_auth`] — e.g. a relay on a private network you
//! trust. `AUTH` is never sent to a server that does not advertise it.

use crate::message::{check_mailbox, Email};
use crate::Transport;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{IpAddr, TcpStream, ToSocketAddrs};
use std::time::Duration;
use sutegi_crypto::base64_encode;

/// Longest reply line accepted (RFC 5321 §4.5.3.1.5 says 512, with CRLF).
const MAX_REPLY_LINE: usize = 1024;
/// Most lines in one multi-line reply (an EHLO list is a dozen or so).
const MAX_REPLY_LINES: usize = 64;

/// SMTP transport config. `Smtp::new("localhost:1025")` is a working Mailpit
/// dev setup; add `.credentials(...)` for relays requiring auth.
#[derive(Clone, Debug)]
pub struct Smtp {
    addr: String,
    helo: String,
    user: Option<String>,
    pass: Option<String>,
    allow_plaintext_auth: bool,
    timeout: Duration,
}

impl Smtp {
    /// `host:port` of the relay (25 for classic, 1025 for Mailpit/MailHog).
    pub fn new(addr: &str) -> Smtp {
        Smtp {
            addr: addr.to_string(),
            helo: "sutegi.localdomain".to_string(),
            user: None,
            pass: None,
            allow_plaintext_auth: false,
            timeout: Duration::from_secs(30),
        }
    }

    /// The hostname announced in `EHLO`.
    pub fn hello_as(mut self, host: &str) -> Smtp {
        self.helo = host.to_string();
        self
    }

    /// `AUTH PLAIN` credentials (falls back to `AUTH LOGIN` if the server
    /// only advertises that). Sent only to a loopback peer unless
    /// [`allow_plaintext_auth`](Smtp::allow_plaintext_auth) is set; a send
    /// that would leak them over the network fails instead.
    pub fn credentials(mut self, user: &str, pass: &str) -> Smtp {
        self.user = Some(user.to_string());
        self.pass = Some(pass.to_string());
        self
    }

    /// Permit sending credentials in cleartext to a **non-loopback** relay.
    /// There is no TLS here, so anyone on the path sees them; enable only
    /// for a private network you trust.
    pub fn allow_plaintext_auth(mut self, allow: bool) -> Smtp {
        self.allow_plaintext_auth = allow;
        self
    }

    /// Connect timeout and per-read/write timeout (default 30 s).
    pub fn timeout(mut self, t: Duration) -> Smtp {
        self.timeout = t;
        self
    }
}

/// `EHLO` argument: a hostname or an address literal (`[192.0.2.1]`,
/// `[IPv6:…]`) — never whitespace, CR/LF or anything that extends the command.
fn check_helo(helo: &str) -> Result<(), String> {
    let ok = !helo.is_empty()
        && helo.len() <= 255
        && helo
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-.[]:".contains(&b));
    if ok {
        Ok(())
    } else {
        Err(format!("smtp: invalid EHLO hostname {helo:?}"))
    }
}

impl Transport for Smtp {
    fn send(&self, email: &Email, rendered: &str, message_id: &str) -> Result<String, String> {
        // `Email`/`Address` fields are public, so a caller can bypass
        // `Address::parse`. Every value that lands on a command line is
        // re-checked here, before anything touches the network.
        check_helo(&self.helo)?;
        let from = email
            .from
            .as_ref()
            .ok_or("email has no From address")?
            .email
            .clone();
        check_mailbox(&from)?;
        for rcpt in email.recipients() {
            check_mailbox(&rcpt.email)?;
        }

        let stream = self.connect()?;
        let creds = match (&self.user, &self.pass) {
            (Some(user), Some(pass)) => {
                let peer = stream
                    .peer_addr()
                    .map_err(|e| format!("smtp peer address: {e}"))?;
                if !may_send_credentials(peer.ip(), self.allow_plaintext_auth) {
                    return Err(format!(
                        "smtp: refusing to send credentials in cleartext to {peer} \
                         (no TLS); use a loopback relay or allow_plaintext_auth(true) \
                         / MAIL_SMTP_ALLOW_PLAINTEXT_AUTH=1"
                    ));
                }
                Some((user, pass))
            }
            _ => None,
        };
        let mut conn = Conn {
            reader: BufReader::new(stream.try_clone().map_err(|e| e.to_string())?),
            stream,
        };

        conn.expect(220)?; // greeting
        let ehlo = conn.command(&format!("EHLO {}", self.helo), 250)?;

        if let Some((user, pass)) = creds {
            let mechs = auth_mechanisms(&ehlo);
            if mechs.iter().any(|m| m == "PLAIN") {
                let ident = base64_encode(format!("\0{user}\0{pass}").as_bytes());
                conn.command(&format!("AUTH PLAIN {ident}"), 235)?;
            } else if mechs.iter().any(|m| m == "LOGIN") {
                conn.command("AUTH LOGIN", 334)?;
                conn.command(&base64_encode(user.as_bytes()), 334)?;
                conn.command(&base64_encode(pass.as_bytes()), 235)?;
            } else {
                let _ = conn.command("QUIT", 221); // best-effort goodbye
                return Err("smtp: credentials configured but the server advertises no \
                     AUTH PLAIN or LOGIN"
                    .to_string());
            }
        }

        conn.command(&format!("MAIL FROM:<{from}>"), 250)?;
        for rcpt in email.recipients() {
            conn.command(&format!("RCPT TO:<{}>", rcpt.email), 250)?;
        }

        conn.command("DATA", 354)?;
        conn.write_raw(&dot_stuff(rendered))?;
        conn.command(".", 250)?;
        let _ = conn.command("QUIT", 221);

        Ok(message_id.to_string())
    }
}

impl Smtp {
    /// Resolve and connect with `timeout` bounding each attempt (a bare
    /// `TcpStream::connect` waits for the OS default — minutes).
    fn connect(&self) -> Result<TcpStream, String> {
        let addrs = self
            .addr
            .to_socket_addrs()
            .map_err(|e| format!("smtp resolve {}: {e}", self.addr))?;
        let mut last = format!("smtp resolve {}: no addresses", self.addr);
        for addr in addrs {
            match TcpStream::connect_timeout(&addr, self.timeout) {
                Ok(stream) => {
                    stream
                        .set_read_timeout(Some(self.timeout))
                        .and_then(|()| stream.set_write_timeout(Some(self.timeout)))
                        .map_err(|e| format!("smtp set timeouts: {e}"))?;
                    return Ok(stream);
                }
                Err(e) => last = format!("smtp connect {addr}: {e}"),
            }
        }
        Err(last)
    }
}

/// Cleartext credentials go to the local machine only, unless opted in.
fn may_send_credentials(peer: IpAddr, opted_in: bool) -> bool {
    opted_in || peer.to_canonical().is_loopback()
}

/// The SASL mechanisms an EHLO reply advertises (`250-AUTH PLAIN LOGIN`, or
/// the legacy `250-AUTH=LOGIN`), uppercased. Empty when AUTH is absent.
fn auth_mechanisms(ehlo: &str) -> Vec<String> {
    ehlo.lines()
        .filter_map(|line| line.get(4..))
        .filter_map(|ext| {
            let ext = ext.trim().to_ascii_uppercase();
            let rest = ext
                .strip_prefix("AUTH ")
                .or_else(|| ext.strip_prefix("AUTH="))?;
            Some(
                rest.split_whitespace()
                    .map(str::to_string)
                    .collect::<Vec<_>>(),
            )
        })
        .flatten()
        .collect()
}

struct Conn {
    stream: TcpStream,
    reader: BufReader<TcpStream>,
}

impl Conn {
    fn command(&mut self, line: &str, expect: u16) -> Result<String, String> {
        self.write_raw(&format!("{line}\r\n"))?;
        self.read_reply(expect, line)
    }

    fn expect(&mut self, code: u16) -> Result<String, String> {
        self.read_reply(code, "(greeting)")
    }

    fn write_raw(&mut self, data: &str) -> Result<(), String> {
        self.stream
            .write_all(data.as_bytes())
            .and_then(|_| self.stream.flush())
            .map_err(|e| format!("smtp write: {e}"))
    }

    fn read_reply(&mut self, expect: u16, after: &str) -> Result<String, String> {
        read_reply(&mut self.reader, expect, after)
    }
}

/// Read a (possibly multi-line `250-…`) reply and check its code. Line length
/// and line count are bounded: the peer is not trusted to end its reply.
fn read_reply<R: BufRead>(r: &mut R, expect: u16, after: &str) -> Result<String, String> {
    let mut all = String::new();
    for _ in 0..MAX_REPLY_LINES {
        let mut line = String::new();
        r.by_ref()
            .take(MAX_REPLY_LINE as u64)
            .read_line(&mut line)
            .map_err(|e| format!("smtp read: {e}"))?;
        if line.is_empty() {
            return Err(format!("smtp: connection closed after {after}"));
        }
        if !line.ends_with('\n') {
            return Err(format!("smtp: reply line exceeds {MAX_REPLY_LINE} bytes"));
        }
        all.push_str(&line);
        if line.len() < 4 || line.as_bytes()[3] != b'-' {
            let code: u16 = line.get(..3).and_then(|c| c.parse().ok()).unwrap_or(0);
            // Credential-bearing commands (AUTH PLAIN blob, bare base64
            // lines of AUTH LOGIN) never appear in error messages.
            let secretish = after.starts_with("AUTH") || (!after.contains(' ') && after.len() > 20);
            let shown = if secretish { "(redacted)" } else { after };
            return if code == expect {
                Ok(all)
            } else {
                Err(format!(
                    "smtp: expected {expect} after {shown}, got: {}",
                    line.trim_end()
                ))
            };
        }
    }
    Err(format!("smtp: reply exceeds {MAX_REPLY_LINES} lines"))
}

/// RFC 5321 §4.5.2: any body line starting with `.` gets one prepended.
fn dot_stuff(rendered: &str) -> String {
    let mut out = String::with_capacity(rendered.len() + 16);
    for line in rendered.split_inclusive("\r\n") {
        if line.starts_with('.') {
            out.push('.');
        }
        out.push_str(line);
    }
    if !out.ends_with("\r\n") {
        out.push_str("\r\n");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    #[test]
    fn dot_stuffing() {
        assert_eq!(dot_stuff("a\r\n.b\r\n..c\r\n"), "a\r\n..b\r\n...c\r\n");
        assert_eq!(dot_stuff("no dots"), "no dots\r\n");
    }

    /// A minimal in-process SMTP server: accepts one session, records what it
    /// saw, answers canned codes.
    fn fake_server(require_auth: bool) -> (String, std::thread::JoinHandle<Vec<String>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let handle = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut w = stream;
            let mut log = Vec::new();
            let say = |w: &mut TcpStream, s: &str| {
                w.write_all(format!("{s}\r\n").as_bytes()).unwrap();
            };
            say(&mut w, "220 fake ESMTP");
            let mut in_data = false;
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 {
                    break;
                }
                let trimmed = line.trim_end().to_string();
                log.push(trimmed.clone());
                if in_data {
                    if trimmed == "." {
                        in_data = false;
                        say(&mut w, "250 queued");
                    }
                    continue;
                }
                match trimmed.split(' ').next().unwrap_or("") {
                    "EHLO" => {
                        if require_auth {
                            say(&mut w, "250-fake\r\n250 AUTH PLAIN LOGIN");
                        } else {
                            say(&mut w, "250 fake");
                        }
                    }
                    "AUTH" => say(&mut w, "235 ok"),
                    "MAIL" | "RCPT" => say(&mut w, "250 ok"),
                    "DATA" => {
                        in_data = true;
                        say(&mut w, "354 go");
                    }
                    "QUIT" => {
                        say(&mut w, "221 bye");
                        break;
                    }
                    _ => say(&mut w, "500 what"),
                }
            }
            log
        });
        (addr, handle)
    }

    #[test]
    fn full_session_against_fake_server() {
        let (addr, server) = fake_server(false);
        let email = Email::new()
            .from("app@example.com")
            .to("you@example.com")
            .cc("cc@example.com")
            .bcc("bcc@example.com")
            .subject("Test")
            .text(".leading dot line\nnormal");
        let rendered = email.render("mid@test", 0);
        let id = Smtp::new(&addr)
            .hello_as("test.local")
            .send(&email, &rendered, "mid@test")
            .unwrap();
        assert_eq!(id, "mid@test");

        let log = server.join().unwrap();
        let joined = log.join("\n");
        assert!(joined.contains("EHLO test.local"));
        assert!(joined.contains("MAIL FROM:<app@example.com>"));
        assert!(joined.contains("RCPT TO:<you@example.com>"));
        assert!(joined.contains("RCPT TO:<cc@example.com>"));
        assert!(joined.contains("RCPT TO:<bcc@example.com>")); // envelope only
        assert!(joined.contains("Subject: Test"));
        assert!(log.contains(&".".to_string())); // terminator
    }

    #[test]
    fn command_injection_is_refused_before_connecting() {
        use crate::message::Address;
        // Port 1 is never listened on: reaching connect would fail with a
        // different error, so "invalid" proves the check came first.
        let smtp = Smtp::new("127.0.0.1:1");
        let base = Email::new().from("a@b.co").subject("s").text("x");
        for bad in [
            "a@b.co> NOTIFY=SUCCESS ORCPT=rfc822;spy@evil.com",
            "a@b.co>\r\nRCPT TO:<spy@evil.com",
            "a@b.co\0",
        ] {
            let mut email = base.clone();
            email.to.push(Address {
                email: bad.to_string(),
                name: String::new(),
            });
            let err = smtp.send(&email, "", "m").unwrap_err();
            assert!(err.contains("invalid"), "{bad:?}: {err}");
        }
        let mut email = base.clone().to("t@b.co");
        email.from.as_mut().unwrap().email = "a@b.co> SIZE=1".to_string();
        assert!(smtp.send(&email, "", "m").unwrap_err().contains("invalid"));
        let email = base.to("t@b.co");
        for helo in ["evil\r\nRSET", "a b", ""] {
            let err = Smtp::new("127.0.0.1:1")
                .hello_as(helo)
                .send(&email, "", "m")
                .unwrap_err();
            assert!(err.contains("EHLO"), "{helo:?}: {err}");
        }
    }

    #[test]
    fn credentials_go_to_loopback_only_unless_opted_in() {
        let ip = |s: &str| s.parse::<IpAddr>().unwrap();
        assert!(may_send_credentials(ip("127.0.0.1"), false));
        assert!(may_send_credentials(ip("::1"), false));
        assert!(may_send_credentials(ip("::ffff:127.0.0.1"), false));
        assert!(!may_send_credentials(ip("203.0.113.5"), false));
        assert!(!may_send_credentials(ip("10.0.0.2"), false));
        assert!(may_send_credentials(ip("203.0.113.5"), true));
    }

    #[test]
    fn auth_is_never_sent_unless_advertised() {
        let (addr, server) = fake_server(false); // EHLO lists no AUTH
        let email = Email::new()
            .from("a@b.co")
            .to("t@b.co")
            .subject("s")
            .text("x");
        let err = Smtp::new(&addr)
            .credentials("user", "pass")
            .send(&email, &email.render("m@t", 0), "m@t")
            .unwrap_err();
        assert!(err.contains("advertises no"), "{err}");
        let log = server.join().unwrap().join("\n");
        assert!(!log.contains("AUTH"), "credentials leaked: {log}");
        assert!(!log.contains(&base64_encode(b"pass")));
    }

    #[test]
    fn auth_mechanisms_are_parsed_per_line() {
        let m = auth_mechanisms("250-fake\r\n250-AUTH=LOGIN\r\n250 SIZE 100\r\n");
        assert_eq!(m, ["LOGIN"]);
        let m = auth_mechanisms("250-fake\r\n250 auth plain CRAM-MD5\r\n");
        assert_eq!(m, ["PLAIN", "CRAM-MD5"]);
        // Substrings elsewhere in the reply are not an advertisement.
        assert!(auth_mechanisms("250-fake AUTH PLAIN\r\n250 X-AUTHPLAIN\r\n").is_empty());
    }

    #[test]
    fn replies_are_bounded() {
        let long = format!("250 {}\r\n", "x".repeat(MAX_REPLY_LINE * 4));
        let err = read_reply(&mut long.as_bytes(), 250, "EHLO x").unwrap_err();
        assert!(err.contains("exceeds"), "{err}");
        let endless = "250-more\r\n".repeat(MAX_REPLY_LINES * 4);
        let err = read_reply(&mut endless.as_bytes(), 250, "EHLO x").unwrap_err();
        assert!(err.contains("lines"), "{err}");
        let ok = "250-a\r\n250 b\r\n";
        assert_eq!(read_reply(&mut ok.as_bytes(), 250, "EHLO x").unwrap(), ok);
    }

    #[test]
    fn auth_plain_is_sent_when_advertised() {
        let (addr, server) = fake_server(true);
        let email = Email::new()
            .from("a@b.co")
            .to("t@b.co")
            .subject("s")
            .text("x");
        let rendered = email.render("m@t", 0);
        Smtp::new(&addr)
            .credentials("user", "pass")
            .send(&email, &rendered, "m@t")
            .unwrap();
        let log = server.join().unwrap().join("\n");
        let expected = base64_encode(b"\0user\0pass");
        assert!(log.contains(&format!("AUTH PLAIN {expected}")));
    }
}
