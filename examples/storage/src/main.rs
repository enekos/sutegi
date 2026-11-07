//! A file server on sutegi's storage layer, with **the backend chosen at boot
//! and the routes never told which**: a directory on disk by default, an
//! S3/R2 bucket when `STORAGE=s3`. Plus the agent-native S3 shape —
//! `presign_upload` / `presign_download` tools that mint time-limited URLs so
//! the **agent moves the bytes itself**, straight to the object store.
//!
//! ```text
//! export OPS_TOKEN=dev-token; AUTH="Authorization: Bearer $OPS_TOKEN"
//! curl -H "$AUTH" -T report.pdf localhost:8080/files/report.pdf
//! curl -H "$AUTH" localhost:8080/files      # list
//! curl localhost:8080/files/report.pdf -o out.pdf   # reads are public
//! curl -H "$AUTH" -X DELETE localhost:8080/files/report.pdf
//! curl -H "$AUTH" -X POST localhost:8080/__tools/presign_download \
//!      -H 'content-type: application/json' \
//!      -d '{"key":"report.pdf"}'            # S3 URL (needs S3_* env)
//! ```
//!
//! Writes, deletes, listing and the agent tools all need
//! `Authorization: Bearer $OPS_TOKEN`; with `OPS_TOKEN` unset they are refused.
//! Downloads are public, but only images, PDF and plain text render inline —
//! anything else (HTML, SVG, …) is sent as an `attachment`, with `nosniff`, so
//! an uploaded file can never run script on this origin.
//!
//! The S3 credentials come from `S3_BUCKET`, `S3_ACCESS_KEY`, `S3_SECRET_KEY`
//! (`S3_REGION` defaults to `us-east-1`; `S3_ENDPOINT` points at R2/MinIO/… and
//! switches to path-style). They power presigning on their own; add
//! `STORAGE=s3` and the file routes themselves move their bytes through the
//! bucket. `S3_INSECURE=1` selects plaintext + the pure-`std` transport for an
//! in-cluster MinIO; otherwise `https` goes through the system `curl`.

use std::sync::Arc;

use sutegi::prelude::*;

fn name<'a>(c: &'a Ctx) -> &'a str {
    c.param("name").unwrap_or("")
}

fn s3_from_env() -> Option<S3Store> {
    let bucket = std::env::var("S3_BUCKET").ok()?;
    let access = std::env::var("S3_ACCESS_KEY").ok()?;
    let secret = std::env::var("S3_SECRET_KEY").ok()?;
    let region = std::env::var("S3_REGION").unwrap_or_else(|_| "us-east-1".to_string());
    let mut s3 = S3Store::new(&bucket, &region, &access, &secret);
    if let Ok(endpoint) = std::env::var("S3_ENDPOINT") {
        s3 = s3.with_endpoint(&endpoint);
    }
    Some(s3)
}

/// The storage backend as the routes see it: a trait object, so the choice
/// below is the only code in this file that knows where bytes live.
type Store = Box<dyn Storage + Send + Sync>;

/// `STORAGE=s3` puts the file routes on the bucket; anything else is a
/// directory on disk. This is the whole cost of swapping the backend.
fn store_from_env() -> Store {
    if std::env::var("STORAGE").as_deref() == Ok("s3") {
        let s3 = s3_from_env().expect("STORAGE=s3 needs S3_BUCKET / S3_ACCESS_KEY / S3_SECRET_KEY");
        return if std::env::var("S3_INSECURE").is_ok() {
            // Plaintext to a store on a trusted network path: no TLS, no
            // subprocess, pure std. SigV4 still signs the payload hash.
            Box::new(s3.insecure_http().storage(PlainHttp::new()))
        } else {
            Box::new(s3.storage(SystemCurl::new()))
        };
    }
    let root = std::env::var("STORAGE_ROOT").unwrap_or_else(|_| "files".to_string());
    Box::new(FsStorage::new(root).expect("open storage root"))
}

fn presign(s3: &Option<S3Store>, args: &Json, put: bool) -> Result<Json, Error> {
    let s3 = s3.as_ref().ok_or_else(|| {
        Error::new(
            503,
            "S3 not configured: set S3_BUCKET / S3_ACCESS_KEY / S3_SECRET_KEY",
        )
    })?;
    let key = args.get("key").and_then(Json::as_str).unwrap_or("");
    let expires = args
        .get("expires_secs")
        .and_then(Json::as_f64)
        .map(|f| f as u64)
        .unwrap_or(900);
    let url = if put {
        s3.presign_put(key, expires)?
    } else {
        s3.presign_get(key, expires)?
    };
    Ok(Json::obj(vec![
        ("url", Json::str(url)),
        ("method", Json::str(if put { "PUT" } else { "GET" })),
        ("expires_secs", Json::num(expires as f64)),
    ]))
}

/// A request guard: `Some(resp)` refuses, `None` lets it through.
type Guard = Arc<dyn Fn(&Request) -> Option<Response> + Send + Sync>;

/// `Authorization: Bearer $OPS_TOKEN`, compared in constant time by
/// [`bearer`]. With no (or an empty) token, everything it guards is refused.
fn token_guard(token: Option<String>) -> Guard {
    match token.filter(|t| !t.is_empty()) {
        Some(token) => Arc::new(bearer(&token)),
        None => Arc::new(|_req: &Request| {
            Some(Error::forbidden("set OPS_TOKEN to enable this endpoint").into_response())
        }),
    }
}

/// Types a browser may render inline from this origin. Everything else is
/// forced to download, so a stored `text/html` or `image/svg+xml` upload is
/// not stored XSS. Returns the normalised media type to send.
fn inline_type(stored: &str) -> Option<&'static str> {
    let essence = stored
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    [
        "image/png",
        "image/jpeg",
        "image/gif",
        "image/webp",
        "image/avif",
        "application/pdf",
        "text/plain",
    ]
    .into_iter()
    .find(|t| *t == essence)
}

fn main() -> std::io::Result<()> {
    app(
        store_from_env(),
        s3_from_env(),
        std::env::var("OPS_TOKEN").ok(),
    )
    .serve()
}

fn app(store: Store, s3: Option<S3Store>, ops_token: Option<String>) -> App {
    let s3_up = s3;
    let s3_down = s3_up.clone();
    // One guard for writes, deletes, listing and the agent surface: presigning
    // mints URLs for arbitrary object keys and the file routes mutate storage,
    // so none of it may be world-invocable.
    let guard = token_guard(ops_token);
    let (g_list, g_put, g_del, g_ops) = (
        Arc::clone(&guard),
        Arc::clone(&guard),
        Arc::clone(&guard),
        guard,
    );

    let presign_args = || {
        schema::object(
            vec![
                ("key", schema::string("object key, e.g. reports/q2.pdf")),
                (
                    "expires_secs",
                    schema::integer("URL lifetime in seconds (default 900, max 604800)"),
                ),
            ],
            &["key"],
        )
    };

    App::new("storage-demo")
        .state(store)
        .get("/", "Health check.", |_| "sutegi storage up")
        .get("/files", "List stored files (needs OPS_TOKEN).", move |c| {
            if let Some(denied) = g_list(c.req) {
                return Ok(denied);
            }
            let items = c.state::<Store>().list("")?;
            Ok::<_, Error>(json(
                200,
                &Json::arr(items.iter().map(ObjectMeta::to_json).collect()),
            ))
        })
        .put("/files/:name", "Store the raw request body as a file (needs OPS_TOKEN).", move |c| {
            if let Some(denied) = g_put(c.req) {
                return Ok(denied);
            }
            let ct = c.header("content-type").unwrap_or("");
            c.state::<Store>().put(name(c), &c.req.body, ct)?;
            Ok::<_, Error>(json(201, &Json::obj(vec![("key", Json::str(name(c)))])))
        })
        .get(
            "/files/:name",
            "Download a file; only images, PDF and plain text render inline.",
            |c| -> Result<Response, Error> {
                let store = c.state::<Store>();
                match store.stat(name(c))? {
                    Some(meta) => {
                        let bytes = store.get(name(c))?.unwrap_or_default();
                        let resp = Response::new(200)
                            .with_header("x-content-type-options", "nosniff")
                            .with_body(bytes);
                        Ok(match inline_type(&meta.content_type) {
                            Some(ct) => resp.with_header("content-type", ct),
                            None => resp
                                .with_header("content-type", "application/octet-stream")
                                .with_header("content-disposition", "attachment"),
                        })
                    }
                    None => Err(Error::not_found("no such file")),
                }
            },
        )
        .delete("/files/:name", "Delete a file (needs OPS_TOKEN).", move |c| {
            if let Some(denied) = g_del(c.req) {
                return Ok(denied);
            }
            let removed = c.state::<Store>().delete(name(c))?;
            Ok::<_, Error>(json(
                200,
                &Json::obj(vec![("deleted", Json::Bool(removed))]),
            ))
        })
        .tool(
            "presign_upload",
            "Mint a time-limited S3 upload URL. PUT the file bytes directly to the returned URL — they never pass through this server.",
            presign_args(),
            move |_c, args| presign(&s3_up, &args, true),
        )
        .tool(
            "presign_download",
            "Mint a time-limited S3 download URL for a stored object. GET the returned URL directly.",
            presign_args(),
            move |_c, args| presign(&s3_down, &args, false),
        )
        .ops_guard(move |req| g_ops(req))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture {
        svc: Box<dyn Fn(Request) -> Response>,
        dir: std::path::PathBuf,
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn fixture(tag: &str, token: Option<&str>) -> Fixture {
        let dir = std::env::temp_dir().join(format!("sutegi-storage-{tag}-{}", std::process::id()));
        let store: Store = Box::new(FsStorage::new(&dir).unwrap());
        let svc = app(store, None, token.map(String::from)).service();
        Fixture {
            svc: Box::new(svc),
            dir,
        }
    }

    fn call(
        f: &Fixture,
        method: Method,
        path: &str,
        headers: &[(&str, &str)],
        body: &[u8],
    ) -> Response {
        (f.svc)(Request {
            method,
            path: path.into(),
            query: String::new(),
            version: "HTTP/1.1".into(),
            headers: headers
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            body: body.to_vec(),
            peer: None,
        })
    }

    fn header<'a>(r: &'a Response, name: &str) -> Option<&'a str> {
        r.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    const AUTH: (&str, &str) = ("authorization", "Bearer s3cret");

    #[test]
    fn writes_list_and_delete_need_the_token() {
        let f = fixture("auth", Some("s3cret"));
        let ct = ("content-type", "text/plain");
        assert_eq!(
            call(&f, Method::Put, "/files/a.txt", &[ct], b"x").status,
            401
        );
        assert_eq!(call(&f, Method::Get, "/files", &[], b"").status, 401);
        assert_eq!(
            call(&f, Method::Delete, "/files/a.txt", &[], b"").status,
            401
        );
        let wrong = ("authorization", "Bearer nope");
        assert_eq!(
            call(&f, Method::Put, "/files/a.txt", &[ct, wrong], b"x").status,
            401
        );

        assert_eq!(
            call(&f, Method::Put, "/files/a.txt", &[ct, AUTH], b"x").status,
            201
        );
        assert_eq!(call(&f, Method::Get, "/files", &[AUTH], b"").status, 200);
        assert_eq!(call(&f, Method::Get, "/files/a.txt", &[], b"").status, 200);
        assert_eq!(
            call(&f, Method::Delete, "/files/a.txt", &[AUTH], b"").status,
            200
        );
    }

    #[test]
    fn everything_guarded_is_refused_without_a_token() {
        let f = fixture("closed", None);
        let ct = ("content-type", "text/plain");
        assert_eq!(
            call(&f, Method::Put, "/files/a.txt", &[ct], b"x").status,
            403
        );
        assert_eq!(call(&f, Method::Get, "/files", &[], b"").status, 403);
        assert_eq!(call(&f, Method::Get, "/__tools", &[], b"").status, 403);
    }

    #[test]
    fn active_content_downloads_as_an_attachment() {
        let f = fixture("xss", Some("s3cret"));
        for (name, ct) in [("x.html", "text/html"), ("x.svg", "image/svg+xml")] {
            let put = call(
                &f,
                Method::Put,
                &format!("/files/{name}"),
                &[("content-type", ct), AUTH],
                b"<script>1</script>",
            );
            assert_eq!(put.status, 201);
            let got = call(&f, Method::Get, &format!("/files/{name}"), &[], b"");
            assert_eq!(
                header(&got, "content-type"),
                Some("application/octet-stream")
            );
            assert_eq!(header(&got, "content-disposition"), Some("attachment"));
            assert_eq!(header(&got, "x-content-type-options"), Some("nosniff"));
        }
        let put = call(
            &f,
            Method::Put,
            "/files/p.png",
            &[("content-type", "IMAGE/PNG"), AUTH],
            b"png",
        );
        assert_eq!(put.status, 201);
        let got = call(&f, Method::Get, "/files/p.png", &[], b"");
        assert_eq!(header(&got, "content-type"), Some("image/png"));
        assert_eq!(header(&got, "content-disposition"), None);
        assert_eq!(header(&got, "x-content-type-options"), Some("nosniff"));
    }
}
