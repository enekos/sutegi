//! End-to-end [`S3Storage`] over a real socket: the client signs, a tiny
//! in-process S3-compatible store answers, and every layer — SigV4 headers,
//! HTTP/1.1 framing, `ListObjectsV2` XML, continuation tokens, ETag
//! verification — is exercised against bytes on the wire rather than a mock.
//!
//! The stub enforces the parts a real store enforces and we can check cheaply:
//! `Authorization` must be present and well-formed, and
//! `x-amz-content-sha256` must equal the SHA-256 of the body actually received
//! (the guarantee that makes plaintext transport tolerable). The signature
//! *value* is pinned separately, against AWS's published known-answer vectors,
//! in `s3.rs`.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use sutegi_crypto::{hex, md5, sha256};
use sutegi_storage::{PlainHttp, S3Storage, S3Store, Storage};

const BUCKET: &str = "test-bucket";

#[derive(Default)]
struct Bucket {
    objects: BTreeMap<String, (Vec<u8>, String)>,
    /// Keys the stub answers with the wrong bytes, to prove the client checks.
    poison: Vec<String>,
    requests: usize,
}

/// Boot the stub on an ephemeral port; returns its `host:port` and state.
fn stub() -> (String, Arc<Mutex<Bucket>>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().unwrap().to_string();
    let state = Arc::new(Mutex::new(Bucket::default()));
    let shared = Arc::clone(&state);
    std::thread::spawn(move || {
        for conn in listener.incoming() {
            let Ok(conn) = conn else { break };
            // One request per connection: the client sends `Connection: close`.
            if let Err(e) = serve_one(conn, &shared) {
                eprintln!("stub: {e}");
            }
        }
    });
    (addr, state)
}

fn store(addr: &str) -> S3Storage<PlainHttp> {
    S3Store::new(BUCKET, "us-east-1", "AKIAEXAMPLE", "secret-key")
        .with_endpoint(addr)
        .insecure_http()
        .storage(PlainHttp::new())
}

fn serve_one(mut conn: TcpStream, state: &Arc<Mutex<Bucket>>) -> Result<(), String> {
    let mut reader = BufReader::new(conn.try_clone().map_err(|e| e.to_string())?);
    let mut line = String::new();
    reader.read_line(&mut line).map_err(|e| e.to_string())?;
    let mut parts = line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_string();
    let target = parts.next().unwrap_or_default().to_string();

    let mut headers: Vec<(String, String)> = Vec::new();
    loop {
        let mut h = String::new();
        reader.read_line(&mut h).map_err(|e| e.to_string())?;
        let h = h.trim_end();
        if h.is_empty() {
            break;
        }
        let (k, v) = h.split_once(':').ok_or("bad header")?;
        headers.push((k.trim().to_ascii_lowercase(), v.trim().to_string()));
    }
    let header = |name: &str| {
        headers
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.clone())
            .unwrap_or_default()
    };
    let length: usize = header("content-length").parse().unwrap_or(0);
    let mut body = vec![0u8; length];
    reader.read_exact(&mut body).map_err(|e| e.to_string())?;

    // What a real store checks before it looks at the request at all.
    let auth = header("authorization");
    assert!(
        auth.starts_with("AWS4-HMAC-SHA256 Credential=AKIAEXAMPLE/")
            && auth.contains(",SignedHeaders=")
            && auth.contains(",Signature="),
        "malformed Authorization: {auth:?}"
    );
    assert_eq!(header("host"), conn.local_addr().unwrap().to_string());
    assert_eq!(
        header("x-amz-content-sha256"),
        hex(&sha256(&body)),
        "payload hash does not cover the body received"
    );

    let (path, query) = match target.split_once('?') {
        Some((p, q)) => (p.to_string(), q.to_string()),
        None => (target.clone(), String::new()),
    };
    let path = percent_decode(&path);
    let prefix = format!("/{BUCKET}/");
    let key = path.strip_prefix(&prefix).unwrap_or_default().to_string();

    let resp = {
        let mut b = state.lock().unwrap();
        b.requests += 1;
        match method.as_str() {
            _ if !path.starts_with(&prefix) => response(
                404,
                &[],
                format!(
                    "<Error><Code>NoSuchBucket</Code><Message>The specified bucket \
                     does not exist</Message><Resource>{path}</Resource></Error>"
                )
                .into_bytes(),
            ),
            _ if key.is_empty() => list(&b, &query),
            "PUT" => {
                let etag = hex(&md5(&body));
                b.objects
                    .insert(key, (body, header("content-type").to_string()));
                response(200, &[("ETag", &format!("\"{etag}\""))], Vec::new())
            }
            "GET" | "HEAD" => match b.objects.get(&key) {
                Some((bytes, ct)) => {
                    let etag = hex(&md5(bytes));
                    let served = if b.poison.contains(&key) {
                        b"corrupted in flight".to_vec()
                    } else {
                        bytes.clone()
                    };
                    let head = [
                        ("ETag", format!("\"{etag}\"")),
                        ("Content-Type", ct.clone()),
                        ("Last-Modified", "Mon, 10 Aug 2026 12:00:00 GMT".to_string()),
                    ];
                    let head: Vec<(&str, &str)> =
                        head.iter().map(|(k, v)| (*k, v.as_str())).collect();
                    if method == "HEAD" {
                        // A HEAD advertises the length it would have sent.
                        let mut h = head.clone();
                        let len = served.len().to_string();
                        h.push(("Content-Length", &len));
                        format!(
                            "HTTP/1.1 200 OK\r\n{}Connection: close\r\n\r\n",
                            h.iter()
                                .map(|(k, v)| format!("{k}: {v}\r\n"))
                                .collect::<String>()
                        )
                        .into_bytes()
                    } else {
                        response(200, &head, served)
                    }
                }
                None => response(404, &[], b"<Error><Code>NoSuchKey</Code></Error>".to_vec()),
            },
            "DELETE" => {
                b.objects.remove(&key);
                response(204, &[], Vec::new())
            }
            other => response(
                405,
                &[],
                format!("<Error><Code>MethodNotAllowed</Code><Message>{other}</Message></Error>")
                    .into_bytes(),
            ),
        }
    };
    conn.write_all(&resp).map_err(|e| e.to_string())?;
    conn.flush().map_err(|e| e.to_string())
}

fn response(status: u16, headers: &[(&str, &str)], body: Vec<u8>) -> Vec<u8> {
    let mut out = format!(
        "HTTP/1.1 {status} {}\r\n",
        if status < 300 { "OK" } else { "Error" }
    );
    for (k, v) in headers {
        out.push_str(&format!("{k}: {v}\r\n"));
    }
    out.push_str(&format!(
        "Content-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    ));
    let mut out = out.into_bytes();
    out.extend_from_slice(&body);
    out
}

/// `ListObjectsV2`, with `encoding-type=url` keys and real continuation tokens.
fn list(b: &Bucket, query: &str) -> Vec<u8> {
    let params: BTreeMap<String, String> = query
        .split('&')
        .filter_map(|kv| kv.split_once('='))
        .map(|(k, v)| (percent_decode(k), percent_decode(v)))
        .collect();
    assert_eq!(params.get("list-type").map(String::as_str), Some("2"));
    assert_eq!(params.get("encoding-type").map(String::as_str), Some("url"));
    let want = params.get("prefix").cloned().unwrap_or_default();
    let max: usize = params
        .get("max-keys")
        .and_then(|m| m.parse().ok())
        .unwrap_or(1000);
    let after = params.get("continuation-token").cloned();

    let matching: Vec<&String> = b
        .objects
        .keys()
        .filter(|k| k.starts_with(&want))
        .filter(|k| after.as_ref().map_or(true, |a| *k > a))
        .collect();
    let page: Vec<&&String> = matching.iter().take(max).collect();
    let truncated = matching.len() > page.len();

    let mut xml = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?><ListBucketResult>");
    for key in &page {
        let (bytes, _) = &b.objects[**key];
        xml.push_str(&format!(
            "<Contents><Key>{}</Key>\
             <LastModified>2026-08-10T12:00:00.000Z</LastModified>\
             <ETag>&quot;{}&quot;</ETag><Size>{}</Size>\
             <StorageClass>STANDARD</StorageClass></Contents>",
            url_encode(key),
            hex(&md5(bytes)),
            bytes.len()
        ));
    }
    xml.push_str(&format!("<IsTruncated>{truncated}</IsTruncated>"));
    if truncated {
        xml.push_str(&format!(
            "<NextContinuationToken>{}</NextContinuationToken>",
            url_encode(page.last().unwrap())
        ));
    }
    xml.push_str("</ListBucketResult>");
    response(
        200,
        &[("Content-Type", "application/xml")],
        xml.into_bytes(),
    )
}

fn url_encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match (b[i], i + 2 < b.len()) {
            (b'%', true) => {
                match u8::from_str_radix(std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or("zz"), 16)
                {
                    Ok(byte) => {
                        out.push(byte);
                        i += 3;
                    }
                    Err(_) => {
                        out.push(b[i]);
                        i += 1;
                    }
                }
            }
            _ => {
                out.push(b[i]);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

// ------------------------------------------------------------------- tests

#[test]
fn full_lifecycle_over_the_wire() {
    let (addr, state) = stub();
    let s = store(&addr);

    // put → get → stat → exists → delete, the whole trait against a socket.
    s.put(
        "reports/q2 final+draft.pdf",
        b"%PDF-1.7 body",
        "application/pdf",
    )
    .unwrap();
    assert_eq!(
        s.get("reports/q2 final+draft.pdf").unwrap().unwrap(),
        b"%PDF-1.7 body"
    );

    let meta = s.stat("reports/q2 final+draft.pdf").unwrap().unwrap();
    assert_eq!(meta.key, "reports/q2 final+draft.pdf");
    assert_eq!(meta.size, 13);
    assert_eq!(meta.content_type, "application/pdf");
    assert_eq!(meta.modified, 1_786_363_200); // 2026-08-10T12:00:00Z
    assert!(s.exists("reports/q2 final+draft.pdf").unwrap());

    // A key that never existed, and one that stops existing.
    assert_eq!(s.get("nope.txt").unwrap(), None);
    assert_eq!(s.stat("nope.txt").unwrap(), None);
    assert!(!s.delete("nope.txt").unwrap());
    assert!(s.delete("reports/q2 final+draft.pdf").unwrap());
    assert!(!s.exists("reports/q2 final+draft.pdf").unwrap());

    // Overwrite keeps the newest bytes and the newest content type.
    s.put("x.bin", b"one", "application/x-one").unwrap();
    s.put("x.bin", b"two!", "application/x-two").unwrap();
    assert_eq!(s.get("x.bin").unwrap().unwrap(), b"two!");
    assert_eq!(
        s.stat("x.bin").unwrap().unwrap().content_type,
        "application/x-two"
    );
    assert!(state.lock().unwrap().requests > 10);
}

#[test]
fn get_reader_streams_the_object() {
    let (addr, _) = stub();
    let s = store(&addr);
    s.put("r.txt", b"stream me", "text/plain").unwrap();
    let mut buf = Vec::new();
    s.get_reader("r.txt")
        .unwrap()
        .unwrap()
        .read_to_end(&mut buf)
        .unwrap();
    assert_eq!(buf, b"stream me");
    assert!(s.get_reader("missing.txt").unwrap().is_none());
}

#[test]
fn tampered_download_is_caught_on_the_wire() {
    let (addr, state) = stub();
    let s = store(&addr);
    s.put("secret.txt", b"the real bytes", "text/plain")
        .unwrap();
    state.lock().unwrap().poison.push("secret.txt".to_string());

    let err = s.get("secret.txt").unwrap_err();
    assert!(err.contains("etag mismatch"), "{err}");
    // Same response, integrity checking off: the caller gets the bad bytes,
    // which is exactly what opting out means.
    let lax = store(&addr).verify_etag(false);
    assert_eq!(
        lax.get("secret.txt").unwrap().unwrap(),
        b"corrupted in flight"
    );
}

#[test]
fn list_paginates_across_continuation_tokens() {
    let (addr, state) = stub();
    let s = store(&addr);
    // Two full pages plus a remainder: 2400 keys is 3 round trips, and the
    // client must stitch them without losing or duplicating one.
    {
        let mut b = state.lock().unwrap();
        for i in 0..2400 {
            b.objects.insert(
                format!("blobs/{i:05}.bin"),
                (vec![b'x'; 3], "application/octet-stream".to_string()),
            );
        }
        b.objects
            .insert("other/z.txt".to_string(), (b"z".to_vec(), String::new()));
        b.requests = 0;
    }

    let objs = s.list("blobs/").unwrap();
    assert_eq!(objs.len(), 2400);
    assert_eq!(objs[0].key, "blobs/00000.bin");
    assert_eq!(objs[2399].key, "blobs/02399.bin");
    assert!(objs.windows(2).all(|w| w[0].key < w[1].key), "sorted");
    assert!(objs.iter().all(|o| o.size == 3));
    assert_eq!(state.lock().unwrap().requests, 3, "1000 + 1000 + 400");

    // An empty prefix lists everything; a prefix that matches nothing is empty.
    assert_eq!(s.list("").unwrap().len(), 2401);
    assert!(s.list("nothing/").unwrap().is_empty());

    // And the cap refuses to answer rather than answering partially.
    let err = store(&addr).max_list_keys(500).list("blobs/").unwrap_err();
    assert!(err.contains("exceeds max_list_keys"), "{err}");
}

#[test]
fn keys_needing_encoding_survive_the_round_trip() {
    let (addr, _) = stub();
    let s = store(&addr);
    for key in [
        "unicode/reçu-café.txt",
        "spaces/a b c.txt",
        "plus/a+b=c.txt",
        "punct/(paren)[brack]&amp.txt",
        "deep/a/b/c/d/e.bin",
    ] {
        s.put(key, key.as_bytes(), "text/plain")
            .unwrap_or_else(|e| panic!("put {key}: {e}"));
        assert_eq!(
            s.get(key).unwrap().unwrap(),
            key.as_bytes(),
            "get {key} came back wrong"
        );
    }
    // The same keys survive the XML listing, which encodes them differently
    // than the request path does.
    let listed: Vec<String> = s.list("").unwrap().into_iter().map(|o| o.key).collect();
    assert!(
        listed.contains(&"unicode/reçu-café.txt".to_string()),
        "{listed:?}"
    );
    assert!(
        listed.contains(&"punct/(paren)[brack]&amp.txt".to_string()),
        "{listed:?}"
    );
    assert_eq!(listed.len(), 5);
}

#[test]
fn store_errors_become_useful_messages() {
    let (addr, _) = stub();
    // A bucket the store does not have: the XML `<Code>`/`<Message>` reach the
    // caller, not a byte dump — and a 404 on a *write* is an error, never a
    // quietly-successful no-op.
    let s = S3Store::new("wrong-bucket", "us-east-1", "AKIAEXAMPLE", "secret-key")
        .with_endpoint(&addr)
        .insecure_http()
        .storage(PlainHttp::new());
    let err = s.put("k.txt", b"x", "text/plain").unwrap_err();
    assert!(err.contains("HTTP 404"), "{err}");
    assert!(err.contains("NoSuchBucket"), "{err}");
    assert!(err.contains("The specified bucket does not exist"), "{err}");
    assert!(err.contains("s3 put 'k.txt'"), "{err}");

    // A missing *bucket* on a read is indistinguishable from a missing key at
    // the HTTP level, so `get` reports absence — the documented consequence of
    // 404 meaning both things.
    assert_eq!(s.get("k.txt").unwrap(), None);
}

#[test]
fn a_dead_endpoint_fails_fast_and_clearly() {
    // Port 1 on loopback: nothing listens, and the error says so rather than
    // hanging or surfacing as a missing object.
    let s = S3Store::new(BUCKET, "us-east-1", "AK", "SK")
        .with_endpoint("127.0.0.1:1")
        .insecure_http()
        .storage(PlainHttp::new());
    let err = s.get("k.txt").unwrap_err();
    assert!(err.contains("connect"), "{err}");
}

/// The `https` transport's plumbing — config on stdin, temp-file upload,
/// `--head` for HEAD, `-i` header parsing — driven against the same stub over
/// plaintext (`allow_http`). TLS itself is `curl`'s business, not ours; what is
/// ours is everything around it.
#[test]
fn system_curl_transport_moves_bytes() {
    let curl_present = std::process::Command::new("curl")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success());
    if !curl_present {
        eprintln!("skipping: no curl on PATH");
        return;
    }

    let (addr, _) = stub();
    let s = S3Store::new(BUCKET, "us-east-1", "AKIAEXAMPLE", "secret-key")
        .with_endpoint(&addr)
        .insecure_http()
        .storage(sutegi_storage::SystemCurl::new().allow_http());

    s.put("via/curl.txt", b"through a subprocess", "text/plain")
        .unwrap();
    assert_eq!(
        s.get("via/curl.txt").unwrap().unwrap(),
        b"through a subprocess"
    );
    let meta = s.stat("via/curl.txt").unwrap().unwrap();
    assert_eq!((meta.size, meta.content_type.as_str()), (20, "text/plain"));
    assert_eq!(s.list("via/").unwrap().len(), 1);
    assert!(s.delete("via/curl.txt").unwrap());
    assert_eq!(s.get("via/curl.txt").unwrap(), None);

    // A body big enough to cross curl's Expect: 100-continue threshold, whose
    // interim header block the parser has to skip.
    let big = vec![b'z'; 2 * 1024 * 1024];
    s.put("via/big.bin", &big, "application/octet-stream")
        .unwrap();
    assert_eq!(s.get("via/big.bin").unwrap().unwrap().len(), big.len());

    // No staged upload survives the call.
    let leftovers = std::fs::read_dir(std::env::temp_dir())
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().starts_with("sutegi-s3-"))
        .count();
    assert_eq!(leftovers, 0, "temp upload files leaked");
}

#[test]
fn presigning_and_moving_bytes_share_one_credential() {
    let (addr, _) = stub();
    let s = store(&addr);
    s.put("shared.txt", b"hi", "text/plain").unwrap();
    let url = s.presigner().presign_get("shared.txt", 300).unwrap();
    assert!(
        url.starts_with(&format!("http://{addr}/{BUCKET}/shared.txt?")),
        "{url}"
    );
    assert!(url.contains("X-Amz-Signature="));
}
