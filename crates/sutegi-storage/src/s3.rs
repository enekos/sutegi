//! A pure-`std` **AWS Signature V4 presigner** for S3-compatible object
//! stores. It mints time-limited GET/PUT/DELETE URLs; the bytes then flow
//! directly between the holder of the URL (a browser, a `curl`, an agent) and
//! the object store — **sutegi never proxies them**.
//!
//! That split is why this needs no HTTP client and no TLS stack: presigning
//! is pure computation — a canonical request, three SHA-256 hashes, and an
//! HMAC chain — all reused from the Postgres driver's SCRAM crypto
//! ([`sutegi_pg::crypto`]). It works against AWS S3, Cloudflare R2, MinIO,
//! Garage, Ceph RGW, and anything else speaking SigV4.
//!
//! The agent-native shape: expose an `App::tool` that calls
//! [`S3Store::presign_put`] and returns the URL — the agent uploads the bytes
//! itself, and your app only ever handles metadata.
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
use sutegi_pg::crypto::{hex, hmac_sha256, sha256};

/// The longest expiry SigV4 allows (7 days).
pub const MAX_EXPIRES: u64 = 604_800;

/// A presigned-URL factory for one bucket on an S3-compatible endpoint.
///
/// Defaults target AWS (`https`, virtual-hosted addressing,
/// `s3.<region>.amazonaws.com`). For R2/MinIO/other endpoints use
/// [`with_endpoint`](S3Store::with_endpoint), which switches to path-style
/// addressing (what most non-AWS stores expect).
#[derive(Clone, Debug)]
pub struct S3Store {
    bucket: String,
    region: String,
    access_key: String,
    secret_key: String,
    session_token: Option<String>,
    /// Host (and optional port), no scheme: `s3.amazonaws.com`, `localhost:9000`.
    endpoint: String,
    https: bool,
    path_style: bool,
}

impl S3Store {
    /// A presigner for `bucket` on AWS S3 in `region`.
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

    /// Presign `http://` URLs instead of `https://` — for in-cluster stores
    /// (e.g. MinIO behind your own network boundary). Anything crossing the
    /// public internet should stay on the default.
    pub fn insecure_http(mut self) -> S3Store {
        self.https = false;
        self
    }

    /// Attach an STS session token (temporary credentials).
    pub fn with_session_token(mut self, token: &str) -> S3Store {
        self.session_token = Some(token.to_string());
        self
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
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|e| e.to_string())?
            .as_secs() as i64;
        self.presign_at(method, key, expires_secs, now)
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
        let scope = format!("{date}/{}/s3/aws4_request", self.region);
        let credential = format!("{}/{scope}", self.access_key);

        let host = if self.path_style {
            self.endpoint.clone()
        } else {
            format!("{}.{}", self.bucket, self.endpoint)
        };
        let path = if self.path_style {
            format!("/{}/{key}", self.bucket)
        } else {
            format!("/{key}")
        };
        let canonical_uri = uri_encode(&path, false);

        // Query parameters, sorted by (encoded) name — these exact names
        // happen to sort in declaration order.
        let mut params: Vec<(String, String)> = vec![
            ("X-Amz-Algorithm".into(), "AWS4-HMAC-SHA256".into()),
            ("X-Amz-Credential".into(), credential),
            ("X-Amz-Date".into(), datetime.clone()),
            ("X-Amz-Expires".into(), expires_secs.to_string()),
        ];
        if let Some(token) = &self.session_token {
            params.push(("X-Amz-Security-Token".into(), token.clone()));
        }
        params.push(("X-Amz-SignedHeaders".into(), "host".into()));
        let canonical_query = params
            .iter()
            .map(|(k, v)| format!("{}={}", uri_encode(k, true), uri_encode(v, true)))
            .collect::<Vec<_>>()
            .join("&");

        let canonical_request = format!(
            "{method}\n{canonical_uri}\n{canonical_query}\nhost:{host}\n\nhost\nUNSIGNED-PAYLOAD"
        );
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
        let signature = hex(&hmac_sha256(&k_signing, string_to_sign.as_bytes()));

        let scheme = if self.https { "https" } else { "http" };
        Ok(format!(
            "{scheme}://{host}{canonical_uri}?{canonical_query}&X-Amz-Signature={signature}"
        ))
    }
}

/// `(YYYYMMDD, YYYYMMDDTHHMMSSZ)` in UTC for a unix timestamp.
fn amz_date(unix_secs: i64) -> (String, String) {
    let days = unix_secs.div_euclid(86_400);
    let rem = unix_secs.rem_euclid(86_400);
    // Civil-from-days (Howard Hinnant's algorithm), valid for all i64 days.
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
    let date = format!("{y:04}{m:02}{d:02}");
    let datetime = format!(
        "{date}T{:02}{:02}{:02}Z",
        rem / 3_600,
        rem % 3_600 / 60,
        rem % 60
    );
    (date, datetime)
}

/// SigV4 URI encoding: RFC 3986 unreserved characters pass through; `/` also
/// passes when encoding a path. Everything else becomes uppercase `%XX`,
/// byte-wise over UTF-8.
fn uri_encode(s: &str, encode_slash: bool) -> String {
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

    /// The published known-answer vector from the AWS SigV4 documentation
    /// ("Authenticating Requests: Using Query Parameters"): a GET of
    /// `test.txt` in `examplebucket`, us-east-1, at 2013-05-24T00:00:00Z,
    /// valid 24h, with the documented example credentials.
    #[test]
    fn aws_known_answer_vector() {
        let s3 = S3Store::new(
            "examplebucket",
            "us-east-1",
            "AKIAIOSFODNN7EXAMPLE",
            "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY",
        );
        let url = s3
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

    #[test]
    fn amz_date_formats() {
        let (date, datetime) = amz_date(1_369_353_600);
        assert_eq!(date, "20130524");
        assert_eq!(datetime, "20130524T000000Z");
        let (_, dt) = amz_date(1_369_353_600 + 3_661);
        assert_eq!(dt, "20130524T010101Z");
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
}
