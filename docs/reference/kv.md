<!-- Generated from landing/src/Docs.svelte. Run: cd landing && pnpm docs:export -->

[← Migrations](migrations.md) · [Documentation index](../README.md) · [The agent surface →](agents.md)

# The key/value store

`Kv<B>` is a namespaced JSON key/value store over *either* backend — one table, single-statement reads and writes — handy for config, feature flags, cached values, or small shared state that does not deserve a schema. Values are arbitrary `Json`; keys are grouped by namespace.

```rust
use sutegi::orm::kv::Kv;

let kv = Kv::new(db);      // over SQLite *or* Postgres — same API
kv.migrate()?;

kv.set("config", "theme", &Json::str("dark"))?;   // namespace, key, value
let theme = kv.get("config", "theme")?;             // Option<Json>
let all   = kv.scan("flags")?;                      // Vec<(String, Json)>
let some  = kv.scan_prefix("flags", "beta_")?;
let n     = kv.count("flags")?;
kv.delete("config", "theme")?;
kv.clear("flags")?;

// It is ordinary app state — no Arc<Mutex<…>>:
App::new("settings")
    .state(kv)
    .get("/kv/:ns/:key", "Read", |c| {
        match c.state::<Kv<Db>>().get(c.param("ns").unwrap(), c.param("key").unwrap())? {
            Some(v) => Ok::<_, Error>(json(200, &v)),
            None => Err(Error::not_found("not found")),
        }
    })
    .serve()
```

On a single SQLite node it is the natural home for config and flags; on Postgres it works for small *shared* state. It is ordinary application state — no `Arc<Mutex<…>>`.

---

[← Migrations](migrations.md) · [Documentation index](../README.md) · [The agent surface →](agents.md)
