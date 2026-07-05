<script lang="ts">
  import { onMount } from 'svelte';
  import { Flame, GitBranch, ArrowLeft, ArrowRight, Menu, X } from 'lucide-svelte';

  // Section to scroll to on load / hash change (from #/docs/<section>)
  let { section = '' }: { section?: string } = $props();

  // --- sidebar structure (Laravel-style grouped nav) ---
  const nav = [
    {
      group: 'Prologue',
      items: [
        { id: 'introduction', title: 'Introduction' },
        { id: 'philosophy', title: 'Philosophy & the bet' },
        { id: 'honesty', title: 'Is it production-ready?' },
      ],
    },
    {
      group: 'Getting started',
      items: [
        { id: 'installation', title: 'Installation' },
        { id: 'first-app', title: 'Your first app' },
        { id: 'features', title: 'Feature flags' },
        { id: 'configuration', title: 'Configuration' },
        { id: 'layout', title: 'Directory & conventions' },
      ],
    },
    {
      group: 'The basics',
      items: [
        { id: 'routing', title: 'Routing' },
        { id: 'requests', title: 'Requests & the Ctx' },
        { id: 'responses', title: 'Responses & errors' },
        { id: 'middleware', title: 'Middleware & groups' },
        { id: 'validation', title: 'Validation' },
      ],
    },
    {
      group: 'Database',
      items: [
        { id: 'models', title: 'Models' },
        { id: 'queries', title: 'The query builder' },
        { id: 'backend', title: 'Backends: SQLite & Postgres' },
        { id: 'migrations', title: 'Migrations' },
        { id: 'kv', title: 'The key/value store' },
      ],
    },
    {
      group: 'Agents & realtime',
      items: [
        { id: 'agents', title: 'The agent surface' },
        { id: 'tools', title: 'Defining tools' },
        { id: 'streaming', title: 'Streaming & SSE' },
        { id: 'queues', title: 'Queues' },
      ],
    },
    {
      group: 'Framework services',
      items: [
        { id: 'auth', title: 'Authentication' },
        { id: 'sessions', title: 'Sessions' },
        { id: 'mail', title: 'Mail' },
        { id: 'storage', title: 'File storage' },
        { id: 'events', title: 'Event sourcing' },
        { id: 'templates', title: 'Templates' },
      ],
    },
    {
      group: 'Architecture & deployment',
      items: [
        { id: 'hexagonal', title: 'Hexagonal architecture' },
        { id: 'testing', title: 'Testing' },
        { id: 'ops', title: 'Operational endpoints' },
        { id: 'deploying', title: 'Deploying' },
        { id: 'security', title: 'Security posture' },
      ],
    },
  ];

  // --- scrollspy ---
  let active = $state('introduction');
  onMount(() => {
    const io = new IntersectionObserver(
      (entries) => entries.forEach((e) => { if (e.isIntersecting) active = e.target.id; }),
      { rootMargin: '-10% 0px -80% 0px', threshold: 0 },
    );
    document.querySelectorAll('article[id]').forEach((el) => io.observe(el));
    return () => io.disconnect();
  });

  // --- deep-link scroll (#/docs/<section>) ---
  function scrollToSection(id: string) {
    const el = document.getElementById(id);
    if (el) el.scrollIntoView({ behavior: 'smooth', block: 'start' });
    else window.scrollTo({ top: 0 });
  }
  onMount(() => {
    if (section) setTimeout(() => scrollToSection(section), 40);
    else window.scrollTo({ top: 0 });
  });
  $effect(() => { if (section) scrollToSection(section); });

  // --- mobile sidebar ---
  let menuOpen = $state(false);

  // --- copy ---
  let copiedKey = $state('');
  function copy(text: string, key: string) {
    navigator.clipboard.writeText(text);
    copiedKey = key;
    setTimeout(() => (copiedKey = ''), 1400);
  }

  // --- code samples (strings so braces stay literal) ---
  const cInstall = `# The default features suit most apps: derive + orm + validate + ai.
cargo add sutegi

# In Cargo.toml, pick the pillars you want. The HTTP core is always present.
[dependencies]
# Single-node app with the SQLite backend + graceful shutdown:
sutegi = { version = "*", features = ["sqlite", "graceful"] }

# Multi-pod app on Postgres, with the durable queue:
# sutegi = { version = "*", features = ["postgres", "queue", "graceful"] }

# Nothing but the HTTP core — ~394 KB, no ORM, no agent layer:
# sutegi = { version = "*", default-features = false }`;

  const cFirstApp = `use sutegi::prelude::*;

fn main() -> std::io::Result<()> {
    App::new("hello")
        .get("/", "Health check", |_| "sutegi up")
        .get("/hello/:name", "Greet someone", |c| {
            format!("hi, {}", c.param("name").unwrap_or("world"))
        })
        .serve()   // reads HOST/PORT/WORKERS (or argv[1]); drains on SIGTERM
}`;

  const cFirstRun = `cargo run
# [sutegi] hello on http://0.0.0.0:8080

curl localhost:8080/hello/ada        # -> hi, ada
curl localhost:8080/__introspect     # the whole app surface, as JSON`;

  const cConfig = `use sutegi::config::Config;

let cfg = Config::load();                  // .env (if present) + process env (env wins)
let port    = cfg.int("PORT", 8080);
let debug   = cfg.bool("DEBUG", false);    // 1/true/yes/on  ->  true
let hosts   = cfg.list("ALLOWED_HOSTS");   // comma-separated -> Vec<String>
cfg.require_all(&["DATABASE_URL", "API_KEY"])?;   // fail fast, listing every missing key
let db_cfg  = cfg.prefixed("DB_");         // DB_HOST/DB_PORT  ->  HOST/PORT`;

  const cRouting = `App::new("api")
    .get("/", "Health check", |_| "ok")
    .get("/todos/:id", "Show a todo", |c| {
        format!("todo #{}", c.param("id").unwrap_or("?"))
    })
    .post("/todos", "Create a todo", |c| {
        let body = c.json()?;                 // the parsed request body
        Ok::<_, Error>((201, body))
    })
    .put("/todos/:id", "Replace a todo", |_| status(204))
    .delete("/todos/:id", "Delete a todo", |_| status(204))
    // Group a prefix + shared middleware, then register routes inside it:
    .group("/admin", vec![mw(require_key)], |g| {
        g.get("/stats", "Admin stats", |_| "…")
    })
    .serve()`;

  const cRequests = `.post("/search", "Search", |c| -> Result<Json, Error> {
    // Path & query
    let id    = c.param("id");                    // Option<&str>
    let query = c.query();                        // BTreeMap<String, String> of ?a=b
    let page  = c.query().get("page").cloned();

    // Headers, cookies, the raw body
    let auth  = c.header("authorization");        // Option<&str>
    let bytes = &c.req.body;                       // &[u8] — the raw request body

    // Bodies, parsed
    let body: Json = c.json()?;                    // application/json
    let form       = c.form();                     // application/x-www-form-urlencoded

    // Shared application state, registered once with .state(...)
    let db = c.db::<Db>();                          // the pooled DB handle
    Ok(body)
})`;

  const cResponses = `// A handler returns anything that is IntoResponse:
|_| "a string"                                  // 200 text/plain
|_| Json::obj(vec![("ok", Json::Bool(true))])   // 200 application/json
|_| (201, some_json)                            // an explicit (status, body)
|_| status(204)                                 // a bare status code
|_| redirect("/login")                          // 302
|c| c.model::<Todo, Db>("id").map(|t| t.to_json())  // Result — ? just works

// Errors carry a status, a message, and optional per-field detail.
// Returning Err(...) from a handler maps to the right HTTP response.
Err(Error::not_found("no such todo"))           // 404
Err(Error::unauthorized("log in first"))        // 401
Err(Error::unprocessable("bad shape")           // 422 with structured fields
    .with_fields(errors.to_json()))`;

  const cMiddleware = `// Before-middleware returns Some(Response) to short-circuit, None to continue.
fn require_key(req: &Request) -> Option<Response> {
    match req.header("x-api-key") {
        Some("secret") => None,                       // allow
        _ => Some(status(401)),                       // block
    }
}

App::new("api")
    // App-wide middleware:
    .middleware(logger())                             // log every request
    .middleware(rate_limit(100, Duration::from_secs(60)))
    .after(secure_headers())                          // after-middleware rewrites the response
    .after(cors("https://app.example.com"))
    // Or scope middleware to a group:
    .group("/admin", vec![mw(require_key)], |g| {
        g.get("/users", "List users", |_| "…")
    })
    .serve()`;

  const cValidation = `#[derive(Model, Validate)]
struct Todo {
    #[model(primary)]
    id: i64,
    #[validate(required, str, min_len = 1, max_len = 200)]
    title: String,
    done: bool,
}

// (a) Model-driven: the ruleset was generated by #[derive(Validate)].
.post("/todos", "Create", |c| {
    let todo: Todo = c.validated()?;   // parse + validate + hydrate, or 422
    let id = todo.save(c.db::<Db>())?;
    Ok::<_, Error>((201, Todo { id, ..todo }.to_json()))
})

// (b) Ad-hoc: a ruleset for a shape that has no model.
let rules = Ruleset::new()
    .field("email", &[Rule::Required, Rule::Email])
    .field("age",   &[Rule::Integer, Rule::Between(18.0, 120.0)])
    .field("site",  &[Rule::Url])
    .field("password_confirmation", &[Rule::Same("password".into())]);
let body = c.validate(&rules)?;   // Err -> { "email": ["must be a valid email"] }`;

  const cModels = `#[derive(Model, Validate)]
#[model(table = "todos")]          // omit to infer snake_case + plural
struct Todo {
    #[model(primary)]
    id: i64,                        // the DB assigns this on insert
    title: String,
    done: bool,                     // round-trips as a real bool
    note: Option<String>,           // Option<T> -> a nullable column
    #[model(skip)]
    cached: bool,                   // not persisted; default-initialised
}

let db = Db::open_or_memory("DATABASE_PATH");   // pooled, Send + Sync + Clone
Todo::migrate(&db).unwrap();

let all: Vec<Todo> = Todo::all_typed(&db)?;               // typed reads
let one: Option<Todo> = Todo::find_typed(&db, 1.into())?;
let count = Todo::count(&db)?;
let mut todo = Todo { id: 0, title: "ship".into(), done: false, note: None, cached: false };
let id = todo.save(&db)?;                                  // insert; returns the new pk

// In a handler, route-model binding parses the param, loads the row, or 404s:
.get("/todos/:id", "show", |c| c.model::<Todo, Db>("id").map(|t| t.to_json()))`;

  const cQueries = `// The parameterized query builder — never string-concatenated, and identifiers
// are guarded against injection (important when an AI tool arg reaches a column).
let overdue = Todo::query()
    .filter("done", "=", false.into())
    .filter("due", "<", now.into())
    .or_group(&[("priority", "=", "high".into()),
                ("pinned",   "=", true.into())])
    .where_not_null("assignee")
    .like("title", "%urgent%")
    .order_by("due", false)          // ascending
    .limit(20)
    .offset(0);

let rows: Vec<Todo> = db.fetch(&overdue)?;      // typed
let page: Page<Todo> = db.paginate_typed(&overdue, 1, 20)?;  // .items / .total / .has_next()

// Raw SQL is always the escape hatch:
let rows = db.query("SELECT count(*) AS n FROM todos WHERE done = ?", &[true.into()])?;`;

  const cBackend = `// Single node — an embedded file, nothing to run.
let db = Db::open_or_memory("DATABASE_PATH");

// Multi-pod — the SAME model code, Postgres underneath.
let pg = Pg::from_env()?;          // reads the PG_* / DATABASE_URL environment
Todo::migrate(&pg).unwrap();
let all = Todo::all_typed(&pg)?;   // an identical call site

// Write your domain against the trait and choose the store at boot:
fn active_count(store: &impl Backend) -> Result<i64, String> {
    Todo::query().filter("done", "=", false.into()).count_on(store)
}`;

  const cMigrations = `use sutegi::prelude::*;

fn migrations() -> Migrator {
    Migrator::new().add(Migration::reversible(
        "20260701_000001",                       // ordered id
        "create_todos",
        |db| db.migrate_schema(&Todo::schema()), // up
        |db| db.execute("DROP TABLE todos", &[]).map(|_| ()), // down
    ))
}

fn main() -> std::io::Result<()> {
    let db = Db::open_or_memory("DATABASE_PATH");
    // \`myapp migrate | migrate:status | migrate:rollback\` runs and exits:
    if sutegi::migrate::dispatch(&migrations(), &db) { return Ok(()); }
    migrations().run(&db).expect("migrate");     // otherwise apply pending, then serve
    App::new("todo").state(db).serve()
}`;

  const cKv = `use sutegi::orm::kv::Kv;

let kv = Kv::new(db);      // over SQLite *or* Postgres — same API
kv.migrate()?;

kv.set("config", "theme", &Json::str("dark"))?;   // namespace, key, value
let theme = kv.get("config", "theme")?;             // Option<Json>
let all   = kv.scan("flags")?;                      // Vec<(String, Json)>
kv.delete("config", "theme")?;

// It is ordinary app state — no Arc<Mutex<…>>:
App::new("settings")
    .state(kv)
    .get("/kv/:ns/:key", "Read", |c| {
        match c.state::<Kv<Db>>().get(c.param("ns").unwrap(), c.param("key").unwrap())? {
            Some(v) => Ok::<_, Error>(json(200, &v)),
            None => Err(Error::not_found("not found")),
        }
    })
    .serve()`;

  const cAgents = `curl localhost:8080/__introspect
# { "framework": "sutegi", "name": "...", "routes": [...], "models": [...], "tools": [...] }

curl localhost:8080/__tools
# [ { "name": "create_todo", "description": "...", "input_schema": {...}, "streaming": false } ]

curl -X POST localhost:8080/__tools/create_todo -d '{"title":"ship sutegi"}'
# args are validated against the tool's schema -> 422 on a bad shape`;

  const cTools = `App::new("todo")
    .state(db)
    // A unary tool: the closure gets schema-validated args and shared state.
    .tool("create_todo", "Create a todo with the given title.",
        schema::object(vec![("title", schema::string("the todo's title"))], &["title"]),
        |c, args| {
            let todo = Todo::from_input(&args)?;      // args already validated
            let id = todo.save(c.db::<Db>())?;
            Ok(Todo { id, ..todo }.to_json())
        })
    // A streaming tool: write Server-Sent Events to the sink.
    .stream_tool("stream_answer", "Stream an answer token by token.",
        schema::object(vec![("prompt", schema::string("the prompt"))], &["prompt"]),
        |_c, args, sink| {
            let prompt = args.get("prompt").and_then(Json::as_str).unwrap_or("");
            for token in prompt.split(' ') { sink.data(token)?; }
            sink.event("done", "{}")
        })
    .serve()`;

  const cStreaming = `.get("/stream", "SSE demo", |_| sse(|sink| {
    for token in answer().split(' ') {
        sink.data(token)?;          // each frame is flushed immediately
    }
    sink.event("done", "{}")        // a named event
}))

// Raw byte streams work the same way via stream(status, content_type, producer).`;

  const cQueues = `use sutegi::queue::{Queue, Workers};

let mut queue = Queue::new(Pg::from_env()?.pool().clone());
queue.migrate()?;                              // creates the sutegi_jobs table
queue.register("notify", |payload| {
    let to = payload.get("to").and_then(Json::as_str).unwrap_or("");
    /* send the email … */ Ok(())             // Err -> retried with backoff
});

// Enqueue from a handler; return immediately.
queue.dispatch("notify", Json::obj(vec![("to", Json::str("a@b.com"))]))?;

// Start N worker threads. Any pod can claim the next job (FOR UPDATE SKIP LOCKED).
let workers: Workers = std::sync::Arc::new(queue).start(4);`;

  const cAuth = `use std::sync::Arc;

let users = Users::new(db.clone());       // PBKDF2-HMAC-SHA256, 600k iters (OWASP)
users.migrate()?;
let tokens = Arc::new(Tokens::new(db.clone()));
tokens.migrate()?;

let auth = Arc::new(Auth::new(
    users,
    Sessions::new(secret.as_bytes()),     // .insecure() drops Secure for local http://
));

App::new("app")
    .state(auth.clone())
    .post("/register", "Sign up", move |c| {
        let body = c.json()?;
        let user = auth_reg.users.register(email, password, name).map_err(Error::unprocessable)?;
        Ok::<_, Error>(auth_reg.login(c.req, &user, json(201, &user.to_json())))
    })
    .post("/login", "Log in", move |c| { /* authenticate -> auth.login(...) */ })
    .get("/me", "Current user", move |c| match auth_me.current(c.req)? {
        Some(u) => Ok::<_, Error>(json(200, &u.to_json())),
        None => Err(Error::unauthorized("unauthenticated")),
    })
    // Guards are just middleware:
    .group("/admin", vec![mw(require_role(auth.clone(), "admin"))], |g| { /* … */ })
    .group("/api",   vec![mw(require_token(tokens.clone()))], |g| { /* stg_ bearer tokens */ })
    .serve()`;

  const cSessions = `// Signed-cookie sessions (HMAC-SHA256). No server-side store needed.
let sessions = Sessions::new(secret.as_bytes());

.post("/cart/add", "Add to cart", move |c| {
    let mut s = sessions.load(c.req);
    s.set("last_item", Json::str("sku-42"));
    Ok::<_, Error>(sessions.save(&s, json(200, &Json::obj(vec![("ok", Json::Bool(true))]))))
})`;

  const cMail = `// Configure once from the environment (MAIL_* vars pick the driver:
// Log for dev, SMTP/Sendmail for real delivery). Drivers implement one Transport method.
let mailer = Mailer::from_env()?;

let email = Email::new()
    .to("ada@example.com")
    .subject("Welcome")
    .text("Thanks for signing up.")
    .html("<h1>Thanks for signing up.</h1>");
mailer.send(email)?;

// Or a themed, Laravel-notification-style message — HTML card + text from the same blocks:
let msg = MailMessage::new()
    .greeting("Hi Ada,")
    .line("Your account is ready.")
    .action("Verify email", "https://app.example.com/verify?token=…")
    .line("If you didn't sign up, ignore this email.");
mailer.send(msg.build("Acme"))?;`;

  const cStorage = `// FsStorage: single-node, one directory on disk. Same Storage trait as DbStorage / S3.
let store = FsStorage::new("files")?;

.put("/files/:name", "Upload", |c| {
    let ct = c.header("content-type").unwrap_or("");
    c.state::<FsStorage>().put(c.param("name").unwrap(), &c.req.body, ct)?;
    Ok::<_, Error>(status(201))
})
.get("/files/:name", "Download", |c| -> Result<Response, Error> {
    let store = c.state::<FsStorage>();
    match store.stat(c.param("name").unwrap())? {
        Some(meta) => Ok(Response::new(200)
            .with_header("content-type", &meta.content_type)
            .with_body(store.get(c.param("name").unwrap())?.unwrap_or_default())),
        None => Err(Error::not_found("no such file")),
    }
})

// Agent-native S3: mint a time-limited URL and let the agent move the bytes itself.
let s3 = S3Store::new(&bucket, &region, &access, &secret);   // .with_endpoint(...) for R2/MinIO
let url = s3.presign_put("reports/q2.pdf", 900)?;            // seconds`;

  const cEvents = `use sutegi::events::{event, Aggregate, EventStore, Expected, Projections, StoredEvent};

#[derive(Default)]
struct Account { balance: i64 }

impl Aggregate for Account {
    fn apply(&mut self, e: &StoredEvent) {
        let amount = e.payload.get("amount").and_then(Json::as_i64).unwrap_or(0);
        match e.name.as_str() {
            "deposited" => self.balance += amount,
            "withdrawn" => self.balance -= amount,
            _ => {}
        }
    }
}

let store = EventStore::new(db.clone());
store.migrate()?;

// Append with optimistic concurrency; fold state back on demand.
store.append("account-42", Expected::Any, &[event("deposited", amount_payload(100))])?;
let (account, version) = store.load::<Account>("account-42")?;   // balance = 100

// A checkpointed projection maintains a read model, exactly once, rebuildable:
let mut projections = Projections::new(db.clone());
projections.register("account_balances", |e, tx| {
    /* write to a read-model table in the same transaction as the checkpoint */ Ok(())
});
let _workers = std::sync::Arc::new(projections).start();`;

  const cTemplates = `// A Blade-lite engine over Json contexts. {{ }} escapes; {!! !!} is raw.
let src = "\\
<h1>Hi {{ name }}</h1>
@if(admin)
  <p>You are an admin.</p>
@endif
<ul>@foreach(items as item)<li>{{ item }}</li>@endforeach</ul>";

let mut views = Templates::new();
views.register("home", src);
let html = views.render("home", &Json::obj(vec![
    ("name",  Json::str("Ada")),
    ("admin", Json::Bool(true)),
    ("items", Json::arr(vec![Json::str("a"), Json::str("b")])),
]))?;`;

  const cHex = `// Domain use case, written against a port trait — no HTTP, no SQL in sight.
impl UseCase for CreateTodo {
    type Input = String;              // the title
    type Output = Todo;
    fn execute(&self, title: String) -> AppResult<Todo> {
        let todo = Todo::new(title).map_err(AppError::invalid)?;
        let id = self.repo.insert(&todo)?;    // outbound port: Db, Pg, or in-memory
        Ok(Todo { id, ..todo })
    }
}

// Inbound HTTP adapter — respond_created maps AppResult to the right HTTP response:
.post("/todos", "Create", move |c| {
    let title = c.json()?.get("title").and_then(Json::as_str).unwrap_or("").to_string();
    respond_created(create.execute(title))
})
// The very same use case can back the AI tool from "Defining tools" — write it once.`;

  const cTesting = `// App::service() hands you a plain Fn(Request) -> Response — no socket, no port.
#[test]
fn creates_a_todo() {
    let db = Db::memory().unwrap();
    Todo::migrate(&db).unwrap();
    let handle = App::new("test").state(db)
        .post("/todos", "create", |c| {
            let t: Todo = c.validated()?;
            Ok::<_, Error>((201, Todo { id: t.save(c.db::<Db>())?, ..t }.to_json()))
        })
        .service();

    let resp = handle(Request::post("/todos", br#"{"title":"x"}"#));
    assert_eq!(resp.status, 201);
}
// See crates/sutegi/tests/server.rs for the full end-to-end suite that boots a real server.`;

  const cOps = `let ready = db.clone();   // the probe keeps its own pooled handle
App::new("api")
    .state(db)
    .readiness(move || ready.query("SELECT 1", &[]).is_ok())
    .serve()?;                // reads HOST/PORT/WORKERS; graceful SIGTERM drain

// Always on, no feature required:
// GET /__health   liveness (200 while up)     GET /__ready       readiness (200/503)
// GET /__metrics  Prometheus text             GET /__introspect  the full app surface`;

  const cDeploy = `./ontzi up 3            # 3 replicas behind an nginx LB on http://localhost:8080
./ontzi curl /api/todos
./ontzi logs
./ontzi k8s apply      # promote deploy/k8s/ — probes, drain, Prometheus annotations wired`;
</script>

<div class="relative min-h-screen text-[#e6e6eb] bg-[#0b0a10] font-sans">
  <!-- Top bar -->
  <nav class="sticky top-0 z-30 flex items-center justify-between px-4 sm:px-6 md:px-10 py-4 bg-[#0b0a10]/90 backdrop-blur border-b border-white/5">
    <div class="flex items-center gap-4">
      <button class="lg:hidden text-[#a0a0b0] hover:text-white" onclick={() => (menuOpen = !menuOpen)} aria-label="Toggle menu">
        {#if menuOpen}<X size={20} />{:else}<Menu size={20} />{/if}
      </button>
      <a href="#/" class="flex items-center gap-2 group">
        <Flame class="text-[#ff6a3d] group-hover:rotate-12 transition-transform duration-300" size={22} />
        <span class="text-lg font-bold text-white tracking-tight">sutegi</span>
        <span class="text-[#7a7a8a] text-sm font-mono hidden sm:inline">/ docs</span>
      </a>
    </div>
    <div class="flex items-center gap-2 sm:gap-3">
      <a href="#/" class="hidden sm:inline-flex items-center gap-1.5 px-4 py-2 text-sm text-[#a0a0b0] hover:text-white transition-colors">
        <ArrowLeft size={14} /> Home
      </a>
      <a href="https://github.com/enekos/sutegi" target="_blank" rel="noopener" class="px-3 sm:px-4 py-2 border border-white/10 rounded-full text-white hover:bg-white/10 hover:border-[#ff6a3d]/50 transition-all text-xs sm:text-sm flex items-center gap-2">
        <GitBranch size={14} /> GitHub
      </a>
    </div>
  </nav>

  <div class="max-w-7xl mx-auto flex">
    <!-- Sidebar -->
    <aside class="fixed lg:sticky top-[57px] z-20 h-[calc(100vh-57px)] w-72 shrink-0 overflow-y-auto custom-scrollbar bg-[#0d0c12] lg:bg-transparent border-r border-white/5 px-5 py-8 transition-transform duration-200 {menuOpen ? 'translate-x-0' : '-translate-x-full lg:translate-x-0'}">
      <nav class="space-y-7">
        {#each nav as grp}
          <div>
            <div class="text-[11px] uppercase tracking-wider text-[#7a7a8a] font-semibold mb-2.5">{grp.group}</div>
            <ul class="border-l border-white/10">
              {#each grp.items as it}
                <li>
                  <a href="#/docs/{it.id}" onclick={() => (menuOpen = false)}
                    class="block pl-4 -ml-px border-l py-1.5 text-sm transition-colors {active === it.id ? 'border-[#ff6a3d] text-white font-medium' : 'border-transparent text-[#9090a0] hover:text-white hover:border-white/30'}">
                    {it.title}
                  </a>
                </li>
              {/each}
            </ul>
          </div>
        {/each}
      </nav>
    </aside>

    {#if menuOpen}
      <button class="fixed inset-0 top-[57px] z-10 bg-black/60 lg:hidden" onclick={() => (menuOpen = false)} aria-label="Close menu"></button>
    {/if}

    <!-- Content -->
    <main class="flex-1 min-w-0 px-5 sm:px-8 md:px-12 py-10 sm:py-14 max-w-3xl mx-auto">
      <div class="prose-doc space-y-16">

        <!-- ===================== PROLOGUE ===================== -->
        <article id="introduction" class="scroll-mt-24">
          <div class="text-[#ff6a3d] text-[11px] font-mono font-semibold uppercase tracking-wider mb-2">Prologue</div>
          <h1 class="text-3xl sm:text-4xl font-bold text-white mb-5">Introduction</h1>
          <p>
            <strong class="text-white">sutegi</strong> — Basque for <em>forge</em> — is a batteries-included web
            framework for Rust with a single unusual constraint: it has
            <strong class="text-white">zero third-party runtime dependencies</strong>. No tokio, no serde, no hyper,
            not even a Postgres driver crate. The HTTP/1.1 parser, the JSON codec, the router, the ORM, the Postgres
            wire driver, and the agent tool layer are all original code, built on the standard library.
          </p>
          <p>
            If you have used Laravel, the shape will feel familiar: an expressive router, an Eloquent-style ORM,
            first-class validation, a queue, mail, sessions, storage, and a CLI that scaffolds the conventional
            pieces. The difference is what sits underneath — nothing you did not choose to compile in — and one
            addition Laravel never had to think about:
            <strong class="text-white">an AI agent is a first-class user of your app</strong>, able to discover and
            drive every route, model, and tool over plain JSON with no SDK.
          </p>

          <h3 class="h3">How to read these docs</h3>
          <p>
            They are written to be read in order the first time and grepped after. The path is deliberate:
          </p>
          <ul class="list">
            <li><strong class="text-white">Getting started</strong> — install, boot a first app, and learn how features and configuration work.</li>
            <li><strong class="text-white">The basics</strong> — routing, the request context, responses, middleware, and validation: the everyday request loop.</li>
            <li><strong class="text-white">Database</strong> — models, the query builder, the one <code>Backend</code> trait behind SQLite and Postgres, migrations, and the KV store.</li>
            <li><strong class="text-white">Agents &amp; realtime</strong> — the introspection surface, tools, streaming, and the durable queue.</li>
            <li><strong class="text-white">Framework services</strong> — auth, sessions, mail, storage, event sourcing, and templates: opt-in pillars you reach for as needed.</li>
            <li><strong class="text-white">Architecture &amp; deployment</strong> — hexagonal structure, testing, the operational endpoints, deploying, and an honest security posture.</li>
          </ul>
          <div class="callout note">
            <div class="callout-title">Start here</div>
            <p>
              New to sutegi? Read <a href="#/docs/philosophy" class="lnk">Philosophy</a> and
              <a href="#/docs/honesty" class="lnk">Is it production-ready?</a>, then jump to
              <a href="#/docs/first-app" class="lnk">Your first app</a>. The whole &ldquo;basics + database&rdquo; arc
              is one continuous Todo example you can build as you go.
            </p>
          </div>
        </article>

        <article id="philosophy" class="scroll-mt-24">
          <h2 class="h2">Philosophy &amp; the bet</h2>
          <p>
            Frameworks usually force a trade: batteries-included but heavy, or tiny but bare. sutegi bets you can
            refuse the trade if you build every layer on <code>std</code> and make each one an
            <em>opt-in compile-time feature</em>. Compile in only the HTTP core and you get a ~394&nbsp;KB binary with
            no async runtime; switch on <code>sqlite</code>, <code>ai</code> and <code>graceful</code> and you get an
            ergonomic, agent-native service — with nothing else along for the ride.
          </p>
          <p>
            The second bet is agent-native design. Because every route, model, and tool registers its own metadata,
            the framework assembles a complete, machine-readable description of your app for free. An LLM points at
            <code>/__introspect</code>, reads the surface, and calls <code>/__tools</code> — the same application you
            built for humans is drivable by a model without a line of glue.
          </p>
          <p>
            The third principle is that <strong class="text-white">the type you hold is the only thing that changes
            when you scale</strong>. Handlers, models, and validation are written once against traits; moving from a
            single SQLite file to a fleet of Postgres-backed pods swaps a constructor, not your code. The cost of
            these bets is a large hand-rolled surface and a young ecosystem — which the next page addresses head-on.
          </p>
        </article>

        <article id="honesty" class="scroll-mt-24">
          <h2 class="h2">Is it production-ready?</h2>
          <p>
            The honest answer is: <strong class="text-white">it depends on what you are asking sutegi to do, and how
            much you are willing to own yourself.</strong> Rather than a marketing yes, here is the real picture.
          </p>

          <h3 class="h3">What is solid</h3>
          <p>
            The core is small enough to read in an afternoon and is exercised hard. Beyond the unit suite, a
            deterministic, pure-<code>std</code> <strong class="text-white">fuzz and differential harness</strong>
            hammers every hand-rolled surface — JSON, HTTP, the crypto primitives, the Postgres wire protocol,
            templating, SigV4 — and runs in CI as a required gate. Building that harness caught and fixed several real
            bugs (a JSON stack-overflow DoS, an HTTP unbounded line-read, an unchecked PG frame panic, a SCRAM
            iteration-count DoS, and more), and the JSON parser is checked round-trip against <code>serde_json</code>
            on hundreds of thousands of cases. The query builder guards identifiers against SQL injection — important
            precisely because an AI tool argument can reach a column or sort slot.
          </p>

          <div class="callout warn">
            <div class="callout-title">The hand-rolled surface is the thing to weigh</div>
            <p>
              Zero dependencies means sutegi implements its own cryptography and protocols. That surface is tested
              against RFC vectors, live servers, and the fuzz harness — but it has <strong class="text-white">not had
              an independent security audit</strong>, and the constant-time defenses in the auth path are implemented
              and reasoned about, not yet <em>measured</em>. For a framework whose selling point is hand-built crypto,
              that distinction matters.
            </p>
          </div>

          <h3 class="h3">The one real gap: TLS</h3>
          <p>
            sutegi does not ship TLS. The intended posture is to terminate HTTPS at a load balancer or service mesh —
            standard, and fine for the front door. The genuine limitation is
            <strong class="text-white">in-cluster Postgres and SMTP</strong>, often expected encrypted; today those
            connections must stay inside a trusted network boundary. TLS is the one primitive we will <em>not</em>
            hand-roll; the plan is a single curated, audited dependency (<code>rustls</code>) behind an opt-in
            <code>tls</code> feature, added when a real consumer needs it. The mail and storage layers already have an
            adapter seam so it drops in without a rewrite.
          </p>

          <h3 class="h3">It is a coherent solo bet, not a mature ecosystem</h3>
          <p>
            Laravel took years and a large community. sutegi is a broad, coherent framework built quickly by one
            maintainer — its breadth currently runs ahead of its battle-tested depth, and no real production workload
            has yet run on it under load. That is not a defect to paper over; it is the honest framing.
          </p>

          <h3 class="h3">Three tiers you can actually claim</h3>
          <ul class="list">
            <li><strong class="text-white">Run it yourself, in-cluster, eyes open.</strong> Internal services and agent tool servers where PG/SMTP stay on a trusted network. Well within reach today.</li>
            <li><strong class="text-white">Run it for a real workload.</strong> Add TLS if your topology needs encrypted PG/SMTP, and a deployed consumer under real traffic to convert &ldquo;one maintainer&rsquo;s correctness&rdquo; into evidence.</li>
            <li><strong class="text-white">1.0 / recommend to others.</strong> Requires measured timing on the auth path and an external security review. Do not cross this line on trust alone.</li>
          </ul>

          <div class="callout note">
            <div class="callout-title">In short</div>
            <p>
              Reach for sutegi when small, legible, and agent-friendly matter and you can keep your data plane on a
              trusted network. Do not reach for it as a drop-in replacement for a mature, audited stack fielding
              hostile traffic with hand-rolled crypto on the open internet — not yet.
            </p>
          </div>
        </article>

        <!-- ===================== GETTING STARTED ===================== -->
        <article id="installation" class="scroll-mt-24">
          <div class="text-[#ff6a3d] text-[11px] font-mono font-semibold uppercase tracking-wider mb-2">Getting started</div>
          <h2 class="h2">Installation</h2>
          <p>
            Every sutegi app is an ordinary Rust binary — <code>cargo new</code>, add the crate, write a
            <code>main</code>. There is no separate runtime to install and nothing trailing behind the binary at run
            time. Choose the feature pillars you want; the HTTP core (<code>json</code> + <code>http</code> +
            <code>web</code>) is always present.
          </p>
          {@render code(cInstall, 'install')}
          <div class="callout tip">
            <div class="callout-title">Scaffolding</div>
            <p>
              In a hurry? <code>sutegi new todo-api</code> lays down the conventional project, and
              <code>sutegi make:model Todo</code> / <code>sutegi make:route todos</code> add pieces afterwards.
            </p>
          </div>
        </article>

        <article id="first-app" class="scroll-mt-24">
          <h2 class="h2">Your first app</h2>
          <p>
            A handler is a closure that takes a single <code>&amp;Ctx</code> and returns anything that implements
            <code>IntoResponse</code>. Register routes fluently on the <code>App</code>, then call
            <code>.serve()</code> — that one call reads <code>HOST</code>/<code>PORT</code>/<code>WORKERS</code> from
            the environment (or <code>argv[1]</code>) and, with the <code>graceful</code> feature, drains in-flight
            requests on <code>SIGTERM</code>.
          </p>
          {@render code(cFirstApp, 'firstapp')}
          {@render code(cFirstRun, 'firstrun')}
          <div class="callout note">
            <div class="callout-title">You already have an agent surface</div>
            <p>
              Notice you never wrote <code>/__introspect</code>. Every route you register is reflected there
              automatically — the foundation of the <a href="#/docs/agents" class="lnk">agent surface</a>.
            </p>
          </div>
        </article>

        <article id="features" class="scroll-mt-24">
          <h2 class="h2">Feature flags</h2>
          <p>
            sutegi&rsquo;s pillars are Cargo features on the facade crate. Only <code>json</code>, <code>http</code>
            and <code>web</code> are compiled unconditionally; everything else is opt-in, so your binary contains
            exactly the surface you use. The defaults — <code>derive</code>, <code>orm</code>, <code>validate</code>,
            <code>ai</code> — suit a typical app.
          </p>
          <ul class="list">
            <li><code>sqlite</code> / <code>postgres</code> — the two runnable ORM backends (single-node / multi-pod).</li>
            <li><code>graceful</code> — SIGTERM/SIGINT draining for rolling deploys.</li>
            <li><code>queue</code> — the durable, cross-pod job queue (Postgres-backed).</li>
            <li><code>session</code> / <code>auth</code> / <code>auth-mail</code> — cookies, the user system, and email verification/reset.</li>
            <li><code>mail</code> / <code>template</code> / <code>storage</code> / <code>storage-db</code> / <code>events</code> / <code>hex</code> — the remaining services and the hexagonal toolkit.</li>
          </ul>
          <p>
            Turn everything off with <code>default-features = false</code> for a minimal HTTP service, then add back
            precisely what a given deployment needs.
          </p>
        </article>

        <article id="configuration" class="scroll-mt-24">
          <h2 class="h2">Configuration</h2>
          <p>
            <code>sutegi::config::Config</code> is a std-only 12-factor config layer: it loads a <code>.env</code>
            file if present, overlays the process environment (which wins), and gives you typed accessors with
            defaults. <code>require_all</code> fails fast and lists every missing key at once; <code>prefixed</code>
            scopes a group of variables.
          </p>
          {@render code(cConfig, 'config')}
          <p>
            <code>.serve()</code> reads <code>HOST</code> (default <code>0.0.0.0</code>), <code>PORT</code> (default
            <code>8080</code>) and <code>WORKERS</code> from the environment on its own, so a bare app is already
            12-factor without touching <code>Config</code>.
          </p>
        </article>

        <article id="layout" class="scroll-mt-24">
          <h2 class="h2">Directory &amp; conventions</h2>
          <p>
            A minimal app is a single <code>main.rs</code>. As it grows, the conventional layout separates your domain
            from the transport edges — and the <a href="#/docs/backend" class="lnk">Backend trait</a> plus the
            <a href="#/docs/hexagonal" class="lnk">hexagonal toolkit</a> keep business logic free of any HTTP or
            database detail. The CLI&rsquo;s <code>make:*</code> generators follow the same conventions, which is also
            what lets an LLM extend the codebase correctly with minimal context.
          </p>
          <div class="callout tip">
            <div class="callout-title">Learn from the examples</div>
            <p>
              The repo&rsquo;s <code>examples/</code> directory is the fastest way in: <code>hello</code> (minimal),
              <code>todo</code> (every pillar in ~60 lines), <code>auth</code>, <code>events</code>, <code>kv</code>,
              <code>storage</code>, <code>hexagonal</code>, and <code>redactor</code> (an agent tool service).
            </p>
          </div>
        </article>

        <!-- ===================== THE BASICS ===================== -->
        <article id="routing" class="scroll-mt-24">
          <div class="text-[#ff6a3d] text-[11px] font-mono font-semibold uppercase tracking-wider mb-2">The basics</div>
          <h2 class="h2">Routing</h2>
          <p>
            Register routes with <code>.get</code>, <code>.post</code>, <code>.put</code>, <code>.delete</code> (or
            the generic <code>.route(method, &hellip;)</code>). Each takes a URL pattern, a short doc string — which
            shows up in <code>/__introspect</code>, so keep it meaningful — and a handler closure. Path parameters use
            <code>:name</code> and are read with <code>c.param("name")</code>. Related routes can share a prefix and
            middleware via <code>.group</code>.
          </p>
          {@render code(cRouting, 'routing')}
        </article>

        <article id="requests" class="scroll-mt-24">
          <h2 class="h2">Requests &amp; the Ctx</h2>
          <p>
            The <code>&amp;Ctx</code> is your window into a request and into shared application state. Path parameters,
            the query string, headers, cookies, and the raw body are all reachable from it, and the body can be parsed
            as JSON (<code>c.json()</code>) or a form (<code>c.form()</code>). State registered once with
            <code>.state(value)</code> is retrieved by type with <code>c.state::&lt;T&gt;()</code> — and, with the ORM
            feature, the database handle with <code>c.db::&lt;Db&gt;()</code>.
          </p>
          {@render code(cRequests, 'requests')}
        </article>

        <article id="responses" class="scroll-mt-24">
          <h2 class="h2">Responses &amp; errors</h2>
          <p>
            <code>IntoResponse</code> is implemented for the shapes you reach for most, so a handler can return a
            string, a <code>Json</code>, an explicit <code>(status, body)</code> pair, a bare status code, a
            <code>redirect</code>, or a <code>Result&lt;T, Error&gt;</code>. That last one is the important one: it
            means the <code>?</code> operator works throughout your handlers, and any <code>Error</code> maps to a
            correct HTTP response.
          </p>
          <p>
            <code>Error</code> carries a status, a message, and optional per-field detail. The constructors
            (<code>Error::bad_request</code>, <code>unauthorized</code>, <code>forbidden</code>,
            <code>not_found</code>, <code>unprocessable</code>, <code>internal</code>) cover the common statuses, and
            <code>.with_fields(json)</code> attaches structured validation detail.
          </p>
          {@render code(cResponses, 'responses')}
        </article>

        <article id="middleware" class="scroll-mt-24">
          <h2 class="h2">Middleware &amp; groups</h2>
          <p>
            Middleware comes in two flavours. <strong class="text-white">Before-middleware</strong>
            (<code>Fn(&amp;Request) -&gt; Option&lt;Response&gt;</code>) runs ahead of the handler and can
            short-circuit by returning <code>Some(response)</code> — this is how auth guards and rate limits work.
            <strong class="text-white">After-middleware</strong> (<code>Fn(&amp;Request, Response) -&gt; Response</code>,
            registered with <code>.after</code>) rewrites the outgoing response — this is how CORS and secure headers
            work. Wrap a before-middleware function with <code>mw(&hellip;)</code> to attach it to a group.
          </p>
          {@render code(cMiddleware, 'middleware')}
          <p>
            The batteries are built in: <code>logger()</code>, <code>rate_limit(max, per)</code>, <code>bearer</code>,
            <code>basic</code>, <code>cors</code>, <code>cors_preflight</code>, and <code>secure_headers</code>.
          </p>
        </article>

        <article id="validation" class="scroll-mt-24">
          <h2 class="h2">Validation</h2>
          <p>
            Never trust the request body. Deriving <code>Validate</code> alongside <code>Model</code> reads the
            <code>#[validate(&hellip;)]</code> field attributes and generates the type&rsquo;s ruleset at build time.
            In a handler, <code>c.validated::&lt;Todo&gt;()</code> parses the body, validates it, and hydrates a typed
            value — or returns a <code>422</code> with structured, per-field messages. For shapes without a model,
            build an ad-hoc <code>Ruleset</code> and call <code>c.validate</code>.
          </p>
          {@render code(cValidation, 'validation')}
          <p>
            The rule set covers the everyday cases: <code>Required</code>, type rules (<code>Str</code>,
            <code>Integer</code>, <code>Number</code>, <code>Bool</code>), formats (<code>Email</code>,
            <code>Url</code>, <code>Alpha</code>, <code>AlphaNum</code>), bounds (<code>Min</code>/<code>Max</code>,
            <code>Between</code>, <code>MinLen</code>/<code>MaxLen</code>), and relational rules (<code>In</code>,
            <code>Same</code>).
          </p>
          <div class="callout note">
            <div class="callout-title">Agents get this for free</div>
            <p>AI tool arguments are validated against each tool&rsquo;s JSON schema automatically, returning the same structured errors.</p>
          </div>
        </article>

        <!-- ===================== DATABASE ===================== -->
        <article id="models" class="scroll-mt-24">
          <div class="text-[#ff6a3d] text-[11px] font-mono font-semibold uppercase tracking-wider mb-2">Database</div>
          <h2 class="h2">Models</h2>
          <p>
            Deriving <code>Model</code> on a plain struct makes it the single source of truth for a table: schema,
            migrations, typed reads, JSON serialization, and <code>save()</code> all come from it. The
            <code>#[model(&hellip;)]</code> attributes tune the mapping — <code>primary</code> marks the key,
            <code>table = "&hellip;"</code> overrides the inferred snake-case plural name, and <code>skip</code> keeps
            a field out of the database. Bools round-trip as real bools and <code>Option&lt;T&gt;</code> becomes a
            nullable column.
          </p>
          {@render code(cModels, 'models')}
        </article>

        <article id="queries" class="scroll-mt-24">
          <h2 class="h2">The query builder</h2>
          <p>
            For anything beyond <code>all</code>/<code>find</code>, <code>Model::query()</code> returns a fluent,
            fully parameterized query builder — <code>filter</code>, <code>or_group</code>, <code>where_null</code>,
            <code>like</code>, <code>join</code>/<code>left_join</code>, <code>group_by</code>, <code>order_by</code>,
            <code>limit</code>/<code>offset</code>, and <code>where_raw</code> as the explicit escape hatch. Run it
            through a backend to get JSON rows, typed values (<code>fetch</code>), or a <code>Page&lt;T&gt;</code>
            (<code>paginate_typed</code>). Values are always bound as parameters, and identifiers are validated against
            an allowlist — an AI tool argument cannot smuggle SQL into a column or sort slot.
          </p>
          {@render code(cQueries, 'queries')}
        </article>

        <article id="backend" class="scroll-mt-24">
          <h2 class="h2">Backends: SQLite &amp; Postgres</h2>
          <p>
            This is the key architectural story. The ORM is written against a <code>Backend</code> trait, not a
            concrete engine. <code>Db</code> (SQLite, the <code>sqlite</code> feature) is the single-node store — one
            embedded file, zero operations. <code>Pg</code> (Postgres, via a <strong class="text-white">pure-std wire
            driver</strong>, the <code>postgres</code> feature) is the multi-pod store. Both implement
            <code>Backend</code>, so <code>Model</code> is written once and every call site is identical: swap
            <code>Db</code> for <code>Pg::from_env()?</code> and your handlers do not change.
          </p>
          {@render code(cBackend, 'backend')}
          <p>
            The trait is small — five required primitives (<code>query</code>, <code>execute</code>,
            <code>insert</code>, <code>upsert</code>, <code>migrate</code>) — and everything else
            (<code>select</code>, <code>count</code>, <code>paginate</code>, transactions&hellip;) is a default method
            implemented once on top. Write your domain against <code>&amp;impl Backend</code> and choose the store at
            boot: SQLite for local dev, edge, and single-node; Postgres when you scale to many pods.
          </p>
        </article>

        <article id="migrations" class="scroll-mt-24">
          <h2 class="h2">Migrations</h2>
          <p>
            A <code>Migrator</code> holds an ordered list of <code>Migration</code>s, each with an id, a name, and up
            (and optionally down) closures. <code>Migration::reversible</code> takes both directions;
            <code>migrate_schema(&amp;T::schema())</code> creates a table straight from a derived model.
            <code>sutegi::migrate::dispatch</code> wires up the <code>migrate</code>, <code>migrate:status</code>, and
            <code>migrate:rollback</code> subcommands — call it early in <code>main</code> and it runs the requested
            command then returns <code>true</code> so you can exit before serving.
          </p>
          {@render code(cMigrations, 'migrations')}
        </article>

        <article id="kv" class="scroll-mt-24">
          <h2 class="h2">The key/value store</h2>
          <p>
            <code>Kv&lt;B&gt;</code> is a namespaced JSON key/value store over <em>either</em> backend — handy for
            config, feature flags, cached values, or small shared state that does not deserve a table. Values are
            arbitrary <code>Json</code>; keys are grouped by namespace and support <code>set</code>/<code>get</code>/
            <code>delete</code>/<code>keys</code>/<code>scan</code>/<code>scan_prefix</code>/<code>count</code>/
            <code>clear</code>. It is ordinary application state.
          </p>
          {@render code(cKv, 'kv')}
        </article>

        <!-- ===================== AGENTS & REALTIME ===================== -->
        <article id="agents" class="scroll-mt-24">
          <div class="text-[#ff6a3d] text-[11px] font-mono font-semibold uppercase tracking-wider mb-2">Agents &amp; realtime</div>
          <h2 class="h2">The agent surface</h2>
          <p>
            Because every route, model, and tool registers its own metadata, sutegi exposes your whole app as
            machine-readable JSON with no extra work. <code>/__introspect</code> returns the full surface — routes
            with their docs, models with their schemas, and tools with their input schemas. An agent reads that once,
            then calls tools through <code>/__tools</code>. There is no SDK and no source access required: the app you
            built for humans is the app the model drives.
          </p>
          {@render code(cAgents, 'agents')}
          <div class="callout note">
            <div class="callout-title">The full contract</div>
            <p>
              <a href="https://github.com/enekos/sutegi/blob/master/AGENTS.md" target="_blank" rel="noopener" class="lnk">AGENTS.md</a>
              specifies the complete discover / manifest / invoke / stream protocol.
            </p>
          </div>
        </article>

        <article id="tools" class="scroll-mt-24">
          <h2 class="h2">Defining tools</h2>
          <p>
            Tools are first-class on the <code>App</code>. <code>.tool(name, description, schema, closure)</code>
            registers a unary tool; the closure receives the already schema-validated arguments and an owned context
            that shares app state (<code>c.db::&lt;Db&gt;()</code>, <code>c.state::&lt;T&gt;()</code>). Build the
            argument schema with the <code>schema::</code> helpers (<code>object</code>, <code>string</code>,
            <code>integer</code>, <code>boolean</code>, <code>array</code>). <code>.stream_tool(&hellip;)</code> is the
            same, but the closure also gets an <code>SseSink</code> to stream results as Server-Sent Events.
          </p>
          {@render code(cTools, 'tools')}
        </article>

        <article id="streaming" class="scroll-mt-24">
          <h2 class="h2">Streaming &amp; SSE</h2>
          <p>
            Because the server is blocking and thread-per-connection, streaming is trivial and naturally
            backpressured — there is no executor to fight. <code>sse(producer)</code> gives the producer an
            <code>SseSink</code> with <code>data</code>, <code>event</code>, and <code>comment</code>; each frame is
            flushed immediately. It is the same transport that carries live LLM tokens back to a UI and the one
            <code>.stream_tool</code> rides on. Regular responses use keep-alive; streams are close-framed by design.
          </p>
          {@render code(cStreaming, 'streaming')}
        </article>

        <article id="queues" class="scroll-mt-24">
          <h2 class="h2">Queues</h2>
          <p>
            Some work should not block the response. The durable queue (the <code>queue</code> feature) is
            <strong class="text-white">Postgres-backed and cross-pod</strong>: it claims jobs with
            <code>FOR UPDATE SKIP LOCKED</code>, so any pod can pull the next one, and a visibility-timeout retry
            recovers work from a crashed worker. Register a handler by name, <code>dispatch</code> a JSON payload
            (optionally delayed), and <code>start</code> N worker threads. A job survives a pod restart and
            dead-letters once its retries are spent.
          </p>
          {@render code(cQueues, 'queues')}
        </article>

        <!-- ===================== FRAMEWORK SERVICES ===================== -->
        <article id="auth" class="scroll-mt-24">
          <div class="text-[#ff6a3d] text-[11px] font-mono font-semibold uppercase tracking-wider mb-2">Framework services</div>
          <h2 class="h2">Authentication</h2>
          <p>
            The <code>auth</code> feature is a complete user system over either backend. <code>Users</code> handles
            registration and authentication with PBKDF2-HMAC-SHA256 passwords (600k iterations by default, per OWASP;
            the hash never leaves the store). <code>Auth</code> issues signed-cookie logins with a server-enforced
            expiry baked into the signed payload. <code>Tokens</code> mints <code>stg_</code> bearer tokens for agents
            — shown in plaintext once, stored hashed, and surviving logout. The guards
            (<code>require_auth</code>, <code>require_role</code>, <code>require_token</code>) are just middleware you
            attach to a group.
          </p>
          {@render code(cAuth, 'auth')}
          <div class="callout tip">
            <div class="callout-title">Verification &amp; reset</div>
            <p>
              Add the <code>auth-mail</code> feature and <code>AuthMail</code> for email verification and
              password-reset flows — reset tokens are enumeration-safe and bound to the current password hash, so they
              are stateless and single-use.
            </p>
          </div>
        </article>

        <article id="sessions" class="scroll-mt-24">
          <h2 class="h2">Sessions</h2>
          <p>
            The <code>session</code> feature provides signed-cookie sessions (HMAC-SHA256) with no server-side store:
            <code>Sessions::new(secret)</code>, then <code>load</code> a session off the request, <code>set</code>/
            <code>get</code>/<code>remove</code> values, and <code>save</code> it onto the response. Call
            <code>.insecure()</code> in local <code>http://</code> development to drop the cookie&rsquo;s
            <code>Secure</code> flag. This is the machinery <code>Auth</code> is built on; use it directly for
            lightweight per-visitor state like a cart or a wizard step.
          </p>
          {@render code(cSessions, 'sessions')}
        </article>

        <article id="mail" class="scroll-mt-24">
          <h2 class="h2">Mail</h2>
          <p>
            The <code>mail</code> feature gives you an <code>Email</code> builder with real RFC&nbsp;2822/MIME
            rendering (multipart/alternative, encoded-words, header-injection folding) and a one-method
            <code>Transport</code> seam. Built-in drivers cover <code>Log</code> (dev default), <code>Memory</code>
            (tests), a pure-std <code>Smtp</code> client, and <code>Sendmail</code>; <code>Mailer::from_env()</code>
            picks one from <code>MAIL_*</code> variables. For notification-style mail, <code>MailMessage</code> builds
            a themed HTML card and matching plain text from the same fluent blocks.
          </p>
          {@render code(cMail, 'mail')}
          <div class="callout note">
            <div class="callout-title">A hosted provider is ~10 lines</div>
            <p>
              A Resend/SendGrid transport is a small adapter over any HTTP client — the same seam that lets sutegi dodge
              the TLS wall until it lands (see <a href="#/docs/honesty" class="lnk">the posture page</a>).
            </p>
          </div>
        </article>

        <article id="storage" class="scroll-mt-24">
          <h2 class="h2">File storage</h2>
          <p>
            The <code>storage</code> feature abstracts object storage behind one <code>Storage</code> trait
            (<code>put</code>/<code>get</code>/<code>stat</code>/<code>delete</code>/<code>list</code>/
            <code>get_reader</code>, with traversal-validated keys). <code>FsStorage</code> writes to a local directory
            (atomic temp-and-rename); <code>DbStorage</code> (the <code>storage-db</code> feature) stores blobs in
            SQLite or Postgres for multi-pod files with no new infrastructure; and <code>S3Store</code> is a pure-std
            SigV4 presigner for AWS/R2/MinIO. Presigning is the agent-native trick: mint a time-limited URL and let the
            client (or agent) move the bytes straight to the object store — they never pass through your server.
          </p>
          {@render code(cStorage, 'storage')}
        </article>

        <article id="events" class="scroll-mt-24">
          <h2 class="h2">Event sourcing</h2>
          <p>
            The <code>events</code> feature is an append-only event store over the <code>Backend</code> seam. You
            <code>append</code> events to a per-entity stream with an <code>Expected</code> version for optimistic
            concurrency (<code>Any</code>, <code>NoStream</code>, or <code>Version(n)</code>), and fold current state
            back on demand by implementing <code>Aggregate::apply</code>. <code>Projections</code> are checkpointed
            consumers: the handler&rsquo;s writes and its checkpoint bump commit in one transaction, giving
            exactly-once read models that you can <code>reset</code> and rebuild from the log.
          </p>
          {@render code(cEvents, 'events')}
        </article>

        <article id="templates" class="scroll-mt-24">
          <h2 class="h2">Templates</h2>
          <p>
            The <code>template</code> feature is a Blade-lite engine over <code>Json</code> contexts:
            <code>{'{{ escaped }}'}</code> and <code>{'{!! raw !!}'}</code> dot-path interpolation,
            <code>@if</code>/<code>@else</code>, <code>@foreach &hellip; as &hellip;</code> (with
            <code>loop.index</code>/<code>first</code>/<code>last</code>), and <code>@include</code> for partials.
            Templates compile once to an AST and report line-numbered errors. It also powers the themed HTML in the
            mail layer.
          </p>
          {@render code(cTemplates, 'templates')}
        </article>

        <!-- ===================== ARCHITECTURE & DEPLOYMENT ===================== -->
        <article id="hexagonal" class="scroll-mt-24">
          <div class="text-[#ff6a3d] text-[11px] font-mono font-semibold uppercase tracking-wider mb-2">Architecture &amp; deployment</div>
          <h2 class="h2">Hexagonal architecture</h2>
          <p>
            As an app grows, the <code>hex</code> toolkit keeps it honest. Your domain stays pure; the application
            layer depends on <em>port</em> traits (an outbound <code>TodoRepository</code>, say); and adapters — an
            HTTP route, an AI tool, a repo over either <code>Backend</code> — plug in at the edges. <code>UseCase</code>
            is the inbound-port trait, <code>AppError</code>/<code>AppResult</code> are transport-agnostic with a
            canonical HTTP mapping, and <code>respond</code>/<code>respond_created</code> are the glue that turns an
            <code>AppResult</code> into a <code>Response</code>. One use case can back both a route and a tool, over
            whichever store the composition root injects — and it is fully testable without starting a server.
          </p>
          {@render code(cHex, 'hex')}
          <div class="callout note">
            <div class="callout-title">Full guide</div>
            <p>
              <a href="https://github.com/enekos/sutegi/blob/master/docs/HEXAGONAL.md" target="_blank" rel="noopener" class="lnk">docs/HEXAGONAL.md</a>
              covers the dependency rule, layer responsibilities, layout, and testing strategy in depth.
            </p>
          </div>
        </article>

        <article id="testing" class="scroll-mt-24">
          <h2 class="h2">Testing</h2>
          <p>
            <code>App::service()</code> returns the app as a plain <code>Fn(Request) -&gt; Response</code>, so you can
            exercise the whole routing, state, validation, and tool surface <em>in process</em> — no socket, no port,
            no async harness. Back it with <code>Db::memory()</code> for a fresh database per test. For full
            end-to-end coverage, the framework&rsquo;s own suite boots a real server over a loopback socket; see
            <code>crates/sutegi/tests/server.rs</code> for the pattern.
          </p>
          {@render code(cTesting, 'testing')}
        </article>

        <article id="ops" class="scroll-mt-24">
          <h2 class="h2">Operational endpoints</h2>
          <p>
            Four operational endpoints are always on, no feature required. <code>/__health</code> is liveness (200
            while the process is up), <code>/__ready</code> runs the probe you register with <code>.readiness(&hellip;)</code>
            and returns 200 or 503, <code>/__metrics</code> exposes Prometheus text (requests total, in-flight, by
            status class), and <code>/__introspect</code> is the full surface. Because <code>Db</code> is
            <code>Clone</code>, clone a handle for the readiness probe before you hand ownership to <code>.state()</code>.
          </p>
          {@render code(cOps, 'ops')}
        </article>

        <article id="deploying" class="scroll-mt-24">
          <h2 class="h2">Deploying</h2>
          <p>
            <code>.serve()</code> already does the right thing for a rolling update: it drains in-flight requests on
            <code>SIGTERM</code> before exiting. <code>ontzi</code> (Basque: <em>vessel</em>) wraps Docker Compose to
            run the horizontally-scaled shape locally — N replicas behind an nginx load balancer configured with
            <code>proxy_buffering off</code> so SSE streams pass straight through — and promotes the same shape to
            Kubernetes with manifests that already wire probes, graceful drain, and Prometheus annotations. For a
            single box, a provisioning script installs the binary as a hardened systemd unit behind nginx instead.
          </p>
          {@render code(cDeploy, 'deploy')}
          <p>
            Pick the backend for the deployment, not the code: one instance runs on SQLite (embedded, zero-ops); many
            pods run on Postgres plus the durable queue. The request surface is stateless and scales horizontally
            either way.
          </p>
        </article>

        <article id="security" class="scroll-mt-24">
          <h2 class="h2">Security posture</h2>
          <p>
            sutegi ships panic isolation (a handler panic becomes a <code>500</code>, not a downed worker),
            configurable body/header size limits, slowloris socket timeouts, per-IP rate limiting, secure-header and
            CORS middleware, bearer/basic guards, and signed-cookie sessions. Passwords are PBKDF2-HMAC-SHA256 PHC
            strings; agent tokens are stored hashed. The query builder guards identifiers against injection, and the
            fuzz and differential harness runs as a required CI gate.
          </p>
          <div class="callout warn">
            <div class="callout-title">Read this before deploying to hostile traffic</div>
            <p>
              The honest limits are laid out on the <a href="#/docs/honesty" class="lnk">Is it production-ready?</a>
              page: no TLS yet (terminate at the LB; keep PG/SMTP in-cluster), no independent security audit, and
              auth-path timing that is defended but not yet measured. Deploy within a trusted network boundary, and do
              not put the hand-rolled crypto directly in front of the open internet until those gaps are closed.
            </p>
          </div>
        </article>

        <!-- Footer nav -->
        <div class="flex items-center justify-between pt-8 border-t border-white/10">
          <a href="#/" class="inline-flex items-center gap-2 text-sm text-[#a0a0b0] hover:text-white transition-colors">
            <ArrowLeft size={15} /> Back to home
          </a>
          <a href="#/docs/introduction" class="inline-flex items-center gap-2 text-sm text-[#ff6a3d] hover:text-[#ffaa33] transition-colors">
            Top of docs <ArrowRight size={15} />
          </a>
        </div>
      </div>
    </main>
  </div>
</div>

{#snippet code(text: string, key: string)}
  <div class="relative my-4">
    <button onclick={() => copy(text, key)} class="absolute top-2 right-2 text-[11px] font-mono text-[#7a7a8a] hover:text-[#ff6a3d] border border-white/10 rounded px-2 py-1 transition-colors z-10">
      {copiedKey === key ? 'copied' : 'copy'}
    </button>
    <pre class="bg-black/40 border border-white/5 rounded-lg p-4 font-mono text-[12px] sm:text-[13px] text-[#d0d0e0] overflow-x-auto custom-scrollbar leading-relaxed">{text}</pre>
  </div>
{/snippet}

<style>
  .prose-doc :global(p) {
    color: #b4b4c2;
    font-size: 15px;
    line-height: 1.75;
    margin: 0.85rem 0;
  }
  .prose-doc :global(.h2) {
    color: #fff;
    font-size: 1.7rem;
    font-weight: 700;
    margin-bottom: 0.9rem;
    scroll-margin-top: 6rem;
  }
  .prose-doc :global(.h3) {
    color: #fff;
    font-size: 1.18rem;
    font-weight: 600;
    margin: 1.6rem 0 0.4rem;
  }
  .prose-doc :global(code) {
    font-family: var(--font-mono);
    font-size: 0.86em;
    color: #ffb38a;
    background: rgba(255, 106, 61, 0.1);
    padding: 0.1em 0.4em;
    border-radius: 4px;
  }
  .prose-doc :global(pre code) {
    color: inherit;
    background: none;
    padding: 0;
  }
  .prose-doc :global(.lnk) {
    color: #ff6a3d;
    text-decoration: none;
    border-bottom: 1px solid rgba(255, 106, 61, 0.3);
    transition: border-color 0.2s;
  }
  .prose-doc :global(.lnk:hover) { border-color: #ff6a3d; }
  .prose-doc :global(.list) {
    color: #b4b4c2;
    font-size: 15px;
    line-height: 1.7;
    list-style: disc;
    padding-left: 1.3rem;
    margin: 0.85rem 0;
  }
  .prose-doc :global(.list li) { margin: 0.5rem 0; }
  .prose-doc :global(.callout) {
    border-radius: 0.6rem;
    padding: 1rem 1.15rem;
    margin: 1.25rem 0;
    border: 1px solid;
  }
  .prose-doc :global(.callout p) { margin: 0; font-size: 14.5px; color: #c8c8d4; }
  .prose-doc :global(.callout-title) {
    font-weight: 600;
    font-size: 13px;
    text-transform: uppercase;
    letter-spacing: 0.04em;
    margin-bottom: 0.4rem;
  }
  .prose-doc :global(.callout.note) { background: rgba(255, 255, 255, 0.03); border-color: rgba(255, 255, 255, 0.12); }
  .prose-doc :global(.callout.note .callout-title) { color: #cfcfe0; }
  .prose-doc :global(.callout.tip) { background: rgba(255, 106, 61, 0.06); border-color: rgba(255, 106, 61, 0.22); }
  .prose-doc :global(.callout.tip .callout-title) { color: #ff6a3d; }
  .prose-doc :global(.callout.warn) { background: rgba(255, 170, 51, 0.07); border-color: rgba(255, 170, 51, 0.28); }
  .prose-doc :global(.callout.warn .callout-title) { color: #ffaa33; }
</style>
