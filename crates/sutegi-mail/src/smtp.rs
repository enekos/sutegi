//! A blocking, pure-`std` **SMTP client** over `std::net::TcpStream` —
//! EHLO, optional `AUTH PLAIN`/`LOGIN`, `MAIL FROM`/`RCPT TO`/`DATA` with
//! dot-stuffing. No TLS (the same stance as the Postgres driver): point it at
//! an in-cluster relay, a host-local Postfix, or a dev catcher like Mailpit /
//! MailHog. For TLS-only providers, terminate at a local relay or implement
//! [`crate::Transport`] over your own HTTP client.

use crate::message::Email;
use crate::Transport;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::time::Duration;
use sutegi_crypto::base64_encode;

/// SMTP transport config. `Smtp::new("localhost:1025")` is a working Mailpit
/// dev setup; add `.credentials(...)` for relays requiring auth.
#[derive(Clone, Debug)]
pub struct Smtp {
    addr: String,
    helo: String,
    user: Option<String>,
    pass: Option<String>,
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
            timeout: Duration::from_secs(30),
        }
    }

    /// The hostname announced in `EHLO`.
    pub fn hello_as(mut self, host: &str) -> Smtp {
        self.helo = host.to_string();
        self
    }

    /// `AUTH PLAIN` credentials (falls back to `AUTH LOGIN` if the server
    /// only advertises that).
    pub fn credentials(mut self, user: &str, pass: &str) -> Smtp {
        self.user = Some(user.to_string());
        self.pass = Some(pass.to_string());
        self
    }

    pub fn timeout(mut self, t: Duration) -> Smtp {
        self.timeout = t;
        self
    }
}

impl Transport for Smtp {
    fn send(&self, email: &Email, rendered: &str, message_id: &str) -> Result<String, String> {
        let stream = TcpStream::connect(&self.addr)
            .map_err(|e| format!("smtp connect {}: {e}", self.addr))?;
        stream.set_read_timeout(Some(self.timeout)).ok();
        stream.set_write_timeout(Some(self.timeout)).ok();
        let mut conn = Conn {
            reader: BufReader::new(stream.try_clone().map_err(|e| e.to_string())?),
            stream,
        };

        conn.expect(220)?; // greeting
        let ehlo = conn.command(&format!("EHLO {}", self.helo), 250)?;

        if let (Some(user), Some(pass)) = (&self.user, &self.pass) {
            if ehlo.contains("AUTH") && ehlo.contains("PLAIN") {
                let ident = base64_encode(format!("\0{user}\0{pass}").as_bytes());
                conn.command(&format!("AUTH PLAIN {ident}"), 235)?;
            } else {
                conn.command("AUTH LOGIN", 334)?;
                conn.command(&base64_encode(user.as_bytes()), 334)?;
                conn.command(&base64_encode(pass.as_bytes()), 235)?;
            }
        }

        let from = email
            .from
            .as_ref()
            .ok_or("email has no From address")?
            .email
            .clone();
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

    /// Read a (possibly multi-line `250-…`) reply and check its code.
    fn read_reply(&mut self, expect: u16, after: &str) -> Result<String, String> {
        let mut all = String::new();
        loop {
            let mut line = String::new();
            self.reader
                .read_line(&mut line)
                .map_err(|e| format!("smtp read: {e}"))?;
            if line.is_empty() {
                return Err(format!("smtp: connection closed after {after}"));
            }
            all.push_str(&line);
            if line.len() < 4 || line.as_bytes()[3] != b'-' {
                let code: u16 = line.get(..3).and_then(|c| c.parse().ok()).unwrap_or(0);
                // Credential-bearing commands (AUTH PLAIN blob, bare base64
                // lines of AUTH LOGIN) never appear in error messages.
                let secretish =
                    after.starts_with("AUTH") || (!after.contains(' ') && after.len() > 20);
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
    }
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
