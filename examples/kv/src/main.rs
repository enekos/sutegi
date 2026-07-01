//! A single-node settings / feature-flag store on sutegi's `Kv` layer over
//! SQLite — the embedded, zero-ops shape (one file on disk, no server to run
//! beyond this one). The same `Kv` API also runs over Postgres for small shared
//! state; here we lean into SQLite's single-node sweet spot.
//!
//! The store is ordinary app state: `.state(kv)`, then `c.state::<Kv<Db>>()` in
//! any handler. No `Arc<Mutex<…>>`, no per-route clones.
//!
//! ```text
//! curl -X PUT  localhost:8080/kv/config/theme -d '"dark"'
//! curl         localhost:8080/kv/config/theme          # -> "dark"
//! curl -X PUT  localhost:8080/kv/flags/beta  -d 'true'
//! curl         localhost:8080/kv/flags                 # -> {"beta": true}
//! curl -X DELETE localhost:8080/kv/config/theme
//! ```

use sutegi::orm::kv::Kv;
use sutegi::prelude::*;

type Store = Kv<Db>;

fn main() -> std::io::Result<()> {
    // One embedded file (override with KV_PATH) — pooled and Send+Sync.
    let db = Db::open(&std::env::var("KV_PATH").unwrap_or_else(|_| "kv.db".to_string()))
        .expect("open db");
    let ready = db.clone();
    let kv = Kv::new(db);
    kv.migrate().expect("migrate kv");

    App::new("kv-demo")
        .state(kv)
        .readiness(move || ready.query("SELECT 1", &[]).is_ok())
        .get("/", "Health check.", |_| "sutegi kv up")
        .get(
            "/kv/:ns/:key",
            "Read a value.",
            |c| -> Result<Response, Error> {
                match c.state::<Store>().get(ns(c), key(c))? {
                    Some(v) => Ok(json(200, &v)),
                    None => Err(Error::not_found("not found")),
                }
            },
        )
        .get("/kv/:ns", "List a namespace as an object.", |c| {
            let pairs = c.state::<Store>().scan(ns(c))?;
            Ok::<_, Error>(json(
                200,
                &Json::obj(pairs.iter().map(|(k, v)| (k.as_str(), v.clone())).collect()),
            ))
        })
        .put("/kv/:ns/:key", "Write a value (body = JSON).", |c| {
            let value = c.json()?;
            c.state::<Store>().set(ns(c), key(c), &value)?;
            Ok::<_, Error>(json(200, &value))
        })
        .delete("/kv/:ns/:key", "Delete a value.", |c| {
            let removed = c.state::<Store>().delete(ns(c), key(c))?;
            Ok::<_, Error>(json(
                200,
                &Json::obj(vec![("deleted", Json::Bool(removed))]),
            ))
        })
        .serve()
}

fn ns<'a>(c: &'a Ctx) -> &'a str {
    c.param("ns").unwrap_or("")
}
fn key<'a>(c: &'a Ctx) -> &'a str {
    c.param("key").unwrap_or("")
}
