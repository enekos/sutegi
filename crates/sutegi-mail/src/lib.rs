//! Mail for sutegi — the Laravel `Mail` shape with zero third-party deps:
//! build an [`Email`], hand it to a [`Mailer`], and the configured
//! [`Transport`] moves it.
//!
//! Built-in transports:
//! - [`transports::Log`] — write to stderr/file; the **dev default**.
//! - [`transports::Memory`] — collect in memory for test assertions.
//! - [`smtp::Smtp`] — blocking pure-`std` SMTP (EHLO/AUTH/DATA). No TLS, the
//!   same stance as the Postgres driver: point it at an in-cluster relay, a
//!   host-local Postfix, or a dev catcher (Mailpit on `localhost:1025`).
//! - [`transports::Sendmail`] — pipe to the local MTA binary.
//!
//! **Third-party providers plug in with one method.** `Transport` receives
//! the parsed [`Email`] *and* its rendered RFC 2822 form, so an adapter for
//! Resend/SendGrid/Postmark/SES is just your HTTP client of choice posting
//! either shape:
//!
//! ```ignore
//! struct Resend { key: String }
//! impl sutegi_mail::Transport for Resend {
//!     fn send(&self, email: &Email, _raw: &str, id: &str) -> Result<String, String> {
//!         my_http_post("https://api.resend.com/emails", &self.key, &email_json(email))?;
//!         Ok(id.to_string())
//!     }
//! }
//! let mailer = Mailer::new(Resend { key }, "App <app@example.com>")?;
//! ```
//!
//! ```no_run
//! use sutegi_mail::{Email, Mailer, transports::Log};
//!
//! let mailer = Mailer::new(Log::new(), "App <app@example.com>").unwrap();
//! mailer.send(
//!     Email::new().to("you@example.com").subject("Hi").text("Hello!"),
//! ).unwrap();
//! ```

pub mod message;
pub mod smtp;
pub mod transports;

pub use message::{Address, Email};
pub use smtp::Smtp;

use std::sync::atomic::{AtomicU64, Ordering};

/// The provider seam: one method. `email` is the structured message (for
/// API-shaped providers), `rendered` its full RFC 2822 form (for wire-shaped
/// ones), `message_id` the id already stamped into the `Message-ID` header.
/// Return the provider's id for the message (or echo `message_id`).
pub trait Transport: Send + Sync {
    fn send(&self, email: &Email, rendered: &str, message_id: &str) -> Result<String, String>;
}

/// The front door: holds a [`Transport`] and the app's default `From`
/// address, renders messages, stamps `Message-ID`/`Date`, and sends.
/// `Send + Sync` — drop it in an `Arc` / `App::state`.
pub struct Mailer {
    transport: Box<dyn Transport>,
    from: Address,
    seq: AtomicU64,
}

impl Mailer {
    /// A mailer over `transport` with a default sender (`App <app@x.com>`).
    pub fn new(transport: impl Transport + 'static, from: &str) -> Result<Mailer, String> {
        Ok(Mailer {
            transport: Box::new(transport),
            from: Address::parse(from)?,
            seq: AtomicU64::new(0),
        })
    }

    /// Build from `MAIL_*` env vars — the 12-factor path:
    ///
    /// - `MAIL_FROM` (required): the default sender.
    /// - `MAIL_DRIVER`: `log` (default) | `smtp` | `sendmail`.
    /// - `MAIL_LOG_PATH`: file for the `log` driver (stderr if unset).
    /// - `MAIL_SMTP_ADDR` (`host:port`), `MAIL_SMTP_USER`, `MAIL_SMTP_PASS`.
    pub fn from_env() -> Result<Mailer, String> {
        let var = |k: &str| std::env::var(k).ok();
        let from = var("MAIL_FROM").ok_or("MAIL_FROM is not set")?;
        let driver = var("MAIL_DRIVER").unwrap_or_else(|| "log".to_string());
        match driver.as_str() {
            "log" => match var("MAIL_LOG_PATH") {
                Some(p) => Mailer::new(transports::Log::to_file(&p), &from),
                None => Mailer::new(transports::Log::new(), &from),
            },
            "smtp" => {
                let addr = var("MAIL_SMTP_ADDR").ok_or("MAIL_SMTP_ADDR is not set")?;
                let mut smtp = Smtp::new(&addr);
                if let (Some(u), Some(p)) = (var("MAIL_SMTP_USER"), var("MAIL_SMTP_PASS")) {
                    smtp = smtp.credentials(&u, &p);
                }
                Mailer::new(smtp, &from)
            }
            "sendmail" => Mailer::new(transports::Sendmail::new(), &from),
            other => Err(format!("unknown MAIL_DRIVER: {other}")),
        }
    }

    /// The default sender.
    pub fn default_from(&self) -> &Address {
        &self.from
    }

    /// Validate, render, and send. Fills the default `From` when the message
    /// has none. Returns the transport's message id.
    pub fn send(&self, email: Email) -> Result<String, String> {
        let mut email = email;
        if email.from.is_none() {
            email.from = Some(self.from.clone());
        }
        email.validate()?;

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let message_id = format!(
            "{now}.{}.{}@{}",
            self.seq.fetch_add(1, Ordering::Relaxed),
            std::process::id(),
            self.from.email.split('@').nth(1).unwrap_or("sutegi.local")
        );
        let rendered = email.render(&message_id, now);
        self.transport.send(&email, &rendered, &message_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use transports::Memory;

    #[test]
    fn mailer_fills_default_from_and_stamps_ids() {
        let mem = Memory::new();
        let mailer = Mailer::new(mem.clone(), "App <app@example.com>").unwrap();

        let id1 = mailer
            .send(Email::new().to("a@b.co").subject("one").text("x"))
            .unwrap();
        let id2 = mailer
            .send(Email::new().to("a@b.co").subject("two").text("y"))
            .unwrap();
        assert_ne!(id1, id2);
        assert!(id1.ends_with("@example.com"));

        let sent = mem.sent();
        assert_eq!(sent.len(), 2);
        assert_eq!(sent[0].0.from.as_ref().unwrap().email, "app@example.com");
        assert!(sent[0].1.contains("From: \"App\" <app@example.com>"));
        assert!(sent[0].1.contains(&format!("Message-ID: <{id1}>")));
    }

    #[test]
    fn explicit_from_wins() {
        let mem = Memory::new();
        let mailer = Mailer::new(mem.clone(), "app@example.com").unwrap();
        mailer
            .send(
                Email::new()
                    .from("other@example.com")
                    .to("a@b.co")
                    .subject("s")
                    .text("x"),
            )
            .unwrap();
        assert_eq!(
            mem.sent()[0].0.from.as_ref().unwrap().email,
            "other@example.com"
        );
    }

    #[test]
    fn invalid_mail_is_rejected_before_transport() {
        let mem = Memory::new();
        let mailer = Mailer::new(mem.clone(), "app@example.com").unwrap();
        assert!(mailer
            .send(Email::new().subject("no rcpt").text("x"))
            .is_err());
        assert!(mailer
            .send(Email::new().to("a@b.co").subject("no body"))
            .is_err());
        assert!(mailer
            .send(Email::new().to("garbage").subject("s").text("x"))
            .is_err());
        assert_eq!(mem.count(), 0);
    }

    #[test]
    fn bad_default_from_fails_construction() {
        assert!(Mailer::new(Memory::new(), "not-an-address").is_err());
    }
}
