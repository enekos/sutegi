<!-- Generated from landing/src/Docs.svelte. Run: cd landing && pnpm docs:export -->

[← Sessions & CSRF](sessions.md) · [Documentation index](../README.md) · [File storage →](storage.md)

# Mail

The `mail` feature gives you an `Email` builder with real RFC 2822/MIME rendering (multipart/alternative, encoded-words, header-injection folding) and a one-method `Transport` seam. Built-in drivers cover `log` (dev default — messages print, nothing escapes), `memory` (test assertions), a pure-std `smtp` client (EHLO, AUTH PLAIN/LOGIN, dot-stuffing) and `sendmail` (pipe to the local Postfix — the VPS shape); `Mailer::from_env()` picks one from `MAIL_*`.

```rust
// Configure once from the environment (MAIL_* vars pick the driver:
// log for dev, memory for tests, smtp/sendmail for real delivery).
let mailer = Mailer::from_env()?;

let email = Email::new()
    .to("ada@example.com")
    .subject("Welcome")
    .text("Thanks for signing up.")
    .html("<h1>Thanks for signing up.</h1>");   // both set -> multipart/alternative
mailer.send(email)?;

// Or a themed, notification-style message — HTML card + text from the same blocks:
let theme = Theme::new("Acme")
    .brand_color("#7c3aed")
    .logo_url("https://cdn.example.com/logo.png")
    .footer("Acme Inc · Bilbao");

mailer.send(theme.message()
    .subject("Welcome!")
    .greeting("Hi Ada,")
    .line("Your account is ready.")
    .action("Verify email", &url)               // brand-colored button + fallback link
    .note("This link is valid for 24 hours.")
    .email()?
    .to("ada@example.com"))?;

// A hosted provider is one method — Transport hands you the structured Email
// AND its rendered RFC 2822 form; post whichever your API wants:
struct Resend { key: String }
impl Transport for Resend {
    fn send(&self, email: &Email, _raw: &str, id: &str) -> Result<String, String> {
        my_http_post("https://api.resend.com/emails", &self.key, &to_json(email))?;
        Ok(id.to_string())
    }
}
```

`Theme` + `MailMessage` produce a clean, email-client-safe HTML card **and** a matching plain-text part from the same blocks, so every message is `multipart/alternative` for free. The outer chrome is a [template](templates.md) source — swap it wholesale with `Theme::layout(…)` while block rendering keeps working.

> [!NOTE]
> **The SMTP client has no TLS**
>
> Same stance as the Postgres driver: point it at an in-cluster relay or Mailpit on `localhost:1025`. For hosted delivery over the public internet, a Resend/SendGrid/Postmark/SES adapter is ~10 lines over any HTTP client — the same seam that lets sutegi dodge the TLS wall until it lands (see [the posture page](honesty.md)).

---

[← Sessions & CSRF](sessions.md) · [Documentation index](../README.md) · [File storage →](storage.md)
