<!-- Generated from landing/src/Docs.svelte. Run: cd landing && pnpm docs:export -->

[← Mail](mail.md) · [Documentation index](../README.md) · [Event sourcing →](events.md)

# File storage

The `storage` feature abstracts object storage behind one `Storage` trait (`put`/`get`/`stat`/`delete`/`list`/ `get_reader`, with traversal-validated keys). `FsStorage` writes to a local directory (atomic temp-and-rename, real streaming reads); `DbStorage` (`storage-db`) stores blobs in SQLite or Postgres — multi-pod files with no new infrastructure, honest ceiling a few MB per object; and `S3Storage` puts the same trait on a real bucket: AWS S3, Cloudflare R2, MinIO, Garage, Ceph RGW.

```rust
// One trait, three backends. Swap the type you construct, not the call sites.
let store = FsStorage::new("data/files")?;                   // single-node, one directory
// let store = DbStorage::new(pg);                            // blobs over the Backend seam
let store = S3Store::r2(&account, "media", &ak, &sk)         // …or a Cloudflare R2 bucket
    .storage(SystemCurl::new());

store.put("reports/q2.pdf", &bytes, "application/pdf")?;
let meta: Option<ObjectMeta> = store.stat("reports/q2.pdf")?;
let objects = store.list("reports/")?;
let reader = store.get_reader("reports/q2.pdf")?;            // real streaming reads
store.delete("reports/q2.pdf")?;

.get("/files/:name", "Download", |c| -> Result<Response, Error> {
    let store = c.state::<FsStorage>();
    match store.stat(c.param("name").unwrap())? {
        Some(meta) => Ok(Response::new(200)
            .with_header("content-type", &meta.content_type)
            .with_body(store.get(c.param("name").unwrap())?.unwrap_or_default())),
        None => Err(Error::not_found("no such file")),
    }
})

// Agent-native S3: mint a time-limited URL and let the agent move the bytes.
let s3 = S3Store::new("bucket", "eu-central-1", &ak, &sk);   // .with_endpoint(…) for R2/MinIO
let url = s3.presign_put("reports/q2.pdf", 900)?;            // seconds
```

## The transport seam (how S3 works with no TLS in the tree)

`S3Storage` never opens a socket itself. It hands a signed request to an `HttpTransport` — one method — and two implementations ship with it. `SystemCurl` delegates the `https` handshake and certificate verification to the system `curl`, so the crypto that must not be hand-rolled isn’t and the dependency count stays at zero. `PlainHttp` is pure `std` and **refuses `https`** rather than pretending — for a store on a trusted path: in-cluster MinIO, a sidecar Garage, a dev container. Your own client is `impl HttpTransport for MyClient`.

Every request is signed with the **real payload hash** (`x-amz-content-sha256`, never `UNSIGNED-PAYLOAD`), so a body altered in flight is refused by the store — integrity that holds even over `PlainHttp`. Uploads and downloads are `ETag`-verified when the store reports a plain MD5 (multipart and encrypted ETags are skipped, not faked; `verify_etag(false)` opts out). `list` follows continuation tokens and is bounded by `max_list_keys` (default 100,000), so a ten-million-object bucket **errors instead of silently truncating**. Signing is verified against AWS’s published known-answer vectors, for presigned URLs and signed headers alike.

`S3Store` deliberately does not implement `Storage`: minting a URL is a different contract than moving bytes. `S3Store::storage(transport)` is the crossing point, and on its own `S3Store` is a pure-std SigV4 presigner — which is the agent-native trick: mint a time-limited URL and let the client or agent move the bytes straight to the object store, never through your server.

---

[← Mail](mail.md) · [Documentation index](../README.md) · [Event sourcing →](events.md)
