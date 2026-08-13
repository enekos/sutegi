//! **AWS Signature V4** for S3-compatible object stores, in pure `std` —
//! query-string presigning *and* `Authorization`-header signing, from one
//! credential type.
//!
//! [`S3Store`] is that credential type plus the endpoint shape (region,
//! virtual-hosted vs path-style, http vs https). It gives you two contracts:
//!
//! - **Presign** ([`presign_get`](S3Store::presign_get) &c.) — mint a
//!   time-limited URL and let the holder (a browser, a `curl`, an agent) move
//!   the bytes **directly** to and from the store. sutegi never sees them, so
//!   this needs no HTTP client and no TLS at all.
//! - **Move the bytes yourself** ([`storage`](S3Store::storage)) — turn the
//!   same credentials into an [`S3Storage`](crate::S3Storage), which implements
//!   [`Storage`](crate::Storage) over an injected
//!   [`HttpTransport`](crate::transport::HttpTransport).
//!
//! Signing is a canonical request, a few SHA-256 hashes and an HMAC chain, all
//! reused from the Postgres driver's SCRAM crypto ([`sutegi_crypto`]), and both
//! paths are verified against AWS's published known-answer vectors. It works
//! against AWS S3, Cloudflare R2, MinIO, Garage, Ceph RGW, and anything else
//! speaking SigV4.
//!
//! ```no_run
//! use sutegi_storage::S3Store;
//!
//! let s3 = S3Store::new("my-bucket", "eu-central-1", "AKIA…", "secret…");
//! let url = s3.presign_get("reports/q2.pdf", 3600).unwrap();
//! // hand `url` to the client; it GETs the object straight from S3
//! # let _ = url;
//! ```

use crate::validate_key;
use std::time::{SystemTime, UNIX_EPOCH};
use sutegi_crypto::{hex, hmac_sha256, sha256};

/// The longest expiry SigV4 allows (7 days).
pub const MAX_EXPIRES: u64 = 604_800;

/// `hex(sha256(b""))` — the payload hash of a body-less request.
pub(crate) const EMPTY_SHA256: &str =
    "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

/// Credentials and endpoint shape for one bucket on an S3-compatible store —
/// a presigned-URL factory, and the configuration an
/// [`S3Storage`](crate::S3Storage) is built from.
///
/// Defaults target AWS (`https`, virtual-hosted addressing,
/// `s3.<region>.amazonaws.com`). For R2 use [`r2`](S3Store::r2); for
/// MinIO/Garage/other use [`with_endpoint`](S3Store::with_endpoint), which
/// switches to path-style addressing (what most non-AWS stores expect).
///
/// `Debug` **redacts the credentials** — printing one of these, or a struct
/// holding one, cannot leak the secret key into a log.
#[derive(Clone)]
pub struct S3Store {
    pub(crate) bucket: String,
    pub(crate) region: String,
    pub(crate) access_key: String,
    pub(crate) secret_key: String,
    pub(crate) session_token: Option<String>,
    /// Host (and optional port), no scheme: `s3.amazonaws.com`, `localhost:9000`.
    pub(crate) endpoint: String,
    pub(crate) https: bool,
    pub(crate) path_style: bool,
}

impl std::fmt::Debug for S3Store {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("S3Store")
            .field("bucket", &self.bucket)
            .field("region", &self.region)
            .field("endpoint", &self.endpoint)
            .field("https", &self.https)
            .field("path_style", &self.path_style)
            .field("access_key", &"<redacted>")
            .field("secret_key", &"<redacted>")
            .field(
                "session_token",
                &self.session_token.as_ref().map(|_| "<redacted>"),
            )
            .finish()
    }
}

impl S3Store {
    /// Credentials for `bucket` on AWS S3 in `region`.
    pub fn new(bucket: &str, region: &str, access_key: &str, secret_key: &str) -> S3Store {
        let endpoint = if region == "us-east-1" {
            "s3.amazonaws.com".to_string()
        } else {
            format!("s3.{region}.amazonaws.com")
        };
        S3Store {
            bucket: bucket.to_string(),
            region: region.to_string(),
            access_key: access_key.to_string(),
            secret_key: secret_key.to_string(),
            session_token: None,
            endpoint,
            https: true,
            path_style: false,
        }
    }

    /// Credentials for a **Cloudflare R2** bucket: endpoint
    /// `<account_id>.r2.cloudflarestorage.com`, region `auto`, path-style.
    /// `access_key`/`secret_key` are an R2 API token's pair.
    pub fn r2(account_id: &str, bucket: &str, access_key: &str, secret_key: &str) -> S3Store {
        S3Store::new(bucket, "auto", access_key, secret_key)
            .with_endpoint(&format!("{account_id}.r2.cloudflarestorage.com"))
    }

    /// Point at a non-AWS endpoint (`host` or `host:port`, no scheme) —
    /// R2's `<account>.r2.cloudflarestorage.com`, a MinIO `localhost:9000`, …
    /// Switches to path-style addressing; override with
    /// [`path_style`](S3Store::path_style) if your store wants virtual-hosted.
    pub fn with_endpoint(mut self, host: &str) -> S3Store {
        self.endpoint = host.to_string();
        self.path_style = true;
        self
    }

    /// Choose path-style (`host/bucket/key`) vs virtual-hosted
    /// (`bucket.host/key`) addressing.
    pub fn path_style(mut self, on: bool) -> S3Store {
        self.path_style = on;
        self
    }

    /// Use `http://` instead of `https://` — for in-cluster stores (e.g. MinIO
    /// behind your own network boundary). Anything crossing the public internet
    /// should stay on the default.
    pub fn insecure_http(mut self) -> S3Store {
        self.https = false;
        self
    }

    /// Attach an STS session token (temporary credentials).
    pub fn with_session_token(mut self, token: &str) -> S3Store {
        self.session_token = Some(token.to_string());
        self
    }

    /// The bucket these credentials address.
    pub fn bucket(&self) -> &str {
        &self.bucket
    }

    /// Turn these credentials into a byte-moving [`Storage`](crate::Storage)
    /// backend over `transport`.
    ///
    /// ```no_run
    /// use sutegi_storage::{transport::SystemCurl, S3Store, Storage};
    ///
    /// let store = S3Store::r2("acct", "bkt", "ak", "sk").storage(SystemCurl::new());
    /// store.put("a/b.pdf", b"%PDF-1.7", "application/pdf").unwrap();
    /// ```
    pub fn storage<T: crate::transport::HttpTransport>(self, transport: T) -> crate::S3Storage<T> {
        crate::S3Storage::new(self, transport)
    }

    /// A time-limited URL to download `key`.
    pub fn presign_get(&self, key: &str, expires_secs: u64) -> Result<String, String> {
        self.presign("GET", key, expires_secs)
    }

    /// A time-limited URL to upload `key`. The holder `PUT`s the bytes (and
    /// any `Content-Type` header) directly to the store.
    pub fn presign_put(&self, key: &str, expires_secs: u64) -> Result<String, String> {
        self.presign("PUT", key, expires_secs)
    }

    /// A time-limited URL to delete `key`.
    pub fn presign_delete(&self, key: &str, expires_secs: u64) -> Result<String, String> {
        self.presign("DELETE", key, expires_secs)
    }

    /// Presign an arbitrary method for `key`, timestamped now.
    pub fn presign(&self, method: &str, key: &str, expires_secs: u64) -> Result<String, String> {
        self.presign_at(method, key, expires_secs, now_secs()?)
    }

    /// The deterministic core: presign as of `unix_secs`. Exposed for
    /// verification against published known-answer vectors.
    pub fn presign_at(
        &self,
        method: &str,
        key: &str,
        expires_secs: u64,
        unix_secs: i64,
    ) -> Result<String, String> {
        validate_key(key)?;
        if expires_secs == 0 || expires_secs > MAX_EXPIRES {
            return Err(format!("expires must be 1..={MAX_EXPIRES} seconds"));
        }

        let (date, datetime) = amz_date(unix_secs);
        let scope = self.scope(&date);
        let host = self.host();
        let canonical_uri = uri_encode(&self.object_path(key), false);

        // Query parameters, sorted by (encoded) name — these exact names
        // happen to sort in declaration order.
        let mut params: Vec<(String, String)> = vec![
            ("X-Amz-Algorithm".into(), "AWS4-HMAC-SHA256".into()),
            (
                "X-Amz-Credential".into(),
                format!("{}/{scope}", self.access_key),
            ),
            ("X-Amz-Date".into(), datetime.clone()),
            ("X-Amz-Expires".into(), expires_secs.to_string()),
        ];
        if let Some(token) = &self.session_token {
            params.push(("X-Amz-Security-Token".into(), token.clone()));
        }
        params.push(("X-Amz-SignedHeaders".into(), "host".into()));
        let canonical_query = encode_query(&params);

        let canonical_request = format!(
            "{method}\n{canonical_uri}\n{canonical_query}\nhost:{host}\n\nhost\nUNSIGNED-PAYLOAD"
        );
        let signature = self.sign_canonical(&canonical_request, &date, &datetime, &scope);

        Ok(format!(
            "{}://{host}{canonical_uri}?{canonical_query}&X-Amz-Signature={signature}",
            self.scheme()
        ))
    }

    /// Sign a request into `Authorization`-header form: the shape a real HTTP
    /// client sends.
    ///
    /// Unlike presigning, the **payload hash is signed** (`x-amz-content-sha256`
    /// is `hex(sha256(body))`, never `UNSIGNED-PAYLOAD`), so the store rejects a
    /// body altered in flight — integrity that holds even over a plaintext
    /// transport. `extra` headers (`content-type`, …) are folded into the
    /// signature; names are lowercased and values trimmed per SigV4.
    ///
    /// Returns `(url, headers)` ready to hand to an
    /// [`HttpTransport`](crate::transport::HttpTransport). `path` is a raw
    /// (unencoded) absolute path such as `/bucket/a b.txt`; `query` is an
    /// already-canonical query string (sorted, SigV4-encoded), or empty.
    ///
    /// [`S3Storage`](crate::S3Storage) covers the object operations behind the
    /// [`Storage`](crate::Storage) trait; this is the seam for the rest of the
    /// S3 API — multipart upload, `CopyObject`, tagging — which you can drive
    /// through the same transport without leaving the crate.
    pub fn sign_request(
        &self,
        method: &str,
        path: &str,
        query: &str,
        extra: &[(String, String)],
        body: &[u8],
        unix_secs: i64,
    ) -> (String, Vec<(String, String)>) {
        let payload_hash = if body.is_empty() {
            EMPTY_SHA256.to_string()
        } else {
            hex(&sha256(body))
        };
        let (date, datetime) = amz_date(unix_secs);
        let scope = self.scope(&date);
        let host = self.host();
        let canonical_uri = uri_encode(path, false);

        let mut headers: Vec<(String, String)> = vec![
            ("host".to_string(), host.clone()),
            ("x-amz-content-sha256".to_string(), payload_hash.clone()),
            ("x-amz-date".to_string(), datetime.clone()),
        ];
        if let Some(token) = &self.session_token {
            headers.push(("x-amz-security-token".to_string(), token.clone()));
        }
        for (k, v) in extra {
            headers.push((k.to_ascii_lowercase(), v.trim().to_string()));
        }
        headers.sort_by(|a, b| a.0.cmp(&b.0));

        let signed_headers = headers
            .iter()
            .map(|(k, _)| k.as_str())
            .collect::<Vec<_>>()
            .join(";");
        let canonical_headers: String = headers.iter().map(|(k, v)| format!("{k}:{v}\n")).collect();
        let canonical_request = format!(
            "{method}\n{canonical_uri}\n{query}\n{canonical_headers}\n{signed_headers}\n{payload_hash}"
        );
        let signature = self.sign_canonical(&canonical_request, &date, &datetime, &scope);

        headers.push((
            "authorization".to_string(),
            format!(
                "AWS4-HMAC-SHA256 Credential={}/{scope},SignedHeaders={signed_headers},Signature={signature}",
                self.access_key
            ),
        ));
        let url = if query.is_empty() {
            format!("{}://{host}{canonical_uri}", self.scheme())
        } else {
            format!("{}://{host}{canonical_uri}?{query}", self.scheme())
        };
        (url, headers)
    }

    /// `<date>/<region>/s3/aws4_request`.
    fn scope(&self, date: &str) -> String {
        format!("{date}/{}/s3/aws4_request", self.region)
    }

    /// The string-to-sign chain and the derived signing key.
    fn sign_canonical(
        &self,
        canonical_request: &str,
        date: &str,
        datetime: &str,
        scope: &str,
    ) -> String {
        let string_to_sign = format!(
            "AWS4-HMAC-SHA256\n{datetime}\n{scope}\n{}",
            hex(&sha256(canonical_request.as_bytes()))
        );
        let k_date = hmac_sha256(
            format!("AWS4{}", self.secret_key).as_bytes(),
            date.as_bytes(),
        );
        let k_region = hmac_sha256(&k_date, self.region.as_bytes());
        let k_service = hmac_sha256(&k_region, b"s3");
        let k_signing = hmac_sha256(&k_service, b"aws4_request");
        hex(&hmac_sha256(&k_signing, string_to_sign.as_bytes()))
    }

    /// `https` or `http`.
    pub(crate) fn scheme(&self) -> &'static str {
        if self.https {
            "https"
        } else {
            "http"
        }
    }

    /// The `Host` header / URL authority.
    pub(crate) fn host(&self) -> String {
        if self.path_style {
            self.endpoint.clone()
        } else {
            format!("{}.{}", self.bucket, self.endpoint)
        }
    }

    /// Raw (unencoded) request path for `key`.
    pub(crate) fn object_path(&self, key: &str) -> String {
        if self.path_style {
            format!("/{}/{key}", self.bucket)
        } else {
            format!("/{key}")
        }
    }

    /// Raw (unencoded) request path for the bucket itself (used by `list`).
    pub(crate) fn bucket_path(&self) -> String {
        if self.path_style {
            format!("/{}/", self.bucket)
        } else {
            "/".to_string()
        }
    }
}

/// Now, as unix seconds.
pub(crate) fn now_secs() -> Result<i64, String> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_secs() as i64)
}

/// `name=value&…`, each side SigV4-encoded. Callers pass params already sorted
/// by name (SigV4 requires the canonical query to be sorted).
pub(crate) fn encode_query(params: &[(String, String)]) -> String {
    params
        .iter()
        .map(|(k, v)| format!("{}={}", uri_encode(k, true), uri_encode(v, true)))
        .collect::<Vec<_>>()
        .join("&")
}

/// `(YYYYMMDD, YYYYMMDDTHHMMSSZ)` in UTC for a unix timestamp.
fn amz_date(unix_secs: i64) -> (String, String) {
    let days = unix_secs.div_euclid(86_400);
    let rem = unix_secs.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    let date = format!("{y:04}{m:02}{d:02}");
    let datetime = format!(
        "{date}T{:02}{:02}{:02}Z",
        rem / 3_600,
        rem % 3_600 / 60,
        rem % 60
    );
    (date, datetime)
}

/// Civil date from a unix day number (Howard Hinnant's algorithm), valid for
/// all `i64` days.
pub(crate) fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = mp + if mp < 10 { 3 } else { -9 };
    (y + i64::from(m <= 2), m, d)
}

/// Unix day number from a civil date — the inverse of [`civil_from_days`].
pub(crate) fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = y - i64::from(m <= 2);
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// SigV4 URI encoding: RFC 3986 unreserved characters pass through; `/` also
/// passes when encoding a path. Everything else becomes uppercase `%XX`,
/// byte-wise over UTF-8.
pub(crate) fn uri_encode(s: &str, encode_slash: bool) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(b as char)
            }
            b'/' if !encode_slash => out.push('/'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// AWS's documented example credentials, used by every published SigV4
    /// known-answer vector.
    fn example_store() -> S3Store {
        S3Store::new(
            "examplebucket",
            "us-east-1",
            "AKIAIOSFODNN7EXAMPLE",
            "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY",
        )
    }

    /// The published known-answer vector from the AWS SigV4 documentation
    /// ("Authenticating Requests: Using Query Parameters"): a GET of
    /// `test.txt` in `examplebucket`, us-east-1, at 2013-05-24T00:00:00Z,
    /// valid 24h, with the documented example credentials.
    #[test]
    fn aws_known_answer_vector() {
        let url = example_store()
            .presign_at("GET", "test.txt", 86_400, 1_369_353_600)
            .unwrap();
        assert_eq!(
            url,
            "https://examplebucket.s3.amazonaws.com/test.txt\
             ?X-Amz-Algorithm=AWS4-HMAC-SHA256\
             &X-Amz-Credential=AKIAIOSFODNN7EXAMPLE%2F20130524%2Fus-east-1%2Fs3%2Faws4_request\
             &X-Amz-Date=20130524T000000Z\
             &X-Amz-Expires=86400\
             &X-Amz-SignedHeaders=host\
             &X-Amz-Signature=aeeed9bbccd4d02ee5c0109b86d86835f995330da4c265957d157751f604d404"
        );
    }

    /// Header-signing known-answer vector: AWS's documented "GET Object" example
    /// (`Range: bytes=0-9`, empty payload), same credentials and timestamp.
    #[test]
    fn aws_header_vector_get_object() {
        let (url, headers) = example_store().sign_request(
            "GET",
            "/test.txt",
            "",
            &[("range".to_string(), "bytes=0-9".to_string())],
            b"",
            1_369_353_600,
        );
        assert_eq!(url, "https://examplebucket.s3.amazonaws.com/test.txt");
        let auth = header(&headers, "authorization");
        assert_eq!(
            auth,
            "AWS4-HMAC-SHA256 \
             Credential=AKIAIOSFODNN7EXAMPLE/20130524/us-east-1/s3/aws4_request,\
             SignedHeaders=host;range;x-amz-content-sha256;x-amz-date,\
             Signature=f0e8bdb87c964420e857bd35b5d6ed310bd44f0170aba48dd91039c6036bdb41"
        );
        assert_eq!(header(&headers, "x-amz-content-sha256"), EMPTY_SHA256);
        assert_eq!(header(&headers, "x-amz-date"), "20130524T000000Z");
    }

    /// Header-signing known-answer vector: AWS's documented "PUT Object"
    /// example — body `Welcome to Amazon S3.`, `date` and `x-amz-storage-class`
    /// signed alongside.
    #[test]
    fn aws_header_vector_put_object() {
        let body = b"Welcome to Amazon S3.";
        let (_, headers) = example_store().sign_request(
            "PUT",
            "/test$file.text",
            "",
            &[
                (
                    "date".to_string(),
                    "Fri, 24 May 2013 00:00:00 GMT".to_string(),
                ),
                (
                    "x-amz-storage-class".to_string(),
                    "REDUCED_REDUNDANCY".to_string(),
                ),
            ],
            body,
            1_369_353_600,
        );
        assert_eq!(
            header(&headers, "x-amz-content-sha256"),
            "44ce7dd67c959e0d3524ffac1771dfbba87d2b6b4b4e99e42034a8b803f8b072"
        );
        assert_eq!(
            header(&headers, "authorization"),
            "AWS4-HMAC-SHA256 \
             Credential=AKIAIOSFODNN7EXAMPLE/20130524/us-east-1/s3/aws4_request,\
             SignedHeaders=date;host;x-amz-content-sha256;x-amz-date;x-amz-storage-class,\
             Signature=98ad721746da40c64f1a55b78f14c238d841ea1380cd77a1b5971af0ece108bd"
        );
    }

    fn header<'a>(headers: &'a [(String, String)], name: &str) -> &'a str {
        headers
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
            .unwrap_or_default()
    }

    #[test]
    fn signed_request_covers_content_type_and_token() {
        let (url, headers) = S3Store::r2("acct", "bkt", "AK", "SK")
            .with_session_token("tok")
            .sign_request(
                "PUT",
                "/bkt/a b.txt",
                "",
                &[("Content-Type".to_string(), " text/plain ".to_string())],
                b"hi",
                1_700_000_000,
            );
        assert_eq!(url, "https://acct.r2.cloudflarestorage.com/bkt/a%20b.txt");
        // Value trimmed, name lowercased, both in SignedHeaders, and the
        // payload hash is the real body digest.
        assert_eq!(header(&headers, "content-type"), "text/plain");
        assert_eq!(
            header(&headers, "x-amz-content-sha256"),
            hex(&sha256(b"hi"))
        );
        let auth = header(&headers, "authorization");
        assert!(auth.contains(
            "SignedHeaders=content-type;host;x-amz-content-sha256;x-amz-date;x-amz-security-token,"
        ));
        assert!(auth.contains("Credential=AK/20231114/auto/s3/aws4_request,"));
    }

    #[test]
    fn amz_date_formats() {
        let (date, datetime) = amz_date(1_369_353_600);
        assert_eq!(date, "20130524");
        assert_eq!(datetime, "20130524T000000Z");
        let (_, dt) = amz_date(1_369_353_600 + 3_661);
        assert_eq!(dt, "20130524T010101Z");
    }

    #[test]
    fn civil_roundtrip() {
        for day in [-25_567_i64, 0, 1, 19_000, 20_000, 200_000] {
            let (y, m, d) = civil_from_days(day);
            assert_eq!(days_from_civil(y, m, d), day, "{y}-{m}-{d}");
        }
        assert_eq!(civil_from_days(days_from_civil(2026, 8, 10)), (2026, 8, 10));
    }

    #[test]
    fn path_style_and_http() {
        let url = S3Store::new("bucket", "us-east-1", "AK", "SK")
            .with_endpoint("localhost:9000")
            .insecure_http()
            .presign_at("PUT", "a/b.txt", 300, 1_369_353_600)
            .unwrap();
        assert!(url.starts_with("http://localhost:9000/bucket/a/b.txt?"));
        assert!(url.contains("X-Amz-Expires=300"));
        assert!(url.contains("&X-Amz-Signature="));
    }

    #[test]
    fn r2_defaults() {
        let r2 = S3Store::r2("abc123", "media", "AK", "SK");
        assert_eq!(r2.host(), "abc123.r2.cloudflarestorage.com");
        assert_eq!(r2.object_path("a/b.png"), "/media/a/b.png");
        assert_eq!(r2.bucket_path(), "/media/");
        assert_eq!(r2.scheme(), "https");
        assert_eq!(r2.region, "auto");
    }

    #[test]
    fn session_token_is_signed_in() {
        let url = S3Store::new("bucket", "eu-central-1", "AK", "SK")
            .with_session_token("tok/en+A=")
            .presign_at("GET", "k.txt", 60, 1_700_000_000)
            .unwrap();
        assert!(url.contains("X-Amz-Security-Token=tok%2Fen%2BA%3D"));
        assert!(url.contains("bucket.s3.eu-central-1.amazonaws.com"));
    }

    #[test]
    fn bounds_enforced() {
        let s3 = S3Store::new("b", "us-east-1", "AK", "SK");
        assert!(s3.presign_at("GET", "k", 0, 0).is_err());
        assert!(s3.presign_at("GET", "k", MAX_EXPIRES + 1, 0).is_err());
        assert!(s3.presign_at("GET", "../k", 60, 0).is_err());
    }

    #[test]
    fn keys_with_special_chars_are_encoded() {
        let url = S3Store::new("b", "us-east-1", "AK", "SK")
            .presign_at("GET", "dir/file with space+plus.txt", 60, 1_700_000_000)
            .unwrap();
        assert!(url.contains("/dir/file%20with%20space%2Bplus.txt?"));
    }

    #[test]
    fn debug_redacts_credentials() {
        let printed = format!(
            "{:?}",
            S3Store::new("b", "us-east-1", "AKIAEXAMPLE", "super-secret")
                .with_session_token("sts-token")
        );
        assert!(!printed.contains("super-secret"), "{printed}");
        assert!(!printed.contains("AKIAEXAMPLE"), "{printed}");
        assert!(!printed.contains("sts-token"), "{printed}");
        assert!(printed.contains("<redacted>") && printed.contains("bucket: \"b\""));
    }

    #[test]
    fn empty_sha256_constant_is_right() {
        assert_eq!(EMPTY_SHA256, hex(&sha256(b"")));
    }
}
