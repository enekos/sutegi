//! A file server on sutegi's storage layer, with **the backend chosen at boot
//! and the routes never told which**: a directory on disk by default, an
//! S3/R2 bucket when `STORAGE=s3`. Plus the agent-native S3 shape —
//! `presign_upload` / `presign_download` tools that mint time-limited URLs so
//! the **agent moves the bytes itself**, straight to the object store.
//!
//! ```text
//! curl -T report.pdf localhost:8080/files/report.pdf
//! curl localhost:8080/files                 # list
//! curl localhost:8080/files/report.pdf -o out.pdf
//! curl -X DELETE localhost:8080/files/report.pdf
//! curl -X POST localhost:8080/__tools/presign_download \
//!      -d '{"key":"report.pdf"}'            # S3 URL (needs S3_* env)
//! ```
//!
//! The S3 credentials come from `S3_BUCKET`, `S3_ACCESS_KEY`, `S3_SECRET_KEY`
//! (`S3_REGION` defaults to `us-east-1`; `S3_ENDPOINT` points at R2/MinIO/… and
//! switches to path-style). They power presigning on their own; add
//! `STORAGE=s3` and the file routes themselves move their bytes through the
//! bucket. `S3_INSECURE=1` selects plaintext + the pure-`std` transport for an
//! in-cluster MinIO; otherwise `https` goes through the system `curl`.

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

fn main() -> std::io::Result<()> {
    let store = store_from_env();
    let s3_up = s3_from_env();
    let s3_down = s3_up.clone();
    // Gate the agent surface: presigning mints URLs for arbitrary object keys,
    // so it must not be world-invocable. `Authorization: Bearer $OPS_TOKEN`;
    // with no token set the surface is closed rather than open.
    let ops_token = std::env::var("OPS_TOKEN").ok();

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
        .get("/files", "List stored files.", |c| {
            let items = c.state::<Store>().list("")?;
            Ok::<_, Error>(json(
                200,
                &Json::arr(items.iter().map(ObjectMeta::to_json).collect()),
            ))
        })
        .put("/files/:name", "Store the raw request body as a file.", |c| {
            let ct = c.header("content-type").unwrap_or("");
            c.state::<Store>().put(name(c), &c.req.body, ct)?;
            Ok::<_, Error>(json(201, &Json::obj(vec![("key", Json::str(name(c)))])))
        })
        .get(
            "/files/:name",
            "Download a file with its stored content type.",
            |c| -> Result<Response, Error> {
                let store = c.state::<Store>();
                match store.stat(name(c))? {
                    Some(meta) => {
                        let bytes = store.get(name(c))?.unwrap_or_default();
                        Ok(Response::new(200)
                            .with_header("content-type", &meta.content_type)
                            .with_body(bytes))
                    }
                    None => Err(Error::not_found("no such file")),
                }
            },
        )
        .delete("/files/:name", "Delete a file.", |c| {
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
        .ops_guard(move |req| {
            let authorized = match &ops_token {
                Some(tok) => {
                    req.header("authorization").map(|h| h == format!("Bearer {tok}")) == Some(true)
                }
                None => false,
            };
            if authorized {
                None
            } else {
                Some(Response::new(401).with_body(b"unauthorized (set OPS_TOKEN)".to_vec()))
            }
        })
        .serve()
}
