//! The auth mail flows (feature `mail`): **email verification** and
//! **password reset**, Laravel-defaults style — sensible templates, signed
//! expiring links, no state tables.
//!
//! - Verification links live 24 h and flip `users.verified_at` on confirm.
//! - Reset links live 1 h and are **bound to the current password hash**, so
//!   the moment the password changes (by this reset or any other means) every
//!   outstanding reset link dies. Stateless single-use, in effect.
//! - [`AuthMail::send_password_reset`] is enumeration-safe: unknown emails
//!   return `Ok` and send nothing.
//!
//! ```ignore
//! let mail = AuthMail::new(mailer, b"link-secret", "https://app.example.com", "MyApp");
//! mail.send_verification(&user)?;                       // on register
//! mail.confirm_email(&auth.users, &token)?;             // GET /verify-email
//! mail.send_password_reset(&auth.users, &email)?;       // POST /forgot-password
//! mail.reset_password(&auth.users, &token, &new_pw)?;   // POST /reset-password
//! ```

use crate::links::Links;
use crate::users::{now_secs, User, Users};
use std::sync::Arc;
use sutegi_crypto::{hex, sha256};
use sutegi_mail::{Email, Mailer};
use sutegi_orm::Backend;

const VERIFY_PURPOSE: &str = "verify-email";
const RESET_PURPOSE: &str = "reset-password";

/// The auth mail glue: a [`Mailer`], a link signer, and your app's URLs.
pub struct AuthMail {
    mailer: Arc<Mailer>,
    links: Links,
    base_url: String,
    app_name: String,
    verify_ttl: i64,
    reset_ttl: i64,
    verify_path: String,
    reset_path: String,
}

impl AuthMail {
    /// `base_url` is your app's public origin (`https://app.example.com`);
    /// links are minted under it. `secret` signs the links — long, random,
    /// configured (rotating it invalidates outstanding links).
    pub fn new(mailer: Arc<Mailer>, secret: &[u8], base_url: &str, app_name: &str) -> AuthMail {
        AuthMail {
            mailer,
            links: Links::new(secret),
            base_url: base_url.trim_end_matches('/').to_string(),
            app_name: app_name.to_string(),
            verify_ttl: 86_400,
            reset_ttl: 3_600,
            verify_path: "/verify-email".to_string(),
            reset_path: "/reset-password".to_string(),
        }
    }

    /// Verification link lifetime (default 24 h).
    pub fn verify_ttl(mut self, secs: i64) -> AuthMail {
        self.verify_ttl = secs;
        self
    }

    /// Reset link lifetime (default 1 h).
    pub fn reset_ttl(mut self, secs: i64) -> AuthMail {
        self.reset_ttl = secs;
        self
    }

    /// The paths the links point at (defaults `/verify-email`,
    /// `/reset-password`); the token rides in `?token=`.
    pub fn paths(mut self, verify: &str, reset: &str) -> AuthMail {
        self.verify_path = verify.to_string();
        self.reset_path = reset.to_string();
        self
    }

    // --- email verification ---

    /// Mail `user` a verification link. Returns the link (for logs/tests).
    pub fn send_verification(&self, user: &User) -> Result<String, String> {
        let token = self
            .links
            .mint(VERIFY_PURPOSE, user.id, now_secs() + self.verify_ttl, "");
        let url = format!("{}{}?token={token}", self.base_url, self.verify_path);
        let hours = self.verify_ttl / 3_600;
        self.mailer.send(
            Email::new()
                .to(&user.email)
                .subject(&format!("Verify your email — {}", self.app_name))
                .text(&format!(
                    "Hi{},\n\nConfirm your email address for {} by opening this link:\n\n{url}\n\nThe link is valid for {hours} hour(s). If you didn't create this account, ignore this email.\n",
                    greeting(user),
                    self.app_name,
                ))
                .html(&format!(
                    "<p>Hi{},</p><p>Confirm your email address for <b>{}</b>:</p><p><a href=\"{url}\">Verify email address</a></p><p>The link is valid for {hours} hour(s). If you didn't create this account, ignore this email.</p>",
                    greeting(user),
                    html_escape(&self.app_name),
                )),
        )?;
        Ok(url)
    }

    /// Confirm a verification token: marks the user verified and returns it.
    /// `Ok(None)` for invalid/expired tokens.
    pub fn confirm_email<B: Backend>(
        &self,
        users: &Users<B>,
        token: &str,
    ) -> Result<Option<User>, String> {
        let Some((uid, _)) = self.links.verify(VERIFY_PURPOSE, token, now_secs()) else {
            return Ok(None);
        };
        match users.find(uid)? {
            Some(_) => {
                users.mark_verified(uid)?;
                users.find(uid)
            }
            None => Ok(None),
        }
    }

    // --- password reset ---

    /// Mail a reset link to `email` if an account exists. Always `Ok` on
    /// unknown addresses (no account enumeration). Returns the link when one
    /// was sent.
    pub fn send_password_reset<B: Backend>(
        &self,
        users: &Users<B>,
        email: &str,
    ) -> Result<Option<String>, String> {
        let Some(user) = users.find_by_email(email).unwrap_or(None) else {
            return Ok(None);
        };
        let Some(bind) = self.hash_bind(users, user.id)? else {
            return Ok(None);
        };
        let token = self
            .links
            .mint(RESET_PURPOSE, user.id, now_secs() + self.reset_ttl, &bind);
        let url = format!("{}{}?token={token}", self.base_url, self.reset_path);
        let minutes = self.reset_ttl / 60;
        self.mailer.send(
            Email::new()
                .to(&user.email)
                .subject(&format!("Reset your password — {}", self.app_name))
                .text(&format!(
                    "Hi{},\n\nReset your {} password by opening this link:\n\n{url}\n\nThe link is valid for {minutes} minute(s) and stops working once your password changes. If you didn't ask for this, ignore this email.\n",
                    greeting(&user),
                    self.app_name,
                ))
                .html(&format!(
                    "<p>Hi{},</p><p>Reset your <b>{}</b> password:</p><p><a href=\"{url}\">Reset password</a></p><p>The link is valid for {minutes} minute(s) and stops working once your password changes. If you didn't ask for this, ignore this email.</p>",
                    greeting(&user),
                    html_escape(&self.app_name),
                )),
        )?;
        Ok(Some(url))
    }

    /// Apply a reset token: sets the new password and returns the user.
    /// `Ok(None)` for invalid, expired, or already-consumed tokens (the
    /// hash binding no longer matches once the password changed).
    pub fn reset_password<B: Backend>(
        &self,
        users: &Users<B>,
        token: &str,
        new_password: &str,
    ) -> Result<Option<User>, String> {
        let Some((uid, bind)) = self.links.verify(RESET_PURPOSE, token, now_secs()) else {
            return Ok(None);
        };
        if self.hash_bind(users, uid)? != Some(bind) {
            return Ok(None); // password changed since the link was minted
        }
        users.set_password(uid, new_password)?;
        users.find(uid)
    }

    /// A short fingerprint of the user's current password hash.
    fn hash_bind<B: Backend>(&self, users: &Users<B>, uid: i64) -> Result<Option<String>, String> {
        Ok(users
            .password_hash_of(uid)?
            .map(|h| hex(&sha256(h.as_bytes()))[..16].to_string()))
    }
}

fn greeting(user: &User) -> String {
    if user.name.is_empty() {
        String::new()
    } else {
        format!(" {}", user.name)
    }
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use super::*;
    use sutegi_mail::transports::Memory;
    use sutegi_orm::db::Db;

    fn rig() -> (Users<Db>, AuthMail, Memory) {
        let users = Users::new(Db::memory().unwrap()).iterations(1_000);
        users.migrate().unwrap();
        let mem = Memory::new();
        let mailer = Arc::new(Mailer::new(mem.clone(), "App <app@example.com>").unwrap());
        let mail = AuthMail::new(mailer, b"link-secret", "https://app.test/", "TestApp");
        (users, mail, mem)
    }

    fn token_of(url: &str) -> String {
        url.split_once("token=").unwrap().1.to_string()
    }

    #[test]
    fn verification_flow_end_to_end() {
        let (users, mail, mem) = rig();
        let user = users
            .register_with("v@a.co", "password1", "Vera", "user")
            .unwrap();
        assert!(!user.is_verified());

        let url = mail.send_verification(&user).unwrap();
        assert!(url.starts_with("https://app.test/verify-email?token="));
        assert_eq!(mem.count(), 1);
        let (sent, rendered) = &mem.sent()[0];
        assert_eq!(sent.to[0].email, "v@a.co");
        assert!(sent.subject.contains("TestApp"));
        assert!(rendered.contains("Message-ID"));

        let confirmed = mail
            .confirm_email(&users, &token_of(&url))
            .unwrap()
            .unwrap();
        assert!(confirmed.is_verified());
        // Idempotent, and garbage tokens are just None.
        assert!(mail
            .confirm_email(&users, &token_of(&url))
            .unwrap()
            .is_some());
        assert!(mail.confirm_email(&users, "junk").unwrap().is_none());
    }

    #[test]
    fn reset_flow_binds_to_password() {
        let (users, mail, mem) = rig();
        users.register("r@a.co", "oldpassword").unwrap();

        let url = mail.send_password_reset(&users, "R@a.co").unwrap().unwrap();
        assert_eq!(mem.count(), 1);
        let token = token_of(&url);

        let reset = mail.reset_password(&users, &token, "newpassword1").unwrap();
        assert!(reset.is_some());
        assert!(users
            .authenticate("r@a.co", "newpassword1")
            .unwrap()
            .is_some());
        assert!(users
            .authenticate("r@a.co", "oldpassword")
            .unwrap()
            .is_none());

        // The same link is dead now — the hash binding changed.
        assert!(mail
            .reset_password(&users, &token, "another-pass1")
            .unwrap()
            .is_none());
        assert!(users
            .authenticate("r@a.co", "newpassword1")
            .unwrap()
            .is_some());
    }

    #[test]
    fn reset_for_unknown_email_sends_nothing() {
        let (users, mail, mem) = rig();
        assert_eq!(
            mail.send_password_reset(&users, "ghost@a.co").unwrap(),
            None
        );
        assert_eq!(mem.count(), 0);
    }

    #[test]
    fn expired_links_are_rejected() {
        let (users, mail, _mem) = rig();
        let user = users.register("e@a.co", "password1").unwrap();
        let mail = AuthMail {
            verify_ttl: -10, // already expired at mint time
            ..mail
        };
        let url = mail.send_verification(&user).unwrap();
        assert!(mail
            .confirm_email(&users, &token_of(&url))
            .unwrap()
            .is_none());
        assert!(!users.find(user.id).unwrap().unwrap().is_verified());
    }

    #[test]
    fn templates_are_multipart_with_link() {
        let (users, mail, mem) = rig();
        let user = users
            .register_with("t@a.co", "password1", "Toni", "user")
            .unwrap();
        let url = mail.send_verification(&user).unwrap();
        let (email, _) = &mem.sent()[0];
        assert!(email.text.as_ref().unwrap().contains(&url));
        assert!(email.html.as_ref().unwrap().contains(&url));
        assert!(email.text.as_ref().unwrap().contains("Hi Toni"));
    }
}
