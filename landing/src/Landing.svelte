<script lang="ts">
  import { onMount } from 'svelte';
  import {
    Flame, Zap, ArrowRight, Copy, Check, GitBranch, ChevronDown, Terminal, TerminalSquare,
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
  const finalTitle = 'Laravel for Rust';
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
  const docsText = useScramble('Documentation', () => !!visibleSections['docs']);
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

  // --- docs accordion ---
  let openSection = $state<string | null>('architecture');
  function toggle(id: string) { openSection = openSection === id ? null : id; }

  // --- code snippets (kept as strings so braces are literal text) ---
  const codeCargo = `[dependencies]
# default = ["derive", "orm", "validate", "ai", "queue"]
sutegi = { version = "*", features = ["sqlite", "graceful"] }
# minimal HTTP service:
# sutegi = { version = "*", default-features = false }`;

  const codeMain = `use sutegi::prelude::*;

fn main() -> std::io::Result<()> {
    App::new("hello")
        .get("/", "Health check", |_req, _p| text(200, "sutegi up"))
        .run_graceful("0.0.0.0:8080")
}`;

  const codeTalk = `curl localhost:8080/__introspect   # full app surface as JSON
curl localhost:8080/__tools        # LLM tool-calling manifest
curl -X POST localhost:8080/__tools/create_todo -d '{"title":"ship sutegi"}'`;

  const docSections = [
    {
      id: 'architecture', icon: Boxes, title: 'Crate architecture',
      intro: 'Only json + http + web are always compiled. Every other pillar is an opt-in cargo feature, so the binary carries exactly what you use — core "hello" is ~362 KB, a full SQLite build ~1.3 MB.',
      code: `orm       schema + query builder + migrations
derive    #[derive(Model)]  (build-time syn/quote only)
validate  request / tool validation
ai        Tool / StreamTool + /__tools
queue     background jobs
sqlite    bundled, runnable SQLite execution
graceful  SIGTERM/SIGINT draining (libc)
hex       hexagonal-architecture primitives`,
    },
    {
      id: 'routing', icon: Workflow, title: 'Routing & middleware',
      intro: 'The request → handler → response spine: path params, route groups with shared middleware, and before/after middleware. Handlers are plain closures.',
      code: `App::new("api")
    .get("/todos/:id", "Show one", |_req, p| {
        text(200, p.get("id").unwrap())
    })
    .group("/admin", vec![mw(auth)], |g| {
        g.get("/stats", "Admin stats", stats_handler)
    })
    .after(cors("*"))                 // transform every response
    .run("0.0.0.0:8080")?;`,
    },
    {
      id: 'orm', icon: Database, title: 'ORM & query builder',
      intro: 'A driver-agnostic, parameterized builder (SELECT/UPDATE/DELETE) with OR groups, IS NULL, LIKE, joins, GROUP BY, DISTINCT, and a raw escape hatch. Plus migrations, transactions, counts, upsert, and pagination. Enable the sqlite feature for a runnable bundled engine; rows come back typed or as JSON.',
      code: `Todo::migrate(&db)?;
let id = Todo::create(&db, &[("title", Value::Text("x".into()))])?;

QueryBuilder::table("todos")
    .filter("done", "=", Value::Bool(false))
    .or_group(&[("priority", "=", Value::Text("high".into())),
                ("pinned", "=", Value::Bool(true))])   // AND (a OR b)
    .where_not_null("title").like("title", "%sutegi%")
    .join("users", "users.id", "todos.user_id")
    .order_by("id", true).build();

db.transaction(|tx| { tx.insert("todos", &[/* … */])?; Ok(()) })?;
Todo::update(&db, Value::Int(id), &[("done", Value::Bool(true))])?;
let page = db.paginate(&Todo::query().order_by("id", true), 2, 20)?;`,
    },
    {
      id: 'derive', icon: FileCode, title: '#[derive(Model)]',
      intro: 'Typed models that hydrate from rows and serialize to JSON, with clean bool round-tripping. Table name is inferred (snake_case + plural) unless you set it. The macro is build-time only — its deps never reach your binary.',
      code: `#[derive(Model)]
#[model(table = "todos")]
struct Todo {
    #[model(primary)]
    id: i64,
    title: String,
    done: bool,            // round-trips as a real bool
    note: Option<String>,  // Option<T> => nullable column
    #[model(skip)]
    cached: bool,          // not persisted; default-initialized
}`,
    },
    {
      id: 'validation', icon: ShieldCheck, title: 'Validation',
      intro: 'A Laravel-Validator-style rule set and a JSON Schema validator, both emitting structured per-field errors. AI tool arguments are validated automatically against each tool’s schema.',
      code: `let rules = Ruleset::new()
    .field("email", &[Rule::Required, Rule::Email])
    .field("age",   &[Rule::Integer, Rule::Between(18.0, 120.0)])
    .field("site",  &[Rule::Url])
    .field("password_confirmation", &[Rule::Same("password".into())]);

rules.validate(&body)?;   // Err(ValidationErrors) -> errs.to_json()
// { "email": ["The email must be a valid email address."] }`,
    },
    {
      id: 'streaming', icon: Radio, title: 'Streaming & SSE',
      intro: 'Because the server is blocking thread-per-connection, streaming is trivial and naturally backpressured. Stream raw bytes or Server-Sent Events — the transport for live LLM tokens.',
      code: `.get("/stream", "SSE demo", |_req, _p| sse(|sink| {
    for token in answer().split(' ') {
        sink.data(token)?;        // each frame flushed immediately
    }
    sink.event("done", "{}")
}))`,
    },
    {
      id: 'jobs', icon: Cpu, title: 'Background jobs',
      intro: 'A zero-dependency in-process queue with worker threads, retries, delayed dispatch, and introspectable stats — for the work you do after the response.',
      code: `struct Notify { to: String }
impl Job for Notify {
    fn name(&self) -> &str { "notify" }
    fn handle(&self) -> Result<(), String> { /* send … */ Ok(()) }
    fn tries(&self) -> u32 { 3 }   // retried on Err
}

let queue = Queue::new(4);
queue.dispatch(Notify { to: "a@b.com".into() });
let stats = queue.stats();         // dispatched / processed / failed / retried`,
    },
    {
      id: 'ai', icon: Zap, title: 'AI tools & the agent contract',
      intro: 'Tool-calling is a first-class concept. Implement Tool (or StreamTool), and sutegi exposes an LLM manifest plus an invocation endpoint. An agent discovers the whole app and acts — over plain JSON, no SDK.',
      code: `struct CreateTodo;
impl Tool for CreateTodo {
    fn name(&self) -> &str { "create_todo" }
    fn description(&self) -> &str { "Create a todo." }
    fn parameters(&self) -> Json {
        schema::object(vec![("title", schema::string("the title"))], &["title"])
    }
    fn call(&self, args: Json) -> Result<Json, String> { /* … */ }
}
// GET  /__tools             -> manifest  { name, description, input_schema }
// POST /__tools/create_todo -> invoke (args validated -> 422 on failure)
// POST /__tools/:name/stream -> SSE for streaming tools`,
    },
    {
      id: 'hex', icon: Layers, title: 'Hexagonal architecture',
      intro: 'The hex toolkit nudges you toward ports & adapters: domain stays pure, the application depends on port traits, and adapters (HTTP, AI, SQLite) plug in at the edges. One use case, many transports; testable without a server.',
      code: `impl UseCase for CreateTodo {
    type Input = String;          // title
    type Output = Todo;
    fn execute(&self, title: String) -> AppResult<Todo> {
        let todo = Todo::new(title).map_err(AppError::invalid)?;
        let id = self.repo.insert(&todo)?;   // outbound port
        Ok(Todo { id, ..todo })
    }
}
// inbound HTTP adapter:
.post("/todos", "Create", move |req, _p| respond_created(uc.execute(title)))`,
    },
    {
      id: 'scaling', icon: Server, title: 'Scaling & pods',
      intro: 'Built-in operational endpoints (always on) and graceful shutdown make a sutegi process safe to run as a fleet. SIGTERM stops accepting, drains in-flight requests, then exits — exactly what a Kubernetes rolling update needs.',
      code: `App::new("api")
    .workers(env_or("WORKERS", "8").parse().unwrap_or(8))
    .readiness(move || db.lock().unwrap().query("SELECT 1", &[]).is_ok())
    .run_graceful("0.0.0.0:8080")?;

// GET /__health   liveness        GET /__ready    readiness (200/503)
// GET /__metrics  Prometheus      GET /__introspect  full surface`,
    },
    {
      id: 'sail', icon: Rocket, title: 'Sail & deployment',
      intro: 'A Laravel-Sail-style harness wraps Docker Compose: N app replicas behind an nginx load balancer (proxy_buffering off, so SSE passes straight through). Kubernetes manifests ship with probes, graceful drain, and Prometheus annotations.',
      code: `./sail up 3            # 3 replicas + LB on http://localhost:8080
./sail curl /api/todos
./sail logs
./sail k8s apply      # apply deploy/k8s/ (probes, drain, metrics)`,
    },
    {
      id: 'cli', icon: Terminal, title: 'CLI',
      intro: 'The sutegi command scaffolds apps with rigid, predictable conventions — one right shape per artifact — so the codebase stays legible (and an LLM can extend it with minimal context). It can also introspect a running app.',
      code: `sutegi new blog            # scaffold an app
sutegi make:model Post     # src/models/post.rs (table: posts)
sutegi make:route health   # src/routes/health.rs
sutegi introspect          # pretty-print a live app's /__introspect`,
    },
  ];

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
          <strong class="text-white">sutegi</strong> (Basque: <em>forge</em>) is a batteries-included web framework with <strong class="text-white">zero third-party dependencies</strong>. The HTTP/1.1 server, JSON, router, ORM, and LLM tool layer are all hand-built on <code class="text-[#ff6a3d]">std</code> — a tiny binary, and an AI agent as a first-class user.
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
          No tokio. No serde. No hyper. The HTTP/1.1 parser, JSON codec, router, ORM query builder, and tool layer are all original code you can read in an afternoon.
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
          <p class="text-[#9090a0] text-sm leading-relaxed">Routes as closures, models with <code>#[derive(Model)]</code>, and tools as <code>Tool</code> impls. Each registers its own metadata.</p>
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
          { icon: Workflow, t: 'Routing & middleware', d: 'Path params, route groups, before/after middleware, typed extractors.' },
          { icon: Database, t: 'ORM & query builder', d: 'Parameterized SELECT/UPDATE/DELETE, migrations, transactions, optional SQLite.' },
          { icon: FileCode, t: '#[derive(Model)]', d: 'Typed models hydrate from rows and serialize to JSON; build-time-only macro.' },
          { icon: ShieldCheck, t: 'Validation', d: 'Laravel-style rules + JSON Schema, structured per-field errors.' },
          { icon: Radio, t: 'Streaming & SSE', d: 'Stream bytes or Server-Sent Events with natural backpressure.' },
          { icon: Cpu, t: 'Background jobs', d: 'In-process queue: workers, retries, delayed dispatch, stats.' },
          { icon: Zap, t: 'Agent-native', d: '/__introspect + /__tools: discover, manifest, invoke — over plain JSON.' },
          { icon: Layers, t: 'Hexagonal toolkit', d: 'AppError, UseCase ports, adapter glue for clean, testable apps.' },
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
          { icon: Boxes, t: 'Internal microservices', d: 'A small, dependency-light service with health/readiness/metrics and graceful shutdown out of the box.' },
          { icon: Plug, t: 'Edge & embedded', d: 'A ~362 KB binary with no async runtime fits where a full stack will not.' },
          { icon: FileCode, t: 'LLM-generated apps', d: 'Rigid scaffolding conventions mean a model can extend the codebase correctly with minimal context.' },
          { icon: Server, t: 'JSON APIs & CRUD', d: 'Routing + ORM + validation + jobs cover the everyday backend without pulling a framework zoo.' },
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
        Or scaffold one: <code class="text-[#ff6a3d]">sutegi new blog</code>. Read the full <a href="#docs" class="text-[#ff6a3d] hover:underline">documentation</a> below.
      </div>
    </div>
  </section>

  <!-- Docs -->
  <section id="docs" class="relative z-10 py-16 sm:py-24">
    <div class="max-w-4xl mx-auto px-4 sm:px-6">
      <div class="text-center mb-10 sm:mb-12">
        <h2 class="text-2xl sm:text-3xl md:text-4xl font-bold text-white mb-3 min-h-[1.2em]">{docsText()}</h2>
        <p class="text-[#a0a0b0] text-sm sm:text-base">Every pillar, with code. Click to expand.</p>
      </div>
      {#each docSections as s}
        {@const Icon = s.icon}
        <div class="border border-white/5 rounded-xl overflow-hidden mb-3 sm:mb-4 bg-[#13121a]">
          <button onclick={() => toggle(s.id)} class="w-full flex items-center justify-between px-5 sm:px-6 py-4 sm:py-5 hover:bg-white/5 transition-colors text-left">
            <div class="flex items-center gap-3 text-white font-semibold text-sm sm:text-base">
              <Icon size={18} class="text-[#ff6a3d] shrink-0" /> {s.title}
            </div>
            <ChevronDown size={18} class="text-[#7a7a8a] transition-transform shrink-0 {openSection === s.id ? 'rotate-180' : ''}" />
          </button>
          {#if openSection === s.id}
            <div class="px-5 sm:px-6 pb-5 sm:pb-6 text-[#b0b0c0] text-sm leading-relaxed space-y-4">
              <p>{s.intro}</p>
              <div class="relative">
                <button onclick={() => copyCmd(s.code)} class="absolute top-2 right-2 text-[11px] font-mono text-[#7a7a8a] hover:text-[#ff6a3d] border border-white/10 rounded px-2 py-1 transition-colors">copy</button>
                <pre class="bg-black/40 border border-white/5 rounded-lg p-3 sm:p-4 font-mono text-[11px] sm:text-[13px] text-[#d0d0e0] overflow-x-auto custom-scrollbar leading-relaxed">{s.code}</pre>
              </div>
            </div>
          {/if}
        </div>
      {/each}
      <div class="mt-8 grid sm:grid-cols-2 gap-3 sm:gap-4">
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
