//! **Nice emails by default**: a [`Theme`] (colors, logo, footer — all
//! configurable) plus the Laravel-`MailMessage`-style fluent builder
//! ([`MailMessage`]): greeting / lines / an action button / muted notes.
//! One builder produces **both** the themed HTML (email-safe: tables +
//! inline styles, 600px card) and a matching plain-text part, so every
//! message is `multipart/alternative` without extra work.
//!
//! ```
//! use sutegi_mail::theme::Theme;
//!
//! let theme = Theme::new("MyApp").brand_color("#7c3aed");
//! let email = theme
//!     .message()
//!     .subject("Welcome!")
//!     .greeting("Hi Vera,")
//!     .line("Thanks for signing up — one more step:")
//!     .action("Confirm your email", "https://app.test/verify?token=abc")
//!     .note("This link is valid for 24 hours.")
//!     .email()
//!     .unwrap()
//!     .to("vera@example.com");
//! # let _ = email;
//! ```
//!
//! The HTML is rendered through [`sutegi_template`], and the layout is
//! swappable: [`Theme::layout`] replaces the outer chrome while the block
//! templates keep working (see [`LAYOUT_TEMPLATE`] for the context shape).

use crate::message::Email;
use sutegi_json::Json;
use sutegi_template::Templates;

/// The default outer layout. Replace it with [`Theme::layout`]; the context
/// exposes `app_name`, `logo_url`, `footer`, the color/font fields under
/// `theme.*`, and the rendered blocks as `{!! content !!}`.
pub const LAYOUT_TEMPLATE: &str = r#"<!DOCTYPE html>
<html>
<body style="margin:0;padding:0;background:{{ theme.background }};">
<table role="presentation" width="100%" cellpadding="0" cellspacing="0" style="background:{{ theme.background }};padding:32px 12px;">
<tr><td align="center">
<table role="presentation" width="600" cellpadding="0" cellspacing="0" style="max-width:600px;width:100%;">
<tr><td style="padding:0 8px 20px;text-align:center;">
@if(logo_url)<img src="{{ logo_url }}" alt="{{ app_name }}" height="40" style="height:40px;border:0;">@else<span style="font:600 20px {{ theme.font }};color:{{ theme.text_color }};letter-spacing:-0.02em;">{{ app_name }}</span>@endif
</td></tr>
<tr><td style="background:{{ theme.card_background }};border-radius:12px;border-top:4px solid {{ theme.brand_color }};padding:36px 40px;font:15px/1.6 {{ theme.font }};color:{{ theme.text_color }};">
{!! content !!}
</td></tr>
<tr><td style="padding:20px 8px;text-align:center;font:12px/1.6 {{ theme.font }};color:{{ theme.muted_color }};">
{{ footer }}
</td></tr>
</table>
</td></tr>
</table>
</body>
</html>"#;

const BLOCKS_TEMPLATE: &str = r#"@foreach(blocks as b)@if(b.greeting)<p style="margin:0 0 16px;font-size:17px;font-weight:600;">{{ b.greeting }}</p>@endif@if(b.line)<p style="margin:0 0 16px;">{{ b.line }}</p>@endif@if(b.action)<table role="presentation" cellpadding="0" cellspacing="0" style="margin:24px auto;"><tr><td style="border-radius:8px;background:{{ theme.brand_color }};"><a href="{{ b.action.url }}" style="display:inline-block;padding:12px 28px;font:600 15px {{ theme.font }};color:#ffffff;text-decoration:none;border-radius:8px;">{{ b.action.label }}</a></td></tr></table>@endif@if(b.note)<p style="margin:0 0 12px;font-size:13px;color:{{ theme.muted_color }};">{{ b.note }}</p>@endif@endforeach@if(action_url)<p style="margin:20px 0 0;font-size:12px;color:{{ theme.muted_color }};word-break:break-all;">If the button doesn't work, copy this link into your browser:<br><a href="{{ action_url }}" style="color:{{ theme.brand_color }};">{{ action_url }}</a></p>@endif"#;

/// Look and identity for themed mail. Every field has a builder; defaults
/// are a clean, email-client-safe card on a light background.
#[derive(Clone, Debug)]
pub struct Theme {
    pub app_name: String,
    pub brand_color: String,
    pub background: String,
    pub card_background: String,
    pub text_color: String,
    pub muted_color: String,
    pub font: String,
    pub logo_url: Option<String>,
    pub footer: Option<String>,
    layout: Option<String>,
}

impl Theme {
    pub fn new(app_name: &str) -> Theme {
        Theme {
            app_name: app_name.to_string(),
            brand_color: "#ea580c".to_string(), // forge ember
            background: "#f4f4f5".to_string(),
            card_background: "#ffffff".to_string(),
            text_color: "#27272a".to_string(),
            muted_color: "#71717a".to_string(),
            font: "-apple-system,BlinkMacSystemFont,'Segoe UI',Roboto,Helvetica,Arial,sans-serif"
                .to_string(),
            logo_url: None,
            footer: None,
            layout: None,
        }
    }

    pub fn brand_color(mut self, hex: &str) -> Theme {
        self.brand_color = hex.to_string();
        self
    }

    pub fn background(mut self, hex: &str) -> Theme {
        self.background = hex.to_string();
        self
    }

    pub fn text_colors(mut self, text: &str, muted: &str) -> Theme {
        self.text_color = text.to_string();
        self.muted_color = muted.to_string();
        self
    }

    pub fn font(mut self, css_font_family: &str) -> Theme {
        self.font = css_font_family.to_string();
        self
    }

    /// Show a logo image in the header instead of the app name.
    pub fn logo_url(mut self, url: &str) -> Theme {
        self.logo_url = Some(url.to_string());
        self
    }

    /// Footer line (default: `© {app_name}. All rights reserved.`... minus
    /// the pretension: `{app_name} — this is an automated message.`).
    pub fn footer(mut self, text: &str) -> Theme {
        self.footer = Some(text.to_string());
        self
    }

    /// Replace the outer HTML layout entirely (a [`sutegi_template`] source;
    /// see [`LAYOUT_TEMPLATE`] for the available context).
    pub fn layout(mut self, template_src: &str) -> Theme {
        self.layout = Some(template_src.to_string());
        self
    }

    /// Start a themed message.
    pub fn message(&self) -> MailMessage {
        MailMessage {
            theme: self.clone(),
            subject: String::new(),
            blocks: Vec::new(),
        }
    }

    fn footer_text(&self) -> String {
        self.footer
            .clone()
            .unwrap_or_else(|| format!("{} — this is an automated message.", self.app_name))
    }

    fn ctx_fields(&self) -> Json {
        Json::obj(vec![
            ("brand_color", Json::str(self.brand_color.clone())),
            ("background", Json::str(self.background.clone())),
            ("card_background", Json::str(self.card_background.clone())),
            ("text_color", Json::str(self.text_color.clone())),
            ("muted_color", Json::str(self.muted_color.clone())),
            ("font", Json::str(self.font.clone())),
        ])
    }
}

enum Block {
    Greeting(String),
    Line(String),
    Action { label: String, url: String },
    Note(String),
}

/// A fluent, themed message: blocks render in the order you add them, into
/// both HTML and plain text.
pub struct MailMessage {
    theme: Theme,
    subject: String,
    blocks: Vec<Block>,
}

impl MailMessage {
    pub fn subject(mut self, s: &str) -> MailMessage {
        self.subject = s.to_string();
        self
    }

    /// A bold opening line ("Hi Vera,").
    pub fn greeting(mut self, s: &str) -> MailMessage {
        self.blocks.push(Block::Greeting(s.to_string()));
        self
    }

    /// A paragraph.
    pub fn line(mut self, s: &str) -> MailMessage {
        self.blocks.push(Block::Line(s.to_string()));
        self
    }

    /// The call-to-action button. The URL is also repeated as a plain link
    /// under the content (button-less clients, text part).
    pub fn action(mut self, label: &str, url: &str) -> MailMessage {
        self.blocks.push(Block::Action {
            label: label.to_string(),
            url: url.to_string(),
        });
        self
    }

    /// A small muted paragraph (validity windows, "ignore this" notes).
    pub fn note(mut self, s: &str) -> MailMessage {
        self.blocks.push(Block::Note(s.to_string()));
        self
    }

    /// Render to `(subject, text, html)`.
    pub fn render(&self) -> Result<(String, String, String), String> {
        Ok((
            self.subject.clone(),
            self.render_text(),
            self.render_html()?,
        ))
    }

    /// Render into a ready [`Email`] (subject + text + html set); add
    /// recipients and hand it to the mailer.
    pub fn email(self) -> Result<Email, String> {
        let (subject, text, html) = self.render()?;
        Ok(Email::new().subject(&subject).text(&text).html(&html))
    }

    fn render_html(&self) -> Result<String, String> {
        let mut templates = Templates::new();
        templates.add("blocks", BLOCKS_TEMPLATE)?;
        templates.add(
            "layout",
            self.theme.layout.as_deref().unwrap_or(LAYOUT_TEMPLATE),
        )?;

        let blocks: Vec<Json> = self
            .blocks
            .iter()
            .map(|b| match b {
                Block::Greeting(s) => Json::obj(vec![("greeting", Json::str(s.clone()))]),
                Block::Line(s) => Json::obj(vec![("line", Json::str(s.clone()))]),
                Block::Action { label, url } => Json::obj(vec![(
                    "action",
                    Json::obj(vec![
                        ("label", Json::str(label.clone())),
                        ("url", Json::str(url.clone())),
                    ]),
                )]),
                Block::Note(s) => Json::obj(vec![("note", Json::str(s.clone()))]),
            })
            .collect();
        let action_url = self.blocks.iter().find_map(|b| match b {
            Block::Action { url, .. } => Some(url.clone()),
            _ => None,
        });

        let block_ctx = Json::obj(vec![
            ("blocks", Json::arr(blocks)),
            (
                "action_url",
                action_url.map(Json::str).unwrap_or(Json::Null),
            ),
            ("theme", self.theme.ctx_fields()),
        ]);
        let content = templates.render("blocks", &block_ctx)?;

        templates.render(
            "layout",
            &Json::obj(vec![
                ("app_name", Json::str(self.theme.app_name.clone())),
                (
                    "logo_url",
                    self.theme
                        .logo_url
                        .clone()
                        .map(Json::str)
                        .unwrap_or(Json::Null),
                ),
                ("footer", Json::str(self.theme.footer_text())),
                ("content", Json::str(content)),
                ("theme", self.theme.ctx_fields()),
            ]),
        )
    }

    fn render_text(&self) -> String {
        let mut out = String::new();
        for b in &self.blocks {
            match b {
                Block::Greeting(s) | Block::Line(s) | Block::Note(s) => {
                    out.push_str(s);
                    out.push_str("\n\n");
                }
                Block::Action { label, url } => {
                    out.push_str(&format!("{label}:\n{url}\n\n"));
                }
            }
        }
        out.push_str(&self.theme.footer_text());
        out.push('\n');
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message() -> MailMessage {
        Theme::new("TestApp")
            .message()
            .subject("Verify")
            .greeting("Hi Vera,")
            .line("One more step:")
            .action("Verify email", "https://app.test/verify?token=a&b=c")
            .note("Valid for 24 hours.")
    }

    #[test]
    fn renders_both_parts_with_action() {
        let (subject, text, html) = message().render().unwrap();
        assert_eq!(subject, "Verify");
        // Text: everything present, URL on its own line.
        assert!(text.contains("Hi Vera,"));
        assert!(text.contains("Verify email:\nhttps://app.test/verify?token=a&b=c"));
        assert!(text.contains("Valid for 24 hours."));
        assert!(text.contains("TestApp — this is an automated message."));
        // HTML: button href escaped, fallback link, default chrome.
        assert!(html.contains("href=\"https://app.test/verify?token=a&amp;b=c\""));
        assert!(html.contains(">Verify email</a>"));
        assert!(html.contains("copy this link"));
        assert!(html.contains("#ea580c")); // default brand
        assert!(html.contains("border-top:4px solid #ea580c"));
        assert!(html.contains(">TestApp</span>")); // header falls back to app name
    }

    #[test]
    fn theme_overrides_apply() {
        let theme = Theme::new("MyApp")
            .brand_color("#7c3aed")
            .background("#0b0b0e")
            .logo_url("https://cdn.test/logo.png")
            .footer("MyApp Inc · Bilbao");
        let (_, text, html) = theme.message().subject("s").line("x").render().unwrap();
        assert!(html.contains("#7c3aed"));
        assert!(!html.contains("#ea580c"));
        assert!(html.contains("background:#0b0b0e"));
        assert!(html.contains("src=\"https://cdn.test/logo.png\""));
        assert!(html.contains("MyApp Inc · Bilbao"));
        assert!(text.contains("MyApp Inc · Bilbao"));
    }

    #[test]
    fn custom_layout_replaces_chrome() {
        let theme = Theme::new("Min").layout("<div>{!! content !!}</div><i>{{ footer }}</i>");
        let (_, _, html) = theme.message().subject("s").line("hello").render().unwrap();
        assert!(html.starts_with("<div>"));
        assert!(html.contains("hello"));
        assert!(!html.contains("<!DOCTYPE html>"));
    }

    #[test]
    fn content_is_escaped() {
        let (_, _, html) = Theme::new("X <script>")
            .message()
            .subject("s")
            .line("<img onerror=alert(1)>")
            .render()
            .unwrap();
        assert!(!html.contains("<img onerror"));
        assert!(html.contains("&lt;img onerror"));
        assert!(html.contains("X &lt;script&gt;"));
    }

    #[test]
    fn email_builder_yields_multipart_message() {
        let email = message().email().unwrap().to("v@a.co").from("app@a.co");
        assert!(email.validate().is_ok());
        let rendered = email.render("id@t", 0);
        assert!(rendered.contains("multipart/alternative"));
    }

    #[test]
    fn no_action_means_no_fallback_link() {
        let (_, _, html) = Theme::new("X")
            .message()
            .subject("s")
            .line("just text")
            .render()
            .unwrap();
        assert!(!html.contains("copy this link"));
    }
}
