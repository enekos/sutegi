//! A file server on sutegi's storage layer — [`FsStorage`] for the bytes
//! (single-node, zero-ops: one directory on disk), plus the agent-native S3
//! shape: `presign_upload` / `presign_download` tools that mint time-limited
//! URLs so the **agent moves the bytes itself**, straight to the object store.
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
//! S3 presigning activates when `S3_BUCKET`, `S3_ACCESS_KEY` and
//! `S3_SECRET_KEY` are set (`S3_REGION` defaults to `us-east-1`;
//! `S3_ENDPOINT` points at R2/MinIO/… and switches to path-style).

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
    let root = std::env::var("STORAGE_ROOT").unwrap_or_else(|_| "files".to_string());
    let store = FsStorage::new(root).expect("open storage root");
    let s3_up = s3_from_env();
    let s3_down = s3_up.clone();

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
            let items = c.state::<FsStorage>().list("")?;
            Ok::<_, Error>(json(
                200,
                &Json::arr(items.iter().map(ObjectMeta::to_json).collect()),
            ))
        })
        .put("/files/:name", "Store the raw request body as a file.", |c| {
            let ct = c.header("content-type").unwrap_or("");
            c.state::<FsStorage>().put(name(c), &c.req.body, ct)?;
            Ok::<_, Error>(json(201, &Json::obj(vec![("key", Json::str(name(c)))])))
        })
        .get(
            "/files/:name",
            "Download a file with its stored content type.",
            |c| -> Result<Response, Error> {
                let store = c.state::<FsStorage>();
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
            let removed = c.state::<FsStorage>().delete(name(c))?;
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
        .serve()
}
