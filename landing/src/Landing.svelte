<script lang="ts">
  import { onMount } from 'svelte';
  import {
    Flame, Zap, ArrowRight, Copy, Check, GitBranch, Terminal,
    Code, Database, Layers, Boxes, ShieldCheck, Radio, Cpu, Workflow, Rocket, Server, Plug,
    FileCode, Feather, Search,
  } from 'lucide-svelte';

  // --- scramble utils ---
  const chars = 'ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789@#$%&*+<>{}[]_-/';
  function scrambleTo(target: string, progress: number) {
    return target.split('').map((ch, i) => {
      if (ch === ' ') return ' ';
      if (progress > i) return ch;
      return chars[Math.floor(Math.random() * chars.length)];
    }).join('');
  }

  // --- hero scramble ---
  let heroTitle = $state('');
  const finalTitle = 'Batteries-included Rust';
  // hero secondary line unchanged: "built from std up"
  onMount(() => {
    let t = 0;
    const iv = setInterval(() => {
      heroTitle = scrambleTo(finalTitle, t);
      t += 0.5;
      if (t >= finalTitle.length + 2) { clearInterval(iv); heroTitle = finalTitle; }
    }, 40);
  });

  // --- section heading scrambles on view ---
  let visibleSections = $state<Record<string, boolean>>({});
  function observeSection(id: string) {
    const el = document.getElementById(id);
    if (!el) return;
    const io = new IntersectionObserver((entries) => {
      entries.forEach((entry) => {
        if (entry.isIntersecting) { visibleSections[id] = true; io.disconnect(); }
      });
    }, { threshold: 0.2 });
    io.observe(el);
  }
  onMount(() => {
    ['what', 'how', 'features', 'use-cases', 'quickstart', 'docs', 'live'].forEach(observeSection);
  });

  function useScramble(finalText: string, activeFn: () => boolean) {
    let text = $state('');
    $effect(() => {
      if (!activeFn()) { text = finalText; return; }
      let t = 0;
      const interval = setInterval(() => {
        text = scrambleTo(finalText, t);
        t += 0.5;
        if (t >= finalText.length + 2) { clearInterval(interval); text = finalText; }
      }, 35);
      return () => clearInterval(interval);
    });
    return () => text;
  }
  const whatText = useScramble('What is sutegi?', () => !!visibleSections['what']);
  const howText = useScramble('How it works', () => !!visibleSections['how']);
  const featuresText = useScramble('Batteries included, std only', () => !!visibleSections['features']);
  const useCasesText = useScramble('Who it’s for', () => !!visibleSections['use-cases']);
  const quickstartText = useScramble('Quick start', () => !!visibleSections['quickstart']);
  const docsText = useScramble('Build your first app', () => !!visibleSections['docs']);
  const liveText = useScramble('Introspect a live app', () => !!visibleSections['live']);

  // --- hover scramble action for card titles ---
  function hoverScramble(node: HTMLElement, finalText: string) {
    let interval: ReturnType<typeof setInterval> | null = null;
    const enter = () => {
      let t = 0;
      interval = setInterval(() => {
        node.textContent = scrambleTo(finalText, t);
        t += 0.6;
        if (t >= finalText.length + 2) { if (interval) clearInterval(interval); node.textContent = finalText; }
      }, 30);
    };
    const leave = () => { if (interval) clearInterval(interval); node.textContent = finalText; };
    node.addEventListener('mouseenter', enter);
    node.addEventListener('mouseleave', leave);
    return { destroy() { node.removeEventListener('mouseenter', enter); node.removeEventListener('mouseleave', leave); if (interval) clearInterval(interval); } };
  }

  // --- terminal animation ---
  let visibleLines = $state(0);
  const terminalLines = [
    { text: '> cargo run -p todo-example', delay: 0 },
    { text: '[sutegi] todo-demo on http://0.0.0.0:8080', delay: 900, success: true },
    { text: '> curl localhost:8080/__introspect', delay: 1800 },
    { text: '{ "framework": "sutegi", "routes": [...], "tools": [...] }', delay: 2700, success: true },
    { text: '> curl -X POST localhost:8080/__tools/create_todo', delay: 3500 },
    { text: '{ "id": 1, "title": "ship sutegi", "done": false }', delay: 4300, success: true },
  ];
  onMount(() => {
    terminalLines.forEach((line, i) => setTimeout(() => { visibleLines = i + 1; }, line.delay));
  });

  // --- background glyphs ---
  const glyphChars = ['{', '}', '<>', '/', '⚒', 'fn', '::', '?', '#[', ']', 'std', '→', 'impl', '&'];
  const glyphs = Array.from({ length: 28 }, () => ({
    char: glyphChars[Math.floor(Math.random() * glyphChars.length)],
    left: Math.random() * 100,
    top: Math.random() * 100,
    delay: Math.random() * 5,
    duration: 5 + Math.random() * 6,
    size: 12 + Math.floor(Math.random() * 24),
  }));

  // --- copy ---
  let copied = $state(false);
  const installCmd = 'cargo add sutegi';
  function copyCmd(cmd?: string) {
    navigator.clipboard.writeText(cmd ?? installCmd);
    copied = true;
    setTimeout(() => copied = false, 1500);
  }

  // --- code snippets (kept as strings so braces are literal text) ---
  const codeCargo = `[dependencies]
# default = ["derive", "orm", "validate", "ai"]
sutegi = { version = "*", features = ["sqlite", "graceful"] }  # single-node
# multi-pod: swap in Postgres + the durable queue instead
# sutegi = { version = "*", features = ["postgres", "queue", "graceful"] }
# minimal HTTP service, nothing else compiled in:
# sutegi = { version = "*", default-features = false }`;

  const codeMain = `use sutegi::prelude::*;

fn main() -> std::io::Result<()> {
    App::new("hello")
        // handler: one &Ctx in, anything IntoResponse out
        .get("/", "Health check", |_| "sutegi up")
        .get("/hello/:name", "Greet", |c| {
            format!("hi, {}", c.param("name").unwrap_or("world"))
        })
        .serve()   // reads HOST/PORT/WORKERS; graceful drain on SIGTERM
}`;

  const codeTalk = `curl localhost:8080/__introspect   # full app surface as JSON
curl localhost:8080/__tools        # LLM tool-calling manifest
curl -X POST localhost:8080/__tools/create_todo -d '{"title":"ship sutegi"}'`;

  // --- tutorial (guided walkthrough: build a Todo API) ---
  // Each chapter builds on the last. `lead`, `tip` and `note` are rendered as
  // HTML (they carry inline <code> spans); `code` is verbatim text.
  const tutorial = [
    {
      group: 'Getting started',
      chapters: [
        {
          id: 'install', icon: Plug, kicker: 'Step 1', title: 'Install & scaffold',
          lead: 'Every sutegi app is an ordinary Rust binary. Add the crate, switch on the feature pillars you want, and you’re ready — there are no runtime dependencies trailing behind it. Only <code>json</code>, <code>http</code> and <code>web</code> are always compiled; everything else is opt-in, so the binary carries exactly what you use.',
          code: codeCargo,
          tip: 'In a hurry? <code>sutegi new todo-api</code> scaffolds the whole app with the conventional layout. Add pieces later with <code>sutegi make:model Todo</code> and <code>sutegi make:route todos</code>.',
        },
        {
          id: 'route', icon: Workflow, kicker: 'Step 2', title: 'Your first route',
          lead: 'A handler is a closure that takes one <code>&Ctx</code> and returns anything that is <code>IntoResponse</code> — a <code>&str</code>/<code>String</code>, a <code>Json</code>, a <code>(u16, Json)</code> pair, <code>()</code> for a 204, an <code>Error</code>, or a <code>Result&lt;T, Error&gt;</code> so <code>?</code> just works. Read path params off the <code>Ctx</code>. Call <code>.serve()</code> and you have a live HTTP/1.1 server.',
          code: `use sutegi::prelude::*;

fn main() -> std::io::Result<()> {
    App::new("todo-api")
        .get("/", "Health check", |_| "sutegi up")
        .get("/todos/:id", "Show one todo", |c| {
            format!("todo #{}", c.param("id").unwrap_or("?"))
        })
        .serve()   // reads HOST/PORT/WORKERS (or argv[1])
}`,
          note: 'Every route you register also shows up in <code>/__introspect</code>. You never write that endpoint — sutegi assembles your app’s whole surface for you. We’ll lean on it in Step&nbsp;6.',
        },
      ],
    },
    {
      group: 'The basics',
      chapters: [
        {
          id: 'model', icon: FileCode, kicker: 'Step 3', title: 'Model your data',
          lead: 'Echoing a path param is fun for about ten seconds. Let’s give the app something real to store. Derive <code>Model</code> <em>and</em> <code>Validate</code> on a plain struct: <code>Model</code> gives you schema, migrations, typed reads and <code>save()</code>; <code>Validate</code> reads the <code>#[validate(...)]</code> field attrs and generates the model’s ruleset. Bools round-trip cleanly and <code>Option&lt;T&gt;</code> becomes a nullable column. Both macros run at build time, so none of their machinery reaches your binary.',
          code: `#[derive(Model, Validate)]
#[model(table = "todos")]
struct Todo {
    #[model(primary)]
    id: i64,
    #[validate(required, str, min_len = 1, max_len = 200)]
    title: String,
    done: bool,            // round-trips as a real bool
    note: Option<String>,  // Option<T> => nullable column
    #[model(skip)]
    cached: bool,          // not persisted; default-initialized
}`,
          tip: 'Drop the <code>#[model(table = "…")]</code> line and the name is inferred as snake_case + plural — <code>Todo</code> becomes <code>todos</code> automatically. The struct is the single source of truth: table, JSON, <code>save()</code> and validation all derive from it.',
        },
        {
          id: 'orm', icon: Database, kicker: 'Step 4', title: 'Talk to the database',
          lead: 'Shared state replaces the hand-rolled <code>Arc&lt;Mutex&lt;Db&gt;&gt;</code>. <code>Db</code> is now a pooled, <code>Send + Sync + Clone</code> handle — no Mutex. Register it once with <code>.state(db)</code>, then reach it from any handler via <code>c.db::&lt;Db&gt;()</code>. Migrate the table, then read and write through your typed model: <code>Todo::all_typed</code>, <code>Todo::find_typed</code>, <code>todo.save</code> (insert, DB assigns the pk). <code>c.model::&lt;Todo, Db&gt;("id")</code> is route-model binding — it parses the path param, loads the row, and hands you a typed <code>Todo</code> or a 404.',
          code: `let db = Db::open_or_memory("DATABASE_PATH");  // or Db::memory()
Todo::migrate(&db).unwrap();

App::new("todo")
    .state(db)                                // shared, no Mutex
    .get("/todos", "list", |c| -> Result<Json, Error> {
        let all = Todo::all_typed(c.db::<Db>())?;
        Ok(Json::arr(all.iter().map(Todo::to_json).collect()))
    })
    .get("/todos/:id", "show", |c| {          // route-model binding
        c.model::<Todo, Db>("id").map(|t| t.to_json())
    })
    .post("/todos", "create", |c| {
        let todo: Todo = c.validated()?;      // parse + validate -> 422
        let id = todo.save(c.db::<Db>())?;    // typed insert; DB assigns id
        Ok::<_, Error>((201, Todo { id, ..todo }.to_json()))
    })
    .serve()`,
          tip: 'Prefer raw SQL? The parameterized query builder is still there — OR groups, <code>IS NULL</code>, <code>LIKE</code>, joins, transactions and pagination — <code>db.query("SELECT 1", &[])</code> is the escape hatch.',
        },
        {
          id: 'backend', icon: Boxes, kicker: 'Step 4b', title: 'Two backends, one trait',
          lead: 'This is the key story. Everything above is written against the <code>Backend</code> trait, not a concrete engine. <code>Db</code> (SQLite, <code>sqlite</code> feature) is the single-node store — one file on disk, zero ops. <code>Pg</code> (Postgres, a <strong>pure-std</strong> wire driver, <code>postgres</code> feature) is the multi-pod store. Both implement <code>Backend</code>, so <code>Model</code> is written once against it: swap <code>Db</code> for <code>Pg::from_env(8)?</code> and every handler above is unchanged. There is also <code>Kv&lt;B&gt;</code> — a namespaced JSON key/value store over either backend.',
          code: `// single node — an embedded file, nothing to run
let db = Db::open_pool("app.db", 8);

// multi-pod — same Model code, Postgres underneath
let pg = Pg::from_env(8)?;      // reads DATABASE_URL
Todo::migrate(&pg).unwrap();
let all = Todo::all_typed(&pg)?;   // identical call site

// a namespaced JSON key/value store over either backend
let kv = Kv::new(db);              // or Kv::new(pg)
kv.migrate()?;
kv.set("config", "theme", &Json::str("dark"))?;
let theme = kv.get("config", "theme")?;   // Option<Json>
let all_flags = kv.scan("flags")?;        // Vec<(String, Json)>`,
          note: 'Write your domain against <code>Backend</code>, choose the store at boot. SQLite for local dev, edge and single-node; Postgres when you scale to many pods — the handlers never learn which one they got.',
        },
        {
          id: 'validate', icon: ShieldCheck, kicker: 'Step 5', title: 'Validate the input',
          lead: 'Never trust the request body. In Step&nbsp;3 the <code>#[validate(...)]</code> attrs already generated the model’s ruleset, so the create handler just calls <code>c.validated::&lt;Todo&gt;()</code> — parse the body, validate it, hydrate a typed model, or return a <code>422</code> with structured per-field errors. Need an ad-hoc ruleset (a login form, a filter)? Build one and call <code>c.validate(&rules)</code>. The available rules cover the common cases.',
          code: `// (a) model-driven: the ruleset comes from #[derive(Validate)]
.post("/todos", "create", |c| {
    let todo: Todo = c.validated()?;   // 422 on any field error
    /* … */
})

// (b) ad-hoc ruleset for a shape without a model
let rules = Ruleset::new()
    .field("email", &[Rule::Required, Rule::Email])
    .field("age",   &[Rule::Integer, Rule::Between(18.0, 120.0)])
    .field("site",  &[Rule::Url])
    .field("password_confirmation", &[Rule::Same("password".into())]);
let body = c.validate(&rules)?;   // Err -> { "email": ["… valid email …"] }`,
          note: 'You get this on the agent surface for free too: AI tool arguments are validated against each tool’s schema automatically — more on that next.',
        },
      ],
    },
    {
      group: 'Agents & realtime',
      chapters: [
        {
          id: 'agent', icon: Zap, kicker: 'Step 6', title: 'Make it agent-native',
          lead: 'Here’s where sutegi differs from every other framework. Tools are first-class on the <code>App</code>: register a closure with <code>.tool(...)</code> and it’s instantly exposed to an LLM — a manifest at <code>/__tools</code>, a validated invocation endpoint, and an SSE variant via <code>.stream_tool(...)</code>. The closure shares app state through an owned <code>ToolCtx</code> (<code>c.db::&lt;Db&gt;()</code>, <code>c.state::&lt;T&gt;()</code>) and gets the already schema-validated <code>Json</code> args. An agent discovers your whole app via <code>/__introspect</code> and acts on it over plain JSON — no SDK, no glue layer.',
          code: `.tool("create_todo", "Create a todo",
    schema::object(vec![("title", schema::string("the title"))], &["title"]),
    |c, args| {
        let todo = Todo::from_input(&args)?;   // args already schema-validated
        let id = todo.save(c.db::<Db>())?;
        Ok(Todo { id, ..todo }.to_json())
    })
.stream_tool("stream_answer", "Stream tokens over SSE",
    schema::object(vec![("prompt", schema::string("the prompt"))], &["prompt"]),
    |_c, args, sink| {
        let prompt = args.get("prompt").and_then(Json::as_str).unwrap_or("");
        for tok in prompt.split(' ') { sink.data(tok)?; }
        sink.event("done", "{}")
    })
// GET  /__tools               -> manifest { name, description, input_schema }
// POST /__tools/create_todo   -> invoke (args validated -> 422 on failure)
// POST /__tools/:name/stream  -> SSE for streaming tools`,
          tip: 'That’s the entire integration — the old <code>Tool</code>/<code>StreamTool</code> traits and <code>ToolRegistry</code> are gone. Point any agent at <code>/__introspect</code> for the surface and <code>/__tools</code> for the call manifest — the same app you built for humans is now drivable by a model.',
        },
        {
          id: 'stream', icon: Radio, kicker: 'Step 7', title: 'Stream responses',
          lead: 'Because the server is blocking and thread-per-connection, streaming is trivial and naturally backpressured — there’s no executor to fight. Send raw bytes or Server-Sent Events; each frame flushes immediately. It’s the same transport that carries live LLM tokens back to a UI (and the one <code>.stream_tool</code> rides on).',
          code: `.get("/stream", "SSE demo", |_| sse(|sink| {
    for token in answer().split(' ') {
        sink.data(token)?;        // each frame flushed immediately
    }
    sink.event("done", "{}")
}))`,
        },
        {
          id: 'jobs', icon: Cpu, kicker: 'Step 8', title: 'Defer the slow work',
          lead: 'Some work shouldn’t block the response — sending mail, calling a webhook. The durable queue (<code>queue</code> feature) is <strong>Postgres-backed and cross-pod</strong>: it claims jobs with <code>FOR UPDATE SKIP LOCKED</code>, so any pod can pull the next one, and visibility-timeout retries recover from a crashed worker. Register a handler by name, <code>dispatch</code> a JSON payload, and <code>start</code> N worker threads.',
          code: `use sutegi::queue::{Queue, Workers};

let mut queue = Queue::new(Pg::from_env(8)?.pool().clone());
queue.migrate()?;                              // creates sutegi_jobs
queue.register("notify", |payload| {
    let to = payload.get("to").and_then(Json::as_str).unwrap_or("");
    /* send … */ Ok(())                        // Err -> retried w/ backoff
});

queue.dispatch("notify", Json::obj(vec![("to", Json::str("a@b.com"))]))?;
let workers: Workers = std::sync::Arc::new(queue).start(4);   // cross-pod`,
          note: 'The old in-process <code>Job</code> trait and <code>Queue::new(4)</code> worker-pool are gone — the queue is durable now, so a job survives a pod restart and dead-letters after its retries are spent.',
        },
      ],
    },
    {
      group: 'Going to production',
      chapters: [
        {
          id: 'hex', icon: Layers, kicker: 'Step 9', title: 'Structure for growth',
          lead: 'As the app grows, the hexagonal toolkit keeps it honest: your domain stays pure, the application layer depends on port traits, and adapters (HTTP, AI, and a repo over either <code>Backend</code>) plug in at the edges. One use case, many transports — and fully testable without ever starting a server.',
          code: `impl UseCase for CreateTodo {
    type Input = String;          // title
    type Output = Todo;
    fn execute(&self, title: String) -> AppResult<Todo> {
        let todo = Todo::new(title).map_err(AppError::invalid)?;
        let id = self.repo.insert(&todo)?;   // outbound port (Db or Pg)
        Ok(Todo { id, ..todo })
    }
}
// inbound HTTP adapter — new &Ctx handler, respond_created maps AppResult:
.post("/todos", "Create", move |c| {
    let title = c.json().ok()
        .and_then(|b| b.get("title").and_then(Json::as_str).map(str::to_string))
        .unwrap_or_default();
    respond_created(create.execute(title))
})`,
          note: 'The very same <code>CreateTodo</code> use case can back both an HTTP route and the AI tool from Step&nbsp;6, over whichever <code>Backend</code> the composition root injects. Write the logic once; expose it everywhere.',
        },
        {
          id: 'ops', icon: Server, kicker: 'Step 10', title: 'Ship to production',
          lead: 'You’re ready to deploy. The operational endpoints are always on, and <code>.serve()</code> drains in-flight requests on SIGTERM before exiting — exactly what a Kubernetes rolling update needs. <code>Db</code> is <code>Clone</code>, so clone a handle for the readiness probe before you hand ownership to <code>.state()</code>, and wire it to whatever “healthy” means for your app.',
          code: `let ready = db.clone();   // probe keeps its own pooled handle
App::new("api")
    .state(db)
    .readiness(move || ready.query("SELECT 1", &[]).is_ok())
    .serve()?;   // reads HOST/PORT/WORKERS; graceful drain on SIGTERM
                 // (.run(addr) / .run_graceful(addr) still exist)

// GET /__health   liveness        GET /__ready    readiness (200/503)
// GET /__metrics  Prometheus      GET /__introspect  full surface`,
        },
        {
          id: 'deploy', icon: Rocket, kicker: 'Step 11', title: 'Deploy with ontzi',
          lead: 'Finally, <code>ontzi</code> (Basque: vessel / container) wraps Docker Compose: spin up N replicas behind an nginx load balancer (with <code>proxy_buffering off</code>, so the SSE streams from Step&nbsp;7 pass straight through), then promote the same shape to Kubernetes — the manifests ship with probes, graceful drain and Prometheus annotations already wired.',
          code: `./ontzi up 3            # 3 replicas + LB on http://localhost:8080
./ontzi curl /api/todos
./ontzi logs
./ontzi k8s apply      # apply deploy/k8s/ (probes, drain, metrics)`,
          tip: 'That’s the whole journey: a route, a database, validation, an agent interface, streaming, jobs and a production deploy — and your binary still has zero runtime dependencies.',
        },
      ],
    },
  ];

  // flat list for the scrollspy + sidebar active state
  const chapters = tutorial.flatMap((g) => g.chapters);
  let activeChapter = $state(chapters[0].id);
  onMount(() => {
    const io = new IntersectionObserver((entries) => {
      entries.forEach((e) => {
        if (e.isIntersecting) activeChapter = e.target.id.replace('ch-', '');
      });
    }, { rootMargin: '-12% 0px -75% 0px', threshold: 0 });
    document.querySelectorAll('[data-chapter]').forEach((el) => io.observe(el));
    return () => io.disconnect();
  });

  // --- live introspect demo ---
  let baseUrl = $state('http://localhost:8080');
  let live = $state<any>(null);
  let liveErr = $state('');
  let loadingLive = $state(false);
  async function introspect() {
    loadingLive = true; liveErr = ''; live = null;
    try {
      const r = await fetch(baseUrl.replace(/\/$/, '') + '/__introspect');
      if (!r.ok) throw new Error('bad status');
      live = await r.json();
    } catch (e) {
      liveErr = 'Could not reach a sutegi app. Run one (e.g. `cargo run -p todo-example`) with CORS enabled, then try again.';
    } finally {
      loadingLive = false;
    }
  }
</script>

<div class="relative min-h-screen text-[#e6e6eb] overflow-x-hidden font-sans bg-[#0b0a10]">
  <!-- Floating glyphs -->
  <div class="fixed inset-0 pointer-events-none z-0 overflow-hidden hidden sm:block">
    {#each glyphs as g, i (i)}
      <div class="absolute text-[#ff6a3d]/10 font-mono select-none"
        style="left: {g.left}%; top: {g.top}%; font-size: {g.size}px; animation: float {g.duration}s ease-in-out infinite; animation-delay: -{g.delay}s;">{g.char}</div>
    {/each}
  </div>

  <!-- Background orbs -->
  <div class="fixed inset-0 pointer-events-none z-0 overflow-hidden opacity-50">
    <div class="absolute top-[10%] left-[15%] w-[35vw] h-[35vw] rounded-full bg-[#ff6a3d] blur-[120px] opacity-20 animate-float"></div>
    <div class="absolute top-[35%] right-[10%] w-[40vw] h-[40vw] rounded-full bg-[#ffaa33] blur-[140px] opacity-15 animate-float" style="animation-delay: -4s;"></div>
    <div class="absolute -bottom-[10%] left-[30%] w-[45vw] h-[45vw] rounded-full bg-[#ff6a3d] blur-[120px] opacity-10 animate-float" style="animation-delay: -2s;"></div>
  </div>

  <!-- Nav -->
  <nav class="relative z-20 flex justify-between items-center px-4 sm:px-6 md:px-12 py-5 sm:py-6 bg-transparent">
    <div class="flex items-center gap-2 sm:gap-3 group cursor-pointer">
      <Flame class="text-[#ff6a3d] group-hover:rotate-12 transition-transform duration-300" size={24} />
      <span class="text-lg sm:text-xl font-bold text-white tracking-tight group-hover:tracking-widest transition-all duration-300">sutegi</span>
    </div>
    <div class="flex items-center gap-2 sm:gap-3">
      <a href="#features" class="hidden sm:inline-flex px-4 py-2 text-sm text-[#a0a0b0] hover:text-white transition-colors">Features</a>
      <a href="#quickstart" class="hidden sm:inline-flex px-4 py-2 text-sm text-[#a0a0b0] hover:text-white transition-colors">Quick start</a>
      <a href="#docs" class="hidden sm:inline-flex px-4 py-2 text-sm text-[#a0a0b0] hover:text-white transition-colors">Docs</a>
      <a href="https://github.com/enekos/sutegi" target="_blank" rel="noopener" class="px-3 sm:px-4 py-2 border border-white/10 rounded-full text-white hover:bg-white/10 hover:border-[#ff6a3d]/50 transition-all duration-300 text-xs sm:text-sm flex items-center gap-2">
        <GitBranch size={14} /> GitHub
      </a>
    </div>
  </nav>

  <!-- Hero -->
  <main class="relative z-10 max-w-6xl mx-auto px-4 sm:px-6 md:px-12 pt-10 sm:pt-16 pb-16 sm:pb-24">
    <div class="flex flex-col lg:flex-row items-center justify-between gap-10 lg:gap-16">
      <div class="flex-1 space-y-5 sm:space-y-6 text-center lg:text-left">
        <div class="inline-flex items-center gap-2 px-3 py-1 border border-[#ff6a3d]/30 rounded-full bg-[#ff6a3d]/10 text-[#ff6a3d] text-xs font-semibold tracking-wide uppercase">
          <Feather size={12} /> Zero-dependency · Agent-native
        </div>
        <h1 class="text-[2.25rem] sm:text-5xl md:text-6xl font-bold leading-[1.05] text-white">
          <span class="inline-block min-w-[10ch]">{heroTitle}</span><br/>
          <span class="bg-clip-text text-transparent bg-gradient-to-r from-[#ff6a3d] to-[#ffaa33]">built from std up</span>
        </h1>
        <p class="text-base sm:text-lg text-[#a0a0b0] max-w-xl mx-auto lg:mx-0 leading-relaxed">
          <strong class="text-white">sutegi</strong> (Basque: <em>forge</em>) is a batteries-included web framework with <strong class="text-white">zero third-party dependencies</strong> — every layer hand-built on <code class="text-[#ff6a3d]">std</code>. Handlers take one <code class="text-[#ff6a3d]">&amp;Ctx</code> and return anything; <code class="text-[#ff6a3d]">.state()</code> shares a pooled DB, <code class="text-[#ff6a3d]">.serve()</code> boots it. One <strong class="text-white">Backend</strong> trait, two stores — SQLite single-node or Postgres multi-pod. A complete Todo app is ~60 lines, and an AI agent is a first-class user.
        </p>
        <div class="flex flex-col sm:flex-row gap-3 sm:gap-4 pt-2 justify-center lg:justify-start">
          <a href="#quickstart" class="px-6 py-3 bg-[#ff6a3d] text-[#1a0d06] rounded-full font-semibold text-sm transition-all hover:shadow-[0_0_25px_rgba(255,106,61,0.4)] hover:-translate-y-0.5 flex items-center justify-center gap-2">
            Get started <ArrowRight size={16} />
          </a>
          <button onclick={() => copyCmd()} class="px-4 sm:px-5 py-3 border border-white/10 rounded-full text-[#a0a0b0] text-xs sm:text-sm flex items-center justify-center gap-2 bg-white/5 hover:bg-white/10 hover:border-white/20 transition-all font-mono max-w-full overflow-hidden">
            <span class="text-[#ff6a3d] shrink-0">$</span>
            <span class="truncate">cargo add sutegi</span>
            {#if copied}<Check size={14} class="text-green-400 shrink-0" />{:else}<Copy size={14} class="shrink-0" />{/if}
          </button>
        </div>
        <div class="flex gap-6 sm:gap-8 pt-4 font-mono justify-center lg:justify-start">
          <div><div class="text-xl sm:text-2xl font-bold text-white">0</div><div class="text-[11px] text-[#7a7a8a] uppercase tracking-wide">runtime deps</div></div>
          <div><div class="text-xl sm:text-2xl font-bold text-white">362 KB</div><div class="text-[11px] text-[#7a7a8a] uppercase tracking-wide">core binary</div></div>
          <div><div class="text-xl sm:text-2xl font-bold text-white">std</div><div class="text-[11px] text-[#7a7a8a] uppercase tracking-wide">only</div></div>
        </div>
      </div>

      <div class="flex-1 w-full max-w-lg relative">
        <div class="rounded-xl bg-[#121018]/90 backdrop-blur-xl border border-white/5 overflow-hidden shadow-2xl hover:shadow-[0_0_40px_rgba(255,106,61,0.15)] transition-shadow duration-500">
          <div class="flex items-center px-4 py-3 bg-black/30 border-b border-white/5">
            <div class="flex space-x-2">
              <div class="w-3 h-3 rounded-full bg-[#ff6a3d]/80"></div>
              <div class="w-3 h-3 rounded-full bg-white/20"></div>
              <div class="w-3 h-3 rounded-full bg-white/20"></div>
            </div>
            <div class="mx-auto text-xs text-[#7a7a8a] font-mono">sutegi</div>
          </div>
          <div class="p-4 sm:p-5 font-mono text-[11px] sm:text-[13px] h-56 sm:h-64 overflow-y-auto leading-relaxed custom-scrollbar">
            {#each terminalLines.slice(0, visibleLines) as line}
              <div class="mb-2 terminal-line">
                {#if line.text.startsWith('>')}
                  <span class="text-[#ff6a3d]">~</span> <span class="text-white">{line.text.substring(2)}</span>
                {:else if line.success}
                  <span class="text-[#ffaa33]">{line.text}</span>
                {:else}
                  <span class="text-[#7a7a8a]">{line.text}</span>
                {/if}
              </div>
            {/each}
            <div class="flex items-center text-white mt-1"><span class="text-[#ff6a3d]">~</span>&nbsp;<span class="animate-pulse">█</span></div>
          </div>
        </div>
      </div>
    </div>
  </main>

  <!-- What -->
  <section id="what" class="relative z-10 py-16 sm:py-20 border-y border-white/5 bg-[#0f0e14]">
    <div class="max-w-4xl mx-auto px-4 sm:px-6">
      <div class="text-center mb-10 sm:mb-14">
        <h2 class="text-2xl sm:text-3xl md:text-4xl font-bold text-white mb-3 min-h-[1.2em]">{whatText()}</h2>
        <p class="text-[#a0a0b0] max-w-2xl mx-auto text-sm sm:text-base">Three goals usually in tension — held at once.</p>
      </div>
      <div class="grid sm:grid-cols-2 gap-5 sm:gap-6">
        <div class="bg-[#13121a] border border-white/5 rounded-xl p-5 sm:p-6">
          <h3 class="text-white font-semibold mb-2 flex items-center gap-2"><Code size={18} class="text-[#ff6a3d]" /> The tension</h3>
          <p class="text-[#a0a0b0] text-sm leading-relaxed">Frameworks are either batteries-included but heavy, or tiny but bare. And almost none are designed for an LLM agent to operate directly.</p>
        </div>
        <div class="bg-[#13121a] border border-white/5 rounded-xl p-5 sm:p-6">
          <h3 class="text-white font-semibold mb-2 flex items-center gap-2"><Flame size={18} class="text-[#ffaa33]" /> The forge</h3>
          <p class="text-[#a0a0b0] text-sm leading-relaxed">Build every component from <strong class="text-white">std</strong>, with no async runtime. The result is small, auditable, and — because the whole surface is introspectable JSON — natively drivable by agents.</p>
        </div>
      </div>
      <div class="mt-6 sm:mt-8 bg-gradient-to-br from-[#ff6a3d]/10 to-[#ffaa33]/10 border border-white/10 rounded-xl p-5 sm:p-6 text-center">
        <p class="text-[#d0d0e0] text-sm sm:text-base leading-relaxed max-w-2xl mx-auto">
          No tokio. No serde. No hyper — not even a Postgres driver crate. The HTTP/1.1 parser, JSON codec, router, ORM, the pure-std Postgres wire driver, and the tool layer are all original code you can read in an afternoon.
        </p>
      </div>
    </div>
  </section>

  <!-- How -->
  <section id="how" class="relative z-10 py-16 sm:py-20">
    <div class="max-w-5xl mx-auto px-4 sm:px-6">
      <div class="text-center mb-10 sm:mb-14">
        <h2 class="text-2xl sm:text-3xl md:text-4xl font-bold text-white mb-3 min-h-[1.2em]">{howText()}</h2>
        <p class="text-[#a0a0b0] max-w-2xl mx-auto text-sm sm:text-base">Define it, run it, and let humans or agents drive it.</p>
      </div>
      <div class="grid md:grid-cols-3 gap-4 sm:gap-6">
        <div class="relative bg-[#13121a] border border-white/5 rounded-xl p-5 sm:p-6">
          <div class="absolute -top-3 -left-3 w-8 h-8 rounded-full bg-[#ff6a3d] text-[#1a0d06] font-bold text-sm flex items-center justify-center shadow-lg">1</div>
          <FileCode class="text-[#ff6a3d] mb-3" size={26} />
          <h3 class="text-white font-semibold mb-2">Define</h3>
          <p class="text-[#9090a0] text-sm leading-relaxed">Routes as <code>&Ctx</code> closures, models with <code>#[derive(Model, Validate)]</code>, and AI tools as <code>.tool(...)</code> closures. Each registers its own metadata.</p>
        </div>
        <div class="relative bg-[#13121a] border border-white/5 rounded-xl p-5 sm:p-6">
          <div class="absolute -top-3 -left-3 w-8 h-8 rounded-full bg-[#ffaa33] text-[#1a0d06] font-bold text-sm flex items-center justify-center shadow-lg">2</div>
          <Feather class="text-[#ffaa33] mb-3" size={26} />
          <h3 class="text-white font-semibold mb-2">Run</h3>
          <p class="text-[#9090a0] text-sm leading-relaxed">A std-only, thread-per-connection server. Tiny binary, graceful SIGTERM draining, health/readiness/metrics built in.</p>
        </div>
        <div class="relative bg-[#13121a] border border-white/5 rounded-xl p-5 sm:p-6">
          <div class="absolute -top-3 -left-3 w-8 h-8 rounded-full bg-[#ff6a3d] text-[#1a0d06] font-bold text-sm flex items-center justify-center shadow-lg">3</div>
          <Zap class="text-[#ff6a3d] mb-3" size={26} />
          <h3 class="text-white font-semibold mb-2">Drive</h3>
          <p class="text-[#9090a0] text-sm leading-relaxed">Curl it like any API — or let an agent hit <code>/__introspect</code> + <code>/__tools</code> to discover and invoke the whole app.</p>
        </div>
      </div>
    </div>
  </section>

  <!-- Features -->
  <section id="features" class="relative z-10 py-16 sm:py-20 border-y border-white/5 bg-[#0f0e14]">
    <div class="max-w-6xl mx-auto px-4 sm:px-6">
      <div class="text-center mb-10 sm:mb-14">
        <h2 class="text-2xl sm:text-3xl md:text-4xl font-bold text-white mb-3 min-h-[1.2em]">{featuresText()}</h2>
        <p class="text-[#a0a0b0] max-w-2xl mx-auto text-sm sm:text-base">Each pillar is an opt-in compile-time feature.</p>
      </div>
      <div class="grid sm:grid-cols-2 lg:grid-cols-4 gap-4 sm:gap-6">
        {#each [
          { icon: Workflow, t: 'Routing & Ctx', d: 'Path params, route groups, middleware; one &Ctx in, any IntoResponse out.' },
          { icon: Boxes, t: 'Two backends, one trait', d: 'SQLite (single-node) or Postgres (multi-pod) behind the Backend trait — swap without touching handlers.' },
          { icon: Database, t: 'ORM & shared state', d: 'Pooled Send+Sync Db via .state(); typed reads, save(), route-model binding, query builder.' },
          { icon: FileCode, t: '#[derive(Model, Validate)]', d: 'Schema, migrations, JSON and the validation ruleset — all from one struct, at build time.' },
          { icon: Plug, t: 'Kv store', d: 'Namespaced JSON key/value over either backend: set / get / scan / delete.' },
          { icon: Radio, t: 'Streaming & SSE', d: 'Stream bytes or Server-Sent Events with natural backpressure; same transport as stream tools.' },
          { icon: Cpu, t: 'Durable queue', d: 'Postgres-backed, cross-pod: SKIP LOCKED claim, visibility-timeout retries, dead-letter.' },
          { icon: Zap, t: 'Agent-native', d: '.tool() / .stream_tool() closures auto-mount /__tools; /__introspect exposes the whole surface.' },
        ] as f}
          {@const Icon = f.icon}
          <div class="group bg-[#13121a] border border-white/5 p-5 sm:p-6 rounded-xl hover:border-[#ff6a3d]/40 transition-all hover:-translate-y-1 hover:shadow-[0_0_20px_rgba(255,106,61,0.1)]">
            <Icon class="text-[#ff6a3d] mb-4 group-hover:scale-110 transition-transform" size={26} />
            <h3 class="text-base sm:text-lg font-semibold text-white mb-2" use:hoverScramble={f.t}>{f.t}</h3>
            <p class="text-[#9090a0] text-sm leading-relaxed">{f.d}</p>
          </div>
        {/each}
      </div>
    </div>
  </section>

  <!-- Use cases -->
  <section id="use-cases" class="relative z-10 py-16 sm:py-20">
    <div class="max-w-5xl mx-auto px-4 sm:px-6">
      <div class="text-center mb-10 sm:mb-14">
        <h2 class="text-2xl sm:text-3xl md:text-4xl font-bold text-white mb-3 min-h-[1.2em]">{useCasesText()}</h2>
        <p class="text-[#a0a0b0] max-w-2xl mx-auto text-sm sm:text-base">When small, legible, and agent-friendly matters.</p>
      </div>
      <div class="grid sm:grid-cols-2 lg:grid-cols-3 gap-4 sm:gap-6">
        {#each [
          { icon: Zap, t: 'Agent tool servers', d: 'Expose capabilities to an LLM with a built-in manifest and validated invocation — no glue layer.' },
          { icon: Boxes, t: 'Internal microservices', d: 'Start single-node on SQLite; scale to many pods on Postgres + the durable queue without rewriting handlers.' },
          { icon: Plug, t: 'Edge & embedded', d: 'A ~362 KB binary with no async runtime and one embedded SQLite file fits where a full stack will not.' },
          { icon: FileCode, t: 'LLM-generated apps', d: 'Rigid scaffolding conventions mean a model can extend the codebase correctly with minimal context.' },
          { icon: Server, t: 'JSON APIs & CRUD', d: 'Routing + typed models + validation cover the everyday backend without pulling a framework zoo.' },
          { icon: Radio, t: 'Streaming endpoints', d: 'SSE token streams for chat/AI UIs, backpressured by the thread-per-connection model.' },
        ] as u}
          {@const Icon = u.icon}
          <div class="bg-[#13121a] border border-white/5 rounded-xl p-5 sm:p-6">
            <Icon class="text-[#ffaa33] mb-3" size={24} />
            <h3 class="text-white font-semibold mb-1.5">{u.t}</h3>
            <p class="text-[#9090a0] text-sm leading-relaxed">{u.d}</p>
          </div>
        {/each}
      </div>
    </div>
  </section>

  <!-- Quick start -->
  <section id="quickstart" class="relative z-10 py-16 sm:py-20 border-y border-white/5 bg-[#0f0e14]">
    <div class="max-w-4xl mx-auto px-4 sm:px-6">
      <div class="text-center mb-10 sm:mb-14">
        <h2 class="text-2xl sm:text-3xl md:text-4xl font-bold text-white mb-3 min-h-[1.2em]">{quickstartText()}</h2>
        <p class="text-[#a0a0b0] max-w-2xl mx-auto text-sm sm:text-base">From zero to a running, introspectable app in under a minute.</p>
      </div>
      <div class="space-y-4 sm:space-y-5">
        {#each [
          { n: '1', title: 'Add the dependency', body: 'Pick the features you need; the core is always there.', code: codeCargo },
          { n: '2', title: 'Write your app', body: 'A handler is a closure; run it with graceful shutdown.', code: codeMain },
          { n: '3', title: 'Talk to it', body: 'As a plain HTTP API — or as an agent surface.', code: codeTalk },
        ] as step, i}
          <div class="bg-[#13121a] border border-white/5 rounded-xl p-5 sm:p-6">
            <div class="flex items-center gap-3 mb-3">
              <div class="w-7 h-7 rounded-full {i === 1 ? 'bg-[#ffaa33]/20 text-[#ffaa33]' : 'bg-[#ff6a3d]/20 text-[#ff6a3d]'} font-bold text-sm flex items-center justify-center">{step.n}</div>
              <h3 class="text-white font-semibold">{step.title}</h3>
            </div>
            <p class="text-[#9090a0] text-sm mb-3">{step.body}</p>
            <div class="relative">
              <button onclick={() => copyCmd(step.code)} class="absolute top-2 right-2 text-[11px] font-mono text-[#7a7a8a] hover:text-[#ff6a3d] border border-white/10 rounded px-2 py-1 transition-colors">copy</button>
              <pre class="bg-black/40 border border-white/5 rounded-lg p-3 sm:p-4 font-mono text-[11px] sm:text-[13px] text-[#d0d0e0] overflow-x-auto custom-scrollbar leading-relaxed">{step.code}</pre>
            </div>
          </div>
        {/each}
      </div>
      <div class="mt-6 text-center text-sm text-[#7a7a8a]">
        That’s the TL;DR. Want the full walkthrough? The <a href="#docs" class="text-[#ff6a3d] hover:underline">step-by-step tutorial</a> below builds a complete Todo API.
      </div>
    </div>
  </section>

  <!-- Docs / Tutorial -->
  <section id="docs" class="relative z-10 py-16 sm:py-24">
    <div class="max-w-6xl mx-auto px-4 sm:px-6">
      <div class="text-center mb-10 sm:mb-14">
        <div class="inline-flex items-center gap-2 px-3 py-1 mb-4 border border-[#ff6a3d]/30 rounded-full bg-[#ff6a3d]/10 text-[#ff6a3d] text-xs font-semibold tracking-wide uppercase">
          <Terminal size={12} /> Tutorial
        </div>
        <h2 class="text-2xl sm:text-3xl md:text-4xl font-bold text-white mb-3 min-h-[1.2em]">{docsText()}</h2>
        <p class="text-[#a0a0b0] max-w-2xl mx-auto text-sm sm:text-base">
          Follow along and build a complete <strong class="text-white">Todo API</strong> — routes, a database, validation, an agent interface, streaming and a production deploy — one step at a time.
        </p>
      </div>

      <div class="flex flex-col lg:flex-row gap-8 lg:gap-12">
        <!-- Sidebar TOC -->
        <aside class="hidden lg:block w-56 shrink-0">
          <nav class="sticky top-8 space-y-6">
            {#each tutorial as grp}
              <div>
                <div class="text-[11px] uppercase tracking-wider text-[#7a7a8a] font-semibold mb-2.5">{grp.group}</div>
                <ul class="border-l border-white/10">
                  {#each grp.chapters as ch}
                    <li>
                      <a href="#ch-{ch.id}"
                        class="block pl-4 -ml-px border-l py-1.5 text-sm transition-colors {activeChapter === ch.id ? 'border-[#ff6a3d] text-white font-medium' : 'border-transparent text-[#9090a0] hover:text-white hover:border-white/30'}">
                        {ch.title}
                      </a>
                    </li>
                  {/each}
                </ul>
              </div>
            {/each}
          </nav>
        </aside>

        <!-- Lessons -->
        <div class="flex-1 min-w-0 space-y-12 sm:space-y-16">
          {#each tutorial as grp}
            {#each grp.chapters as ch}
              {@const Icon = ch.icon}
              <article id="ch-{ch.id}" data-chapter class="scroll-mt-24">
                <div class="flex items-center gap-2 text-[#ff6a3d] text-[11px] font-mono font-semibold uppercase tracking-wider mb-2">
                  <Icon size={14} /> {ch.kicker}
                </div>
                <h3 class="text-xl sm:text-2xl font-bold text-white mb-3" use:hoverScramble={ch.title}>{ch.title}</h3>
                <p class="lesson text-[#b0b0c0] text-sm sm:text-[15px] leading-relaxed mb-4">{@html ch.lead}</p>
                <div class="relative">
                  <button onclick={() => copyCmd(ch.code)} class="absolute top-2 right-2 text-[11px] font-mono text-[#7a7a8a] hover:text-[#ff6a3d] border border-white/10 rounded px-2 py-1 transition-colors">copy</button>
                  <pre class="bg-black/40 border border-white/5 rounded-lg p-3 sm:p-4 font-mono text-[11px] sm:text-[13px] text-[#d0d0e0] overflow-x-auto custom-scrollbar leading-relaxed">{ch.code}</pre>
                </div>
                {#if ch.tip}
                  <div class="mt-4 flex gap-3 rounded-lg border border-[#ff6a3d]/20 bg-[#ff6a3d]/[0.06] p-3 sm:p-4">
                    <Flame size={16} class="text-[#ff6a3d] shrink-0 mt-0.5" />
                    <p class="lesson text-[#c8c8d4] text-sm leading-relaxed"><strong class="text-[#ff6a3d] not-italic">Tip&nbsp;·&nbsp;</strong>{@html ch.tip}</p>
                  </div>
                {/if}
                {#if ch.note}
                  <div class="mt-4 flex gap-3 rounded-lg border border-white/10 bg-white/[0.03] p-3 sm:p-4">
                    <Feather size={16} class="text-[#9090a0] shrink-0 mt-0.5" />
                    <p class="lesson text-[#b0b0c0] text-sm leading-relaxed"><strong class="text-white not-italic">Note&nbsp;·&nbsp;</strong>{@html ch.note}</p>
                  </div>
                {/if}
              </article>
            {/each}
          {/each}

          <!-- Keep reading -->
          <div class="pt-2">
            <div class="text-[11px] uppercase tracking-wider text-[#7a7a8a] font-semibold mb-3">Keep reading</div>
            <div class="grid sm:grid-cols-2 gap-3 sm:gap-4">
              <a href="https://github.com/enekos/sutegi/blob/master/AGENTS.md" target="_blank" rel="noopener" class="bg-[#13121a] border border-white/5 rounded-xl p-4 sm:p-5 hover:border-[#ff6a3d]/40 transition-colors">
                <div class="text-white font-mono text-sm">AGENTS.md <span class="text-[#ff6a3d]">→</span></div>
                <div class="text-[#9090a0] text-xs mt-1">The complete agent-facing contract: discover, manifest, invoke, stream.</div>
              </a>
              <a href="https://github.com/enekos/sutegi/blob/master/docs/HEXAGONAL.md" target="_blank" rel="noopener" class="bg-[#13121a] border border-white/5 rounded-xl p-4 sm:p-5 hover:border-[#ff6a3d]/40 transition-colors">
                <div class="text-white font-mono text-sm">Hexagonal guide <span class="text-[#ff6a3d]">→</span></div>
                <div class="text-[#9090a0] text-xs mt-1">The dependency rule, layer responsibilities, layout, and testing strategy.</div>
              </a>
            </div>
          </div>
        </div>
      </div>
    </div>
  </section>

  <!-- Live introspect -->
  <section id="live" class="relative z-10 py-16 sm:py-20 border-y border-white/5 bg-[#0f0e14]">
    <div class="max-w-3xl mx-auto px-4 sm:px-6">
      <div class="text-center mb-8 sm:mb-10">
        <h2 class="text-2xl sm:text-3xl md:text-4xl font-bold text-white mb-3 min-h-[1.2em]">{liveText()}</h2>
        <p class="text-[#a0a0b0] text-sm sm:text-base">Run a sutegi app locally, then read its whole surface from here.</p>
      </div>
      <div class="bg-[#13121a] border border-white/5 rounded-xl p-5 sm:p-6">
        <div class="flex flex-col sm:flex-row gap-3">
          <input type="text" bind:value={baseUrl} placeholder="http://localhost:8080"
            class="flex-1 px-4 py-3 bg-black/30 border border-white/10 rounded-lg text-white placeholder-[#606070] focus:outline-none focus:border-[#ff6a3d]/50 text-sm font-mono" />
          <button onclick={introspect} class="px-6 py-3 bg-[#ff6a3d] text-[#1a0d06] rounded-lg font-semibold text-sm hover:bg-[#ffaa33] transition-colors flex items-center justify-center gap-2">
            {#if loadingLive}<span class="inline-block w-4 h-4 border-2 border-black/30 border-t-black rounded-full animate-spin"></span> …{:else}<Search size={16} /> Introspect{/if}
          </button>
        </div>
        {#if liveErr}
          <div class="mt-4 text-[#ff6a3d] text-sm">{liveErr}</div>
        {/if}
        {#if live}
          <div class="mt-5 space-y-3">
            <div class="font-mono text-sm text-white">{live.framework} · <span class="text-[#ffaa33]">{live.name}</span> <span class="text-[#7a7a8a]">v{live.version}</span></div>
            <div class="flex gap-4 text-xs font-mono text-[#9090a0]">
              <span>{(live.routes || []).length} routes</span>
              <span>{(live.models || []).length} models</span>
              <span>{(live.tools || []).length} tools</span>
            </div>
            <div class="space-y-1">
              {#each (live.routes || []).slice(0, 8) as r}
                <div class="font-mono text-[12px] text-[#b0b0c0]"><span class="text-[#ff6a3d]">{r.method}</span> {r.pattern} <span class="text-[#7a7a8a]">— {r.doc}</span></div>
              {/each}
            </div>
          </div>
        {/if}
      </div>
    </div>
  </section>

  <!-- CTA -->
  <section class="relative z-10 py-16 sm:py-20">
    <div class="max-w-3xl mx-auto px-4 sm:px-6 text-center">
      <h2 class="text-2xl sm:text-3xl md:text-4xl font-bold text-white mb-4">Light the forge.</h2>
      <p class="text-[#a0a0b0] mb-6 sm:mb-8 text-sm sm:text-base max-w-xl mx-auto">Zero dependencies, a tiny binary, and an agent-native surface — from std up.</p>
      <div class="flex flex-col sm:flex-row gap-3 sm:gap-4 justify-center">
        <a href="#quickstart" class="px-6 py-3 bg-[#ff6a3d] text-[#1a0d06] rounded-full font-semibold text-sm hover:shadow-[0_0_25px_rgba(255,106,61,0.4)] hover:-translate-y-0.5 transition-all flex items-center justify-center gap-2">Get started <ArrowRight size={16} /></a>
        <a href="https://github.com/enekos/sutegi" target="_blank" rel="noopener" class="px-6 py-3 border border-white/10 rounded-full text-white text-sm hover:bg-white/10 hover:border-[#ff6a3d]/50 transition-all flex items-center justify-center gap-2"><GitBranch size={14} /> Star on GitHub</a>
      </div>
    </div>
  </section>

  <!-- Footer -->
  <footer class="relative z-10 py-8 sm:py-10 bg-[#0b0a10] border-t border-white/5">
    <div class="max-w-6xl mx-auto px-4 sm:px-6 flex flex-col sm:flex-row justify-between items-center gap-4 text-[#707080] text-xs sm:text-sm">
      <div class="flex items-center gap-2 font-semibold text-white"><Flame size={18} class="text-[#ff6a3d]" /> sutegi</div>
      <div class="flex items-center gap-4 sm:gap-6">
        <a href="https://github.com/enekos/sutegi" target="_blank" rel="noopener" class="hover:text-white transition-colors">GitHub</a>
        <a href="https://github.com/enekos/sutegi#readme" target="_blank" rel="noopener" class="hover:text-white transition-colors">README</a>
        <a href="https://github.com/enekos/sutegi/tree/master/examples" target="_blank" rel="noopener" class="hover:text-white transition-colors">Examples</a>
      </div>
      <div>MIT · Built by <a href="https://github.com/enekos" target="_blank" rel="noopener" class="text-[#a0a0b0] hover:text-white transition-colors">enekos</a></div>
    </div>
  </footer>
</div>
