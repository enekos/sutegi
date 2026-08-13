//! [`S3Storage`] — S3-compatible object storage that **implements
//! [`Storage`]**, so `put`/`get`/`stat`/`delete`/`list` call sites stop caring
//! whether the bytes live on a disk, in Postgres, or in a bucket.
//!
//! It moves the bytes itself over an injected
//! [`HttpTransport`](crate::transport::HttpTransport) — [`SystemCurl`] for
//! `https` (AWS, Cloudflare R2), [`PlainHttp`] for an in-cluster MinIO/Garage,
//! or your own client. No TLS stack, no third-party dependency.
//!
//! ```no_run
//! use sutegi_storage::{transport::SystemCurl, S3Store, Storage};
//!
//! // Cloudflare R2 (account id, bucket, and an R2 API token pair)
//! let store = S3Store::r2("acct123", "media", "ak", "sk").storage(SystemCurl::new());
//! store.put("avatars/42.png", b"\x89PNG\r\n", "image/png")?;
//! let bytes = store.get("avatars/42.png")?;              // Some(vec![…])
//! for obj in store.list("avatars/")? { println!("{}", obj.key); }
//!
//! // The same credentials still presign, for bytes that should bypass the app:
//! let url = store.presigner().presign_get("avatars/42.png", 900)?;
//! # Ok::<(), String>(())
//! ```
//!
//! **Security posture**, beyond the transport's own:
//! - Every request is SigV4-signed with the **real payload hash** — not
//!   `UNSIGNED-PAYLOAD` — so an altered body is refused by the store.
//! - Downloads and uploads are **verified against the `ETag`** when the store
//!   reports a plain MD5 one (single-part, no SSE-C/KMS): end-to-end integrity
//!   on top of the transport's. Multipart/encrypted ETags are skipped, not
//!   faked. Opt out with [`verify_etag`](S3Storage::verify_etag).
//! - Keys go through [`validate_key`], so a traversal-shaped key is rejected
//!   before it can be signed.
//! - [`list`](Storage::list) is bounded by
//!   [`max_list_keys`](S3Storage::max_list_keys) — a bucket with ten million
//!   objects returns an error, never a silently truncated page or an OOM.

use crate::s3::{civil_from_days, days_from_civil, encode_query, now_secs};
use crate::transport::{HttpRequest, HttpResponse, HttpTransport};
use crate::{content_type_of, validate_key, ObjectMeta, S3Store, Storage};
use sutegi_crypto::{hex, md5};

/// Objects requested per `ListObjectsV2` page (the S3 maximum).
const PAGE_SIZE: usize = 1000;

/// An S3-compatible bucket as a [`Storage`] backend.
///
/// Cheap to clone when the transport is; `Send + Sync`, so it drops straight
/// into `App::state`. Build one with [`S3Store::storage`].
#[derive(Clone, Debug)]
pub struct S3Storage<T: HttpTransport> {
    s3: S3Store,
    transport: T,
    verify_etag: bool,
    max_list_keys: usize,
}

impl<T: HttpTransport> S3Storage<T> {
    /// Wrap credentials + transport. Prefer [`S3Store::storage`].
    pub fn new(s3: S3Store, transport: T) -> S3Storage<T> {
        S3Storage {
            s3,
            transport,
            verify_etag: true,
            max_list_keys: 100_000,
        }
    }

    /// Turn MD5 `ETag` verification on uploads and downloads off. On by
    /// default; the only reason to disable it is a store that reports
    /// non-MD5 ETags it does not label as such.
    pub fn verify_etag(mut self, on: bool) -> S3Storage<T> {
        self.verify_etag = on;
        self
    }

    /// Cap on how many keys one [`list`](Storage::list) may accumulate
    /// (default 100 000). Exceeding it is an error, not a truncation.
    pub fn max_list_keys(mut self, n: usize) -> S3Storage<T> {
        self.max_list_keys = n;
        self
    }

    /// The credentials behind this store — also a presigner, for bytes that
    /// should flow directly between the client and the bucket.
    pub fn presigner(&self) -> &S3Store {
        &self.s3
    }

    /// The transport in use.
    pub fn transport(&self) -> &T {
        &self.transport
    }

    /// Sign and send one request against `path` (raw, unencoded) with an
    /// already-canonical `query`.
    fn request(
        &self,
        method: &str,
        path: &str,
        query: &str,
        extra: &[(String, String)],
        body: &[u8],
    ) -> Result<HttpResponse, String> {
        let (url, headers) = self
            .s3
            .sign_request(method, path, query, extra, body, now_secs()?);
        self.transport.send(&HttpRequest {
            method,
            url,
            headers,
            body,
        })
    }

    /// One object request, keyed. Rejects the key before signing anything.
    fn object(
        &self,
        method: &str,
        key: &str,
        extra: &[(String, String)],
        body: &[u8],
    ) -> Result<HttpResponse, String> {
        validate_key(key)?;
        self.request(method, &self.s3.object_path(key), "", extra, body)
    }

    /// The store's MD5 `ETag` for a body we know, when it is comparable.
    /// `None` means "the store did not give us something to check against".
    fn etag_mismatch(&self, resp: &HttpResponse, body: &[u8]) -> Option<String> {
        if !self.verify_etag {
            return None;
        }
        let etag = resp.header("etag")?.trim_matches('"');
        // Multipart (`…-3`) and SSE-C/KMS ETags are not MD5 of the object.
        let comparable = etag.len() == 32 && etag.bytes().all(|b| b.is_ascii_hexdigit());
        if !comparable {
            return None;
        }
        let ours = hex(&md5(body));
        (!ours.eq_ignore_ascii_case(etag))
            .then(|| format!("etag mismatch: store says {etag}, body hashes to {ours}"))
    }
}

impl<T: HttpTransport> Storage for S3Storage<T> {
    fn put(&self, key: &str, bytes: &[u8], content_type: &str) -> Result<(), String> {
        let ct = if content_type.trim().is_empty() {
            content_type_of(key)
        } else {
            content_type
        };
        let resp = self.object(
            "PUT",
            key,
            &[("content-type".to_string(), ct.to_string())],
            bytes,
        )?;
        if !matches!(resp.status, 200 | 201 | 204) {
            return Err(s3_error("put", key, &resp));
        }
        match self.etag_mismatch(&resp, bytes) {
            Some(e) => Err(format!("put {key}: {e}")),
            None => Ok(()),
        }
    }

    fn get(&self, key: &str) -> Result<Option<Vec<u8>>, String> {
        let resp = self.object("GET", key, &[], b"")?;
        match resp.status {
            200 => match self.etag_mismatch(&resp, &resp.body) {
                Some(e) => Err(format!("get {key}: {e}")),
                None => Ok(Some(resp.body)),
            },
            404 => Ok(None),
            _ => Err(s3_error("get", key, &resp)),
        }
    }

    fn stat(&self, key: &str) -> Result<Option<ObjectMeta>, String> {
        let resp = self.object("HEAD", key, &[], b"")?;
        match resp.status {
            200 => Ok(Some(ObjectMeta {
                key: key.to_string(),
                size: resp
                    .header("content-length")
                    .and_then(|v| v.trim().parse().ok())
                    .unwrap_or(0),
                content_type: resp
                    .header("content-type")
                    .filter(|v| !v.is_empty())
                    .unwrap_or(content_type_of(key))
                    .to_string(),
                modified: resp
                    .header("last-modified")
                    .and_then(parse_http_date)
                    .unwrap_or(0),
            })),
            404 => Ok(None),
            // A HEAD on a bucket without `s3:ListBucket` answers 403 for a
            // missing key. Surfacing that as "absent" would turn a permissions
            // bug into silent data loss on the next `put`.
            _ => Err(s3_error("stat", key, &resp)),
        }
    }

    /// Removes `key`, reporting whether it existed — which costs a `HEAD`
    /// first, because S3 answers `204` to a `DELETE` either way. Concurrent
    /// deletes can therefore both report `true`.
    fn delete(&self, key: &str) -> Result<bool, String> {
        if self.stat(key)?.is_none() {
            return Ok(false);
        }
        let resp = self.object("DELETE", key, &[], b"")?;
        match resp.status {
            200 | 202 | 204 | 404 => Ok(true),
            _ => Err(s3_error("delete", key, &resp)),
        }
    }

    /// Lists via `ListObjectsV2`, following continuation tokens.
    ///
    /// Two honest differences from the fs/db backends: `content_type` is
    /// **guessed from the extension** (a listing carries no content type —
    /// [`stat`](Storage::stat) reports the real one), and keys that fail
    /// [`validate_key`] are skipped, since no backend can address them — this
    /// is what hides the empty `dir/` markers S3 GUIs create.
    fn list(&self, prefix: &str) -> Result<Vec<ObjectMeta>, String> {
        let mut out: Vec<ObjectMeta> = Vec::new();
        let mut token: Option<String> = None;
        let path = self.s3.bucket_path();
        // Pages are bounded so a store that keeps handing back the same
        // continuation token cannot spin forever.
        let max_pages = self.max_list_keys / PAGE_SIZE + 2;

        for _ in 0..max_pages {
            // SigV4 requires the canonical query sorted by name; these are.
            let mut params: Vec<(String, String)> = Vec::new();
            if let Some(t) = &token {
                params.push(("continuation-token".to_string(), t.clone()));
            }
            params.push(("encoding-type".to_string(), "url".to_string()));
            params.push(("list-type".to_string(), "2".to_string()));
            params.push(("max-keys".to_string(), PAGE_SIZE.to_string()));
            if !prefix.is_empty() {
                params.push(("prefix".to_string(), prefix.to_string()));
            }

            let resp = self.request("GET", &path, &encode_query(&params), &[], b"")?;
            if resp.status != 200 {
                return Err(s3_error("list", prefix, &resp));
            }
            let xml = String::from_utf8_lossy(&resp.body);

            for entry in contents_blocks(&xml) {
                let Some(key) = tag(entry, "Key").map(|k| percent_decode(&xml_unescape(k))) else {
                    continue;
                };
                if validate_key(&key).is_err() {
                    continue;
                }
                out.push(ObjectMeta {
                    size: tag(entry, "Size")
                        .and_then(|s| s.trim().parse().ok())
                        .unwrap_or(0),
                    modified: tag(entry, "LastModified")
                        .and_then(parse_iso8601)
                        .unwrap_or(0),
                    content_type: content_type_of(&key).to_string(),
                    key,
                });
                if out.len() > self.max_list_keys {
                    return Err(format!(
                        "list('{prefix}') exceeds max_list_keys ({}); narrow the prefix",
                        self.max_list_keys
                    ));
                }
            }

            let truncated = tag(&xml, "IsTruncated").is_some_and(|v| v.trim() == "true");
            if !truncated {
                out.sort_by(|a, b| a.key.cmp(&b.key));
                return Ok(out);
            }
            token = tag(&xml, "NextContinuationToken")
                .map(xml_unescape)
                .filter(|t| !t.is_empty())
                .ok_or("list truncated but no NextContinuationToken")?
                .into();
        }
        Err(format!(
            "list('{prefix}') did not terminate within {max_pages} pages"
        ))
    }
}

/// A readable error from a non-success response, including S3's XML `<Code>`
/// and `<Message>` when present. Never echoes the whole body.
fn s3_error(op: &str, key: &str, resp: &HttpResponse) -> String {
    let body = String::from_utf8_lossy(&resp.body);
    let code = tag(&body, "Code").map(xml_unescape);
    let message = tag(&body, "Message").map(xml_unescape);
    let detail = match (code, message) {
        (Some(c), Some(m)) => format!(": {c}: {m}"),
        (Some(c), None) => format!(": {c}"),
        (None, Some(m)) => format!(": {m}"),
        (None, None) => String::new(),
    };
    format!("s3 {op} '{key}' failed with HTTP {}{detail}", resp.status)
}

/// The text inside the first `<tag>…</tag>` in `xml`.
fn tag<'a>(xml: &'a str, name: &str) -> Option<&'a str> {
    let open = format!("<{name}>");
    let close = format!("</{name}>");
    let start = xml.find(&open)? + open.len();
    let end = xml[start..].find(&close)? + start;
    Some(&xml[start..end])
}

/// Every `<Contents>…</Contents>` block, in document order.
fn contents_blocks(xml: &str) -> impl Iterator<Item = &str> {
    xml.split("<Contents>")
        .skip(1)
        .filter_map(|rest| rest.find("</Contents>").map(|end| &rest[..end]))
}

/// The five predefined XML entities. Numeric character references do not occur
/// in the fields we read — keys arrive percent-encoded (`encoding-type=url`).
fn xml_unescape(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

/// Decode `%XX` escapes; a malformed escape is kept verbatim.
fn percent_decode(s: &str) -> String {
    if !s.contains('%') {
        return s.to_string();
    }
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            let hi = (b[i + 1] as char).to_digit(16);
            let lo = (b[i + 2] as char).to_digit(16);
            if let (Some(hi), Some(lo)) = (hi, lo) {
                out.push((hi * 16 + lo) as u8);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// `Fri, 24 May 2013 00:00:00 GMT` (RFC 7231 IMF-fixdate) → unix seconds.
fn parse_http_date(s: &str) -> Option<i64> {
    let rest = s.split_once(", ").map(|(_, r)| r).unwrap_or(s).trim();
    let mut parts = rest.split_whitespace();
    let day: i64 = parts.next()?.parse().ok()?;
    let month = month_number(parts.next()?)?;
    let year: i64 = parts.next()?.parse().ok()?;
    let mut hms = parts.next()?.split(':');
    let h: i64 = hms.next()?.parse().ok()?;
    let m: i64 = hms.next()?.parse().ok()?;
    let sec: i64 = hms.next()?.parse().ok()?;
    unix_from(year, month, day, h, m, sec)
}

/// `2013-05-24T00:00:00.000Z` (S3's XML timestamps) → unix seconds.
fn parse_iso8601(s: &str) -> Option<i64> {
    let (date, time) = s.trim().split_once('T')?;
    let mut d = date.split('-');
    let year: i64 = d.next()?.parse().ok()?;
    let month: i64 = d.next()?.parse().ok()?;
    let day: i64 = d.next()?.parse().ok()?;
    let time = time.trim_end_matches('Z');
    let time = time.split_once('.').map(|(t, _)| t).unwrap_or(time);
    let mut t = time.split(':');
    let h: i64 = t.next()?.parse().ok()?;
    let m: i64 = t.next()?.parse().ok()?;
    let sec: i64 = t.next()?.parse().ok()?;
    unix_from(year, month, day, h, m, sec)
}

fn unix_from(y: i64, mo: i64, d: i64, h: i64, mi: i64, s: i64) -> Option<i64> {
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) {
        return None;
    }
    if h > 23 || mi > 59 || s > 60 {
        return None;
    }
    let days = days_from_civil(y, mo, d);
    // Reject a date the calendar rejects (2013-02-31 &c.).
    if civil_from_days(days) != (y, mo, d) {
        return None;
    }
    Some(days * 86_400 + h * 3_600 + mi * 60 + s)
}

fn month_number(name: &str) -> Option<i64> {
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    MONTHS.iter().position(|m| *m == name).map(|i| i as i64 + 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::PlainHttp;
    use std::sync::Mutex;

    /// One request as the fake transport saw it.
    struct Sent {
        method: String,
        url: String,
        headers: Vec<(String, String)>,
        body: Vec<u8>,
    }

    impl Sent {
        fn header(&self, name: &str) -> String {
            self.headers
                .iter()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.clone())
                .unwrap_or_default()
        }
    }

    /// A transport that records what it was asked to send and replays canned
    /// responses — the whole client is exercised without a network.
    struct Fake {
        replies: Mutex<Vec<HttpResponse>>,
        seen: Mutex<Vec<Sent>>,
    }

    impl Fake {
        fn new(replies: Vec<HttpResponse>) -> Fake {
            Fake {
                replies: Mutex::new(replies),
                seen: Mutex::new(Vec::new()),
            }
        }

        fn ok(status: u16, headers: &[(&str, &str)], body: &[u8]) -> HttpResponse {
            HttpResponse {
                status,
                headers: headers
                    .iter()
                    .map(|(k, v)| (k.to_string(), v.to_string()))
                    .collect(),
                body: body.to_vec(),
            }
        }

        fn last_url(&self) -> String {
            self.seen.lock().unwrap().last().unwrap().url.clone()
        }
    }

    impl HttpTransport for Fake {
        fn send(&self, req: &HttpRequest<'_>) -> Result<HttpResponse, String> {
            self.seen.lock().unwrap().push(Sent {
                method: req.method.to_string(),
                url: req.url.clone(),
                headers: req.headers.clone(),
                body: req.body.to_vec(),
            });
            let mut replies = self.replies.lock().unwrap();
            if replies.is_empty() {
                return Err("fake transport out of replies".to_string());
            }
            Ok(replies.remove(0))
        }
    }

    fn store(replies: Vec<HttpResponse>) -> S3Storage<std::sync::Arc<Fake>> {
        S3Store::r2("acct", "bkt", "AK", "SK").storage(std::sync::Arc::new(Fake::new(replies)))
    }

    #[test]
    fn put_signs_content_type_and_payload() {
        let etag = format!("\"{}\"", hex(&md5(b"hello")));
        let s = store(vec![Fake::ok(200, &[("etag", &etag)], b"")]);
        s.put("a/b.txt", b"hello", "text/plain").unwrap();

        let seen = s.transport().seen.lock().unwrap();
        let sent = &seen[0];
        assert_eq!(sent.method, "PUT");
        assert_eq!(
            sent.url,
            "https://acct.r2.cloudflarestorage.com/bkt/a/b.txt"
        );
        assert_eq!(sent.body, b"hello");
        assert_eq!(sent.header("content-type"), "text/plain");
        assert_eq!(
            sent.header("x-amz-content-sha256"),
            hex(&sutegi_crypto::sha256(b"hello"))
        );
        assert!(sent
            .header("authorization")
            .starts_with("AWS4-HMAC-SHA256 Credential=AK/"));
        assert_eq!(sent.header("host"), "acct.r2.cloudflarestorage.com");
    }

    #[test]
    fn put_guesses_content_type_when_blank() {
        let s = store(vec![Fake::ok(200, &[], b"")]);
        s.put("x/pic.png", b"\x89PNG", "  ").unwrap();
        let seen = s.transport().seen.lock().unwrap();
        assert_eq!(seen[0].header("content-type"), "image/png");
    }

    #[test]
    fn get_roundtrip_and_missing() {
        let etag = format!("\"{}\"", hex(&md5(b"bytes")));
        let s = store(vec![
            Fake::ok(200, &[("etag", &etag), ("content-length", "5")], b"bytes"),
            Fake::ok(404, &[], b"<Error><Code>NoSuchKey</Code></Error>"),
        ]);
        assert_eq!(s.get("k.bin").unwrap().unwrap(), b"bytes");
        assert_eq!(s.get("gone.bin").unwrap(), None);
    }

    #[test]
    fn corrupted_download_is_rejected() {
        let etag = format!("\"{}\"", hex(&md5(b"the real bytes")));
        let s = store(vec![Fake::ok(200, &[("etag", &etag)], b"tampered")]);
        let err = s.get("k.bin").unwrap_err();
        assert!(err.contains("etag mismatch"), "{err}");
        // Multipart and encrypted ETags are not MD5s — skipped, not failed.
        let s = store(vec![Fake::ok(200, &[("etag", "\"abc-3\"")], b"tampered")]);
        assert_eq!(s.get("k.bin").unwrap().unwrap(), b"tampered");
        // And the check can be turned off wholesale.
        let s = store(vec![Fake::ok(200, &[("etag", &etag)], b"tampered")]).verify_etag(false);
        assert_eq!(s.get("k.bin").unwrap().unwrap(), b"tampered");
    }

    #[test]
    fn stat_reads_metadata() {
        let s = store(vec![Fake::ok(
            200,
            &[
                ("content-length", "1234"),
                ("content-type", "application/pdf"),
                ("last-modified", "Fri, 24 May 2013 00:00:00 GMT"),
            ],
            b"",
        )]);
        let meta = s.stat("r/q2.pdf").unwrap().unwrap();
        assert_eq!(meta.size, 1234);
        assert_eq!(meta.content_type, "application/pdf");
        assert_eq!(meta.modified, 1_369_353_600);
        assert_eq!(s.transport().seen.lock().unwrap()[0].method, "HEAD");
    }

    #[test]
    fn stat_403_is_an_error_not_absence() {
        let s = store(vec![Fake::ok(
            403,
            &[],
            b"<Error><Code>AccessDenied</Code>\
            <Message>Access Denied</Message></Error>",
        )]);
        let err = s.stat("k").unwrap_err();
        assert!(
            err.contains("HTTP 403") && err.contains("AccessDenied"),
            "{err}"
        );
    }

    #[test]
    fn delete_reports_existence() {
        let s = store(vec![
            Fake::ok(200, &[("content-length", "2")], b""), // HEAD: present
            Fake::ok(204, &[], b""),                        // DELETE
        ]);
        assert!(s.delete("k").unwrap());
        let s = store(vec![Fake::ok(404, &[], b"")]);
        assert!(!s.delete("k").unwrap());
        assert_eq!(
            s.transport().seen.lock().unwrap().len(),
            1,
            "no DELETE after 404"
        );
    }

    fn page(keys: &[(&str, u64)], next: Option<&str>) -> HttpResponse {
        let mut xml = String::from("<?xml version=\"1.0\"?><ListBucketResult>");
        for (key, size) in keys {
            xml.push_str(&format!(
                "<Contents><Key>{key}</Key><LastModified>2026-08-10T12:00:00.000Z</LastModified>\
                 <ETag>&quot;deadbeef&quot;</ETag><Size>{size}</Size>\
                 <StorageClass>STANDARD</StorageClass></Contents>"
            ));
        }
        match next {
            Some(t) => xml.push_str(&format!(
                "<IsTruncated>true</IsTruncated><NextContinuationToken>{t}</NextContinuationToken>"
            )),
            None => xml.push_str("<IsTruncated>false</IsTruncated>"),
        }
        xml.push_str("</ListBucketResult>");
        Fake::ok(200, &[("content-type", "application/xml")], xml.as_bytes())
    }

    #[test]
    fn list_paginates_sorts_and_decodes() {
        let s = store(vec![
            page(
                &[("logs/b.txt", 2), ("logs/a%20one.txt", 1)],
                Some("tok/2+x"),
            ),
            page(
                &[("logs/c.png", 3), ("logs/", 0), ("logs/../evil", 9)],
                None,
            ),
        ]);
        let objs = s.list("logs/").unwrap();
        assert_eq!(
            objs.iter().map(|o| o.key.as_str()).collect::<Vec<_>>(),
            // %20 decoded, sorted, `logs/` marker and the traversal key dropped
            vec!["logs/a one.txt", "logs/b.txt", "logs/c.png"]
        );
        assert_eq!(objs[0].size, 1);
        assert_eq!(objs[0].modified, 1_786_363_200); // 2026-08-10T12:00:00Z
        assert_eq!(objs[2].content_type, "image/png");

        let seen = s.transport().seen.lock().unwrap();
        assert_eq!(
            seen[0].url,
            "https://acct.r2.cloudflarestorage.com/bkt/\
             ?encoding-type=url&list-type=2&max-keys=1000&prefix=logs%2F"
        );
        // The continuation token is sorted first and URI-encoded.
        assert!(seen[1]
            .url
            .contains("?continuation-token=tok%2F2%2Bx&encoding-type=url"));
    }

    #[test]
    fn list_refuses_to_truncate_or_spin() {
        // Never-ending truncation is bounded, not infinite.
        let pages: Vec<_> = (0..12).map(|_| page(&[("k", 1)], Some("same"))).collect();
        let s = store(pages).max_list_keys(2_000);
        let err = s.list("").unwrap_err();
        assert!(err.contains("did not terminate"), "{err}");

        // Over the key cap: an error, not a short answer.
        let many: Vec<(String, u64)> = (0..50).map(|i| (format!("k{i}"), 1)).collect();
        let refs: Vec<(&str, u64)> = many.iter().map(|(k, s)| (k.as_str(), *s)).collect();
        let s = store(vec![page(&refs, None)]).max_list_keys(10);
        assert!(s.list("").unwrap_err().contains("exceeds max_list_keys"));

        // Truncated with no token is a protocol error, not a silent stop.
        let s = store(vec![Fake::ok(
            200,
            &[],
            b"<ListBucketResult><IsTruncated>true</IsTruncated></ListBucketResult>",
        )]);
        assert!(s.list("").unwrap_err().contains("no NextContinuationToken"));
    }

    #[test]
    fn empty_bucket_lists_empty() {
        let s = store(vec![page(&[], None)]);
        assert!(s.list("").unwrap().is_empty());
        assert!(!s.transport().last_url().contains("prefix="));
    }

    #[test]
    fn bad_keys_never_reach_the_wire() {
        let s = store(vec![]);
        for key in ["../etc/passwd", "", "a//b", "a\\b"] {
            assert!(s.put(key, b"x", "text/plain").is_err(), "{key:?}");
            assert!(s.get(key).is_err(), "{key:?}");
            assert!(s.delete(key).is_err(), "{key:?}");
        }
        assert!(s.transport().seen.lock().unwrap().is_empty());
    }

    #[test]
    fn control_characters_in_content_type_are_rejected() {
        // The transport is the second line of defence; the header never forms.
        let s = S3Store::r2("acct", "bkt", "AK", "SK")
            .insecure_http()
            .storage(PlainHttp::new());
        let err = s
            .put("k.txt", b"x", "text/plain\r\nauthorization: leak")
            .unwrap_err();
        assert!(err.contains("control characters"), "{err}");
    }

    #[test]
    fn error_bodies_become_readable_messages() {
        let s = store(vec![Fake::ok(
            500,
            &[],
            b"<Error><Code>InternalError</Code><Message>We encountered an \
              internal error &amp; gave up.</Message></Error>",
        )]);
        let err = s.put("k", b"x", "text/plain").unwrap_err();
        assert!(err.contains("HTTP 500"), "{err}");
        assert!(err.contains("InternalError"), "{err}");
        assert!(err.contains("internal error & gave up"), "{err}");
    }

    #[test]
    fn transport_errors_propagate() {
        let s = store(vec![]);
        assert!(s.get("k").unwrap_err().contains("out of replies"));
    }

    #[test]
    fn presigner_shares_the_credentials() {
        let s = store(vec![]);
        let url = s.presigner().presign_get("k.txt", 60).unwrap();
        assert!(url.starts_with("https://acct.r2.cloudflarestorage.com/bkt/k.txt?"));
        assert_eq!(s.presigner().bucket(), "bkt");
    }

    #[test]
    fn debug_does_not_leak_the_secret() {
        let printed = format!(
            "{:?}",
            S3Store::r2("acct", "bkt", "AK", "top-secret").storage(PlainHttp::new())
        );
        assert!(!printed.contains("top-secret"), "{printed}");
    }

    #[test]
    fn dates_parse_and_reject() {
        assert_eq!(
            parse_http_date("Fri, 24 May 2013 00:00:00 GMT"),
            Some(1_369_353_600)
        );
        assert_eq!(
            parse_http_date("Sun, 06 Nov 1994 08:49:37 GMT"),
            Some(784_111_777)
        );
        assert_eq!(
            parse_iso8601("2013-05-24T00:00:00.000Z"),
            Some(1_369_353_600)
        );
        assert_eq!(
            parse_iso8601("2013-05-24T01:01:01Z"),
            Some(1_369_353_600 + 3_661)
        );
        for bad in ["", "not a date", "Fri, 31 Feb 2013 00:00:00 GMT"] {
            assert_eq!(parse_http_date(bad), None, "{bad:?}");
        }
        for bad in [
            "2013-02-31T00:00:00Z",
            "2013-13-01T00:00:00Z",
            "2013-05-24T25:00:00Z",
        ] {
            assert_eq!(parse_iso8601(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn xml_and_percent_helpers() {
        assert_eq!(xml_unescape("a &amp;lt; b"), "a &lt; b");
        assert_eq!(xml_unescape("&lt;k&gt;&quot;x&quot;&apos;"), "<k>\"x\"'");
        assert_eq!(percent_decode("a%20b%2Fc"), "a b/c");
        assert_eq!(percent_decode("100%"), "100%");
        assert_eq!(percent_decode("a%zzb"), "a%zzb");
        assert_eq!(percent_decode("caf%C3%A9"), "café");
        assert_eq!(tag("<a><b>x</b></a>", "b"), Some("x"));
        assert_eq!(tag("<a></a>", "b"), None);
        assert_eq!(
            contents_blocks("<Contents>1</Contents><Contents>2").count(),
            1
        );
    }
}
