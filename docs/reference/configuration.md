<!-- Generated from landing/src/Docs.svelte. Run: cd landing && pnpm docs:export -->

[← Feature flags](features.md) · [Documentation index](../README.md) · [Directory & conventions →](layout.md)

# Configuration

`sutegi::config::Config` is a std-only 12-factor config layer: it loads a `.env` file if present, overlays the process environment (which wins), and gives you typed accessors with defaults. `require_all` fails fast and lists every missing key at once; `prefixed` scopes a group of variables.

```rust
use sutegi::config::Config;

let cfg = Config::load();                  // .env (if present) + process env (env wins)
let port    = cfg.int("PORT", 8080);
let debug   = cfg.bool("DEBUG", false);    // 1/true/yes/on  ->  true
let hosts   = cfg.list("ALLOWED_HOSTS");   // comma-separated -> Vec<String>
cfg.require_all(&["DATABASE_URL", "API_KEY"])?;   // fail fast, listing every missing key
let db_cfg  = cfg.prefixed("DB_");         // DB_HOST/DB_PORT  ->  HOST/PORT
```

You do not have to use it: the framework reads a handful of conventional variables itself, so a bare app is already 12-factor.

```dotenv
# read by .serve() itself
HOST=0.0.0.0          # bind host (default 0.0.0.0; argv[1] overrides both)
PORT=8080             # bind port (default 8080)
WORKERS=8             # HTTP worker threads (default 8)

# read by the Postgres backend (Pg::from_env / Pool::from_env)
DATABASE_URL=postgres://user:pw@host:5432/db
# …or the standard discrete vars:
PGHOST=localhost PGPORT=5432 PGUSER=postgres PGPASSWORD=… PGDATABASE=postgres

# read by the SQLite backend helper Db::open_or_memory("DATABASE_PATH")
DATABASE_PATH=app.db  # unset -> an in-memory database

# read by Mailer::from_env()
MAIL_DRIVER=log|memory|smtp|sendmail
MAIL_FROM=noreply@example.com
MAIL_HOST=… MAIL_PORT=… MAIL_USERNAME=… MAIL_PASSWORD=…
```

---

[← Feature flags](features.md) · [Documentation index](../README.md) · [Directory & conventions →](layout.md)
