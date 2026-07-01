//! A single-node settings / feature-flag store built on sutegi's `Kv` layer
//! over SQLite — the embedded, zero-ops shape (no server to run, one file on
//! disk). The same `Kv` API also runs over Postgres for small shared state;
//! here we lean into SQLite's single-node sweet spot.
//!
//! ```text
//! curl -X PUT  localhost:8080/kv/config/theme -d '"dark"'
//! curl         localhost:8080/kv/config/theme          # -> "dark"
//! curl -X PUT  localhost:8080/kv/flags/beta  -d 'true'
//! curl         localhost:8080/kv/flags                 # -> {"beta": true}
//! curl -X DELETE localhost:8080/kv/config/theme
//! ```

use std::sync::{Arc, Mutex};

use sutegi::orm::kv::Kv;
use sutegi::orm::{db::Db, Backend};
use sutegi::prelude::*;

type Store = Arc<Mutex<Kv<Db>>>;

fn err(code: u16, msg: impl Into<String>) -> Response {
    json(code, &Json::obj(vec![("error", Json::str(msg.into()))]))
}

fn main() -> std::io::Result<()> {
    // One embedded file — or `Db::memory()` for a throwaway store.
    let db = Db::open(&sutegi::env_or("KV_PATH", "kv.db")).expect("open db");
    let kv = Kv::new(db);
    kv.migrate().expect("migrate kv");
    let kv: Store = Arc::new(Mutex::new(kv));

    let get_kv = Arc::clone(&kv);
    let set_kv = Arc::clone(&kv);
    let del_kv = Arc::clone(&kv);
    let scan_kv = Arc::clone(&kv);
    let ready_kv = Arc::clone(&kv);

    let app = App::new("kv-demo")
        .workers(sutegi::env_or("WORKERS", "8").parse().unwrap_or(8))
        // Readiness gates traffic on the store being reachable.
        .readiness(move || {
            ready_kv
                .lock()
                .map(|kv| kv.backend().query("SELECT 1", &[]).is_ok())
                .unwrap_or(false)
        })
        .get("/", "Health check.", |_req, _p| text(200, "sutegi kv up"))
        .get("/kv/:ns/:key", "Read a value.", move |_req, p| {
            let (ns, key) = (p.get("ns"), p.get("key"));
            let (Some(ns), Some(key)) = (ns, key) else {
                return err(400, "ns and key required");
            };
            match get_kv.lock().unwrap().get(ns, key) {
                Ok(Some(v)) => json(200, &v),
                Ok(None) => err(404, "not found"),
                Err(e) => err(500, e),
            }
        })
        .get(
            "/kv/:ns",
            "List a namespace as an object.",
            move |_req, p| {
                let Some(ns) = p.get("ns") else {
                    return err(400, "ns required");
                };
                match scan_kv.lock().unwrap().scan(ns) {
                    Ok(pairs) => json(
                        200,
                        &Json::obj(pairs.iter().map(|(k, v)| (k.as_str(), v.clone())).collect()),
                    ),
                    Err(e) => err(500, e),
                }
            },
        )
        .put(
            "/kv/:ns/:key",
            "Write a value (body = JSON).",
            move |req, p| {
                let (Some(ns), Some(key)) = (p.get("ns"), p.get("key")) else {
                    return err(400, "ns and key required");
                };
                let value = match json_body(req) {
                    Ok(b) => b,
                    Err(e) => return err(400, e),
                };
                match set_kv.lock().unwrap().set(ns, key, &value) {
                    Ok(()) => json(200, &value),
                    Err(e) => err(500, e),
                }
            },
        )
        .delete("/kv/:ns/:key", "Delete a value.", move |_req, p| {
            let (Some(ns), Some(key)) = (p.get("ns"), p.get("key")) else {
                return err(400, "ns and key required");
            };
            match del_kv.lock().unwrap().delete(ns, key) {
                Ok(removed) => json(200, &Json::obj(vec![("deleted", Json::Bool(removed))])),
                Err(e) => err(500, e),
            }
        });

    let addr = std::env::args().nth(1).unwrap_or_else(|| {
        format!(
            "{}:{}",
            sutegi::env_or("HOST", "0.0.0.0"),
            sutegi::env_or("PORT", "8080")
        )
    });
    println!("sutegi kv-demo on http://{addr}");
    println!("  ops:  /__health | /__ready | /__metrics | /__introspect");
    println!("  app:  GET/PUT/DELETE /kv/:ns/:key | GET /kv/:ns");
    app.run(&addr)
}
