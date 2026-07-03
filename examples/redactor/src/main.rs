//! **redactor** — the first real *consumer* of sutegi: a mairu-adjacent PII
//! redaction service, built to exercise the whole agent contract end to end
//! (JSON tools + introspection + SSE streaming), backed by a real SQLite audit
//! log and gated by a bearer token — the shape an agent actually deploys.
//!
//! It reuses mairu's "pii-redact" idea (mask emails / IPs / phone-like digit
//! runs in free text) as a small, self-contained, zero-dep engine.
//!
//! ```text
//! export REDACTOR_TOKEN=dev-token          # or read the one printed at startup
//! curl localhost:8080/__introspect         # the agent contract, self-described
//! curl localhost:8080/__tools              # tool manifest
//! # agent tools (open, framework-mounted):
//! curl -X POST localhost:8080/__tools/redact -d '{"text":"mail a@b.com from 10.0.0.1"}'
//! curl -X POST localhost:8080/__tools/detect -d '{"text":"call 555-123-4567"}'
//! curl -N  -X POST localhost:8080/__tools/redact_stream/stream -d '{"text":"line a@b.com\nline 2"}'
//! # token-gated HTTP API + audit log:
//! curl -H "Authorization: Bearer dev-token" -X POST localhost:8080/api/redact -d '{"text":"a@b.com"}'
//! curl -H "Authorization: Bearer dev-token" localhost:8080/api/log
//! ```

use sutegi::prelude::*;

// ---------------------------------------------------------------------------
// The redaction engine — pure, zero-dep, unit-tested below.
// ---------------------------------------------------------------------------
mod redact {
    use std::collections::BTreeMap;

    /// A redaction result: the masked text plus a per-kind match count.
    pub struct Redacted {
        pub text: String,
        pub counts: BTreeMap<String, u32>,
    }

    fn is_edge_punct(c: char) -> bool {
        matches!(
            c,
            '.' | ',' | ';' | ':' | '(' | ')' | '<' | '>' | '"' | '\'' | '!' | '?' | '[' | ']'
        )
    }

    fn is_email(s: &str) -> bool {
        let parts: Vec<&str> = s.split('@').collect();
        parts.len() == 2
            && !parts[0].is_empty()
            && parts[1].len() >= 3
            && parts[1].contains('.')
            && !parts[1].starts_with('.')
            && !parts[1].ends_with('.')
    }

    fn is_ipv4(s: &str) -> bool {
        let octets: Vec<&str> = s.split('.').collect();
        octets.len() == 4 && octets.iter().all(|o| o.parse::<u8>().is_ok())
    }

    fn is_phone(s: &str) -> bool {
        let digits = s.chars().filter(|c| c.is_ascii_digit()).count();
        digits >= 7
            && s.chars()
                .all(|c| c.is_ascii_digit() || matches!(c, '+' | '-' | '(' | ')'))
    }

    /// Classify the punctuation-trimmed core of a token as a kind of PII.
    fn classify(core: &str) -> Option<&'static str> {
        if is_email(core) {
            Some("EMAIL")
        } else if is_ipv4(core) {
            Some("IP")
        } else if is_phone(core) {
            Some("PHONE")
        } else {
            None
        }
    }

    /// Mask every PII token in `input`, preserving all whitespace and any
    /// surrounding punctuation, and count matches per kind.
    pub fn redact(input: &str) -> Redacted {
        let mut text = String::with_capacity(input.len());
        let mut counts: BTreeMap<String, u32> = BTreeMap::new();
        let mut word = String::new();

        let flush = |word: &mut String, text: &mut String, counts: &mut BTreeMap<String, u32>| {
            if word.is_empty() {
                return;
            }
            let lead: String = word.chars().take_while(|c| is_edge_punct(*c)).collect();
            let trail: String = word
                .chars()
                .rev()
                .take_while(|c| is_edge_punct(*c))
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect();
            let core = &word[lead.len()..word.len() - trail.len()];
            match (core.is_empty(), classify(core)) {
                (false, Some(kind)) => {
                    text.push_str(&lead);
                    text.push_str(&format!("[{kind}]"));
                    text.push_str(&trail);
                    *counts.entry(kind.to_string()).or_insert(0) += 1;
                }
                _ => text.push_str(word),
            }
            word.clear();
        };

        for ch in input.chars() {
            if ch.is_whitespace() {
                flush(&mut word, &mut text, &mut counts);
                text.push(ch);
            } else {
                word.push(ch);
            }
        }
        flush(&mut word, &mut text, &mut counts);

        Redacted { text, counts }
    }

    /// Total matches across all kinds.
    pub fn total(counts: &BTreeMap<String, u32>) -> u32 {
        counts.values().sum()
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn masks_each_kind() {
            let r = redact("mail me at alice@example.com now");
            assert!(r.text.contains("[EMAIL]"), "{}", r.text);
            assert_eq!(r.counts.get("EMAIL"), Some(&1));

            let r = redact("host 192.168.0.1 is down");
            assert_eq!(r.text, "host [IP] is down");

            let r = redact("call 555-123-4567 please");
            assert_eq!(r.text, "call [PHONE] please");
        }

        #[test]
        fn preserves_punctuation_and_whitespace() {
            let r = redact("(a@b.com).\nnext line");
            // surrounding parens/period kept, only the address masked
            assert_eq!(r.text, "([EMAIL]).\nnext line");
        }

        #[test]
        fn no_false_positives_on_plain_text() {
            let r = redact("the quick brown fox jumps over 3 lazy dogs");
            assert!(r.counts.is_empty(), "{:?}", r.counts);
            assert_eq!(r.text, "the quick brown fox jumps over 3 lazy dogs");
        }

        #[test]
        fn counts_multiple() {
            let r = redact("a@b.com c@d.com 10.0.0.1");
            assert_eq!(r.counts.get("EMAIL"), Some(&2));
            assert_eq!(r.counts.get("IP"), Some(&1));
            assert_eq!(total(&r.counts), 3);
        }
    }
}

// ---------------------------------------------------------------------------
// The audit log — a real model over SQLite, written on every redaction.
// ---------------------------------------------------------------------------

/// One recorded redaction call, for the audit trail.
#[derive(Model)]
#[model(table = "redactions")]
struct Redaction {
    #[model(primary)]
    id: i64,
    /// Which entry point ran it: "redact", "detect", or "stream".
    kind: String,
    /// Number of PII matches masked.
    matches: i64,
    /// Length of the input text in characters.
    chars: i64,
}

fn migrations() -> Migrator {
    Migrator::new().add(Migration::reversible(
        "20260703_000001",
        "create_redactions",
        |db| db.migrate_schema(&Redaction::schema()),
        |db| db.execute("DROP TABLE redactions", &[]).map(|_| ()),
    ))
}

/// Record a redaction and return the `{text?, counts, matches}` JSON payload.
fn record<B: Backend>(
    db: &B,
    kind: &str,
    input: &str,
    counts: &std::collections::BTreeMap<String, u32>,
) -> Result<(), Error> {
    Redaction {
        id: 0,
        kind: kind.to_string(),
        matches: redact::total(counts) as i64,
        chars: input.chars().count() as i64,
    }
    .save(db)?;
    Ok(())
}

fn counts_json(counts: &std::collections::BTreeMap<String, u32>) -> Json {
    Json::Obj(
        counts
            .iter()
            .map(|(k, v)| (k.clone(), Json::int(*v as i64)))
            .collect(),
    )
}

fn text_of(args: &Json) -> Result<String, Error> {
    args.get("text")
        .and_then(Json::as_str)
        .map(str::to_string)
        .ok_or_else(|| Error::bad_request("missing `text`"))
}

fn main() -> std::io::Result<()> {
    let db = Db::open_or_memory("REDACTOR_DB");
    if sutegi::migrate::dispatch(&migrations(), &db) {
        return Ok(());
    }
    migrations().run(&db).expect("migrate");

    let token = std::env::var("REDACTOR_TOKEN").unwrap_or_else(|_| "dev-token".to_string());
    eprintln!("[redactor] agent API bearer token: {token}");

    let ready = db.clone();
    App::new("redactor")
        .state(db)
        .readiness(move || ready.query("SELECT 1", &[]).is_ok())
        .get("/", "Health check.", |_| "redactor up")
        // --- token-gated HTTP API with the audit log ------------------------
        .group("/api", vec![mw(bearer(&token))], |g| {
            g.post(
                "/redact",
                "Redact PII in `text`; records the call in the audit log.",
                |c| -> Result<Json, Error> {
                    let text = text_of(&c.json()?)?;
                    let r = redact::redact(&text);
                    record(c.db::<Db>(), "redact", &text, &r.counts)?;
                    Ok(Json::obj(vec![
                        ("text", Json::str(r.text)),
                        ("counts", counts_json(&r.counts)),
                    ]))
                },
            )
            .get(
                "/log",
                "Recent redaction audit entries.",
                |c| -> Result<Json, Error> {
                    let rows = Redaction::all_typed(c.db::<Db>())?;
                    Ok(Json::arr(rows.iter().map(Redaction::to_json).collect()))
                },
            )
        })
        // --- agent tools: introspectable + JSON, the reason sutegi exists ----
        .tool(
            "redact",
            "Mask personally-identifiable information (emails, IPs, phone \
             numbers) in the given text. Returns the redacted text and per-kind \
             match counts.",
            schema::object(
                vec![("text", schema::string("the text to redact"))],
                &["text"],
            ),
            |c, args| {
                let text = text_of(&args)?;
                let r = redact::redact(&text);
                record(c.db::<Db>(), "redact", &text, &r.counts)?;
                Ok(Json::obj(vec![
                    ("text", Json::str(r.text)),
                    ("counts", counts_json(&r.counts)),
                ]))
            },
        )
        .tool(
            "detect",
            "Detect (but do not mask) PII in the text; returns per-kind match \
             counts only. Useful for an agent deciding whether to redact.",
            schema::object(
                vec![("text", schema::string("the text to scan"))],
                &["text"],
            ),
            |c, args| {
                let text = text_of(&args)?;
                let r = redact::redact(&text);
                record(c.db::<Db>(), "detect", &text, &r.counts)?;
                Ok(Json::obj(vec![
                    ("matches", Json::int(redact::total(&r.counts) as i64)),
                    ("counts", counts_json(&r.counts)),
                ]))
            },
        )
        .stream_tool(
            "redact_stream",
            "Redact PII and stream the result back one line at a time as \
             Server-Sent Events — the shape an agent consumes token-by-token.",
            schema::object(
                vec![("text", schema::string("the multi-line text to redact"))],
                &["text"],
            ),
            |_c, args, sink| {
                let text = args.get("text").and_then(Json::as_str).unwrap_or("");
                for line in text.split('\n') {
                    sink.data(&redact::redact(line).text)?;
                }
                sink.event("done", "{}")
            },
        )
        .serve()
}
