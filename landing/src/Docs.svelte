<script lang="ts">
  import { onMount, tick } from 'svelte';
  import { Flame, GitBranch, ArrowLeft, ArrowRight, Menu, X, Search, FileText, BookOpen } from 'lucide-svelte';

  // #/docs/<section>[/<anchor>]: the page to show, and a heading on it.
  let { section = '', anchor = '' }: { section?: string; anchor?: string } = $props();

  // --- sidebar structure (Laravel-style grouped nav) ---
  // One page per item. `deep` names the hand-written long-form guide in docs/
  // that the page links to at its foot.
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
      group: 'Getting Started',
      items: [
        { id: 'installation', title: 'Installation' },
        { id: 'first-app', title: 'Your first app' },
        { id: 'configuration', title: 'Configuration' },
        { id: 'features', title: 'Feature flags' },
        { id: 'layout', title: 'Directory structure' },
        { id: 'deploying', title: 'Deployment' },
      ],
    },
    {
      group: 'Architecture Concepts',
      items: [
        { id: 'internals', title: 'Request lifecycle' },
        { id: 'crates', title: 'The workspace map' },
        { id: 'hexagonal', title: 'Hexagonal architecture', deep: 'HEXAGONAL.md' },
        { id: 'listeners', title: 'Listeners', deep: 'LISTENERS.md' },
      ],
    },
    {
      group: 'The Basics',
      items: [
        { id: 'routing', title: 'Routing' },
        { id: 'middleware', title: 'Middleware, CORS & guards' },
        { id: 'requests', title: 'Requests & the Ctx' },
        { id: 'responses', title: 'Responses & errors' },
        { id: 'templates', title: 'Templates' },
        { id: 'sessions', title: 'Sessions & CSRF' },
        { id: 'validation', title: 'Validation' },
        { id: 'static', title: 'Static files' },
        { id: 'logging', title: 'Logging', deep: 'LOGGING.md' },
      ],
    },
    {
      group: 'Digging Deeper',
      items: [
        { id: 'cli', title: 'The CLI' },
        { id: 'repl', title: 'The REPL' },
        { id: 'collections', title: 'Collections' },
        { id: 'events', title: 'Event sourcing' },
        { id: 'mail', title: 'Mail' },
        { id: 'queues', title: 'Queues' },
        { id: 'actors', title: 'Actors & supervision', deep: 'ACTORS.md' },
        { id: 'storage', title: 'File storage' },
      ],
    },
    {
      group: 'Security',
      items: [
        { id: 'auth', title: 'Authentication' },
        { id: 'crypto', title: 'Crypto primitives' },
        { id: 'security', title: 'Security posture' },
      ],
    },
    {
      group: 'Database',
      items: [
        { id: 'backend', title: 'Backends & capabilities' },
        { id: 'queries', title: 'The query builder' },
        { id: 'migrations', title: 'Migrations', deep: 'MIGRATIONS.md' },
        { id: 'concurrency', title: 'Locks, isolation, bulk', deep: 'CONCURRENCY.md' },
        { id: 'advisory', title: 'Advisory locks', deep: 'LOCKS.md' },
        { id: 'json', title: 'JSON path queries', deep: 'JSON.md' },
        { id: 'search', title: 'Full-text & hybrid search', deep: 'SEARCH.md' },
        { id: 'vectors', title: 'Embeddings & vectors' },
        { id: 'reactive', title: 'Reactive queries', deep: 'REACTIVE.md' },
        { id: 'kv', title: 'The key/value store' },
      ],
    },
    {
      group: 'ORM',
      items: [
        { id: 'models', title: 'Models' },
        { id: 'relations', title: 'Relations' },
      ],
    },
    {
      group: 'AI Agents',
      items: [
        { id: 'agents', title: 'The agent surface' },
        { id: 'tools', title: 'Defining tools' },
      ],
    },
    {
      group: 'Realtime',
      items: [
        { id: 'streaming', title: 'Streaming & SSE' },
        { id: 'websockets', title: 'WebSockets', deep: 'REALTIME.md' },
        { id: 'pubsub', title: 'PubSub', deep: 'REALTIME.md' },
        { id: 'channels', title: 'Channels & presence', deep: 'REALTIME.md' },
      ],
    },
    {
      group: 'Testing',
      items: [
        { id: 'testing', title: 'Testing' },
      ],
    },
    {
      group: 'Operations',
      items: [
        { id: 'options', title: 'Tuning & limits' },
        { id: 'ops', title: 'Operational endpoints' },
      ],
    },
  ];
  const topics = nav.flatMap((g) => g.items.map((it) => ({ ...it, group: g.group })));
  const ghBlob = 'https://github.com/enekos/sutegi/blob/master/';

  // --- one page per topic ---
  // Every article stays in the markup (the Markdown export reads them all);
  // only the current one is shown.
  const current = $derived(topics.some((t) => t.id === section) ? section : 'introduction');
  const index = $derived(topics.findIndex((t) => t.id === current));
  const page = $derived(topics[index]);
  const prev = $derived(topics[index - 1]);
  const next = $derived(topics[index + 1]);

  // "On this page": the current article's h3s, given stable ids.
  const slug = (t: string) => t.toLowerCase().replace(/&/g, 'and').replace(/[^a-z0-9]+/g, '-').replace(/^-|-$/g, '');
  let toc = $state<{ id: string; text: string }[]>([]);
  let activeHeading = $state('');

  $effect(() => {
    const id = current;
    const target = anchor;
    let io: IntersectionObserver | undefined;
    tick().then(() => {
      document.querySelectorAll('article[id]').forEach((el) => el.classList.toggle('is-active', el.id === id));
      const article = document.getElementById(id);
      if (!article) return;
      const hs = [...article.querySelectorAll('h3')];
      toc = hs.map((h) => {
        h.id ||= slug(h.textContent ?? '');
        return { id: h.id, text: h.textContent ?? '' };
      });
      activeHeading = '';
      io = new IntersectionObserver(
        (entries) => entries.forEach((e) => { if (e.isIntersecting) activeHeading = e.target.id; }),
        { rootMargin: '-10% 0px -75% 0px', threshold: 0 },
      );
      hs.forEach((h) => io!.observe(h));
      const el = target && article.querySelector(`[id="${target}"]`);
      if (el) el.scrollIntoView({ behavior: 'smooth', block: 'start' });
      else window.scrollTo({ top: 0 });
      document.querySelector('aside a[aria-current="page"]')?.scrollIntoView({ block: 'nearest' });
      renderDiagrams(article);
    });
    return () => io?.disconnect();
  });

  // --- Mermaid, loaded on first use from a pinned CDN build ---
  // Kept out of the single-file bundle: most pages have no diagram, and the
  // source stays readable in the <pre> if the CDN is unreachable.
  const MERMAID = 'https://cdn.jsdelivr.net/npm/mermaid@11.4.1/dist/mermaid.esm.min.mjs';
  let mermaid: Promise<any> | undefined;
  function renderDiagrams(root: HTMLElement) {
    const nodes = [...root.querySelectorAll<HTMLElement>('pre.mermaid:not([data-processed])')];
    if (!nodes.length) return;
    mermaid ??= import(/* @vite-ignore */ MERMAID).then(({ default: m }) => {
      m.initialize({
        startOnLoad: false,
        securityLevel: 'strict',
        theme: 'base',
        fontFamily: 'JetBrains Mono, monospace',
        themeVariables: {
          darkMode: true,
          background: '#0b0a10',
          fontSize: '13px',
          primaryColor: '#1a1822',
          primaryTextColor: '#e6e6eb',
          primaryBorderColor: '#ff6a3d',
          secondaryColor: '#15141c',
          tertiaryColor: '#13121a',
          lineColor: '#8a8a9a',
          textColor: '#c8c8d4',
          mainBkg: '#1a1822',
          nodeBorder: '#ff6a3d',
          clusterBkg: '#111018',
          clusterBorder: '#2e2c38',
          edgeLabelBackground: '#0b0a10',
          actorBkg: '#1a1822',
          actorBorder: '#ff6a3d',
          actorTextColor: '#e6e6eb',
          actorLineColor: '#4a4858',
          signalColor: '#c8c8d4',
          signalTextColor: '#e6e6eb',
          labelBoxBkgColor: '#1a1822',
          labelBoxBorderColor: '#ff6a3d',
          labelTextColor: '#e6e6eb',
          loopTextColor: '#c8c8d4',
          noteBkgColor: '#241a14',
          noteBorderColor: '#ff6a3d',
          noteTextColor: '#e6e6eb',
          activationBkgColor: '#2a2230',
          activationBorderColor: '#ff6a3d',
        },
      });
      return m;
    });
    mermaid.then((m) => m.run({ nodes })).catch(() => {});
  }

  // --- search: titles first, then page text ---
  let query = $state('');
  let searchEl: HTMLInputElement | undefined = $state();
  let text: Record<string, string> = {};
  onMount(() => {
    document.querySelectorAll('article[id]').forEach((el) => (text[el.id] = (el.textContent ?? '').toLowerCase()));
    const key = (e: KeyboardEvent) => {
      const typing = e.target instanceof HTMLInputElement || e.target instanceof HTMLTextAreaElement;
      if ((e.key === 'k' && (e.metaKey || e.ctrlKey)) || (e.key === '/' && !typing)) {
        e.preventDefault();
        menuOpen = true;
        searchEl?.focus();
      }
    };
    window.addEventListener('keydown', key);
    return () => window.removeEventListener('keydown', key);
  });
  const shown = $derived.by(() => {
    const q = query.trim().toLowerCase();
    if (!q) return nav;
    return nav
      .map((g) => ({ ...g, items: g.items.filter((it) => it.title.toLowerCase().includes(q) || text[it.id]?.includes(q)) }))
      .filter((g) => g.items.length);
  });
  function pick(e: KeyboardEvent) {
    if (e.key === 'Enter' && shown[0]) {
      window.location.hash = `#/docs/${shown[0].items[0].id}`;
      query = '';
      menuOpen = false;
      searchEl?.blur();
    } else if (e.key === 'Escape') {
      query = '';
      searchEl?.blur();
    }
  }

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
  const dWsUpgrade = `sequenceDiagram
    participant C as Client
    participant W as HTTP worker thread
    participant R as WsRuntime
    participant S as Reactor shard thread
    C->>W: GET /ws with Upgrade: websocket
    W->>W: check handshake, Origin, authorize, has_capacity
    alt refused before the 101
        W-->>C: 400, 403, 426 or 503
    else accepted
        W-->>C: 101 Switching Protocols
        W->>R: takeover(socket, pipelined bytes)
        R->>R: reserve slot (global and per-IP caps)
        R->>S: inject Adopt (round-robin) and wake the shard
        Note over W: worker goes back to serving HTTP
        S->>S: non-blocking, register fd with kqueue or epoll
        S->>S: on_open, then parse pipelined frames
        C->>S: frames
        S->>S: on_message inline, per-connection order
        S-->>C: queued frames (dropped past max_buffered)
    end`;

  const dPubsubFanout = `flowchart LR
    subgraph PodA["Pod A"]
        P["publish(topic, msg)"] --> LA["local subscribers on A (synchronous)"]
        P --> Q["bounded queue (full: dropped)"]
        Q --> PUB["publisher thread"]
        LSA["listener thread A"]
    end
    PUB -->|"pg_notify with envelope (instance, topic, msg)"| PG[("Postgres channel sutegi_pubsub")]
    PG -->|"NOTIFY"| LSA
    PG -->|"NOTIFY"| LSB
    LSA --> SKIP["own instance id: echo skipped"]
    subgraph PodB["Pod B"]
        LSB["listener thread B"] --> LB["local subscribers on B"]
    end`;

  const dChannelsJoin = `sequenceDiagram
    participant C as Client
    participant H as ChannelHub on the shard
    participant CB as Channel callbacks
    participant B as Broker
    participant O as Other members
    C->>H: stg:join room:1 with payload
    H->>CB: on_join(socket, payload)
    CB-->>H: Ok(response)
    H->>B: subscribe stg:chan:room:1 (first member on this pod)
    H-->>C: stg:reply ok
    H->>CB: after_join callbacks, e.g. Presence::track
    CB->>B: presence join diff, for other pods
    CB-->>O: presence_diff to local members
    CB-->>C: presence_diff, then presence_state
    C->>H: new_msg with payload
    H->>CB: on("new_msg")
    CB->>B: socket.broadcast publishes to stg:chan:room:1
    B->>H: deliver on this pod and on every subscribed pod
    H-->>C: new_msg frame
    H-->>O: same shared frame, one inject per shard`;

  const dQueueLifecycle = `stateDiagram-v2
    [*] --> Ready: dispatch
    [*] --> Scheduled: dispatch with delay
    Scheduled --> Ready: run_at reached
    Ready --> Running: claim sets attempts + 1 and locked_at
    Running --> Running: heartbeat
    Running --> [*]: Ok, row deleted
    Running --> Scheduled: Err and attempts below max, run_at = now + backoff x attempts
    Running --> Dead: Err or panic on the last attempt
    Running --> LeaseExpired: no heartbeat within visibility_timeout
    LeaseExpired --> Running: reclaimed by another worker
    LeaseExpired --> Dead: next claim finds attempts at max, worker died
    Dead --> Ready: retry(id) resets attempts
    Dead --> [*]: purge_failed
    Dead: Dead letter (failed_at set)
    note right of LeaseExpired
        heartbeat and settle are fenced on
        attempts and locked_at: after a reclaim
        the old worker matches no row and
        its result is dropped.
    end note`;

  const dActorsSupervision = `flowchart TD
    X["child thread exits: panic caught, or stopped"] --> E["Exit with id, generation, reason sent to the supervisor"]
    E --> S{"stale generation?"}
    S -->|yes| IG["ignored"]
    S -->|no| P{"restart policy"}
    P -->|"Temporary, or Transient after a clean stop"| DOWN["child stays down"]
    P -->|"Permanent, or Transient after a crash"| I{"more than max restarts in window?"}
    I -->|yes| FAIL["stop every child in reverse order, supervisor Failed"]
    I -->|no| BO["sleep the child's backoff"]
    BO --> ST{"strategy"}
    ST -->|OneForOne| R1["the exited child"]
    ST -->|RestForOne| R2["it and every later sibling"]
    ST -->|OneForAll| R3["every child"]
    R1 & R2 & R3 --> RS["stop the set in reverse order, restart in start order from the factory"]`;

  const dEventsFlow = `sequenceDiagram
    participant H as Command handler
    participant S as EventStore
    participant DB as sutegi_events
    participant P as Projection worker
    participant RM as Read model tables
    H->>S: load(stream)
    S->>DB: read the stream in version order
    S-->>H: state folded by Aggregate::apply, and version v
    H->>H: decide
    H->>S: append(stream, Expected::Version(v), events)
    S->>DB: one transaction: lock, check version, insert at head + 1
    alt stream moved past v
        S-->>H: EventError::Conflict, reload and re-decide
    else another append took the position
        S->>S: retry with quadratic backoff
    end
    S-->>H: new version
    loop every poll_interval, or on wake()
        P->>DB: lock checkpoint row, read events after it
        P->>RM: handler(event, tx) for each event
        P->>DB: advance checkpoint and commit, same transaction
    end
    Note over P,RM: a handler Err rolls back the batch and the checkpoint holds`;

  const dStorageSeam = `flowchart TD
    APP["your code"] --> ST{"Storage trait"}
    ST --> FS["FsStorage: local directory"]
    ST --> DBS["DbStorage: blobs in SQLite or Postgres"]
    ST --> S3S["S3Storage"]
    S3S -->|"sign with SigV4 and the real payload hash"| S3["S3Store"]
    S3S -->|"signed HttpRequest"| HT{"HttpTransport trait"}
    HT --> CURL["SystemCurl: https via the system curl"]
    HT --> PH["PlainHttp: pure std, refuses https"]
    HT --> OWN["your HTTP client"]
    CURL & PH & OWN --> BUCKET[("bucket: S3, R2, MinIO, Garage")]
    APP -->|"presign"| S3
    CLIENT["browser or agent"] -->|"PUT or GET the presigned URL"| BUCKET`;

  const dOpsGate = `flowchart TD
    REQ["request"] --> H{"path is /__health?"}
    H -->|yes| H200["200 ok"]
    H -->|no| RD{"path is /__ready?"}
    RD -->|yes| PR{"readiness probe passes, or none set?"}
    PR -->|yes| R200["200 ready"]
    PR -->|no| R503["503 not ready"]
    RD -->|no| OPS{"first path segment starts with __?"}
    OPS -->|no| MW["global middleware, then routes"]
    OPS -->|yes| G{"ops_guard set?"}
    G -->|yes| GR{"guard returns a response?"}
    GR -->|yes| DENY["that response, e.g. 401"]
    GR -->|no| MW2["global middleware, then /__metrics, /__introspect, /__ routes"]
    G -->|no| PUBQ{"ops_public?"}
    PUBQ -->|yes| MW2
    PUBQ -->|no| F403["403 ops surface closed"]`;

  const dDeployShape = `flowchart LR
    subgraph Compose["ontzi up N: Docker Compose"]
        LB["nginx lb on :8080"] -->|"/ with proxy_buffering off"| A1["app replica 1"]
        LB -->|"/ with proxy_buffering off"| AN["app replica N"]
        LB -->|"/__health and /__ready"| A1
        LB --> NF["any other /__ path: 404 at the LB"]
    end
    subgraph K8s["ontzi k8s apply: deploy/k8s"]
        SVC["Service :80"] --> POD["pod :8080, 3 replicas"]
        KL["kubelet"] -->|"liveness /__health"| POD
        KL -->|"readiness /__ready"| POD
        KL -->|"SIGTERM, 30s grace: stop accepting, drain"| POD
        PROM["Prometheus"] -->|"/__metrics with Bearer OPS_TOKEN"| POD
    end
    POD -.->|"shared state across pods"| PGDB[("Postgres")]`;

  const dInternals = `flowchart TD
  A["accept loop (non-blocking, honours shutdown flag)"] --> Q["bounded job queue (max_pending)"]
  Q --> P["pool worker: parse request line, headers, body"]
  P -->|"too large, bad framing or header_timeout"| REJ["413, 400, 408 or 501, then close"]
  P --> S["App service runs under catch_unwind"]
  S -->|"handler panicked"| E500["500 Internal Server Error"]
  S --> B{"response body?"}
  B -->|"Body::Upgrade"| UP["write 101 head, hand the socket to the ws reactor, worker freed"]
  B -->|"Body::Stream"| ST["write head, run producer, close"]
  B -->|"Body::Full"| W["write head and body"]
  E500 --> W
  W --> K{"keep-alive and under keep_alive_max?"}
  K -->|"yes, wait up to keep_alive_idle"| P
  K -->|"no"| X["close, worker takes the next connection"]
  ST --> X`;

  const dCrates = `flowchart TD
  CLI["sutegi-cli"] --> REPL["sutegi-repl"]
  REPL --> WEB["sutegi-web"]
  REPL --> HTTP["sutegi-http"]
  REPL -.-> ORM["sutegi-orm"]
  AUTH["sutegi-auth"] --> SESSION["sutegi-session"]
  AUTH --> WEB
  AUTH --> ORM
  AUTH -.-> MAIL["sutegi-mail"]
  SESSION --> WEB
  HEX["sutegi-hexagon"] --> WEB
  WEB --> HTTP
  WEB -.-> ORM
  WEB -.-> VALIDATE["sutegi-validate"]
  WEB -.-> ACTORS["sutegi-actors"]
  WEB -.-> CHANNELS["sutegi-channels"]
  WEB -.-> WS["sutegi-ws"]
  CHANNELS --> WS
  CHANNELS --> PUBSUB["sutegi-pubsub"]
  CHANNELS --> HTTP
  WS --> HTTP
  MAIL --> TEMPLATE["sutegi-template"]
  JOBS["sutegi-events, sutegi-queue"] --> ORM
  STORAGE["sutegi-storage"] -.-> ORM
  ORM -.-> PG["sutegi-pg"]
  PUBSUB -.-> PG`;

  const dMiddleware = `flowchart TD
  IN["request"] --> PR{"path is /__health or /__ready?"}
  PR -->|"yes"| PROBE["probe answers 200 or 503"]
  PR -->|"no"| OPS{"first routed segment starts with __?"}
  OPS -->|"yes"| G["ops_guard (403 when unset, unless ops_public)"]
  OPS -->|"no"| GM["global middleware, in registration order"]
  G -->|"passes"| GM
  G -->|"rejects"| AFTER
  GM -->|"Some(response)"| AFTER
  GM -->|"None"| BI{"/__metrics, /__logs or /__introspect?"}
  BI -->|"yes"| AFTER
  BI -->|"no"| RM{"route match"}
  RM -->|"no route"| NF["404, or 405 if another method matches"]
  RM -->|"match"| GR["group middleware"]
  GR -->|"Some(response)"| AFTER
  GR -->|"None"| H["handler(ctx)"]
  H --> AFTER["after-middleware, on every response"]
  NF --> AFTER
  PROBE --> AFTER
  AFTER --> OUT["access log line and metrics, then write"]`;

  const dHexagonal = `flowchart LR
  HTTP["inbound adapter: HTTP route"] -->|"execute(input)"| UC["use case (impl UseCase)"]
  TOOL["inbound adapter: AI tool"] -->|"execute(input)"| UC
  UC -->|"uses"| DOM["domain: entities and rules, no framework"]
  UC -->|"depends on"| PORT["outbound port: TodoRepository trait"]
  MEM["outbound adapter: in-memory repo"] -.->|"implements"| PORT
  SQL["outbound adapter: repo over a Backend"] -.->|"implements"| PORT
  ROOT["composition root (main.rs)"] -->|"picks one repo, injects it"| UC`;

  const dListeners = `sequenceDiagram
  participant App as run_until or run_graceful
  participant L as listener thread
  participant S as HTTP server
  participant F as shutdown flag
  App->>L: spawn sutegi-listener-name, call the closure with ListenerCtx
  App->>S: serve_until(addr, workers, flag)
  loop each lap
    L->>F: should_stop()?
    F-->>L: false
    L->>L: recv with a read timeout, handle the packet
  end
  Note over F: SIGTERM or SIGINT (or your code) sets the flag
  par server drains
    S->>S: stop accepting, finish in-flight requests
  and listener notices
    L->>F: should_stop()?
    F-->>L: true
    L->>L: leave the loop, return
  end
  S-->>App: serve_until returns
  App->>L: join() waits until the closure has returned
  Note over L: a panic is caught and logged once, the thread ends and is not restarted`;

  const dSessions = `sequenceDiagram
  participant B as Browser
  participant E as GET endpoint (Auth::csrf)
  participant G as require_csrf guard
  participant S as Sessions
  B->>E: GET with sutegi_session cookie, if any
  E->>S: load(req): HMAC must verify, else an empty session
  E->>S: csrf(session): reuse _csrf or mint 32 random bytes
  E->>S: save(session, resp): re-sign the payload
  E-->>B: token in the body, Set-Cookie sutegi_session=payload.signature
  B->>G: POST with the cookie and X-CSRF-Token
  G->>S: load(req), then verify_csrf in constant time
  alt token matches
    G->>G: pass, the handler runs
  else missing or wrong
    G-->>B: 419 csrf token mismatch
  end`;

  const dAuthLogin = `sequenceDiagram
  participant C as Client
  participant H as POST /login handler
  participant T as Throttle
  participant U as Users
  participant A as Auth
  C->>H: email and password
  H->>T: too_many(login key)
  alt over the limit
    H-->>C: 429 with retry_after
  else under the limit
    H->>U: authenticate(email, password)
    Note over U: PBKDF2 verify, a decoy hash for unknown emails, rehash if iterations are stale
    alt no match
      H->>T: hit(key)
      H-->>C: 401 bad credentials
    else match
      H->>T: clear(key)
      H->>A: login_remembered(req, user, resp)
      Note over A: fresh session with uid, exp and a password-hash fingerprint, pre-login data and CSRF dropped
      A-->>C: Set-Cookie session and Set-Cookie remember (selector.validator)
    end
  end`;

  const dAuthIdentify = `flowchart TD
  R["Auth::identify(req)"] --> S{"session HMAC valid and exp in the future?"}
  S -->|"yes"| B{"fingerprint matches the current password hash?"}
  B -->|"yes"| OK["user from the session, no new cookies"]
  B -->|"no"| RC
  S -->|"no"| RC{"remember store and remember cookie present?"}
  RC -->|"no"| ANON["anonymous (None)"]
  RC -->|"yes"| SEL{"selector row exists?"}
  SEL -->|"no"| ANON
  SEL -->|"yes"| VAL{"validator SHA-256 matches, unexpired, bound to the current password hash?"}
  VAL -->|"no"| DEL["delete the row, anonymous"]
  VAL -->|"yes"| ROT["rotate the validator, mint a fresh session"]
  ROT --> ATT["Identified::attach sets both cookies"]`;

  const dAgents = `sequenceDiagram
  participant A as Agent
  participant G as ops_guard
  participant App as sutegi app
  Note over G,App: every /__ path except /__health and /__ready passes the guard, 403 when none is set
  A->>G: GET /__introspect with the credential
  G->>App: pass
  App-->>A: routes, models, tools, capabilities, listeners
  A->>G: GET /__tools
  G->>App: pass
  App-->>A: name, description, input_schema, streaming for each tool
  alt streaming is false
    A->>G: POST /__tools/name with JSON arguments
    G->>App: pass
    App-->>A: 200 with the JSON result, or a JSON error
  else streaming is true
    A->>G: POST /__tools/name/stream with JSON arguments
    G->>App: pass
    App-->>A: text/event-stream frames until the server closes
  end`;

  const dTools = `flowchart TD
  REQ["POST /__tools/name"] --> OG{"ops_guard passes?"}
  OG -->|"no"| EG["the guard's 401 or 403 (403 when unset)"]
  OG -->|"yes"| MW{"global middleware passes?"}
  MW -->|"no"| EM["the middleware's response"]
  MW -->|"yes"| CT{"content-type JSON, or no body?"}
  CT -->|"no"| E415["415"]
  CT -->|"yes"| PJ{"body parses as JSON?"}
  PJ -->|"no"| E400["400"]
  PJ -->|"yes"| FIND{"unary tool with this name?"}
  FIND -->|"no, streaming tools included"| E404["404 unknown tool"]
  FIND -->|"yes"| VAL{"args match input_schema?"}
  VAL -->|"no"| E422["422 with errors by field path"]
  VAL -->|"yes"| RUN["closure(ctx, args)"]
  RUN -->|"Ok"| OK["200 JSON result"]
  RUN -->|"Err"| ERR["the error's own status"]`;

  const dStreaming = `sequenceDiagram
  participant C as Client
  participant W as pool worker
  participant H as handler
  participant P as SSE producer
  C->>W: request to an sse() route or POST /__tools/name/stream
  W->>H: run the handler
  alt stream tool arguments rejected
    H-->>C: 4xx JSON error, no stream opened
  else accepted
    H-->>W: 200 response wrapping the producer
    W->>W: after-middleware, access log, metrics
    W->>C: head with text/event-stream and connection close
    W->>P: run the producer on the same thread
    loop each chunk
      P->>C: data frame, flushed at once
    end
    P->>C: event done (by convention)
    W->>C: close the socket, worker freed
  end
  Note over P,C: once the head is out the status is fixed, a producer error or panic just closes the socket`;

  const dModelDerive = `flowchart LR
  S["derive(Model) on struct Todo"] --> M["impl Model: schema(), table(), primary_key()"]
  S --> R["impl FromRow"]
  S --> I["impl FromInput"]
  S --> H["to_json(), to_values(), save()"]
  S --> L["with_field() batch loader per relation"]
  M --> IR["TableSchema: the schema IR"]
  IR --> G["migrate:gen diff and migrate:drift"]
  IR --> Y["Model::migrate: additive dev sync"]
  R --> T["all_typed, find_typed, fetch"]
  I --> C["from_input: lenient hydrate of a partial payload"]`;

  const dRelationsEr = `erDiagram
  users ||--o{ posts : "has_many posts / belongs_to author"
  users {
    i64 id PK
    String name
  }
  posts {
    i64 id PK
    String title
    i64 author_id FK "indexed, ON DELETE CASCADE"
  }`;

  const dRelationsLoad = `sequenceDiagram
  participant App
  participant Loader as User::with_posts
  participant DB as Backend
  App->>DB: User::all_typed - SELECT * FROM users
  DB-->>App: users 1, 2, 3
  App->>Loader: with_posts(db, users)
  Loader->>DB: SELECT * FROM posts WHERE author_id IN (1, 2, 3)
  DB-->>Loader: every child row, one round-trip
  Loader->>Loader: group rows by author_id, hydrate Post via FromRow
  Loader-->>App: users with posts attached (empty Vec when none)`;

  const dQueryPipeline = `flowchart TD
  Q["Model::query() or QueryBuilder::table()"] --> S["Backend::select(qb)"]
  S --> G{"JSON used and caps.json_path off?"}
  G -- yes --> E1["Err: unsupported json_path"]
  G -- no --> B["qb.build_for(dialect)"]
  B -- "bad identifier, operator or path" --> E2["Err from the builder"]
  B -- ok --> P["SQL with ? placeholders + bound params"]
  P --> L{"row lock requested?"}
  L -- yes --> R["append FOR UPDATE / FOR SHARE per caps, or Err"]
  L -- no --> X["Backend::query(sql, params)"]
  R --> X
  X --> D1["SQLite: ? bound natively"]
  X --> D2["Postgres: ? rewritten to $1..$n"]`;

  const dBackendTraits = `classDiagram
  class Backend {
    <<trait>>
    +query(sql, params) rows
    +execute(sql, params) usize
    +insert(table, cols, pk) i64
    +upsert(table, cols, conflict, pk) i64
    +migrate(schema)
    +capabilities() BackendCaps
    +defaults: select, count, paginate, insert_many, try_lock, lock
  }
  class Transactional {
    <<trait>>
    +run_in_tx(f)
    +run_in_tx_with(isolation, f)
  }
  class BackendCaps {
    +advisory_locks CapScope
    +live_queries CapScope
    +row_locks, skip_locked, bulk_copy bool
    +json_path, json_contains, fts, vector bool
  }
  class Db {
    sqlite feature
    +try_lock via a process-wide registry
  }
  class Pg {
    postgres feature
    +try_lock via a dedicated session
    +insert_many via COPY FROM STDIN
  }
  class PgTx {
    +try_lock via xact lock, freed at COMMIT
  }
  class SqliteTx {
    caps: no locks, no isolation
  }
  Backend <|-- Transactional
  Backend <|.. Db
  Backend <|.. Pg
  Backend <|.. PgTx
  Backend <|.. SqliteTx
  Transactional <|.. Db
  Transactional <|.. Pg
  Backend ..> BackendCaps : capabilities()`;

  const dSerializableRetry = `sequenceDiagram
  participant App as App retry loop
  participant A as pg.transact_with
  participant PG as Postgres
  participant B as Other writer
  App->>A: transact_with(Serializable, f)
  A->>PG: BEGIN ISOLATION LEVEL SERIALIZABLE
  A->>PG: SELECT n - reads 0
  B->>PG: BEGIN ISOLATION LEVEL SERIALIZABLE
  B->>PG: SELECT n - reads 0
  B->>PG: UPDATE n = 1, COMMIT
  A->>PG: UPDATE n = 1
  PG-->>A: postgres error [40001] could not serialize
  A->>PG: ROLLBACK
  A-->>App: Err containing 40001
  Note over App: sutegi does not retry - the loop is yours
  App->>A: transact_with again
  A->>PG: BEGIN, SELECT n - reads 1, UPDATE n = 2, COMMIT
  A-->>App: Ok`;

  const dSkipLockedClaim = `sequenceDiagram
  participant A as Worker A
  participant PG as Postgres
  participant B as Worker B
  A->>PG: BEGIN
  A->>PG: SELECT * FROM jobs WHERE state = $1 ORDER BY id ASC LIMIT 1 FOR UPDATE SKIP LOCKED
  PG-->>A: job 1, now row-locked by A
  B->>PG: BEGIN
  B->>PG: the same SELECT ... FOR UPDATE SKIP LOCKED
  PG-->>B: job 2 - job 1 is skipped, not waited on
  A->>PG: mark job 1 claimed, COMMIT (row lock released)
  B->>PG: mark job 2 claimed, COMMIT`;

  const dAdvisoryContend = `sequenceDiagram
  participant A as Pod A
  participant PG as Postgres
  participant B as Pod B
  A->>PG: open a dedicated session, not a pooled one
  A->>PG: SELECT pg_try_advisory_lock(lock_key(janitor))
  PG-->>A: true - guard owns the session
  B->>PG: open a dedicated session
  B->>PG: SELECT pg_try_advisory_lock(lock_key(janitor))
  PG-->>B: false
  B->>PG: close the session
  Note over B: with_lock returns Ok(None) - another pod ran it
  A->>A: run f() while the guard holds the session
  A->>PG: guard drops, session closes
  Note over PG: session gone = lock released, the same path as a crashed holder`;

  const dJsonCompile = `flowchart TD
  W["where_json(meta, $.stats.views, >, 50)"] --> P{"parse_json_path"}
  P -- malformed --> E["builder error, returned at build"]
  P -- ok --> S["segments: Key stats, Key views"]
  S --> X{"executing backend dialect"}
  X -- SQLite --> L["json_extract(meta, ?) > ? with $.stats.views and 50 bound"]
  X -- Postgres --> G["(meta #>> ?)::numeric > ? with {stats,views} and 50 bound"]
  C["where_json_contains(meta, doc)"] --> Y{"executing backend dialect"}
  Y -- SQLite --> N["Err: unsupported json_contains"]
  Y -- Postgres --> K["meta @> ?::jsonb"]`;

  const dHybridRrf = `flowchart TD
  Q["hybrid_search(query text, query vector, k)"] --> LX["lexical leg: search(), top n = max(4k, 50) by _rank"]
  Q --> VQ{"capabilities().vector?"}
  VQ -- yes --> VP["pushdown: ORDER BY col <=> ? LIMIT n"]
  VQ -- no --> VB["brute force: embedding::nearest, cosine, in Rust"]
  LX --> F["RRF in Rust: score += 1 / (60 + rank) per leg"]
  VP --> F
  VB --> F
  F --> T["sort by fused score, keep top k"]
  T --> R["SELECT rows WHERE pk IN (top k)"]
  R --> O["re-order to fused rank, attach _score"]`;

  const dVectorPaths = `flowchart LR
  T["query vector + Metric"] --> BF["nearest / nearest_typed"]
  T --> PD["nearest_pushdown_typed"]
  BF --> S1["Backend::select(query): load candidate rows"]
  S1 --> S2["decode each vector, Metric::distance in Rust"]
  S2 --> S3["sort ascending, keep k"]
  PD --> P1["SELECT *, (col op ?) AS _distance ORDER BY _distance LIMIT k"]
  P1 --> P2["pgvector computes it, ANN index if one exists"]
  S3 --> OUT["(row, distance), lower is closer"]
  P2 --> OUT`;

  const dReactiveFlow = `sequenceDiagram
  participant W as Writer (any pod or psql)
  participant PG as Postgres
  participant L as LISTEN session
  participant K as Watcher worker
  participant S as Subscription
  W->>PG: BEGIN, UPDATE todos
  Note over PG: statement trigger _sutegi_watch_todos calls pg_notify(sutegi_changes, todos)
  alt the write commits
    W->>PG: COMMIT
    PG-->>L: NOTIFY delivered, payload todos
    L->>K: table todos changed
    Note over K: debounce 25 ms, coalesce the burst
    K->>PG: re-run every watched query on todos
    PG-->>K: current rows
    K->>K: diff by pk against the last snapshot
    opt diff not empty
      K->>S: Change with added, updated, removed
    end
  else the write rolls back
    W->>PG: ROLLBACK
    Note over PG,L: the queued notification is discarded, no watcher wakes
  end`;

  const dMigrateShadow = `flowchart LR
  F["migration files: ops JSON"] --> SH["shadow schema: ops folded in memory"]
  M["derive(Model) schemas"] --> GEN["migrate:gen = diff(shadow, models)"]
  SH --> GEN
  GEN --> NF["new timestamped migration file"]
  SH --> D1["drift: diff(shadow, live DB)"]
  DB["live database, introspected"] --> D1
  SH --> D2["drift: diff(shadow, models)"]
  M --> D2`;

  const dMigrateRun = `flowchart TD
  A["Migrator::run"] --> V{"versions well-formed and unique?"}
  V -- no --> X1["Err, database untouched"]
  V -- yes --> LK["poll try_lock(sutegi:migrations), up to 300 s"]
  LK -- timeout --> X2["Err: another runner is active"]
  LK -- held --> H["ensure _sutegi_migrations, read history"]
  H --> C{"checksums, names and order OK?"}
  C -- no --> X1
  C -- yes --> N{"next pending migration?"}
  N -- none --> DONE["drop lock, return applied versions"]
  N -- yes --> T["write tx on one connection"]
  T --> RC{"already in history? re-check"}
  RC -- yes --> N
  RC -- no --> UP["run up, insert history row"]
  UP -- ok --> CM["COMMIT"] --> N
  UP -- error --> RB["ROLLBACK: no schema change, no row; Err"]`;

  const dMigrationLifecycle = `stateDiagram-v2
  [*] --> Pending: file written by migrate:gen or by hand
  Pending --> Applied: migrate - up + history row in one tx
  Applied --> Pending: migrate:rollback - down + row delete in one tx
  Applied --> Tampered: file edited or renamed
  Tampered --> Applied: Migrator repair re-stamps the history
  Applied --> Orphan: file deleted from code
  note right of Tampered
    the next migrate errors before touching anything
  end note
  note right of Orphan
    a rollback batch containing it is refused whole
  end note`;

  const cInstall = `# The defaults — derive + orm + validate — suit a typical app.
cargo add sutegi

# In Cargo.toml, pick the pillars you want. The HTTP core and the agent
# tool surface are always present; there is no "ai" feature to enable.
[dependencies]
# Single-node app: bundled SQLite + graceful shutdown.
sutegi = { version = "0.13", features = ["sqlite", "graceful"] }

# Multi-pod app: Postgres, the durable queue, cross-pod realtime.
# sutegi = { version = "0.13", features = [
#     "postgres", "queue", "channels", "pubsub-postgres", "graceful" ] }

# Nothing but the HTTP core — ~480 KB, no ORM, no derives:
# sutegi = { version = "0.13", default-features = false }`;

  const cFirstApp = `use sutegi::prelude::*;

fn main() -> std::io::Result<()> {
    let ops_token = std::env::var("OPS_TOKEN").expect("set OPS_TOKEN");
    App::new("hello")
        .ops_guard(bearer(&ops_token))
        .get("/", "Health check", |_| "sutegi up")
        .get("/hello/:name", "Greet someone", |c| {
            format!("hi, {}", c.param("name").unwrap_or("world"))
        })
        .serve()   // reads HOST/PORT/WORKERS (or argv[1]); drains on SIGTERM
}`;

  const cFirstRun = `OPS_TOKEN=dev-token cargo run
# sutegi · hello on http://0.0.0.0:8080
#   ops: /__health /__ready /__metrics /__introspect

# From another terminal:
curl localhost:8080/hello/ada        # -> hi, ada
curl -H 'Authorization: Bearer dev-token' localhost:8080/__introspect`;

  const cConfig = `use sutegi::config::Config;

let cfg = Config::load();                  // .env (if present) + process env (env wins)
let port    = cfg.int("PORT", 8080);
let debug   = cfg.bool("DEBUG", false);    // 1/true/yes/on  ->  true
let hosts   = cfg.list("ALLOWED_HOSTS");   // comma-separated -> Vec<String>
cfg.require_all(&["DATABASE_URL", "API_KEY"])?;   // fail fast, listing every missing key
let db_cfg  = cfg.prefixed("DB_");         // DB_HOST/DB_PORT  ->  HOST/PORT`;

  const cEnv = `# read by .serve() itself
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
MAIL_HOST=… MAIL_PORT=… MAIL_USERNAME=… MAIL_PASSWORD=…`;

  const cCli = `sutegi new blog              # scaffold an app in the conventional layout
sutegi make:model Post       # src/models/post.rs   (table: posts)
sutegi make:route health     # src/routes/health.rs with register(app)
sutegi introspect [addr]     # pretty-print a running app's /__introspect
sutegi repl 127.0.0.1:8080   # drive a running app over the agent contract
sutegi version | help`;

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
    // Any other verb goes through the generic .route(...):
    .route(Method::Patch, "/todos/:id", "Patch a todo", |_| status(204))
    // A rest pattern captures the remainder of the path:
    .get("/files/*path", "Serve a file", |c| c.param("path").unwrap_or("").to_string())
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

    // Headers, the raw body, the peer
    let auth  = c.header("authorization");        // Option<&str>, case-insensitive
    let bytes = &c.req.body;                      // &[u8] — the raw request body
    let peer  = c.req.peer.as_deref();            // Option<&str> — the socket address

    // Bodies, parsed
    let body: Json = c.json()?;                    // application/json
    let form       = c.form();                     // application/x-www-form-urlencoded

    // Shared application state, registered once with .state(...)
    let db  = c.db::<Db>();                        // the pooled DB handle
    let cfg = c.state::<AppConfig>();              // panics if not registered
    let opt = c.try_state::<Mailer>();             // Option<&Mailer>
    Ok(body)
})`;

  const cResponses = `// A handler returns anything that is IntoResponse:
|_| "a string"                                  // 200 text/plain
|_| Json::obj(vec![("ok", Json::Bool(true))])   // 200 application/json
|_| (201, some_json)                            // an explicit (status, body)
|_| status(204)                                 // a bare status code
|_| no_content()                                // 204, spelled out
|_| html(200, "<h1>hi</h1>")                    // 200 text/html
|_| redirect("/login")                          // 302
|c| c.model::<Todo, Db>("id").map(|t| t.to_json())  // Result — ? just works

// Errors carry a status, a message, and optional per-field detail.
Err(Error::not_found("no such todo"))           // 404
Err(Error::unauthorized("log in first"))        // 401
Err(Error::unprocessable("bad shape")           // 422 with structured fields
    .with_fields(errors.to_json()))

// 4xx messages ARE the API and render verbatim. A 5xx does not:
// its message goes to stderr for the operator and the client gets
// {"error":"internal error"} — a SQL string or a disk path never leaks.`;

  const cMiddleware = `// Before-middleware returns Some(Response) to short-circuit, None to continue.
fn require_key(req: &Request) -> Option<Response> {
    match req.header("x-api-key") {
        Some("secret") => None,                       // allow
        _ => Some(status(401)),                       // block
    }
}

App::new("api")
    // App-wide middleware, in registration order:
    .middleware(logger())                             // log every request
    .middleware(rate_limit(100, Duration::from_secs(60)))
    // After-middleware rewrites the outgoing response:
    .after(secure_headers())
    .after(cors("https://app.example.com"))
    // A browser frontend on another origin that must send its session cookie
    // needs the credentialed pair — plain cors() is not enough:
    .after(cors_credentialed("https://app.example.com"))
    .middleware(cors_preflight_credentialed("https://app.example.com", "GET, POST", "content-type"))
    // Gate the whole /__ agent + ops surface (probes stay open):
    .ops_guard(|req| match req.header("authorization") {
        Some(t) if t == "Bearer ops-token" => None,
        _ => Some(status(401)),
    })
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
    let todo: Todo = c.validated()?;   // JSON body: parse + validate + hydrate, or 422
    let id = todo.save(c.db::<Db>())?;
    Ok::<_, Error>((201, Todo { id, ..todo }.to_json()))
})

// The same, over the other three input surfaces:
let todo: Todo = c.validated_form()?;    // application/x-www-form-urlencoded
let filter: Filter = c.validated_query()?;  // the query string
let key: Key = c.validated_path()?;      // the path parameters

// (b) Ad-hoc: a ruleset for a shape that has no model.
let rules = Ruleset::new()
    .field("email", &[Rule::Required, Rule::Email])
    .field("age",   &[Rule::Integer, Rule::Between(18.0, 120.0)])
    .field("site",  &[Rule::Url])
    .field("password_confirmation", &[Rule::Same("password".into())]);
let body = c.validate(&rules)?;   // Err -> { "email": ["must be a valid email"] }`;

  const cStatic = `App::new("site")
    .get("/api/health", "Health check", |_| "ok")   // API routes first…
    .static_dir("/assets", "public/assets")        // …then the file trees
    .static_dir("/", "dist")                        // dist/index.html is the site root
    .serve()

// Routes match in registration order, so register static_dir last.
// A directory (or the bare prefix) serves its index.html; traversal,
// dotfiles and backslashes are 404s, never a read outside the root.`;

  const cModels = `#[derive(Model, Validate)]
#[model(table = "todos")]          // omit to infer snake_case + plural
struct Todo {
    #[model(primary)]
    id: i64,                        // the DB assigns this on insert
    #[model(unique)]
    slug: String,                   // a UNIQUE constraint in the schema
    #[model(index)]
    owner_id: i64,                  // a secondary index
    title: String,
    #[model(default = false)]
    done: bool,                     // DEFAULT false, so an ADD COLUMN is safe
    note: Option<String>,           // Option<T> -> a nullable column
    #[model(column = "created_ms")]
    created: i64,                   // a column name that differs from the field
    #[model(vector(dim = 384))]
    embedding: Vec<f32>,            // vector(384) on PG, TEXT on SQLite
    #[model(skip)]
    cached: bool,                   // not persisted; default-initialised
}

let db = Db::open_or_memory("DATABASE_PATH");   // pooled, Send + Sync + Clone
Todo::migrate(&db).unwrap();                     // dev-mode additive sync

let all: Vec<Todo> = Todo::all_typed(&db)?;               // typed reads
let one: Option<Todo> = Todo::find_typed(&db, 1.into())?;
let count = Todo::count(&db)?;
let id = todo.save(&db)?;                                  // insert; returns the new pk
Todo::update(&db, 1.into(), &[("done", true.into())])?;   // by primary key
Todo::delete(&db, 1.into())?;

// In a handler, route-model binding parses the param, loads the row, or 404s:
.get("/todos/:id", "show", |c| c.model::<Todo, Db>("id").map(|t| t.to_json()))`;

  const cRelations = `#[derive(Model)]
struct User {
    #[model(primary)] id: i64,
    name: String,
    #[model(has_many(Post, foreign_key = "author_id"))]
    posts: Vec<Post>,               // not a column
}

#[derive(Model)]
struct Post {
    #[model(primary)] id: i64,
    title: String,
    #[model(index)] author_id: i64,
    #[model(belongs_to(User, foreign_key = "author_id", on_delete = "cascade"))]
    author: Option<User>,           // the FK flows into the schema IR
}

// The derive generates one batch loader per relation: two queries, never N+1.
let users = User::all_typed(&db)?;
let users = User::with_posts(&db, users)?;      // WHERE author_id IN (…)
for u in &users { println!("{} wrote {}", u.name, u.posts.len()); }

let posts = Post::with_author(&db, Post::all_typed(&db)?)?;`;

  const cQueries = `// The parameterized query builder — never string-concatenated, and identifiers
// are guarded against injection (important when an AI tool arg reaches a column).
let overdue = Todo::query()
    .filter("done", "=", false.into())
    .filter("due", "<", now.into())
    .filter_in("owner_id", vec![1.into(), 2.into()])
    .or_group(&[("priority", "=", "high".into()),
                ("pinned",   "=", true.into())])
    .where_not_null("assignee")
    .like("title", "%urgent%")
    .join("users", "users.id", "todos.owner_id")     // JOIN / LEFT JOIN
    .group_by(&["users.name"]).distinct()
    .where_raw("created_at > ?", vec![0.into()])      // the explicit escape hatch
    .order_by("due", false)          // false = ASCENDING; true = DESC
    .limit(20)
    .offset(0);

let rows: Vec<Todo> = db.fetch(&overdue)?;                   // typed
let one: Option<Todo> = db.fetch_one(&overdue)?;
let page: Page<Todo> = db.paginate_typed(&overdue, 1, 20)?;  // .items / .total / .has_next()
let n = db.count(&overdue)?;
let any = db.exists(&overdue)?;

// Writes have builders too — and RETURNING, so no re-SELECT race:
UpdateBuilder::table("todos").set("done", true.into())
    .filter("id", "=", 5.into()).returning(&["id", "title"]);
DeleteBuilder::table("todos").filter("id", "=", 5.into());

// Raw SQL is always available:
let rows = db.query("SELECT count(*) AS n FROM todos WHERE done = ?", &[true.into()])?;`;

  const cBackend = `// Single node — an embedded file, nothing to run.
let db = Db::open_or_memory("DATABASE_PATH");   // or Db::memory() / Db::open_pool("app.db", 8)

// Multi-pod — the SAME model code, Postgres underneath.
let pg = Pg::from_env(8)?;         // DATABASE_URL, else PGHOST/PGPORT/…; 8 pooled conns
Todo::migrate(&pg).unwrap();
let all = Todo::all_typed(&pg)?;   // an identical call site

// Write your domain against the trait and choose the store at boot:
fn active_count(store: &impl Backend) -> Result<i64, String> {
    store.count(&Todo::query().filter("done", "=", false.into()))
}

// Transactions work on either backend; the closure gets a Backend, so the
// query builder and Model helpers run inside the transaction:
db.transact(|tx| { tx.insert("todos", &[("title", "x".into())], "id")?; Ok(()) })?;`;

  const cCaps = `// Ask the store what it can do — never find out from a dialect SQL error.
let caps = db.capabilities();
if caps.skip_locked { /* the claim shape is safe to use */ }
if caps.json_contains { /* @> containment is available */ }
match caps.advisory_locks {
    CapScope::Cluster => elect_a_leader(&db)?,   // Postgres
    CapScope::Process => run_locally(&db)?,      // SQLite
    CapScope::None => run_unlocked(&db)?,
}

// Publish the block so an agent can read it too:
App::new("api")
    .register_capabilities(db.capabilities())   // -> "capabilities" in /__introspect
    .state(db)
    .serve()

// Gated features return one uniform error instead of leaking dialect SQL:
// Err("unsupported: json_contains is not available on the sqlite backend")`;

  const cConcurrency = `// Row locks — the work-queue claim shape (Postgres; see the matrix above).
db.transact(|tx| {
    let claimed = tx.select(
        &QueryBuilder::table("jobs")
            .filter("state", "=", "ready".into())
            .order_by("id", false)
            .limit(1)
            .for_update()          // or .for_share()
            .skip_locked(),        // or .nowait()
    )?;
    Ok(claimed)
})?;

// Isolation levels — a lost-update race surfaces as PG error 40001, so retry.
db.transact_with(Isolation::Serializable, |tx| {
    let n = read_counter(tx)?;
    tx.execute("UPDATE counters SET n = ? WHERE id = 1", &[(n + 1).into()])
})?;

// RETURNING on DML — the affected rows in one round-trip.
let rows = db.update_returning(
    &UpdateBuilder::table("todos")
        .set("done", true.into())
        .filter("id", "=", 7.into())
        .returning(&["id", "title", "done"]),
)?;

// Bulk insert — multi-row VALUES anywhere, wire-native COPY on Postgres
// (measured 30.8x faster than row-at-a-time at 5k rows).
let n = db.insert_many("events", &["id", "kind", "payload"], &rows)?;`;

  const cAdvisory = `use std::time::Duration;

// Try once; None = someone else holds it.
if let Some(_guard) = db.try_lock("nightly-report")? {
    run_report(&db)?;
}   // dropping the guard releases

// Wait up to 5s, then give up.
let guard = db.lock("reindex", Duration::from_secs(5))?;

// The singleton-job shape: at most one pod runs f. Ok(None) = another pod did.
db.with_lock("janitor", Duration::ZERO, || sweep(&db))?;

// Leader election — hold it for the process lifetime; followers retry.
std::thread::spawn(move || loop {
    if let Ok(Some(_leader)) = db.try_lock("scheduler-leader") {
        run_scheduler(&db);       // returns only if the scheduler stops
    }
    std::thread::sleep(Duration::from_secs(5));
});

// An operator can take the same lock from psql:
//   SELECT pg_try_advisory_lock(<sutegi_orm::lock_key("scheduler-leader")>);`;

  const cJson = `// WHERE inside the document, typed comparison:
let hot = db.select(
    &QueryBuilder::table("docs")
        .where_json("meta", "$.stats.views", ">", 50.into()),
)?;

// Project a path out as a column:
let rows = db.select(
    &QueryBuilder::table("docs")
        .select(&["id"])
        .select_json("meta", "$.author.name", "author"),
)?;

// Containment — Postgres only (capabilities().json_contains):
let posts = db.select(
    &QueryBuilder::table("docs")
        .where_json_contains("meta", Json::obj(vec![("kind", Json::str("post"))])),
)?;

// The grammar is $.key.nested[0].deeper — identifier keys and [n] indexes.
// A malformed path is a builder error, and the compiled path is always
// BOUND as a parameter: SQLite json_extract(col, ?), Postgres col #>> ?.`;

  const cSearch = `use sutegi::orm::search;

search::setup(&db, "docs", "id", &["title", "body"])?;   // idempotent DDL

// Lexical, ranked best-first, with "_rank" attached to each row:
let hits = search::search(&db, "docs", "id", &["title", "body"],
                          "rust \\"job queue\\" -django", 20)?;

// Hybrid: the lexical leg + a vector leg, fused with reciprocal-rank fusion
// (sum of 1/(60+rank)); "_score" attached. One code path on both engines.
let hits = search::hybrid_search(&db, "docs", "id", &["title", "body"],
                                 "rust queue", "embedding", &query_vec, 10)?;

// Tell agents what is searchable, without source access:
App::new("api").register_search("docs", &["title", "body"])`;

  const cVectors = `use sutegi::orm::embedding::{self, Metric};

#[derive(Model)]
struct Doc {
    #[model(primary)] id: i64,
    body: String,
    #[model(vector(dim = 384))] embedding: Vec<f32>,
}

// Portable brute force — correct on every backend, ideal for SQLite:
let hits: Vec<(Doc, f32)> = embedding::nearest_typed::<Doc, _>(
    &db, &Doc::query(), "embedding", &query_vec, 10, Metric::Cosine,
)?;

// Pushdown — pgvector computes the distance and can use an ANN index:
// SELECT *, (embedding <=> ?) AS _distance FROM docs ORDER BY _distance LIMIT k
// Check capabilities().vector first (Postgres + the pgvector extension); the
// call itself does not, and on SQLite fails with an engine error.
let hits = embedding::nearest_pushdown_typed::<Doc, _>(
    &pg, "docs", "embedding", &query_vec, 10, Metric::Cosine,
)?;

// Values travel in pgvector's canonical [1,2,3] text form, so the same row
// round-trips identically on either backend. Lower distance = closer.`;

  const cReactive = `use sutegi::orm::watch::Watcher;

let watcher = Watcher::postgres(&pg)?;      // one per process (Watcher::sqlite(&db))
let sub = watcher.watch(
    Todo::query().filter("done", "=", false.into()),
    "id",                                    // the pk the diff keys on
)?;

for row in sub.rows() { /* the result at watch time */ }
while let Some(change) = sub.recv_timeout(Duration::from_secs(30)) {
    // Change { table, added, updated, removed } — only when the watched
    // result actually moved. to_json() is broadcast-ready:
    hub.broadcast("todos:lobby", "changed", &change.to_json());
}

// Postgres: watch() idempotently installs a statement-level _sutegi_watch_<t>
// trigger that pg_notify's a shared channel, received on a dedicated LISTEN
// session — so ANY pod's committed write (psql included) wakes every pod's
// watchers, and a rolled-back write never fires. SQLite: update_hook on every
// pooled connection, this process only.`;

  const cMigrations = `use sutegi::prelude::*;

fn migrations() -> Migrator {
    Migrator::new().load_dir(sutegi::migrate::MIGRATIONS_DIR).expect("load migrations")
}

fn main() -> std::io::Result<()> {
    let db = Db::open_or_memory("DATABASE_PATH");
    let models = sutegi::schemas![Todo, User];        // the desired state
    // migrate | :rollback | :status | :gen | :plan | :drift | :fresh
    if sutegi::migrate::dispatch_full(&migrations(), &db, &models,
                                      sutegi::migrate::MIGRATIONS_DIR) {
        return Ok(());                                // a subcommand ran; exit
    }
    migrations().run(&db).expect("migrate");          // else apply pending, then serve
    App::new("todo").state(db).serve()
}`;

  const cMigrateCli = `myapp migrate:gen create_todos   # diff models <-> shadow schema; write the file
myapp migrate:plan               # …show what :gen would write, without writing
myapp migrate                    # apply all pending migrations
myapp migrate:status             # the ledger: check applied, ? orphan
myapp migrate:rollback 1         # roll back the last n batches (default 1)
myapp migrate:drift              # three-way report: DB vs migrations vs models
myapp migrate:fresh              # roll everything back and re-run (dev only)`;

  const cKv = `use sutegi::orm::kv::Kv;

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
    .serve()`;

  const cAgents = `# For the included demo: OPS_TOKEN=dev-token cargo run -p todo-example
# For your own app, configure .ops_guard(bearer(&ops_token)) first.
AUTH="Authorization: Bearer dev-token"
curl -H "$AUTH" localhost:8080/__introspect
# {
#   "framework": "sutegi", "version": "0.13.0", "name": "todo",
#   "routes": [ { "method": "GET", "pattern": "/todos/:id", "doc": "…" } ],
#   "models": [ … ], "tools": [ … ],
#   "capabilities": { "backend": "postgres", "skip_locked": true, … },
#   "search": [ { "table": "docs", "columns": ["title","body"] } ],
#   "listeners": [ { "name": "statsd", "doc": "Ingests statsd on udp/8125." } ],
#   "endpoints": { "introspect": "/__introspect", "health": "/__health",
#                  "ready": "/__ready", "metrics": "/__metrics" }
# }

curl -H "$AUTH" localhost:8080/__tools
# [ { "name": "create_todo", "description": "…", "input_schema": {…}, "streaming": false } ]

curl -H "$AUTH" -H 'Content-Type: application/json' -X POST localhost:8080/__tools/create_todo -d '{"title":"ship sutegi"}'
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
    .serve()

// POST /__tools/create_todo         -> invoke (422 on a bad shape)
// POST /__tools/stream_answer/stream -> the SSE variant`;

  const cStreaming = `.get("/stream", "SSE demo", |_| sse(|sink| {
    for token in answer().split(' ') {
        sink.data(token)?;          // each frame is flushed immediately
    }
    sink.comment("keep-alive")?;    // a ':'-prefixed no-op frame
    sink.event("done", "{}")        // a named event
}))

// Raw byte streams — NDJSON, large exports — take the same shape:
.get("/export", "Stream rows", |_| stream(200, "application/x-ndjson", |sink| {
    for row in rows() { sink.write_str(&format!("{}\\n", row.to_json()))?; }
    Ok(())
}))

// Regular responses use keep-alive; streams are close-framed by design.`;

  const cWs = `use sutegi::prelude::*;

App::new("live")
    // Tune the engine BEFORE the first .ws(...) — that call starts the reactor.
    .ws_config(WsConfig { shards: 4, ping_interval: Duration::from_secs(20),
                          ..WsConfig::default() })
    .ws("/socket", "A raw WebSocket endpoint.", Ws::new()
        // A cookie-authenticated socket MUST pin its origins (CSWSH):
        .check_origin(["https://app.example.com"])
        .authorize(|req| req.header("authorization").is_some())
        .on_open(|conn, _req| conn.send_text("welcome"))
        .on_message(|conn, msg| match msg {
            Msg::Text(t) => conn.send_text(&t.to_uppercase()),
            Msg::Binary(b) => conn.send_binary(&b),
        })
        .on_close(|conn, code| println!("{} left ({code})", conn.id())))
    .serve()

// Broadcast: encode the frame ONCE and share the Arc across every queue.
let frame = text_frame("maintenance at noon");
for conn in &conns { conn.send_shared(&frame); }`;

  const cPubsub = `use sutegi::prelude::*;      // Broker, BrokerExt, PubSub (+ PgPubSub)

// Single pod: an in-process broker. Clone-cheap — it shares one registry.
let broker = PubSub::new();

// Many pods: the same Broker trait over PG LISTEN/NOTIFY. Nothing else changes.
// let broker = PgPubSub::connect(&pg_config)?;   // sutegi_pg::Config

// Subscribe with a closure (BrokerExt::on); the id is for unsubscribe.
let id = broker.on("orders", |msg: &str| { handle(msg); });
broker.publish("orders", &Json::obj(vec![("id", Json::Int(42))]).to_string());
broker.unsubscribe("orders", id);

// Local delivery is synchronous and in subscription order. Cross-pod,
// PgPubSub echoes back its own instance id and skips it — so a publish is
// delivered exactly once locally and once per other pod.
// NOTIFY payloads cap at ~8 KB: try_publish returns the error instead of
// dropping it silently. Ship ids, not blobs; delivery is at-most-once.`;

  const cChannels = `use sutegi::prelude::*;

let hub = Channels::new()
    .channel(
        Channel::new("room:*")
            .doc("A chat room. Join with a nick; messages fan out to the room.")
            .join_schema("A display name.", schema::object(
                vec![("nick", schema::string("Display name"))], &["nick"]))
            .on_join(|socket, payload| {
                let nick = payload.pointer("/nick").and_then(Json::as_str)
                    .ok_or_else(|| Json::str("nick required"))?;
                socket.assign("nick", Json::str(nick));
                Presence::track(socket, nick, Json::obj(vec![]));   // feature "presence"
                Ok(Json::Null)                        // rides the ok reply
            })
            .on("new_msg", |socket, payload| {
                socket.broadcast("new_msg", payload);  // all members, all pods
                Reply::None
            })
            .on_leave(|socket, _reason| {
                socket.broadcast_from("left", &Json::obj(vec![]));
            }),
    )
    // .broker(PgPubSub::connect(&pg_cfg)?)          // <- the only cross-pod change
    .check_origin(["https://app.example.com"])       // page on another host; same-origin is the default
    .build();

App::new("chat")
    .channels("/channels", "The chat socket.", hub.clone())   // + the /__channels manifest
    .serve()?;

// From anywhere — an HTTP handler, a background thread, the REPL:
hub.broadcast("room:1", "announcement", &Json::str("maintenance at noon"));`;

  const cChannelsWire = `// One JSON object per text frame — an object, not a positional array, so
// /__channels alone teaches an agent the protocol:
{"topic":"room:1","event":"new_msg","ref":"3","join_ref":"1","payload":{"body":"hi"}}

// Control events are stg:-prefixed and reserved:
//   stg:join  stg:leave  stg:reply {status:"ok"|"error", response}  stg:error  stg:close
// Heartbeats are a ref'd push on topic "stg", event "heartbeat".`;

  const cChannelsJs = `// sutegi_channels::JS_CLIENT is a bundled ~4 KB dependency-free client:
// auto-reconnect with capped backoff, automatic rejoin, heartbeat liveness.
const socket = new SutegiSocket("/channels");
socket.connect();
const room = socket.channel("room:1", {nick: "ada"});
room.on("new_msg", p => render(p));
room.join().receive("ok", () => {});
room.push("new_msg", {body: "hello"});`;

  const cQueues = `use std::sync::Arc;
use sutegi::queue::Queue;

let mut queue = Queue::new(db.clone());        // ANY Backend: Db or Pg
queue.migrate()?;                              // creates sutegi_jobs
queue.register("notify", |job| {
    let to = job.payload().get("to").and_then(Json::as_str).unwrap_or("");
    if job.is_last_attempt() { /* write a user-visible failure */ }
    job.heartbeat()?;                          // push the visibility window forward
    Ok(())                                     // Err -> retried with backoff
});

// Enqueue from a handler and return immediately:
queue.dispatch("notify", Json::obj(vec![("to", Json::str("a@b.com"))]))?;

// …or shape the dispatch: its own pool, one in flight per key, 3 tries.
queue.job("video.ingest", Json::obj(vec![("id", Json::str("abc"))]))
    .queue("video")
    .unique("yt:abc")          // hands back the live row's id instead of a duplicate
    .priority(10)              // runs ahead of older work in the same queue
    .max_attempts(3)
    .delay(Duration::from_secs(30))
    .dispatch()?;

let queue = Arc::new(queue);
let fast = Arc::clone(&queue).start(4);             // 4 workers on "default"
let slow = Arc::clone(&queue).start_on("video", 1); // 1 on the slow queue
// … later: fast.stop(); slow.stop();

// Ops:
queue.failed(20)?;                  // the dead letters
queue.retry(job_id, 3)?;            // revive one
queue.purge_failed(Duration::ZERO)?;
queue.stats()?; queue.stats_for("video")?;
queue.cross_pod();                  // true on Postgres — the honest answer`;

  const cActors = `use sutegi::prelude::*;

struct Counter { n: u64 }
enum Msg { Bump, Get(ReplyTo<u64>) }

impl Actor for Counter {
    type Msg = Msg;
    fn handle(&mut self, msg: Msg) {
        match msg {
            Msg::Bump => self.n += 1,
            Msg::Get(reply) => reply.reply(self.n),
        }
    }
}

let counter = spawn(Counter { n: 0 });
counter.tell(Msg::Bump)?;                                  // cast; TellError::Full = backpressure
let n = counter.ask(Msg::Get, Duration::from_secs(1))?;    // call

// OTP-style supervision: the factory re-runs on restart, so a crashed child
// comes back as a FRESH value — never with the state it died holding.
let sup = Supervisor::new("pipeline")
    .strategy(Strategy::OneForOne)          // RestForOne | OneForAll
    .intensity(3, Duration::from_secs(5))   // exceeded -> stop all, Failed
    .child(ChildSpec::new("worker-1", || Worker::new()))
    .child(ChildSpec::new("flaky-api", || ApiClient::new())
        .restart(Restart::Transient)        // Permanent | Transient | Temporary
        .backoff(Duration::from_millis(250)))
    .start();

App::new("myapp").actors(sup.registry()).serve()?;   // GET /__actors`;

  const cAuth = `use std::sync::Arc;

let users = Users::new(db.clone());       // PBKDF2-HMAC-SHA256, 600k iters (OWASP)
users.migrate()?;
let tokens = Arc::new(Tokens::new(db.clone()));
tokens.migrate()?;

let auth = Arc::new(
    Auth::new(users, Sessions::new(secret.as_bytes()))   // .insecure() for local http://
        .remember(Remember::new(db.clone())),            // selector/validator cookies
);
let throttle = Throttle::new(db.clone());                // 5 attempts / 60s by default

App::new("app")
    .state(auth.clone())
    .post("/login", "Log in", move |c| {
        let key = format!("login:{}|{}", email, c.req.peer.as_deref().unwrap_or(""));
        if let Some(retry_after) = throttle.too_many(&key)? {
            return Err(Error::new(429, "too many attempts").with_fields(
                Json::obj(vec![("retry_after", Json::Int(retry_after))])));
        }
        match auth.users.authenticate(email, password)? {
            Some(u) => { throttle.clear(&key)?;
                         Ok::<_, Error>(auth.login_remembered(c.req, &u, json(200, &u.to_json()))?) }
            None => { throttle.hit(&key)?; Err(Error::unauthorized("bad credentials")) }
        }
    })
    // identify() = session-or-remember revival; attach() sets the fresh cookies.
    .get("/me", "Current user", move |c| match auth_me.identify(c.req)? {
        Some(id) => Ok::<_, Error>(id.attach(json(200, &id.user.to_json()))),
        None => Err(Error::unauthorized("unauthenticated")),
    })
    // Guards are just middleware:
    .group("/admin", vec![mw(require_role(auth.clone(), "admin")),
                          mw(require_verified(auth.clone())),
                          mw(require_csrf(auth.clone()))], |g| { /* … */ })
    .group("/api", vec![mw(require_token(tokens.clone()))], |g| { /* stg_ bearer tokens */ })
    .serve()`;

  const cSessions = `// Signed-cookie sessions (HMAC-SHA256). No server-side store needed.
let sessions = Sessions::new(secret.as_bytes());

.post("/cart/add", "Add to cart", move |c| {
    let mut s = sessions.load(c.req);
    s.set("last_item", Json::str("sku-42"));
    Ok::<_, Error>(sessions.save(&s, json(200, &Json::obj(vec![("ok", Json::Bool(true))]))))
})

// CSRF lives inside the signed session — get-or-mint, then verify in constant time.
let mut s = sessions.load(c.req);
let token = sessions.csrf(&mut s)?;              // 32 random bytes, stable per session
let ok = sessions.verify_csrf(&s, presented);
// Auth::csrf(req, resp) is the handler shape; the require_csrf guard enforces
// X-CSRF-Token on mutating methods (419 on mismatch). It passes reads, and a
// Bearer stg_… API token sent with no session cookie — no ambient credential.
// Basic auth, other bearers, or any session cookie keep the check on.

// For callers that collect cookies before touching a response:
let set_cookie = sessions.cookie_for(&s);`;

  const cMail = `// Configure once from the environment (MAIL_* vars pick the driver:
// log for dev, memory for tests, smtp/sendmail for real delivery).
let mailer = Mailer::from_env()?;

let email = Email::new()
    .to("ada@example.com")
    .subject("Welcome")
    .text("Thanks for signing up.")
    .html("<h1>Thanks for signing up.</h1>");   // both set -> multipart/alternative
mailer.send(email)?;

// Or a themed, notification-style message — HTML card + text from the same blocks:
let theme = Theme::new("Acme")
    .brand_color("#7c3aed")
    .logo_url("https://cdn.example.com/logo.png")
    .footer("Acme Inc · Bilbao");

mailer.send(theme.message()
    .subject("Welcome!")
    .greeting("Hi Ada,")
    .line("Your account is ready.")
    .action("Verify email", &url)               // brand-colored button + fallback link
    .note("This link is valid for 24 hours.")
    .email()?
    .to("ada@example.com"))?;

// A hosted provider is one method — Transport hands you the structured Email
// AND its rendered RFC 2822 form; post whichever your API wants:
struct Resend { key: String }
impl Transport for Resend {
    fn send(&self, email: &Email, _raw: &str, id: &str) -> Result<String, String> {
        my_http_post("https://api.resend.com/emails", &self.key, &to_json(email))?;
        Ok(id.to_string())
    }
}`;

  const cStorage = `// One trait, three backends. Swap the type you construct, not the call sites.
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

// Append with optimistic concurrency (Any | NoStream | Version(n)):
store.append("account-42", Expected::Any, &[event("deposited", amount_payload(100))])?;
let (account, version) = store.load::<Account>("account-42")?;   // balance = 100
// append_tx composes with a transaction you already own.

// A checkpointed projection maintains a read model, exactly once, rebuildable:
let mut projections = Projections::new(db.clone());
projections.register("account_balances", |e, tx| {
    /* write to a read-model table in the same transaction as the checkpoint */ Ok(())
});
let _workers = std::sync::Arc::new(projections).start();`;

  const cTemplates = `let mut views = Templates::new();
views.add("row", "<li>{{ item.name }}@if(item.admin) *@endif</li>")?;
views.add("list", "<ul>@foreach(users as item)@include(row)@endforeach</ul>")?;

let html = views.render("list", &Json::obj(vec![
    ("users", Json::arr(vec![
        Json::obj(vec![("name", Json::str("Ada")), ("admin", Json::Bool(true))]),
    ])),
]))?;

// {{ }} escapes, {!! !!} does not; dot paths reach into nested objects.
// @if / @else, @foreach … as … (with loop.index / loop.first / loop.last),
// @include for partials. Templates compile once to an AST and report
// line-numbered errors. The mail layer's themed HTML renders through this.`;

  const cCollections = `use sutegi::collect;

let report = collect(orders)
    .filter(|o| o.paid)
    .group_by(|o| o.country.clone())      // HashMap<String, Collection<Order>>
    .into_iter()
    .map(|(country, os)| format!("{country}: {}", os.sum_by(|o| o.total)))
    .collect::<Vec<_>>();

// Numeric chains read left-to-right:
let total: i64 = collect(vec![1, 2, 3, 4]).filter(|n| n % 2 == 0).map(|n| n * 10).sum();

// filter/reject, map/filter_map/flat_map, pluck, partition, chunk, unique,
// sort/sort_by/sort_by_key, take/skip, reduce, sum_by, implode, each,
// tap/pipe. It Derefs to [T] and round-trips through Vec, so it costs
// nothing over doing the work by hand.`;

  const cCrypto = `use sutegi::crypto;

// Hashing & MACs — the primitives the rest of the framework is built on.
let digest = crypto::sha256(b"payload");
let mut h = crypto::Sha256::new();                   // incremental: constant memory
h.update(&chunk); let digest = h.finalize();          // S3 bodies never live twice
let mac = crypto::hmac_sha256(secret, message);
let dk  = crypto::pbkdf2_hmac_sha256(pw, &salt, 600_000);

// Per-purpose subkeys, so a signing key and an encryption key never alias:
let enc_key = crypto::hkdf_sha256(master, b"", b"session-enc", 32);

// Two-way encryption: ChaCha20-Poly1305 (RFC 8439). seal() prepends a fresh
// random nonce (nonce || ct || tag, +28 bytes), so nonce reuse is impossible
// by construction. ChaCha over AES deliberately: add-rotate-xor is
// constant-time in plain software, where AES table lookups leak via cache.
let sealed = crypto::seal(&key, b"secret")?;
let plain  = crypto::open(&key, &sealed);            // Option<Vec<u8>>

// Utilities: constant_time_eq (piped through black_box), random_bytes (OS
// entropy), hex/from_hex, base64_encode/decode, now_secs/now_millis.
// Known-answer tested against the RFC vectors, with deterministic fuzz
// coverage for round-trip, bit-flip, AAD-mismatch and truncation.`;

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

// Inbound HTTP adapter — respond_created maps AppResult to the right response:
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
// The tool surface is reachable the same way — POST /__tools/:name through
// service(), no server, no agent. For end-to-end coverage the framework's own
// suite boots a real server over a loopback socket: crates/sutegi/tests/server.rs.`;

  const cRepl = `// In-process: consume the built App; data commands light up via .db(...).
Repl::new(app).db(db).run()?;

// Or against a RUNNING app with no source access — the agent contract,
// driven by a human:
//   sutegi repl 127.0.0.1:8080

sutegi> routes
GET     /api/todos                       List todos
sutegi> tools
create_todo              [unary    ] Create a todo
sutegi> call create_todo {"title":"ship"}
{ "id": 1, "title": "ship", "done": false }
sutegi> q todos where done = false order id desc limit 5
sutegi> sql SELECT count(*) FROM todos
sutegi> kv scan flags
sutegi> events account-42 10
sutegi> jobs

// Line editing is plain stdin (zero deps); wrap with rlwrap for history.`;

  const cInternals = `accept()  ->  a fixed thread pool (WORKERS, default 8)
              |
              |  one connection per worker thread, blocking I/O
              v
   parse request line + headers   (bounded: max_header_bytes, header_timeout)
              |
              v
   ops guard  ->  global middleware  ->  route match  ->  group middleware
              |                                              |
              |                                              v
              |                                        handler(&Ctx)
              |                                              |
              v                                              v
        after-middleware  <---------------------------  IntoResponse
              |
              v
   write response  ->  keep-alive (keep_alive_idle / keep_alive_max) or close
                    ->  or detach: Body::Upgrade hands the socket to the
                        ws reactor and frees the worker immediately

// A handler panic is caught per request and becomes a 500 — one bad request
// never takes a worker down. (This needs panic = "unwind", the Rust default —
// an app that sets panic = "abort" gives that isolation up.)
// /__metrics counts requests total, in-flight, and by status class.`;

  const cListeners = `use sutegi::prelude::*;
use std::net::UdpSocket;
use std::time::Duration;

App::new("metrics-demo")
    .state(db)
    .listener("statsd", "Ingests statsd counters on udp/8125.", |ctx| {
        let sock = UdpSocket::bind("0.0.0.0:8125").unwrap();
        // Bounded read + a should_stop() check each lap IS the shutdown
        // contract: serve() joins listener threads before returning.
        sock.set_read_timeout(Some(Duration::from_millis(250))).unwrap();
        let mut buf = [0u8; 1500];
        while !ctx.should_stop() {
            if let Ok((n, _from)) = sock.recv_from(&mut buf) {
                ingest(ctx.db::<Db>(), &buf[..n]);   // the same state handlers see
            }
        }
    })
    .serve()

// The closure runs once, on a thread named sutegi-listener-statsd, started by
// run / run_until / run_graceful / serve. App::service() never spawns them, so
// the in-process request closure stays socket-free for tests and benches.
// GET /__introspect gains: "listeners": [ { "name": …, "doc": … } ]`;

  const cOptions = `use std::time::Duration;

App::new("api")
    .workers(16)                              // HTTP threads (WORKERS env wins)
    .max_body(8 * 1024 * 1024)                // 413 above this (default 2 MiB)
    .request_timeout(Some(Duration::from_secs(60)))   // per-socket read/write
    .limits(Limits {                          // …or replace the whole set
        max_body: 2 * 1024 * 1024,            // default 2 MiB
        max_header_bytes: 64 * 1024,          // default 64 KiB
        timeout: Some(Duration::from_secs(30)),
        header_timeout: Duration::from_secs(15),      // whole-request deadline
        keep_alive_idle: Duration::from_secs(5),      // idle keep-alive pins a thread
        keep_alive_max: 100,                          // requests per connection
    })
    .ws_config(WsConfig {                     // the realtime engine, if enabled
        shards: 0,                            // 0 = one per core
        max_frame: 1 << 20,
        max_message: 1 << 20,
        ping_interval: Duration::from_secs(30),
        idle_timeout: Duration::from_secs(75),        // must exceed ping_interval
        max_buffered: 1 << 20,                        // slow consumers get dropped
        max_connections: 1 << 20,
        max_connections_per_ip: 1024,
        raise_nofile: true,                           // lift RLIMIT_NOFILE at start
    })
    .serve()`;

  const cOps = `let ready = db.clone();   // the probe keeps its own pooled handle
App::new("api")
    .state(db)
    .readiness(move || ready.query("SELECT 1", &[]).is_ok())
    .register_capabilities(caps)
    .actors(sup.registry())                   // GET /__actors  (feature "actors")
    .get("/__migrations", "Migration status + drift.",
         move |_| json(200, &report))          // your own /__-mounted route
    .ops_guard(|req| gate(req))                // gates every /__ EXCEPT the probes
    .serve()?;                                 // HOST/PORT/WORKERS; SIGTERM drain

// Always on, no feature required:
// GET /__health   liveness (200 while up)     GET /__ready       readiness (200/503)
// GET /__metrics  Prometheus text             GET /__introspect  the full app surface
// GET /__tools    the LLM manifest            GET /__channels    the channel manifest`;

  const cLogging = `use sutegi::log;

log::info!("billing", "invoice sent", invoice = id, cents = total, to = email);
log::warn!("llm", "call failed", provider = "gemini", ms = elapsed, error = %err);
// {"ts":"…","level":"warn","target":"llm","msg":"call failed","provider":"gemini",
//  "ms":812,"error":"429 Too Many Requests","rid":"5f3a91c2000004d1"}

// Context every line inside the scope inherits:
let _scope = log::scope(&[("tenant", log::Value::Str(slug.into()))]);
log::annotate("user", user_id);

// One access line per request, a request id on every line it causes:
App::new("shop").access_log(AccessLog::new())

// In tests, collect instead of writing:
let (result, records) = log::capture(|| svc(request));`;

  const cDeploy = `./ontzi up 3            # 3 replicas behind an nginx LB on http://localhost:8080
./ontzi curl /api/todos
./ontzi logs
./ontzi down
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
      <button onclick={() => { menuOpen = true; tick().then(() => searchEl?.focus()); }}
        class="hidden md:flex items-center gap-2 ml-4 w-64 px-3 py-1.5 rounded-lg border border-white/10 bg-white/[0.03] text-sm text-[#7a7a8a] hover:border-white/20 transition-colors">
        <Search size={14} /> Search docs <kbd class="ml-auto text-[10px] font-mono border border-white/10 rounded px-1.5 py-0.5">⌘K</kbd>
      </button>
    </div>
    <div class="flex items-center gap-2 sm:gap-3">
      <span class="hidden md:inline text-[11px] font-mono text-[#7a7a8a] border border-white/10 rounded-full px-2.5 py-1">v0.13</span>
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
      <div class="relative mb-6">
        <Search size={14} class="absolute left-3 top-1/2 -translate-y-1/2 text-[#7a7a8a]" />
        <input bind:this={searchEl} bind:value={query} onkeydown={pick} type="search" placeholder="Search…"
          aria-label="Search the docs"
          class="w-full pl-8 pr-3 py-2 rounded-lg bg-white/[0.04] border border-white/10 text-sm text-white placeholder-[#7a7a8a] outline-none focus:border-[#ff6a3d]/60" />
      </div>
      <nav class="space-y-7">
        {#each shown as grp}
          <div>
            <div class="text-[11px] uppercase tracking-wider text-[#7a7a8a] font-semibold mb-2.5">{grp.group}</div>
            <ul class="border-l border-white/10">
              {#each grp.items as it}
                <li>
                  <a href="#/docs/{it.id}" onclick={() => { menuOpen = false; query = ''; }}
                    aria-current={current === it.id ? 'page' : undefined}
                    class="block pl-4 -ml-px border-l py-1.5 text-sm transition-colors {current === it.id ? 'border-[#ff6a3d] text-white font-medium' : 'border-transparent text-[#9090a0] hover:text-white hover:border-white/30'}">
                    {it.title}
                  </a>
                </li>
              {/each}
            </ul>
          </div>
        {:else}
          <p class="text-sm text-[#7a7a8a]">No page mentions &ldquo;{query}&rdquo;.</p>
        {/each}
      </nav>
    </aside>

    {#if menuOpen}
      <button class="fixed inset-0 top-[57px] z-10 bg-black/60 lg:hidden" onclick={() => (menuOpen = false)} aria-label="Close menu"></button>
    {/if}

    <!-- Content -->
    <main class="flex-1 min-w-0 px-5 sm:px-8 md:px-12 py-10 sm:py-14 max-w-3xl mx-auto">
      <div class="text-[#ff6a3d] text-[11px] font-mono font-semibold uppercase tracking-wider mb-2">{page.group}</div>
      <div class="prose-doc">

        <!-- ===================== PROLOGUE ===================== -->
        <article id="introduction" class="scroll-mt-24">
          <h2 class="h2">Introduction</h2>
          <p>
            <strong class="text-white">sutegi</strong> — Basque for <em>forge</em> — is a batteries-included web
            framework for Rust with a single unusual constraint: it has
            <strong class="text-white">zero third-party runtime dependencies</strong>. No tokio, no serde, no hyper,
            not even a Postgres driver crate. The HTTP/1.1 parser, the JSON codec, the router, the ORM, the Postgres
            wire protocol, the WebSocket reactor, the crypto primitives and the agent tool layer are all original
            code, built on the standard library.
          </p>
          <p>
            If you have used Laravel, the shape will feel familiar: an expressive router, an Eloquent-style ORM with
            relations and migrations, first-class validation, a durable queue, mail, sessions, storage, and a CLI that
            scaffolds the conventional pieces. If you have used Phoenix, the realtime half will: channels, presence,
            and pushed live queries. The difference is what sits underneath — nothing you did not choose to compile in
            — and one addition neither had to think about:
            <strong class="text-white">an AI agent is a first-class user of your app</strong>, able to discover and
            drive every route, model, and tool over plain JSON with no SDK.
          </p>

          <h3 class="h3">How to read these docs</h3>
          <p>
            They are written to be read in order the first time and grepped after. The path is deliberate:
          </p>
          <ul class="list">
            <li><strong class="text-white">Getting Started</strong> — install, boot a first app, configure it, pick features, and ship it.</li>
            <li><strong class="text-white">Architecture Concepts</strong> — what happens to a request, how the crates fit together, hexagonal structure, and non-HTTP listeners.</li>
            <li><strong class="text-white">The Basics</strong> — routing, middleware, the request context, responses, templates, sessions, validation and logging: the everyday request loop.</li>
            <li><strong class="text-white">Digging Deeper</strong> — the CLI and REPL, collections, event sourcing, mail, the durable queue, actors and file storage.</li>
            <li><strong class="text-white">Security</strong> — authentication, the crypto primitives, and an honest security posture.</li>
            <li><strong class="text-white">Database</strong> and <strong class="text-white">ORM</strong> — the one <code>Backend</code> trait behind SQLite and Postgres, the query builder, migrations, then the deeper seams: locks, JSON paths, search, embeddings, live queries, KV; models and relations on top.</li>
            <li><strong class="text-white">AI Agents</strong> and <strong class="text-white">Realtime</strong> — the introspection surface and tools, then streaming, WebSockets, pubsub and channels.</li>
            <li><strong class="text-white">Testing</strong> and <strong class="text-white">Operations</strong> — testing in-process, every tuning knob, and the operational endpoints.</li>
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
            <em>opt-in compile-time feature</em>. Compile in only the HTTP core and you get a ~480&nbsp;KB binary with
            no async runtime; switch on <code>sqlite</code> and <code>graceful</code> and you get an ergonomic,
            agent-native service — with nothing else along for the ride. The full <code>todo</code> example, every
            pillar plus bundled SQLite, is ~1.31&nbsp;MB.
          </p>
          <p>
            The second bet is agent-native design. Because every route, model, and tool registers its own metadata,
            the framework assembles a complete, machine-readable description of your app for free. An LLM points at
            <code>/__introspect</code>, reads the surface — including what the <em>store</em> can do — and calls
            <code>/__tools</code>. The same application you built for humans is drivable by a model without a line of
            glue.
          </p>
          <p>
            The third principle is that <strong class="text-white">the type you hold is the only thing that changes
            when you scale</strong>. Handlers, models, validation, the queue, the event store, file storage and
            channel broadcasts are written once against traits; moving from a single SQLite file to a fleet of
            Postgres-backed pods swaps a constructor, not your code. Where the engines genuinely differ, a
            <a href="#/docs/backend" class="lnk">capability bit</a> says so out loud and the gated call fails with a
            named error instead of a dialect SQL surprise.
          </p>
          <p>
            The cost of these bets is a large hand-rolled surface and a young ecosystem — which the next page
            addresses head-on.
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
            hammers every hand-rolled surface — JSON, HTTP, the crypto primitives, the Postgres wire protocol, the
            RFC&nbsp;6455 WebSocket codec, templating, SigV4 — and runs in CI as a required gate. Building that
            harness caught and fixed several real bugs (a JSON stack-overflow DoS, an HTTP unbounded line-read, an
            unchecked PG frame panic, a SCRAM iteration-count DoS, and more), and the JSON parser is checked
            round-trip against <code>serde_json</code> on hundreds of thousands of cases. The query builder guards
            identifiers against SQL injection, and the search grammar sanitizes input before it can reach engine query
            syntax — both matter precisely because an AI tool argument can reach a column, a sort slot or a match
            expression.
          </p>
          <p>
            The concurrency claims are tested against real servers, not asserted: queue claim exclusivity under six
            concurrent workers, crash recovery through an expired lease, a lost-update race surfacing as
            <code>40001</code> under <code>Serializable</code>, a rolled-back write never waking a Postgres watcher, and two OS
            processes chatting through one PostgreSQL over channels.
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
            connections must stay inside a trusted network boundary. (Object storage is already solved: the
            <a href="#/docs/storage" class="lnk">storage transport seam</a> borrows the system <code>curl</code> for
            <code>https</code>, so S3/R2 works today without a TLS stack in the tree.) TLS is the one primitive we
            will <em>not</em> hand-roll; the plan is a single curated, audited dependency (<code>rustls</code>) behind
            an opt-in <code>tls</code> feature, added when a real consumer needs it.
          </p>

          <h3 class="h3">It is a coherent solo bet, not a mature ecosystem</h3>
          <p>
            Laravel and Phoenix took years and large communities. sutegi is a broad, coherent framework built quickly
            by one maintainer — its breadth currently runs ahead of its battle-tested depth, and no real production
            workload has yet run on it under load. That is not a defect to paper over; it is the honest framing.
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

        <article id="crates" class="scroll-mt-24">
          <h2 class="h2">The workspace map</h2>
          <p>
            sutegi is one facade crate over twenty-odd small ones. You depend on <code>sutegi</code> and switch
            features on; the table is here so you know what you are reading when you open the source, and so an
            error message from <code>sutegi_orm</code> or <code>sutegi_pg</code> tells you where to look.
          </p>
          {@render diagram(dCrates, 'Internal dependencies between workspace crates; dotted edges are optional. sutegi-json, sutegi-crypto and sutegi-log, used almost everywhere, are left out.')}
          <div class="tbl-wrap">
            <table class="tbl">
              <thead><tr><th>Crate</th><th>Feature</th><th>Responsibility</th></tr></thead>
              <tbody>
                <tr><td><code>sutegi-json</code></td><td>core</td><td>JSON value, parser, serializer (deterministic key order).</td></tr>
                <tr><td><code>sutegi-http</code></td><td>core</td><td>HTTP/1.1 parsing + the blocking thread-pool server on <code>std::net</code>.</td></tr>
                <tr><td><code>sutegi-web</code></td><td>core</td><td>Router, <code>App</code>, middleware, groups, streaming, static files, <code>/__introspect</code>, and the whole agent tool surface.</td></tr>
                <tr><td><code>sutegi-crypto</code></td><td>core</td><td>SHA-256/1, MD5, HMAC, PBKDF2, HKDF, ChaCha20-Poly1305, base64, CSPRNG.</td></tr>
                <tr><td><code>sutegi-orm</code></td><td><code>orm</code></td><td>Schema IR, query builder, the <code>Backend</code> trait, migrations/diff, KV, JSON paths, search, embeddings, watchers, and the two runnable backends.</td></tr>
                <tr><td><code>sutegi-pg</code></td><td><code>postgres</code></td><td>Pure-<code>std</code> PostgreSQL driver: wire protocol v3, SCRAM-SHA-256, COPY, LISTEN, pool.</td></tr>
                <tr><td><code>sutegi-macros</code></td><td><code>derive</code></td><td><code>#[derive(Model)]</code> / <code>#[derive(Validate)]</code>. Build-time only — syn/quote never reach your binary.</td></tr>
                <tr><td><code>sutegi-validate</code></td><td><code>validate</code></td><td>Rulesets <em>and</em> a JSON Schema subset validator, one structured error shape.</td></tr>
                <tr><td><code>sutegi-queue</code></td><td><code>queue</code></td><td>Durable job queue over the <code>Backend</code> seam.</td></tr>
                <tr><td><code>sutegi-events</code></td><td><code>events</code></td><td>Append-only event store, aggregates, checkpointed projections.</td></tr>
                <tr><td><code>sutegi-ws</code></td><td><code>ws</code></td><td>RFC 6455 codec + the sharded kqueue/epoll reactor.</td></tr>
                <tr><td><code>sutegi-pubsub</code></td><td><code>pubsub</code></td><td>The <code>Broker</code> seam: in-process, or <code>PgPubSub</code> over LISTEN/NOTIFY.</td></tr>
                <tr><td><code>sutegi-channels</code></td><td><code>channels</code></td><td>Phoenix-style channels, presence, the <code>/__channels</code> manifest, the JS client.</td></tr>
                <tr><td><code>sutegi-actors</code></td><td><code>actors</code></td><td>Actor processes, OTP-style supervision trees, <code>/__actors</code>.</td></tr>
                <tr><td><code>sutegi-session</code></td><td><code>session</code></td><td>Signed-cookie sessions + CSRF tokens.</td></tr>
                <tr><td><code>sutegi-auth</code></td><td><code>auth</code></td><td>Users, passwords, guards, API tokens, remember-me, throttling, verification/reset.</td></tr>
                <tr><td><code>sutegi-mail</code></td><td><code>mail</code></td><td><code>Email</code> builder, RFC 2822/MIME rendering, themed messages, the <code>Transport</code> seam.</td></tr>
                <tr><td><code>sutegi-template</code></td><td><code>template</code></td><td>Blade-lite engine over <code>Json</code> contexts.</td></tr>
                <tr><td><code>sutegi-storage</code></td><td><code>storage</code></td><td>The <code>Storage</code> trait: local fs, DB blobs, S3/R2 + SigV4, over an injected HTTP transport.</td></tr>
                <tr><td><code>sutegi-hexagon</code></td><td><code>hexagon</code></td><td>Ports &amp; adapters primitives: <code>UseCase</code>, <code>AppError</code>, <code>respond</code>.</td></tr>
                <tr><td><code>sutegi-repl</code></td><td><code>repl</code></td><td>The tinker-style shell, in-process or over the wire.</td></tr>
                <tr><td><code>sutegi-cli</code></td><td>binary</td><td>The <code>sutegi</code> command: scaffold, introspect, repl.</td></tr>
              </tbody>
            </table>
          </div>
        </article>

        <!-- ===================== GETTING STARTED ===================== -->
        <article id="installation" class="scroll-mt-24">
          <h2 class="h2">Installation</h2>
          <p>
            Every sutegi app is an ordinary Rust binary — <code>cargo new</code>, add the crate, write a
            <code>main</code>. There is no separate runtime to install and nothing trailing behind the binary at run
            time. Choose the feature pillars you want; the HTTP core (<code>json</code> + <code>http</code> +
            <code>web</code> + <code>crypto</code>) is always present, and so is the agent tool surface.
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
            sutegi&rsquo;s pillars are Cargo features on the facade crate. Only <code>json</code>, <code>http</code>,
            <code>web</code> and <code>crypto</code> are compiled unconditionally; everything else is opt-in, so your
            binary contains exactly the surface you use. The defaults are
            <code>derive</code>, <code>orm</code>, <code>validate</code>.
          </p>
          <div class="tbl-wrap">
            <table class="tbl">
              <thead><tr><th>Feature</th><th>Default</th><th>Gives you</th></tr></thead>
              <tbody>
                <tr><td><code>orm</code></td><td>yes</td><td>Schema, query builder, <code>Backend</code> trait, migrations, KV, JSON paths, search, embeddings, watchers.</td></tr>
                <tr><td><code>derive</code></td><td>yes</td><td><code>#[derive(Model)]</code> / <code>#[derive(Validate)]</code> (build-time only).</td></tr>
                <tr><td><code>validate</code></td><td>yes</td><td>Request + tool validation, <code>Ctx::validate</code>/<code>validated*</code>.</td></tr>
                <tr><td><code>sqlite</code></td><td></td><td>The bundled single-node backend, <code>Db</code>.</td></tr>
                <tr><td><code>postgres</code></td><td></td><td>The pure-std multi-pod backend, <code>Pg</code>.</td></tr>
                <tr><td><code>graceful</code></td><td></td><td>SIGTERM/SIGINT draining for rolling deploys.</td></tr>
                <tr><td><code>queue</code></td><td></td><td>The durable job queue (SQLite <em>or</em> Postgres).</td></tr>
                <tr><td><code>events</code></td><td></td><td>Event store, aggregates, projections.</td></tr>
                <tr><td><code>ws</code></td><td></td><td><code>App::ws</code> on the kqueue/epoll reactor.</td></tr>
                <tr><td><code>pubsub</code> / <code>pubsub-postgres</code></td><td></td><td>The in-process broker / cross-pod <code>PgPubSub</code>.</td></tr>
                <tr><td><code>channels</code> / <code>presence</code></td><td></td><td>Phoenix-style channels + <code>/__channels</code> / who&rsquo;s-online tracking.</td></tr>
                <tr><td><code>actors</code></td><td></td><td>Actors, supervision trees, <code>/__actors</code>.</td></tr>
                <tr><td><code>session</code> / <code>auth</code> / <code>auth-mail</code></td><td></td><td>Cookies + CSRF / the user system / verification &amp; reset flows.</td></tr>
                <tr><td><code>mail</code> / <code>template</code></td><td></td><td>The mailer (pulls <code>template</code>) / the Blade-lite engine.</td></tr>
                <tr><td><code>storage</code> / <code>storage-db</code></td><td></td><td>Local fs + S3/R2 objects / blobs over the <code>Backend</code> seam.</td></tr>
                <tr><td><code>hexagon</code></td><td></td><td>Ports &amp; adapters primitives.</td></tr>
                <tr><td><code>repl</code></td><td></td><td>The tinker-style shell (data commands light up with <code>orm</code>).</td></tr>
              </tbody>
            </table>
          </div>
          <p>
            Features compose the obvious way: <code>auth</code> implies <code>session</code> + <code>orm</code>,
            <code>channels</code> implies <code>ws</code> + <code>pubsub</code>, <code>mail</code> implies
            <code>template</code>. Turn everything off with <code>default-features = false</code> for a minimal HTTP
            service, then add back precisely what a given deployment needs.
          </p>
          <div class="callout note">
            <div class="callout-title">There is no <code>ai</code> feature any more</div>
            <p>
              It was removed in 0.6.0. The tool surface — <code>App::tool</code>/<code>stream_tool</code>, the
              <code>schema</code> helpers, <code>ToolCtx</code>, <code>/__tools</code> — lives in
              <code>sutegi-web</code> and is always compiled. Drop <code>ai</code> from your feature list; no code
              change is needed.
            </p>
          </div>
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
            You do not have to use it: the framework reads a handful of conventional variables itself, so a bare app
            is already 12-factor.
          </p>
          {@render code(cEnv, 'env')}
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
              <code>storage</code>, <code>hexagonal</code>, <code>redactor</code> (an agent tool service),
              <code>chat</code> (channels + presence, single-pod or cross-pod), <code>ws-chat</code>,
              <code>ws-load</code> and <code>http-load</code> (the stress harnesses behind the numbers quoted here).
            </p>
          </div>
        </article>

        <article id="cli" class="scroll-mt-24">
          <h2 class="h2">The CLI</h2>
          <p>
            The <code>sutegi</code> binary is a scaffolder and a remote control. Scaffolding follows rigid conventions
            on purpose — one right shape per artifact — so an LLM can extend a sutegi app correctly with minimal
            context. <code>introspect</code> and <code>repl</code> need no source access at all: they speak the same
            agent contract an LLM would.
          </p>
          {@render code(cCli, 'cli')}
          <p>
            Migration verbs are <em>not</em> here — they live in your own binary, because they need your model set.
            See <a href="#/docs/migrations" class="lnk">Migrations</a>.
          </p>
        </article>

        <!-- ===================== THE BASICS ===================== -->
        <article id="routing" class="scroll-mt-24">
          <h2 class="h2">Routing</h2>
          <p>
            Register routes with <code>.get</code>, <code>.post</code>, <code>.put</code>, <code>.delete</code> — or
            <code>.route(method, &hellip;)</code> for anything else, including <code>PATCH</code>. Each takes a URL
            pattern, a short doc string — which shows up in <code>/__introspect</code>, so keep it meaningful — and a
            handler closure. Path parameters use <code>:name</code> and are read with <code>c.param("name")</code>; a
            trailing <code>*name</code> captures the rest of the path. Related routes can share a prefix and
            middleware via <code>.group</code>.
          </p>
          {@render code(cRouting, 'routing')}
          <p>
            The router splits on segments after trimming leading and trailing slashes, so <code>/todos/1</code> and
            <code>/todos/1/</code> are the same route. Patterns are matched in registration order; a path that exists
            under another method answers <code>405</code> rather than <code>404</code>.
          </p>
        </article>

        <article id="requests" class="scroll-mt-24">
          <h2 class="h2">Requests &amp; the Ctx</h2>
          <p>
            The <code>&amp;Ctx</code> is your window into a request and into shared application state. Path parameters,
            the query string, headers, the peer address and the raw body are all reachable from it, and the body can be
            parsed as JSON (<code>c.json()</code>) or a form (<code>c.form()</code>). State registered once with
            <code>.state(value)</code> is retrieved by type with <code>c.state::&lt;T&gt;()</code> (or
            <code>try_state</code> if it may be absent) — and, with the ORM feature, the database handle with
            <code>c.db::&lt;Db&gt;()</code>.
          </p>
          {@render code(cRequests, 'requests')}
          <p>
            State is keyed by type, one value per type: registering the same type twice replaces it. Wrap a handle in
            a newtype when you need two of the same kind.
          </p>
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
            <code>not_found</code>, <code>unprocessable</code>, <code>internal</code>, or
            <code>Error::new(status, msg)</code> for anything else) cover the common statuses, and
            <code>.with_fields(json)</code> attaches structured validation detail.
          </p>
          {@render code(cResponses, 'responses')}
          <div class="callout note">
            <div class="callout-title">5xx bodies are redacted</div>
            <p>
              A <code>4xx</code> message is part of your API and renders verbatim. A <code>5xx</code> message is an
              internal detail — a SQL error carrying your schema, a subprocess&rsquo;s stderr, a path on disk — so it
              is logged to stderr and the client gets <code>{'{'}"error":"internal error"{'}'}</code>. Anything the
              caller is meant to act on belongs in a 4xx.
            </p>
          </div>
        </article>

        <article id="middleware" class="scroll-mt-24">
          <h2 class="h2">Middleware, CORS &amp; guards</h2>
          <p>
            Middleware comes in two flavours. <strong class="text-white">Before-middleware</strong>
            (<code>Fn(&amp;Request) -&gt; Option&lt;Response&gt;</code>) runs ahead of the handler and can
            short-circuit by returning <code>Some(response)</code> — this is how auth guards and rate limits work.
            <strong class="text-white">After-middleware</strong> (<code>Fn(&amp;Request, Response) -&gt; Response</code>,
            registered with <code>.after</code>) rewrites the outgoing response — this is how CORS and secure headers
            work. Wrap a before-middleware function with <code>mw(&hellip;)</code> to attach it to a group.
          </p>
          {@render code(cMiddleware, 'middleware')}
          {@render diagram(dMiddleware, 'Where a request can be answered early; every response, early or not, still passes through after-middleware.')}
          <p>
            The batteries are built in: <code>logger()</code>, <code>rate_limit(max, per)</code>, <code>bearer</code>,
            <code>basic</code>, <code>cors</code>, <code>cors_preflight</code>, <code>cors_credentialed</code>,
            <code>cors_preflight_credentialed</code>, and <code>secure_headers</code>.
          </p>
          <div class="callout warn">
            <div class="callout-title">A cookie-authenticated frontend needs the credentialed pair</div>
            <p>
              Plain <code>cors</code> stamps <code>Access-Control-Allow-Origin</code> and nothing else — correct for a
              public API, quietly useless for a browser app on another origin that must send a session cookie. Three
              things have to be right and each fails silently alone: <code>Allow-Credentials</code> must be on the
              <em>real</em> response and not only the preflight; <code>Allow-Headers: *</code> stops being a wildcard
              once credentials are in play, so the first JSON <code>POST</code> is refused by a server that believes
              it allows everything; and a preflight without <code>Max-Age</code> is paid on every mutating call. The
              credentialed pair handles all three and is idempotent, so composing both halves cannot emit a duplicate
              <code>Allow-Origin</code> — which a browser rejects outright while <code>curl -i</code> shows a
              perfectly good <code>204</code>.
            </p>
          </div>
          <div class="callout note">
            <div class="callout-title"><code>ops_guard</code> is the one that protects <code>/__</code></div>
            <p>
              Introspection and tool invocation are <strong class="text-white">closed by default</strong>: without
              an <code>ops_guard</code>, they return <code>403</code>. Configure the guard and send the credential it
              expects. It runs ahead of the global middleware chain for every <code>/__</code>-mounted
              route (<code>/__introspect</code>, <code>/__metrics</code>, <code>/__tools*</code>,
              <code>/__channels</code>, <code>/__actors</code>, your own), while <code>/__health</code> and
              <code>/__ready</code> stay open so orchestrator probes need no credential. It is gated on the segments
              the router actually matches, not on the raw path string — see
              <a href="#/docs/security" class="lnk">Security posture</a> for why that distinction was a CVE-shaped
              bug.
            </p>
          </div>
        </article>

        <article id="validation" class="scroll-mt-24">
          <h2 class="h2">Validation</h2>
          <p>
            Never trust the request body. Deriving <code>Validate</code> alongside <code>Model</code> reads the
            <code>#[validate(&hellip;)]</code> field attributes and generates the type&rsquo;s ruleset at build time.
            In a handler, <code>c.validated::&lt;Todo&gt;()</code> parses the body, validates it, and hydrates a typed
            value — or returns a <code>422</code> with structured, per-field messages. The same ruleset drives the
            other three input surfaces (<code>validated_form</code>, <code>validated_query</code>,
            <code>validated_path</code>). For shapes without a model, build an ad-hoc <code>Ruleset</code> and call
            <code>c.validate</code>.
          </p>
          {@render code(cValidation, 'validation')}
          <p>
            The rule set covers the everyday cases: <code>Required</code>, type rules (<code>Str</code>,
            <code>Integer</code>, <code>Number</code>, <code>Bool</code>), formats (<code>Email</code>,
            <code>Url</code>, <code>Alpha</code>, <code>AlphaNum</code>), bounds (<code>Min</code>/<code>Max</code>,
            <code>Between</code>, <code>MinLen</code>/<code>MaxLen</code>), and relational rules (<code>In</code>,
            <code>Same</code>). Every path produces the same error shape:
            <code>{'{'} field: [messages] {'}'}</code>.
          </p>
          <div class="callout note">
            <div class="callout-title">Agents get this for free</div>
            <p>
              AI tool arguments are validated against each tool&rsquo;s JSON schema by a second validator in the same
              crate (types, <code>required</code>, <code>enum</code>, bounds), returning the same structured errors —
              and, for streaming tools, <em>before</em> the stream opens, so a malformed call still gets a normal JSON
              <code>422</code>.
            </p>
          </div>
        </article>

        <article id="static" class="scroll-mt-24">
          <h2 class="h2">Static files</h2>
          <p>
            <code>App::static_dir(prefix, dir)</code> mounts a directory as a rest route. It is enough to serve a
            built SPA next to your API: point <code>/</code> at <code>dist</code> and register it last, since routes
            match in registration order.
          </p>
          {@render code(cStatic, 'static')}
          <p>
            Traversal attempts, dotfiles and backslashes are <code>404</code>s rather than reads outside the root; a
            directory (or the bare prefix) serves its <code>index.html</code>.
          </p>
        </article>

        <article id="logging" class="scroll-mt-24">
          <h2 class="h2">Logging</h2>
          <p>
            <code>sutegi::log</code> is the log every sutegi crate writes through, and the one your app should use too.
            One event is one line on stderr: JSON when stderr is not a terminal, aligned text when it is. Stdout stays
            for command output. <code>key = value</code> keeps numbers and booleans typed in the JSON;
            <code>key = %value</code> formats anything <code>Display</code>. A field whose name ends in
            <code>password</code>, <code>secret</code>, <code>token</code>, <code>cookie</code>,
            <code>api_key</code> and the like is written as <code>[redacted]</code>, wherever it came from.
          </p>
          {@render code(cLogging, 'logging')}
          <p>
            <code>SUTEGI_LOG</code> sets the level with per-target overrides
            (<code>info,queue=debug,http=warn</code>) and <code>SUTEGI_LOG_FORMAT</code> picks
            <code>json</code>, <code>text</code> or <code>auto</code>. <code>AccessLog</code> opens a scope with a
            <code>rid</code> before the ops guard runs, reuses a safe inbound <code>x-request-id</code>, and returns
            the id in the response. A queued job carries the dispatching request&rsquo;s context, so the request and
            the job that later fails share one <code>rid</code>. With an <code>ops_guard</code> set,
            <code>GET /__logs</code> returns the last 512 events, filterable by level and by any field.
          </p>
        </article>

        <!-- ===================== DATABASE ===================== -->
        <article id="models" class="scroll-mt-24">
          <h2 class="h2">Models</h2>
          <p>
            Deriving <code>Model</code> on a plain struct makes it the single source of truth for a table: schema,
            migrations, typed reads, JSON serialization, and <code>save()</code> all come from it. Bools round-trip as
            real bools and <code>Option&lt;T&gt;</code> becomes a nullable column.
          </p>
          {@render code(cModels, 'models')}
          {@render diagram(dModelDerive, 'One derive on a plain struct generates the schema IR, the typed readers and writers, and a batch loader per relation.')}
          <p>
            The <code>#[model(&hellip;)]</code> attributes are the mapping controls, and everything except
            <code>skip</code> flows into the schema IR the migration diff reads:
          </p>
          <ul class="list">
            <li><code>table = "&hellip;"</code> (struct level) — override the inferred snake_case plural name.</li>
            <li><code>primary</code> — the primary key. <code>column = "&hellip;"</code> — a differing column name.</li>
            <li><code>unique</code> / <code>index</code> — a UNIQUE constraint / a secondary index.</li>
            <li><code>default = &lt;lit&gt;</code> — a column default (int, float, bool or string), which is what makes adding a <code>NOT NULL</code> column a safe migration.</li>
            <li><code>vector</code> / <code>vector(dim = N)</code> — an embedding column on a <code>Vec&lt;f32&gt;</code>.</li>
            <li><code>has_many</code> / <code>has_one</code> / <code>belongs_to</code> — a <a href="#/docs/relations" class="lnk">relation</a>, not a column.</li>
            <li><code>skip</code> — not persisted, not serialized, default-initialised on load.</li>
          </ul>
          <p>
            The derive also generates <code>from_input()</code> — a lenient hydrate from a partial client payload,
            which is what tool closures use on already-validated args — and <code>to_json()</code>. Both macros run at
            build time, so syn/quote never reach your binary; turn them off with
            <code>default-features = false</code> and hand-write the <code>Model</code> impl if you prefer.
          </p>
        </article>

        <article id="relations" class="scroll-mt-24">
          <h2 class="h2">Relations</h2>
          <p>
            A relation field is declared with an attribute, is not a column, and gets its own generated batch loader.
            <code>User::with_posts(&amp;db, users)</code> runs one <code>WHERE author_id IN (&hellip;)</code> query for
            the whole batch and attaches the typed children — the classic two-query strategy, so N+1 never arises.
          </p>
          {@render code(cRelations, 'relations')}
          {@render diagram(dRelationsEr, 'The running example: posts.author_id is the indexed foreign key both relation fields join on.')}
          <p>
            <code>belongs_to</code> also carries the foreign key into the schema, including
            <code>on_delete = "cascade"</code>, so the migration diff sees it. Join keys are integers (the usual
            primary/foreign-key case), and loading is explicit rather than lazy: there is no hidden query behind a
            field access.
          </p>
          {@render diagram(dRelationsLoad, 'with_posts loads every child in one IN (…) query and groups the rows in Rust: two queries, not N+1.')}
        </article>

        <article id="queries" class="scroll-mt-24">
          <h2 class="h2">The query builder</h2>
          <p>
            For anything beyond <code>all</code>/<code>find</code>, <code>Model::query()</code> returns a fluent,
            fully parameterized query builder. Run it through a backend to get JSON rows
            (<code>select</code>), typed values (<code>fetch</code>/<code>fetch_one</code>), a count, an existence
            check, or a <code>Page&lt;T&gt;</code> (<code>paginate_typed</code>). Values are always bound as
            parameters, and identifiers are validated against an allowlist — an AI tool argument cannot smuggle SQL
            into a column or sort slot.
          </p>
          {@render code(cQueries, 'queries')}
          <div class="callout warn">
            <div class="callout-title"><code>order_by</code>&rsquo;s bool is <em>descending</em></div>
            <p>
              <code>.order_by("due", false)</code> is <strong class="text-white">ascending</strong>;
              <code>true</code> is descending. Chain several for multi-column ordering.
            </p>
          </div>
          <p>
            <code>QueryBuilder</code>, <code>UpdateBuilder</code> and <code>DeleteBuilder</code> can also be used
            directly against a table name when there is no model, and <code>build()</code> hands you the
            <code>(sql, params)</code> pair — which is why the core ships no driver at all and the default binary stays
            tiny.
          </p>
          {@render diagram(dQueryPipeline, 'The builder only stores the request; Backend::select gates on capabilities, compiles for its dialect, adds row locks, then binds the parameters.')}
        </article>

        <article id="backend" class="scroll-mt-24">
          <h2 class="h2">Backends &amp; capabilities</h2>
          <p>
            This is the key architectural story. The ORM is written against a <code>Backend</code> trait, not a
            concrete engine. <code>Db</code> (SQLite, the <code>sqlite</code> feature) is the single-node store — one
            embedded file, zero operations. <code>Pg</code> (Postgres, via a <strong class="text-white">pure-std wire
            driver</strong>, the <code>postgres</code> feature) is the multi-pod store. Both implement
            <code>Backend</code>, so <code>Model</code> is written once and every call site is identical.
          </p>
          {@render code(cBackend, 'backend')}
          <p>
            The trait is small — five required primitives (<code>query</code>, <code>execute</code>,
            <code>insert</code>, <code>upsert</code>, <code>migrate</code>) — and everything else
            (<code>select</code>, <code>count</code>, <code>paginate</code>, transactions, locks, bulk insert&hellip;)
            is a default method implemented once on top. It is object-safe, so <code>&amp;dyn Backend</code> works;
            the typed helpers are <code>Self: Sized</code>-gated.
          </p>
          {@render diagram(dBackendTraits, 'Five required primitives plus capabilities(); each store, and each transaction handle, overrides only what it does differently.')}
          <h3 class="h3">Where the engines differ, a bit says so</h3>
          <p>
            Some things genuinely cannot be papered over. <code>Backend::capabilities()</code> returns a
            <code>BackendCaps</code> descriptor — the honest default is everything off, so a backend never advertises
            what it has not implemented — and a gated call returns
            <code>unsupported: &lt;cap&gt; is not available on the &lt;backend&gt; backend</code> before any SQL is sent.
            <code>App::register_capabilities(db.capabilities())</code> publishes the block in
            <code>/__introspect</code>, so an agent reads what the store can do instead of finding out from a dialect
            error.
          </p>
          {@render code(cCaps, 'caps')}
          <div class="tbl-wrap">
            <table class="tbl">
              <thead><tr><th>Capability</th><th>SQLite</th><th>Postgres</th></tr></thead>
              <tbody>
                <tr><td><code>advisory_locks</code></td><td>process</td><td>cluster</td></tr>
                <tr><td><code>live_queries</code></td><td>process</td><td>cluster</td></tr>
                <tr><td><code>isolation_levels</code></td><td>yes</td><td>yes</td></tr>
                <tr><td><code>returning_dml</code></td><td>yes</td><td>yes</td></tr>
                <tr><td><code>json_path</code></td><td>yes</td><td>yes</td></tr>
                <tr><td><code>fts</code></td><td>yes</td><td>yes</td></tr>
                <tr><td><code>row_locks</code> / <code>skip_locked</code></td><td>no</td><td>yes</td></tr>
                <tr><td><code>bulk_copy</code></td><td>no</td><td>yes</td></tr>
                <tr><td><code>json_contains</code></td><td>no</td><td>yes</td></tr>
                <tr><td><code>listen_notify</code></td><td>no</td><td>yes</td></tr>
                <tr><td><code>vector</code></td><td>no</td><td>yes</td></tr>
              </tbody>
            </table>
          </div>
          <p>
            A capability describes the <em>framework</em> surface, not the underlying C library: bundled SQLite ships
            JSON1, FTS5 and <code>RETURNING</code>, but a bit stays off until sutegi exposes the feature through the
            builder. Pick the backend for the deployment: SQLite for local dev, edge, and single-node; Postgres when
            you scale to many pods or need cluster-wide coordination.
          </p>
        </article>

        <article id="concurrency" class="scroll-mt-24">
          <h2 class="h2">Locks, isolation, bulk</h2>
          <p>
            Four throughput and correctness primitives, all through the same <code>Backend</code> seam and all gated
            on the capability bits above.
          </p>
          {@render code(cConcurrency, 'concurrency')}
          {@render diagram(dSkipLockedClaim, 'With SKIP LOCKED, two workers running the same claim query take different rows instead of queueing behind each other.')}
          {@render diagram(dSerializableRetry, 'A serializable lost-update race fails with 40001 and transact_with rolls back; the retry loop is yours, not the framework\'s.')}
          <ul class="list">
            <li><strong class="text-white">Row locks.</strong> The builder stores the request; the executing backend emits it. Postgres emits <code>FOR UPDATE [SKIP LOCKED|NOWAIT]</code>. SQLite treats plain <code>for_update</code>/<code>for_share</code> as a no-op — its write transaction already holds the whole database, strictly coarser — but <code>skip_locked</code>/<code>nowait</code> <em>error</em>: altered contention semantics are the point, and SQLite cannot express them.</li>
            <li><strong class="text-white">Isolation.</strong> Postgres gets <code>BEGIN ISOLATION LEVEL &hellip;</code>. SQLite is always serializable, so levels map to when the write lock is taken (<code>Serializable</code> → <code>BEGIN EXCLUSIVE</code>, <code>RepeatableRead</code> → <code>BEGIN IMMEDIATE</code>, <code>ReadCommitted</code> → <code>BEGIN</code>) — stronger than asked is honest, weaker would not be.</li>
            <li><strong class="text-white">RETURNING on DML.</strong> Same syntax on both engines (SQLite ≥ 3.35; the bundled build is newer), routed through <code>query</code> so the rows come back.</li>
            <li><strong class="text-white">Bulk insert.</strong> The default batches multi-row <code>INSERT … VALUES</code> under the placeholder budget on any backend; outside a transaction, Postgres overrides it with wire-native <code>COPY FROM STDIN</code>, with tabs, newlines, backslashes and NULLs escaped correctly (hostile content round-trips in the tests).</li>
          </ul>
        </article>

        <article id="advisory" class="scroll-mt-24">
          <h2 class="h2">Advisory locks</h2>
          <p>
            Named locks are the coordination primitive for &ldquo;exactly one of us&rdquo;: singleton jobs, janitors,
            leader election, a migration mutex. <code>try_lock</code> returns an RAII <code>LockGuard</code>;
            <code>with_lock</code> is the singleton-job shape, where <code>Ok(None)</code> means another pod ran it.
          </p>
          {@render code(cAdvisory, 'advisory')}
          {@render diagram(dAdvisoryContend, 'Two pods race with_lock("janitor", Duration::ZERO, …): one wins and holds the lock on its own session; the loser gets Ok(None).')}
          <ul class="list">
            <li><strong class="text-white">Postgres is cluster-scoped</strong> via <code>pg_try_advisory_lock</code> on a <em>dedicated</em> session — never a pooled connection, so a leader lock held for the process lifetime cannot starve request traffic. Release is closing that session, which is exactly what the server does when a holder crashes, so crash-release needs no cleanup path.</li>
            <li><strong class="text-white">SQLite is process-scoped</strong>: a named-mutex registry keyed per database file. Two OS processes on the same file do <em>not</em> contend — believe the capability.</li>
            <li>Inside a Postgres transaction, <code>try_lock</code> is transaction-scoped (<code>pg_try_advisory_xact_lock</code>) and releases at COMMIT, <strong class="text-white">not</strong> at guard drop.</li>
            <li><code>with_lock</code> reconnects per call on Postgres — fine for a janitor loop, wrong in a request hot path. These are for coordination, not thousands of concurrent holds.</li>
          </ul>
        </article>

        <article id="json" class="scroll-mt-24">
          <h2 class="h2">JSON path queries</h2>
          <p>
            Query <em>inside</em> JSON columns through the builder — document-store mode for data whose schema is not
            known up front, which is exactly the shape agent-authored payloads arrive in.
          </p>
          {@render code(cJson, 'json')}
          <p>
            The path grammar is a deliberate subset — identifier keys and <code>[n]</code> indexes — because exotic
            keys need per-engine quoting rules while identifiers compile everywhere. This is the first builder feature
            whose SQL <em>shape</em> differs per engine, so the builder stores parsed segments and the executing
            backend compiles them: <code>json_extract(col, ?)</code> with typed values on SQLite,
            <code>col #&gt;&gt; ?</code> with a bound <code>{'{'}a,b,0{'}'}</code> text array and value-driven casts on
            Postgres (<code>::numeric</code> so <code>9 &lt; 10</code> compares as numbers). Containment is Postgres
            only; SQLite errors honestly rather than emulating subset semantics with <code>json_each</code> walks.
          </p>
          {@render diagram(dJsonCompile, 'The path is parsed once into segments; the executing backend compiles them for its dialect, always binding the path as a parameter.')}
          <div class="callout note">
            <div class="callout-title">Indexing</div>
            <p>
              A <code>jsonb</code> GIN index for containment-heavy tables is not emitted by migrations yet — the
              schema IR learns non-btree index kinds alongside the search work. Add one by hand if <code>@&gt;</code>
              gets hot: <code>CREATE INDEX ON docs USING GIN (meta)</code>.
            </p>
          </div>
        </article>

        <article id="search" class="scroll-mt-24">
          <h2 class="h2">Full-text &amp; hybrid search</h2>
          <p>
            One grammar and one API over <code>tsvector</code> (Postgres) and FTS5 (SQLite), plus RAG-shaped hybrid
            retrieval. <code>search::setup</code> creates the artifacts, <code>search::search</code> returns ranked
            base-table rows, and <code>search::hybrid_search</code> fuses a lexical leg with a vector leg.
          </p>
          {@render code(cSearch, 'search')}
          <p>
            The grammar is <code>word "a phrase" -negated OR alternative</code> — implicit AND, <code>-</code>
            negation, <code>OR</code> between groups, quoted phrases. Raw input
            <strong class="text-white">never touches engine query syntax</strong>: words are sanitized to
            alphanumerics at parse time, so neither FTS5 operators nor tsquery syntax can be injected — a stray
            <code>*</code> or <code>NEAR/2</code> cannot even cause an engine syntax error — and the rendered query is
            bound as a parameter. Pure-negative queries are rejected.
          </p>
          <p>
            The artifacts are framework-managed, <code>_sutegi_</code>-prefixed, and invisible to schema introspection
            and <code>migrate:drift</code>: on Postgres a single <em>expression</em> GIN index (nothing added to your
            table; the search query uses the byte-identical expression, EXPLAIN-verified to hit the index), on SQLite
            an external-content FTS5 table plus sync triggers and a one-time rebuild. The <code>simple</code>,
            unstemmed configuration matches FTS5&rsquo;s tokenizer so semantics agree across engines; a
            language/stemming knob is deferred until a consumer needs it.
          </p>
          <p>
            Hybrid search is reciprocal-rank fusion (<code>Σ 1/(60+rank)</code>) merged in Rust over two legs — the
            lexical one above, and pgvector pushdown where <code>capabilities().vector</code> is true, portable
            brute-force cosine otherwise. A document ranked well in <em>both</em> legs beats one ranked first in a
            single leg and absent from the other.
          </p>
          {@render diagram(dHybridRrf, 'Each leg returns ranked primary keys; reciprocal-rank fusion merges them in Rust, then the winners are fetched in one query.')}
        </article>

        <article id="vectors" class="scroll-mt-24">
          <h2 class="h2">Embeddings &amp; vectors</h2>
          <p>
            Embeddings are a first-class column type, not a blob you decode by hand. A
            <code>#[model(vector(dim = N))]</code> field becomes <code>vector(N)</code> on Postgres (pgvector) and
            <code>TEXT</code> on SQLite, and travels in pgvector&rsquo;s canonical <code>[1,2,3]</code> text form so
            the same value round-trips identically on either backend.
          </p>
          {@render code(cVectors, 'vectors')}
          <p>
            Two search paths share the same distance semantics (lower is closer, matching pgvector&rsquo;s operators):
            portable brute force, which loads candidates and ranks them in Rust, and pushdown, which sends
            <code>ORDER BY col &lt;=&gt; ? LIMIT k</code> to the database and uses its ANN index. <code>Metric</code>
            covers cosine, L2 and inner product. sutegi does not compute embeddings — call whatever model you use and
            store the vector.
          </p>
          {@render diagram(dVectorPaths, 'Brute force ranks candidates in Rust on any backend; pushdown has pgvector compute the distance and can use an ANN index.')}
          <div class="callout warn">
            <div class="callout-title">Dimension changes are invisible to drift</div>
            <p>
              A <code>vector(dim)</code> column reflects back dimensionless from Postgres introspection (the dimension
              lives in a catalog the reflection does not read), so changing it is not detected by
              <code>migrate:drift</code>. Declare that change in a migration explicitly.
            </p>
          </div>
        </article>

        <article id="reactive" class="scroll-mt-24">
          <h2 class="h2">Reactive queries</h2>
          <p>
            <code>watch(query)</code> gives you the current rows plus <strong class="text-white">pushed diffs</strong>
            whenever the watched result actually moves. It is the primitive that unifies LISTEN/NOTIFY, the
            event-store wakeups and cross-pod pubsub into one shape — and it turns a channel topic into a live query
            in one loop.
          </p>
          {@render code(cReactive, 'reactive')}
          <p>
            The semantics are table-coarse requery-diff (v1): on a change to a watched table — debounced 25&nbsp;ms,
            bursts coalesced — each watcher re-runs its query and diffs by primary key. A new pk is
            <code>added</code>, a vanished pk is <code>removed</code>, a same-pk-different-row is
            <code>updated</code>, and a write that does not move the watched result emits
            <strong class="text-white">nothing</strong> (the requery runs, the diff is empty, nothing is sent). Twenty
            writes in one window typically become one <code>Change</code> carrying twenty rows.
          </p>
          {@render diagram(dReactiveFlow, 'On Postgres a committed write wakes every watcher through NOTIFY, then a requery and a pk diff; a rolled-back write wakes no one.')}
          <p>
            Guardrails: 1024 live subscriptions per watcher, dropping a subscription unregisters it, and dropping the
            watcher shuts the worker down — interrupting the blocked LISTEN session on Postgres. One watcher per
            process per backend handle. On SQLite, attach the watcher before serving traffic, and remember that writes
            from other processes on the same file are invisible.
          </p>
        </article>

        <article id="migrations" class="scroll-mt-24">
          <h2 class="h2">Migrations</h2>
          <p>
            sutegi turns a change to a <code>#[derive(Model)]</code> struct into a versioned, reversible,
            backend-portable migration — the TypeORM <em>generate</em> workflow without the live-database
            nondeterminism. There are two kinds: <strong class="text-white">declarative</strong> migrations, a list of
            schema ops stored as JSON, generated by diffing and reversible for free; and
            <strong class="text-white">closure</strong> migrations, hand-written <code>up</code>/<code>down</code>
            closures for data backfills and DDL the diff engine does not model.
          </p>
          {@render code(cMigrations, 'migrations')}
          {@render code(cMigrateCli, 'migratecli')}
          {@render diagram(dMigrationLifecycle, 'A migration\'s life as _sutegi_migrations sees it, including the edited-file and deleted-file states the guards catch.')}
          <h3 class="h3">Diff against history, not the live database</h3>
          <p>
            <code>migrate:gen</code> computes changes by diffing your models against the <em>shadow schema</em> — the
            schema you get by folding every existing migration&rsquo;s ops in memory. It never consults your live
            database to decide what to generate, so the same repository state always produces the same migration no
            matter what anyone&rsquo;s local database happens to contain. The live database is consulted for exactly
            one thing: <strong class="text-white">drift detection</strong>, which reports both
            <em>DB vs migrations</em> (a hand-edit, or migrations not fully applied) and
            <em>models vs migrations</em> (you changed a model and have not run <code>:gen</code>).
            <code>sutegi::migrate::report_json</code> is the same report as JSON, ready to mount at
            <code>/__migrations</code> for agents and dashboards.
          </p>
          {@render diagram(dMigrateShadow, 'migrate:gen diffs models against the folded history; the live database is read only for the two drift comparisons.')}
          <h3 class="h3">Safety classes</h3>
          <ul class="list">
            <li><strong class="text-white">Safe</strong> — create a table, add a nullable or defaulted column, create an index, add a foreign key, widen a column.</li>
            <li><strong class="text-white">NeedsData</strong> — add a <code>NOT NULL</code> column with no default, or tighten a column to <code>NOT NULL</code>. Valid only against an empty table; give it a <code>default</code> or write a backfill.</li>
            <li><strong class="text-white">Destructive</strong> — drop a table or column, a lossy type change. <code>:gen</code> writes these but flags them loudly; the committed, reviewed file is your approval.</li>
          </ul>
          <p>
            <strong class="text-white">Renames are never guessed.</strong> A dropped column plus an added column of
            the same type generate a drop + add pair with a &ldquo;possible rename?&rdquo; warning — edit the file to a
            rename op if that is what you meant, which preserves the data. Applied migrations are checksummed in
            <code>_sutegi_migrations</code>, so editing a file after the fact fails the next <code>migrate</code>
            rather than silently diverging (<code>Migrator::repair</code> re-stamps after a deliberate edit). On
            Postgres, <code>run</code>/<code>rollback</code> take a session advisory lock, so many pods booting at
            once serialize instead of racing.
          </p>
          {@render diagram(dMigrateRun, 'Migrator::run validates before touching anything, serializes runners on a lock, and applies each migration atomically with its history row.')}
          <div class="callout tip">
            <div class="callout-title">Dev-mode sync</div>
            <p>
              <code>Model::migrate</code> (and <code>sutegi::orm::migrate::sync</code>) apply
              <em>additive, non-destructive</em> changes directly — create missing tables, add columns, indexes and
              foreign keys — with no migration file. They never drop anything and refuse, pointing you at
              <code>migrate:gen</code>, any change that needs a real migration. Use this locally; use files in
              production.
            </p>
          </div>
          <div class="callout warn">
            <div class="callout-title">Honest caveats</div>
            <p>
              <code>down</code> restores schema <em>shape, not data</em> — a dropped column comes back empty. The
              schema IR covers tables, columns, secondary indexes and single-column foreign keys; views, triggers,
              sequences, CHECK constraints and multi-column FKs are out of scope (use a closure migration). On SQLite,
              changing a column&rsquo;s type or nullability is a full table rebuild — rows survive, but it is heavier
              than the in-place <code>ALTER</code> Postgres does.
            </p>
          </div>
        </article>

        <article id="kv" class="scroll-mt-24">
          <h2 class="h2">The key/value store</h2>
          <p>
            <code>Kv&lt;B&gt;</code> is a namespaced JSON key/value store over <em>either</em> backend — one table,
            single-statement reads and writes — handy for config, feature flags, cached values, or small shared state
            that does not deserve a schema. Values are arbitrary <code>Json</code>; keys are grouped by namespace.
          </p>
          {@render code(cKv, 'kv')}
          <p>
            On a single SQLite node it is the natural home for config and flags; on Postgres it works for small
            <em>shared</em> state. It is ordinary application state — no <code>Arc&lt;Mutex&lt;&hellip;&gt;&gt;</code>.
          </p>
        </article>

        <!-- ===================== AGENTS & REALTIME ===================== -->
        <article id="agents" class="scroll-mt-24">
          <h2 class="h2">The agent surface</h2>
          <p>
            Because every route, model, tool, capability and searchable table registers its own metadata, sutegi
            exposes your whole app as machine-readable JSON with no extra work. <code>/__introspect</code> returns the
            full surface; an agent reads it once, then calls tools through <code>/__tools</code>. There is no SDK and
            no source access required: the app you built for humans is the app the model drives.
          </p>
          {@render code(cAgents, 'agents')}
          <p>
            The contract is three steps — discover (<code>/__introspect</code>), read the manifest
            (<code>/__tools</code>), invoke (<code>POST /__tools/:name</code>, or
            <code>/__tools/:name/stream</code> for SSE) — and it is extended, not replaced, by the realtime surfaces:
            <code>/__channels</code> teaches an agent the channel protocol, <code>/__actors</code> shows live process
            status. Everything under <code>/__</code> except the two probes sits behind
            <a href="#/docs/middleware" class="lnk"><code>ops_guard</code></a>.
          </p>
          {@render diagram(dAgents, 'The agent contract: discover, read the manifest, then invoke the unary or the streaming endpoint, all behind ops_guard.')}
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
            same, but the closure also gets an <code>SseSink</code> to stream results as Server-Sent Events, and the
            manifest marks it <code>"streaming": true</code> so an agent knows to hit the SSE endpoint.
          </p>
          {@render code(cTools, 'tools')}
          {@render diagram(dTools, 'Each check a unary tool call passes, and the status it returns when it fails (the 422 schema check needs the validate feature).')}
          <p>
            A tool and an HTTP route can — and usually should — be two adapters over one
            <a href="#/docs/hexagonal" class="lnk">use case</a>. Write the logic once; expose it to both audiences.
          </p>
        </article>

        <article id="streaming" class="scroll-mt-24">
          <h2 class="h2">Streaming &amp; SSE</h2>
          <p>
            Because the server is blocking and thread-per-connection, streaming is trivial and naturally
            backpressured — there is no executor to fight. <code>sse(producer)</code> gives the producer an
            <code>SseSink</code> with <code>data</code>, <code>event</code>, and <code>comment</code>; each frame is
            flushed immediately. <code>stream(status, content_type, producer)</code> is the raw-bytes equivalent for
            NDJSON and large exports.
          </p>
          {@render code(cStreaming, 'streaming')}
          {@render diagram(dStreaming, 'A stream fixes its 200 status before the producer runs; frames are then flushed one by one on the same worker until the socket closes.')}
          <p>
            It is the same transport that carries live LLM tokens back to a UI and the one
            <code>.stream_tool</code> rides on. Regular responses use keep-alive; streams are close-framed by design,
            which is valid HTTP/1.1 and needs no chunked encoding. Behind nginx, set
            <code>proxy_buffering off</code> — the <code>ontzi</code> config already does.
          </p>
        </article>

        <article id="websockets" class="scroll-mt-24">
          <h2 class="h2">WebSockets</h2>
          <p>
            The <code>ws</code> feature adds <code>App::ws</code>. The HTTP side stays blocking thread-per-connection,
            but an upgraded socket <strong class="text-white">detaches</strong> into a sharded kqueue/epoll reactor —
            no async runtime, no futures, just poller syscalls — so the worker thread is freed immediately and an idle
            connection costs ~340&nbsp;bytes of user-space RSS and <strong class="text-white">zero threads</strong>.
          </p>
          {@render code(cWs, 'ws')}
          {@render diagram(dWsUpgrade, 'The HTTP worker checks the handshake, writes the 101 and hands the raw socket to a reactor shard, then goes back to serving HTTP.')}
          <p>
            Measured on a dev laptop: 80,000 live sockets at 0.0% idle CPU, a broadcast enqueue of 80k shared-<code>Arc</code>
            frames in ~1.5&nbsp;ms, and 5k-fleet delivery at p50 15&nbsp;ms / max 30&nbsp;ms end-to-end. The codec is
            strict RFC&nbsp;6455 — masking required, minimal length encodings, control-frame rules, close-code
            validation, UTF-8 enforcement — with a deterministic fuzz suite behind it.
          </p>
          <ul class="list">
            <li><strong class="text-white">Callbacks run inline on the shard</strong>, which is what guarantees per-connection ordering. Keep them CPU-quick; push blocking work to your own threads and answer later through the cloneable <code>Conn</code>.</li>
            <li><strong class="text-white">Broadcast by cloning one encoded frame.</strong> <code>text_frame</code>/<code>binary_frame</code> encode once into an <code>Arc</code>; <code>send_shared</code> puts it on each queue.</li>
            <li><strong class="text-white">Slow consumers are dropped</strong> at <code>max_buffered</code>; ping and idle sweeps close dead sockets; <code>RLIMIT_NOFILE</code> is raised at startup.</li>
            <li>Every knob lives in <a href="#/docs/options" class="lnk"><code>WsConfig</code></a>, and <code>ws_config</code> must come <em>before</em> the first <code>.ws(&hellip;)</code> — that call starts the reactor.</li>
          </ul>
          <div class="callout warn">
            <div class="callout-title">Cookie-authenticated sockets and origins</div>
            <p>
              A browser sends cookies on a cross-origin WebSocket handshake and the same-origin policy does not stop
              it, so sutegi does: by default an upgrade whose <code>Origin</code> names a different host than
              <code>Host</code> gets a <code>403</code>. Call <code>check_origin([&hellip;])</code> when the page is
              served from another host or a proxy rewrites <code>Host</code>. <code>allow_any_origin()</code> turns
              the check off — only for public sockets that do not authenticate by cookie, or any page on the internet
              can open one as your user (CSWSH).
            </p>
          </div>
        </article>

        <article id="pubsub" class="scroll-mt-24">
          <h2 class="h2">PubSub</h2>
          <p>
            Topic fan-out behind one <code>Broker</code> trait, so the layer above it — channels, presence, your own
            code — does not know whether it is talking to one pod or a fleet. <code>PubSub</code> is the in-process
            broker; <code>PgPubSub</code> (<code>pubsub-postgres</code>) is the same trait over Postgres
            <code>LISTEN</code>/<code>NOTIFY</code>.
          </p>
          {@render code(cPubsub, 'pubsub')}
          {@render diagram(dPubsubFanout, 'PgPubSub delivers to local subscribers synchronously, then one NOTIFY reaches every other pod; the in-process PubSub has only the local branch.')}
          <p>
            <code>PgPubSub</code> uses one shared PG channel with topics inside a JSON envelope — immune to the 63-byte
            identifier truncation trap — plus a lazy publisher with one transparent retry and a listener that
            reconnects with capped backoff. Fan-out cost on the database is per <em>message</em>, not per subscriber:
            one connection per pod for listening, one for publishing.
          </p>
          <div class="callout warn">
            <div class="callout-title">At-most-once, by design</div>
            <p>
              Delivery is fire-and-forget (Phoenix&rsquo;s contract too): a pod that is reconnecting misses messages
              sent meanwhile, and <code>NOTIFY</code> payloads cap at ~8&nbsp;KB. If a message must survive, put it in
              a table (or the <a href="#/docs/events" class="lnk">event store</a>) and broadcast <em>that there is
              news</em>.
            </p>
          </div>
        </article>

        <article id="channels" class="scroll-mt-24">
          <h2 class="h2">Channels &amp; presence</h2>
          <p>
            Channels (<code>channels</code>) are the realtime identity feature: many topics multiplexed over one
            socket, with joins, replies, broadcasts, per-membership state — and an agent manifest. Phoenix is the
            design reference; the deltas are deliberate.
          </p>
          {@render code(cChannels, 'channels')}
          <p>
            Broadcasts ride the pubsub <code>Broker</code> seam, so
            <strong class="text-white">the same channel code is single-pod on the in-process broker and cross-pod on
            <code>PgPubSub</code> with zero changes</strong> — verified by two OS processes chatting through a real
            PostgreSQL. A broadcast to a large room encodes the frame once and takes each reactor shard&rsquo;s lock
            once, so the 80k-socket transport numbers apply unchanged.
          </p>
          <h3 class="h3">The wire protocol</h3>
          {@render code(cChannelsWire, 'chwire')}
          <p>
            <code>GET /__channels</code> returns the full manifest — envelope shape, control events, every
            channel&rsquo;s pattern, docs and join/event schemas — which is enough for an agent to join and speak over
            a raw WebSocket with no client library. That manifest is part of the agent contract, next to
            <code>/__introspect</code> and <code>/__tools</code>.
          </p>
          {@render code(cChannelsJs, 'chjs')}
          {@render diagram(dChannelsJoin, 'A join, a tracked presence and a broadcast. Every broadcast goes through the broker, so one path serves one pod or many.')}
          <h3 class="h3">Join lifecycle notes</h3>
          <ul class="list">
            <li>Inside <code>on_join</code> the member is <strong class="text-white">not admitted yet</strong>: a broadcast there reaches the room but not the joiner, and a push lands before the join reply. Use <code>socket.after_join(&hellip;)</code> for welcome pushes — it runs after the ok reply is on the wire, and never runs if the join is refused.</li>
            <li>A join on an already-joined topic <strong class="text-white">replaces</strong> the membership: the old one gets the leave callback with <code>LeaveReason::Rejoin</code>, and <code>assigns</code> start fresh.</li>
            <li><code>assigns</code> are per-membership JSON state (<code>socket.assign</code> / <code>assign_get</code>), gone on leave or disconnect.</li>
          </ul>
          <h3 class="h3">Presence</h3>
          <p>
            The <code>presence</code> feature adds who&rsquo;s-online tracking: <code>Presence::track</code> /
            <code>untrack</code> / <code>list</code>. The tracked member receives <code>presence_state</code> (the full
            view, after its join reply); the room receives <code>presence_diff {'{'}joins, leaves{'}'}</code> on every
            change. Untrack is automatic on leave, rejoin and disconnect, and several memberships may track the same
            key — one user, many tabs, each contributing a meta.
          </p>
          <div class="callout warn">
            <div class="callout-title">Presence is heartbeat-based, not a CRDT</div>
            <p>
              Each pod re-publishes its local state every <code>presence_heartbeat</code> (default 30&nbsp;s) and
              expires pods silent for ~2.5×, reporting their members as leaves. So a crashed pod&rsquo;s users can
              linger up to ~75&nbsp;s, and partition conflicts resolve by expiry. That is the right trade for a
              &ldquo;who&rsquo;s online&rdquo; sidebar; keep anything stronger in a table.
            </p>
          </div>
        </article>

        <article id="queues" class="scroll-mt-24">
          <h2 class="h2">Queues</h2>
          <p>
            Some work should not block the response. The durable queue (<code>queue</code>) runs over the
            <code>Backend</code> seam, so one jobs table and one set of SQL work on bundled SQLite (a single box) and
            on Postgres (many pods). Jobs survive a crash: the claim stamps a lease instead of deleting the row, so a
            dead worker&rsquo;s job becomes visible again after the visibility timeout — at-least-once. Retries,
            delays, priorities and dedupe keys are columns, so a restart forgets nothing.
          </p>
          {@render code(cQueues, 'queues')}
          {@render diagram(dQueueLifecycle, 'A job row\'s lifecycle: the claim takes a lease, heartbeats and results are fenced on it, and failures retry with linear backoff until the row is a dead letter.')}
          <p>
            Claims are exclusive on both backends: <code>FOR UPDATE SKIP LOCKED</code> wherever
            <code>capabilities().skip_locked</code> says it exists, and on SQLite the serialized writer already
            provides it, since the second <code>UPDATE … RETURNING</code> no longer sees the claimed row. Postgres
            additionally makes that exclusivity <em>cross-pod</em>; <code>queue.cross_pod()</code> tells you which
            guarantee you actually have instead of letting the docs imply the stronger one.
          </p>
          <ul class="list">
            <li><strong class="text-white">Named queues with their own pools</strong> — <code>.queue("video")</code> plus <code>start_on("video", 1)</code>, so a slow job class cannot starve a fast one.</li>
            <li><strong class="text-white">Dedupe keys</strong> — <code>.unique("yt:abc")</code> hands back the live row&rsquo;s id instead of enqueueing a second copy, backed by a partial unique index that deliberately excludes dead letters, so a failure never owns a key forever.</li>
            <li><strong class="text-white">A panicking handler is a failed job, not a lost worker</strong> — the panic is caught, the row retries or dead-letters like any other, and the pool keeps going.</li>
            <li><strong class="text-white">Dispatch wakes an idle worker</strong> over a condvar instead of making it wait out the poll interval; the interval stays as the safety net for delayed jobs and work enqueued by another pod.</li>
            <li><code>JobCtx</code> gives a handler what it needs to behave well: <code>is_last_attempt()</code> to tell a retryable blip from a terminal failure <em>before</em> writing a user-visible error, <code>heartbeat()</code> to outlive the visibility timeout, and <code>should_stop()</code> for loops.</li>
          </ul>
          <p>
            Timestamps are epoch milliseconds supplied by the caller rather than SQL <code>now()</code> — which is
            what lets one statement work in both dialects, and makes schedules testable without sleeping. Tuning:
            <code>visibility_timeout</code>, <code>poll_interval</code>, <code>retry_backoff</code>.
          </p>
        </article>

        <article id="actors" class="scroll-mt-24">
          <h2 class="h2">Actors &amp; supervision</h2>
          <p>
            The <code>actors</code> feature is the OTP half of the stack: isolated processes with typed mailboxes, and
            supervision trees that restart them when they crash. An <code>Actor</code> owns its state outright on its
            own thread — no locks, no <code>Sync</code> bound on the state — and other threads communicate by message
            through an <code>ActorRef</code>.
          </p>
          {@render code(cActors, 'actors')}
          {@render diagram(dActorsSupervision, 'What the supervisor does with each child exit: restart policy, then the intensity cap, then the strategy picks which children restart.')}
          <ul class="list">
            <li><strong class="text-white">Bounded mailbox</strong> (default 1024): a full mailbox fails <code>tell</code> fast with <code>TellError::Full</code>, so backpressure is explicit rather than a hidden unbounded queue.</li>
            <li><strong class="text-white">Let it crash.</strong> A panic in <code>handle</code> kills only that actor and is reported as <code>ExitReason::Crashed</code>. Lifecycle hooks <code>started()</code> / <code>stopped(&amp;reason)</code> run on every (re)start and exit. <code>stop()</code> is queued behind pending messages, so the mailbox drains first.</li>
            <li><strong class="text-white">Restart intensity</strong> (default 3 per 5&nbsp;s) fails the supervisor and stops all children when exceeded, so a crash-looping child cannot burn a thread forever. <code>RestForOne</code>/<code>OneForAll</code> stop dependents in reverse order and restart them in start order.</li>
            <li><code>SupervisorHandle::child_ref(name)</code> is the <code>whereis</code> analog — re-fetch after a crash, because an old ref points at the dead generation&rsquo;s queue. <code>start()</code> returns only once every child&rsquo;s first generation is up.</li>
            <li><code>App::actors(registry)</code> mounts <code>GET /__actors</code>: state, mailbox depth, restart counts and last crash message per actor — the Observer-lite half of the agent contract, <code>ops_guard</code>-gated like the rest.</li>
          </ul>
          <div class="callout warn">
            <div class="callout-title">One OS thread per actor</div>
            <p>
              This is supervision and fault isolation, not BEAM-scale lightweight processes: thousands of actors, not
              millions. There are no links or monitors beyond the supervisor relationship, no distribution (Postgres
              is the bus), and no hot code reload. Supervision rests on <code>catch_unwind</code>, so it needs
              <code>panic = "unwind"</code> (the Rust default): an app that sets <code>panic = "abort"</code> turns every
              actor crash into a process exit.
            </p>
          </div>
        </article>

        <!-- ===================== FRAMEWORK SERVICES ===================== -->
        <article id="auth" class="scroll-mt-24">
          <h2 class="h2">Authentication</h2>
          <p>
            The <code>auth</code> feature is a complete user system over either backend — everything Laravel&rsquo;s
            auth scaffolding does, with no third-party dependency. <code>Users</code> handles registration and
            authentication with PBKDF2-HMAC-SHA256 passwords (600k iterations by default, per OWASP; the hash never
            leaves the store). <code>Auth</code> issues signed-cookie logins. <code>Tokens</code> mints
            <code>stg_</code> bearer tokens for agents. The guards are just middleware you attach to a group.
          </p>
          {@render code(cAuth, 'auth')}
          {@render diagram(dAuthLogin, 'Login: throttle, verify the password, then mint a fresh session and a remember cookie.')}
          <h3 class="h3">What the production pieces do</h3>
          <ul class="list">
            <li><strong class="text-white">Remember me</strong> — selector/validator cookies where only the validator&rsquo;s SHA-256 is stored, the validator <em>rotates on every use</em> (a stolen-then-replayed copy revokes the row, surfacing the theft), tokens are bound to the password hash at mint, and server-side expiry (default 30 days) ignores whatever the client claims.</li>
            <li><strong class="text-white">Sessions bound to the password hash</strong> — <code>login</code> stamps a fingerprint of the current PHC string into the signed session, and a stale binding reads as anonymous. Changing a password logs out every other device on its next store-checking request.</li>
            <li><strong class="text-white">Login throttling</strong> — a DB-backed fixed window (default 5 attempts / 60&nbsp;s, keyed however you like; the convention is <code>login:&lt;email&gt;|&lt;ip&gt;</code>), so every pod counts the same attempts.</li>
            <li><strong class="text-white">CSRF</strong> — a token minted inside the signed session and compared in constant time; the <code>require_csrf</code> guard enforces <code>X-CSRF-Token</code> on mutating methods (419, Laravel&rsquo;s &ldquo;Page Expired&rdquo;) while passing reads and <code>Authorization</code>-header callers.</li>
            <li><strong class="text-white">Auto-rehash at login</strong> — a verified password whose stored iteration count is below the store&rsquo;s is transparently re-hashed (best-effort; a rehash failure never fails a valid login). Raise the work factor once and the fleet upgrades itself credential-by-credential.</li>
            <li><strong class="text-white">Guards</strong> — <code>require_auth</code>, <code>require_role</code>, <code>require_verified</code>, <code>require_csrf</code>, <code>require_token</code>. <code>Auth::identify</code> is the revival point (session-or-remember) and its <code>Identified::attach</code> sets the fresh cookies, because sutegi middleware cannot set cookies on pass-through.</li>
            <li><strong class="text-white">API tokens</strong> — the agent door. Plaintext shown once, only the SHA-256 stored; <code>issue_expiring</code> adds a deadline, every successful verify stamps <code>last_used_at</code>, and tokens survive logout.</li>
            <li><strong class="text-white">Profile operations</strong> — <code>change_password</code> (verifies the current one first), <code>set_name</code>, and <code>set_email</code> (normalizes, checks uniqueness, and resets <code>verified_at</code> so the new address must re-verify).</li>
          </ul>
          <p>
            Unknown emails burn the same PBKDF2 time as wrong passwords, so the endpoint does not become a user
            enumeration oracle.
          </p>
          {@render diagram(dAuthIdentify, 'How Auth::identify resolves a request: a live session, else a remember-me revival that rotates the token, else anonymous.')}
          <div class="callout tip">
            <div class="callout-title">Verification &amp; reset</div>
            <p>
              Add the <code>auth-mail</code> feature and <code>AuthMail</code> for email verification and
              password-reset flows: built-in text+HTML templates, signed expiring links (24&nbsp;h / 1&nbsp;h), no
              state tables. Reset tokens are enumeration-safe (an unknown email is a silent <code>Ok</code>) and bound
              to the current password hash — the moment the password changes, every outstanding link dies, which makes
              them single-use without storing anything.
            </p>
          </div>
        </article>

        <article id="sessions" class="scroll-mt-24">
          <h2 class="h2">Sessions &amp; CSRF</h2>
          <p>
            The <code>session</code> feature provides signed-cookie sessions (HMAC-SHA256) with no server-side store:
            <code>Sessions::new(secret)</code>, then <code>load</code> a session off the request, <code>set</code>/
            <code>get</code>/<code>remove</code> values, and <code>save</code> it onto the response. A bare session
            expires only through the cookie&rsquo;s <code>Max-Age</code> (24&nbsp;hours by default,
            <code>.max_age(&hellip;)</code>), which the client controls: a stolen cookie stays valid. For logins, use
            <a href="#/docs/auth" class="lnk">Auth</a>, which stamps an <code>exp</code> inside the signed payload
            (<code>Auth::ttl</code>) so a stolen session dies on schedule no matter what the client claims. Call <code>.insecure()</code> in local <code>http://</code> development to drop the
            cookie&rsquo;s <code>Secure</code> flag.
          </p>
          {@render code(cSessions, 'sessions')}
          {@render diagram(dSessions, 'The CSRF token lives inside the signed cookie, so issuing and checking it needs no server-side store.')}
          <p>
            This is the machinery <a href="#/docs/auth" class="lnk"><code>Auth</code></a> is built on; use it directly
            for lightweight per-visitor state like a cart or a wizard step.
          </p>
        </article>

        <article id="mail" class="scroll-mt-24">
          <h2 class="h2">Mail</h2>
          <p>
            The <code>mail</code> feature gives you an <code>Email</code> builder with real RFC&nbsp;2822/MIME
            rendering (multipart/alternative, encoded-words, header-injection folding) and a one-method
            <code>Transport</code> seam. Built-in drivers cover <code>log</code> (dev default — messages print,
            nothing escapes), <code>memory</code> (test assertions), a pure-std <code>smtp</code> client (EHLO, AUTH
            PLAIN/LOGIN, dot-stuffing) and <code>sendmail</code> (pipe to the local Postfix — the VPS shape);
            <code>Mailer::from_env()</code> picks one from <code>MAIL_*</code>.
          </p>
          {@render code(cMail, 'mail')}
          <p>
            <code>Theme</code> + <code>MailMessage</code> produce a clean, email-client-safe HTML card
            <strong class="text-white">and</strong> a matching plain-text part from the same blocks, so every message
            is <code>multipart/alternative</code> for free. The outer chrome is a
            <a href="#/docs/templates" class="lnk">template</a> source — swap it wholesale with
            <code>Theme::layout(&hellip;)</code> while block rendering keeps working.
          </p>
          <div class="callout note">
            <div class="callout-title">The SMTP client has no TLS</div>
            <p>
              Same stance as the Postgres driver: point it at an in-cluster relay or Mailpit on
              <code>localhost:1025</code>. For hosted delivery over the public internet, a Resend/SendGrid/Postmark/SES
              adapter is ~10 lines over any HTTP client — the same seam that lets sutegi dodge the TLS wall until it
              lands (see <a href="#/docs/honesty" class="lnk">the posture page</a>).
            </p>
          </div>
        </article>

        <article id="storage" class="scroll-mt-24">
          <h2 class="h2">File storage</h2>
          <p>
            The <code>storage</code> feature abstracts object storage behind one <code>Storage</code> trait
            (<code>put</code>/<code>get</code>/<code>stat</code>/<code>delete</code>/<code>list</code>/
            <code>get_reader</code>, with traversal-validated keys). <code>FsStorage</code> writes to a local directory
            (atomic temp-and-rename, real streaming reads); <code>DbStorage</code> (<code>storage-db</code>) stores
            blobs in SQLite or Postgres — multi-pod files with no new infrastructure, honest ceiling a few MB per
            object; and <code>S3Storage</code> puts the same trait on a real bucket: AWS S3, Cloudflare R2, MinIO,
            Garage, Ceph RGW.
          </p>
          {@render code(cStorage, 'storage')}
          <h3 class="h3">The transport seam (how S3 works with no TLS in the tree)</h3>
          {@render diagram(dStorageSeam, 'S3Storage signs through S3Store and moves bytes through a one-method HttpTransport; a presigned URL skips your server entirely.')}
          <p>
            <code>S3Storage</code> never opens a socket itself. It hands a signed request to an
            <code>HttpTransport</code> — one method — and two implementations ship with it. <code>SystemCurl</code>
            delegates the <code>https</code> handshake and certificate verification to the system <code>curl</code>, so
            the crypto that must not be hand-rolled isn&rsquo;t and the dependency count stays at zero.
            <code>PlainHttp</code> is pure <code>std</code> and <strong class="text-white">refuses
            <code>https</code></strong> rather than pretending — for a store on a trusted path: in-cluster MinIO, a
            sidecar Garage, a dev container. Your own client is <code>impl HttpTransport for MyClient</code>.
          </p>
          <p>
            Every request is signed with the <strong class="text-white">real payload hash</strong>
            (<code>x-amz-content-sha256</code>, never <code>UNSIGNED-PAYLOAD</code>), so a body altered in flight is
            refused by the store — integrity that holds even over <code>PlainHttp</code>. Uploads and downloads are
            <code>ETag</code>-verified when the store reports a plain MD5 (multipart and encrypted ETags are skipped,
            not faked; <code>verify_etag(false)</code> opts out). <code>list</code> follows continuation tokens and is
            bounded by <code>max_list_keys</code> (default 100,000), so a ten-million-object bucket
            <strong class="text-white">errors instead of silently truncating</strong>. Signing is verified against
            AWS&rsquo;s published known-answer vectors, for presigned URLs and signed headers alike.
          </p>
          <p>
            <code>S3Store</code> deliberately does not implement <code>Storage</code>: minting a URL is a different
            contract than moving bytes. <code>S3Store::storage(transport)</code> is the crossing point, and on its own
            <code>S3Store</code> is a pure-std SigV4 presigner — which is the agent-native trick: mint a time-limited
            URL and let the client or agent move the bytes straight to the object store, never through your server.
          </p>
        </article>

        <article id="events" class="scroll-mt-24">
          <h2 class="h2">Event sourcing</h2>
          <p>
            The <code>events</code> feature is an append-only event store over the <code>Backend</code> seam. You
            <code>append</code> events to a per-entity stream with an <code>Expected</code> version for optimistic
            concurrency (<code>Any</code>, <code>NoStream</code>, or <code>Version(n)</code>), and fold current state
            back on demand by implementing <code>Aggregate::apply</code>. Global log positions are gap-free.
          </p>
          {@render code(cEvents, 'events')}
          {@render diagram(dEventsFlow, 'Load, decide, append with an expected version; projections advance their checkpoint in the same transaction as their read-model writes.')}
          <p>
            <code>Projections</code> are checkpointed consumers: the handler&rsquo;s writes and its checkpoint bump
            commit in one transaction, giving exactly-once read models that you can <code>reset</code> and rebuild
            from the log. <code>append_tx</code> composes with a transaction you already own, and position-race
            retries back off quadratically so a thundering herd cannot exhaust one writer&rsquo;s retries.
          </p>
        </article>

        <article id="templates" class="scroll-mt-24">
          <h2 class="h2">Templates</h2>
          <p>
            The <code>template</code> feature is a Blade-lite engine over <code>Json</code> contexts:
            <code>{'{{ escaped }}'}</code> and <code>{'{!! raw !!}'}</code> dot-path interpolation,
            <code>@if</code>/<code>@else</code>, <code>@foreach &hellip; as &hellip;</code> (with
            <code>loop.index</code>/<code>first</code>/<code>last</code>), and <code>@include</code> for partials.
          </p>
          {@render code(cTemplates, 'templates')}
          <p>
            Templates compile once to an AST and report line-numbered errors. It also powers the themed HTML in the
            <a href="#/docs/mail" class="lnk">mail layer</a>.
          </p>
        </article>

        <article id="collections" class="scroll-mt-24">
          <h2 class="h2">Collections</h2>
          <p>
            <code>collect(..)</code> wraps any iterable in a <code>Collection&lt;T&gt;</code> — a fluent, chainable API
            for the everyday shaping that raw <code>Iterator</code> makes verbose. It is part of the facade, no feature
            needed.
          </p>
          {@render code(cCollections, 'collections')}
          <p>
            It is a thin layer over <code>Vec&lt;T&gt;</code>: it <code>Deref</code>s to <code>[T]</code> and
            round-trips through <code>Vec</code> and iterators, so it adds no allocation over doing the work by hand —
            and you can drop back to plain iterator code at any point in a chain.
          </p>
        </article>

        <article id="crypto" class="scroll-mt-24">
          <h2 class="h2">Crypto primitives</h2>
          <p>
            <code>sutegi::crypto</code> is always compiled — it is what the sessions, auth, SigV4 signing, SCRAM
            handshake and WebSocket handshake are built from — and it is directly usable.
          </p>
          {@render code(cCrypto, 'crypto')}
          <p>
            The choices are deliberate: ChaCha20-Poly1305 over AES because add-rotate-xor is constant-time in plain
            software where AES table lookups leak through cache timing; a fresh random nonce prepended by
            <code>seal</code> so nonce reuse is impossible by construction; HKDF so a signing key and an encryption
            key derived from one master secret never alias; and <code>constant_time_eq</code> piped through
            <code>black_box</code> so the optimizer cannot rewrite it into an early exit. The raw stream cipher and
            one-shot Poly1305 stay private, because those are the pieces that are dangerous to hold directly.
          </p>
          <div class="callout warn">
            <div class="callout-title">Read the posture page first</div>
            <p>
              This is hand-rolled cryptography, known-answer tested and fuzzed but
              <a href="#/docs/honesty" class="lnk">not independently audited</a>. Use it inside a trusted boundary;
              do not make it the only thing between hostile traffic and your secrets.
            </p>
          </div>
        </article>

        <!-- ===================== ARCHITECTURE & OPERATIONS ===================== -->
        <article id="hexagonal" class="scroll-mt-24">
          <h2 class="h2">Hexagonal architecture</h2>
          <p>
            As an app grows, the <code>hexagon</code> toolkit keeps it honest. Your domain stays pure; the application
            layer depends on <em>port</em> traits (an outbound <code>TodoRepository</code>, say); and adapters — an
            HTTP route, an AI tool, a repo over either <code>Backend</code> — plug in at the edges.
            <code>UseCase</code> is the inbound-port trait, <code>AppError</code>/<code>AppResult</code> are
            transport-agnostic with a canonical HTTP mapping, and <code>respond</code>/<code>respond_created</code> are
            the glue that turns an <code>AppResult</code> into a <code>Response</code>.
          </p>
          {@render code(cHex, 'hex')}
          {@render diagram(dHexagonal, 'Every dependency points inward: two inbound adapters share one use case, and the composition root picks which outbound repo fills the port.')}
          <p>
            One use case can back both a route and a tool, over whichever store the composition root injects — and it
            is fully testable without starting a server. <code>Command</code>, <code>Query</code>, <code>Event</code>
            and <code>EventBus</code> are there when you want the CQRS vocabulary too.
          </p>
          <div class="callout note">
            <div class="callout-title">Full guide</div>
            <p>
              <a href="https://github.com/enekos/sutegi/blob/master/docs/HEXAGONAL.md" target="_blank" rel="noopener" class="lnk">docs/HEXAGONAL.md</a>
              covers the dependency rule, layer responsibilities, layout, and testing strategy in depth, and
              <code>examples/hexagonal</code> is a worked reference with two interchangeable repositories
              (in-memory ↔ SQLite) selected at the composition root.
            </p>
          </div>
        </article>

        <article id="testing" class="scroll-mt-24">
          <h2 class="h2">Testing</h2>
          <p>
            <code>App::service()</code> returns the app as a plain <code>Fn(Request) -&gt; Response</code>, so you can
            exercise the whole routing, state, validation, and tool surface <em>in process</em> — no socket, no port,
            no async harness. Back it with <code>Db::memory()</code> for a fresh database per test.
          </p>
          {@render code(cTesting, 'testing')}
          <p>
            The pieces that need a server or a real engine are testable too, and the framework&rsquo;s own suite shows
            how: <code>Mailer</code>&rsquo;s <code>memory</code> driver for mail assertions, the queue&rsquo;s
            caller-supplied millisecond timestamps so schedules need no sleeping, an in-process S3 stub over a real
            socket for the storage wire suite, and <code>crates/sutegi/tests/server.rs</code> for the full loopback
            end-to-end pattern.
          </p>
        </article>

        <article id="repl" class="scroll-mt-24">
          <h2 class="h2">The REPL</h2>
          <p>
            The <code>repl</code> feature is a tinker-style interactive shell. It is deliberately
            <strong class="text-white">not</strong> a Rust evaluator — that would need a compiler toolchain and
            third-party machinery — but a command shell over the surfaces your app already exposes: the agent contract
            (introspection, the tool manifest, tool invocation with SSE frames printed live, raw HTTP through your own
            routes) and, with a <code>Backend</code> attached, the data layer (raw SQL, a <code>where</code>-clause
            query DSL, KV, the event store, the job queue).
          </p>
          {@render code(cRepl, 'repl')}
          <p>
            Two transports, one command set. In-process it consumes the built <code>App</code>; remote mode drives a
            <em>running</em> app over plain HTTP with no source access — exactly the way an LLM does — which is why
            <code>sutegi repl &lt;addr&gt;</code> works against any sutegi app. <code>Repl::eval(line)</code> is the
            programmatic seam the loop is built on, so the same commands are scriptable and testable.
          </p>
        </article>

        <article id="internals" class="scroll-mt-24">
          <h2 class="h2">Inside the server</h2>
          <p>
            There is no executor and no hidden machinery: a fixed thread pool accepts connections and each worker
            handles one connection with blocking I/O. That is the whole model, and it is why streaming is trivial,
            backpressure is free, and a stack trace points at your handler rather than at a runtime.
          </p>
          {@render code(cInternals, 'internals')}
          {@render diagram(dInternals, 'One connection on one pool worker: parse, run the app under catch_unwind, then keep-alive, stream-and-close or hand the socket to the ws reactor.')}
          <p>
            The consequences are worth internalising, because they are what you tune:
          </p>
          <ul class="list">
            <li><strong class="text-white">A connection costs a thread while it is open.</strong> Keep-alive is therefore capped twice — <code>keep_alive_idle</code> (default 5&nbsp;s, deliberately much shorter than the socket timeout) and <code>keep_alive_max</code> (default 100 requests) — so an idle client cannot pin a worker indefinitely.</li>
            <li><strong class="text-white">Reading a request is bounded twice too.</strong> <code>timeout</code> bounds a single stalled <code>recv</code>; <code>header_timeout</code> bounds the <em>total</em> time a peer may take to deliver request line, headers and body, so a byte-per-interval dripper cannot hold a worker (slowloris, CWE-400).</li>
            <li><strong class="text-white">Panics are isolated per request.</strong> A handler panic becomes a <code>500</code> rather than a downed worker. That rests on <code>catch_unwind</code>, so keep <code>panic = "unwind"</code> (the Rust default) in your release profile.</li>
            <li><strong class="text-white">WebSockets escape the model entirely.</strong> An upgrade returns <code>Body::Upgrade</code>, the reactor adopts the socket, and the worker is released — which is how one process holds 80k connections on eight threads.</li>
            <li><strong class="text-white">The release profile is tuned for size</strong>: <code>opt-level = "z"</code>, LTO, one codegen unit, stripped symbols. That is where ~480&nbsp;KB comes from; the unwind tables that panic isolation needs are about 10% of it.</li>
          </ul>
          <p>
            Ordering is fixed and worth knowing when you place a guard: ops guard → global middleware →
            <code>/__metrics</code> and <code>/__introspect</code> → route match → group middleware → handler →
            after-middleware. The two probes are matched before everything, so an orchestrator never needs a
            credential. Metrics are recorded on every path, including short-circuits.
          </p>
        </article>

        <article id="listeners" class="scroll-mt-24">
          <h2 class="h2">Listeners</h2>
          <p>
            <code>App::listener</code> registers a long-running loop — a UDP ingest port, a raw TCP protocol, a
            discovery beacon — that runs on its own thread for the life of the server and shuts down with it. It is a
            <strong class="text-white">lifecycle seam, not a protocol</strong>: sutegi does not frame your packets.
            <code>std::net</code> already gives you <code>UdpSocket</code> and <code>TcpListener</code>; what a
            hand-spawned thread cannot do is see shared state, drain gracefully, or show up where an agent can
            discover it. This closes those three gaps and nothing more.
          </p>
          {@render code(cListeners, 'listeners')}
          {@render diagram(dListeners, 'Listeners and the HTTP server share one shutdown flag; the server drains, then joins each listener once its closure returns.')}
          <p>
            <code>ListenerCtx</code> is deliberately tiny: <code>should_stop()</code> (true once shutdown has begun),
            <code>state::&lt;T&gt;()</code> / <code>try_state::&lt;T&gt;()</code> — the same typed state handlers and
            tools see — <code>db::&lt;B&gt;()</code> as sugar for a backend, and <code>name()</code>.
          </p>
          <div class="callout warn">
            <div class="callout-title">The shutdown contract is cooperative</div>
            <p>
              On shutdown the server stops accepting, drains in-flight HTTP requests, then
              <strong class="text-white">joins every listener thread before returning</strong>. That join waits for
              your loop to notice, so never park in an unbounded blocking read: set a read timeout on the socket and
              poll <code>should_stop()</code> each lap. A listener that never polls holds the process until the
              orchestrator&rsquo;s grace period expires and SIGKILL lands — honest, but avoidable.
            </p>
          </div>
          <p>
            A panicking listener <strong class="text-white">does not restart and does not take the server down</strong>:
            the panic is caught, one line lands on stderr, and the thread ends. If the loop must survive crashes, own
            that policy — wrap the body in a retry loop, or run it under an
            <a href="#/docs/actors" class="lnk">actor supervisor</a> and keep the listener as the thin socket shim.
            Discovery is name and doc only; which port and protocol the loop speaks belongs in the doc string, where
            agents read it.
          </p>
        </article>

        <article id="options" class="scroll-mt-24">
          <h2 class="h2">Tuning &amp; limits</h2>
          <p>
            Every knob is a builder method with a documented default; nothing here is required to run. The HTTP
            defaults are conservative on purpose — raise <code>max_body</code> before you accept large uploads, and
            remember that in a thread-per-connection server the keep-alive settings are a capacity decision, not a
            cosmetic one.
          </p>
          {@render code(cOptions, 'options')}
          <div class="tbl-wrap">
            <table class="tbl">
              <thead><tr><th>Setting</th><th>Default</th><th>What it bounds</th></tr></thead>
              <tbody>
                <tr><td><code>workers</code></td><td>8</td><td>HTTP threads; <code>WORKERS</code> in the environment overrides it.</td></tr>
                <tr><td><code>max_body</code></td><td>2 MiB</td><td>Request body size; <code>413</code> above it.</td></tr>
                <tr><td><code>max_header_bytes</code></td><td>64 KiB</td><td>Total header bytes; <code>413</code> above it.</td></tr>
                <tr><td><code>timeout</code></td><td>30 s</td><td>One stalled socket read or write.</td></tr>
                <tr><td><code>header_timeout</code></td><td>15 s</td><td>The whole request delivery, start to finish.</td></tr>
                <tr><td><code>keep_alive_idle</code></td><td>5 s</td><td>Idle time between requests on one connection.</td></tr>
                <tr><td><code>keep_alive_max</code></td><td>100</td><td>Requests served per connection.</td></tr>
                <tr><td><code>ws.shards</code></td><td>0 (per core)</td><td>Reactor threads.</td></tr>
                <tr><td><code>ws.max_frame</code> / <code>max_message</code></td><td>1 MiB</td><td>One frame / an assembled message (close 1009 above).</td></tr>
                <tr><td><code>ws.ping_interval</code> / <code>idle_timeout</code></td><td>30 s / 75 s</td><td>Server ping cadence / drop a silent socket.</td></tr>
                <tr><td><code>ws.max_buffered</code></td><td>1 MiB</td><td>Per-connection outbound queue; a slower consumer is dropped.</td></tr>
                <tr><td><code>ws.max_connections</code> / <code>per_ip</code></td><td>1,048,576 / 1024</td><td>Process-wide cap / single-source exhaustion.</td></tr>
                <tr><td><code>queue.visibility_timeout</code></td><td>&mdash;</td><td>How long a claimed job may run before it becomes visible again.</td></tr>
                <tr><td><code>actor mailbox</code></td><td>1024</td><td>Queued messages before <code>tell</code> fails with <code>Full</code>.</td></tr>
              </tbody>
            </table>
          </div>
          <p>
            Pool sizes are constructor arguments rather than builder methods: <code>Db::open_pool(path, n)</code> and
            <code>Pg::from_env(n)</code>. Size the Postgres pool with the advisory-lock note in mind — a held lock uses
            its own dedicated connection, outside the pool.
          </p>
        </article>

        <article id="ops" class="scroll-mt-24">
          <h2 class="h2">Operational endpoints</h2>
          <p>
            Four endpoints are always on, no feature required. <code>/__health</code> is liveness (200 while the
            process is up), <code>/__ready</code> runs the probe you register with <code>.readiness(&hellip;)</code>
            and returns 200 or 503, <code>/__metrics</code> exposes Prometheus text (requests total, in-flight, by
            status class), and <code>/__introspect</code> is the full surface — routes, models, tools, capabilities,
            searchable tables and <a href="#/docs/listeners" class="lnk">listeners</a>. Features add more endpoints —
            <code>/__tools</code>, <code>/__channels</code>, <code>/__actors</code> — and you can mount your own, like
            a read-only <code>/__migrations</code>.
          </p>
          {@render code(cOps, 'ops')}
          {@render diagram(dOpsGate, 'Probes answer before any guard; every other /__ path passes the ops_guard before global middleware and routing.')}
          <p>
            Because <code>Db</code> is <code>Clone</code>, clone a handle for the readiness probe before you hand
            ownership to <code>.state()</code>. The probes are intentionally credential-free and disclose nothing;
            everything else under <code>/__</code> is closed by default (<code>403</code>). Configure an
            <a href="#/docs/middleware" class="lnk"><code>ops_guard</code></a> to grant authenticated access.
          </p>
        </article>

        <article id="deploying" class="scroll-mt-24">
          <h2 class="h2">Deploying</h2>
          <p>
            With the <code>graceful</code> feature, <code>.serve()</code> does the right thing for a rolling update: it traps SIGTERM/SIGINT, stops
            accepting new connections, and drains in-flight requests before exiting. (<code>run(addr)</code> serves
            forever and <code>run_until(addr, flag)</code> gives manual control without the signal feature.)
            <code>ontzi</code> (Basque: <em>vessel</em>) wraps Docker Compose to run the horizontally-scaled shape
            locally — N replicas behind an nginx load balancer configured with <code>proxy_buffering off</code> so SSE
            streams pass straight through — and promotes the same shape to Kubernetes with manifests that already wire
            probes, graceful drain and Prometheus annotations. For a single box, a provisioning script installs the
            binary as a hardened systemd unit behind nginx instead.
          </p>
          {@render code(cDeploy, 'deploy')}
          {@render diagram(dDeployShape, 'The same shape locally under ontzi (replicas behind nginx) and on Kubernetes (probes, SIGTERM drain, authenticated metrics scrape).')}
          <p>
            Pick the backend for the deployment, not the code: one instance runs on SQLite (embedded, zero-ops); many
            pods run on Postgres, which is also what turns the queue&rsquo;s exclusivity, the advisory locks, the
            watchers and the channel broker from process-scoped into cluster-scoped. The request surface is stateless
            and scales horizontally either way, and the binary is small enough that
            <code>requests: 32Mi</code> is a reasonable ask.
          </p>
        </article>

        <article id="security" class="scroll-mt-24">
          <h2 class="h2">Security posture</h2>
          <p>
            sutegi ships panic isolation, bounded bodies and headers, two slowloris deadlines, capped keep-alive,
            per-IP rate limiting, secure-header and CORS middleware, bearer/basic guards, signed-cookie sessions with
            server-side expiry, CSRF tokens, login throttling and hash-bound sessions. Passwords are
            PBKDF2-HMAC-SHA256 PHC strings; agent tokens are stored hashed. 5xx messages never reach the client. The
            query builder guards identifiers, the search grammar sanitizes input before it can reach engine syntax,
            JSON paths and search queries are bound as parameters, and the fuzz and differential harness runs as a
            required CI gate.
          </p>
          <h3 class="h3">Two traps worth reading even if you never touch the code</h3>
          <ul class="list">
            <li>
              <strong class="text-white">Gate on router segments, not on the path string.</strong> The ops guard used
              to test <code>req.path.starts_with("/__")</code> — but the router trims and splits, so
              <code>//__tools/x</code> reached the same route while failing that test, leaving tool invocation and
              introspection reachable without the configured credential (CWE-288). The check and the dispatch now
              agree by construction. <strong class="text-white">Audit your reverse proxy too</strong>:
              Apache&rsquo;s idiomatic <code>&lt;LocationMatch "^/__"&gt;</code> has the identical blind spot, so a
              deployment can look doubly protected and be neither. (<code>MergeSlashes</code>, on by default since
              2.4.39, collapses it — which makes the anchored form correct by accident and one directive away from
              not being. <code>^/+__</code> is free.)
            </li>
            <li>
              <strong class="text-white">A credentialed WebSocket must not <code>allow_any_origin</code>.</strong> The
              same-origin policy does not stop a cross-origin handshake from carrying cookies; sutegi&rsquo;s
              same-origin default does, and <code>check_origin</code> widens it to an allowlist. See
              <a href="#/docs/websockets" class="lnk">WebSockets</a>.
            </li>
          </ul>
          <p>
            The S3 path is hardened in the places subprocess-based HTTP usually is not: credentials go to
            <code>curl</code> on stdin so <code>Authorization</code> is invisible to <code>ps</code>, the protocol is
            pinned with <code>--proto =https</code>, redirect following is off so a 3xx cannot replay a signature at
            an attacker-chosen host, TLS floors at 1.2, bodies are capped, certificate verification cannot be
            disabled, <code>PUT</code> bodies stage through a <code>0600</code> <code>O_EXCL</code> temp file, and
            <code>S3Store</code>&rsquo;s <code>Debug</code> redacts the secret key.
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

      </div>

      {#if page.deep}
        <a href="{ghBlob}docs/{page.deep}" target="_blank" rel="noopener"
          class="mt-10 flex items-center gap-3 rounded-xl border border-white/10 bg-white/[0.03] px-5 py-4 hover:border-[#ff6a3d]/50 transition-colors">
          <BookOpen size={18} class="text-[#ff6a3d] shrink-0" />
          <span class="text-sm text-[#b4b4c2]"><span class="text-white font-medium">Deep dive</span> — the long-form guide, with every option and edge case.</span>
          <ArrowRight size={15} class="ml-auto text-[#7a7a8a]" />
        </a>
      {/if}

      <!-- Prev / next -->
      <div class="mt-12 grid grid-cols-2 gap-4 pt-8 border-t border-white/10">
        {#if prev}
          <a href="#/docs/{prev.id}" class="group rounded-xl border border-white/10 px-4 py-3 hover:border-[#ff6a3d]/50 transition-colors">
            <div class="text-[11px] uppercase tracking-wider text-[#7a7a8a] flex items-center gap-1"><ArrowLeft size={12} /> Previous</div>
            <div class="text-sm text-white mt-1">{prev.title}</div>
          </a>
        {:else}<span></span>{/if}
        {#if next}
          <a href="#/docs/{next.id}" class="group rounded-xl border border-white/10 px-4 py-3 text-right hover:border-[#ff6a3d]/50 transition-colors">
            <div class="text-[11px] uppercase tracking-wider text-[#7a7a8a] flex items-center justify-end gap-1">Next <ArrowRight size={12} /></div>
            <div class="text-sm text-white mt-1">{next.title}</div>
          </a>
        {/if}
      </div>
    </main>

    <!-- On this page -->
    <aside class="hidden xl:block sticky top-[57px] h-[calc(100vh-57px)] w-60 shrink-0 overflow-y-auto custom-scrollbar py-14 pr-6">
      {#if toc.length}
        <div class="text-[11px] uppercase tracking-wider text-[#7a7a8a] font-semibold mb-3">On this page</div>
        <ul class="space-y-2 text-[13px]">
          {#each toc as h}
            <li>
              <a href="#/docs/{current}/{h.id}" class="block leading-snug transition-colors {activeHeading === h.id ? 'text-[#ff6a3d]' : 'text-[#9090a0] hover:text-white'}">{h.text}</a>
            </li>
          {/each}
        </ul>
      {/if}
      <a href="{ghBlob}docs/reference/{current}.md" target="_blank" rel="noopener"
        class="mt-8 inline-flex items-center gap-1.5 text-[12px] text-[#7a7a8a] hover:text-white transition-colors">
        <FileText size={13} /> View as Markdown
      </a>
    </aside>
  </div>
</div>

{#snippet diagram(src: string, caption: string)}
  <figure class="diagram">
    <pre class="mermaid">{src}</pre>
    <figcaption>{caption}</figcaption>
  </figure>
{/snippet}

{#snippet code(text: string, key: string)}
  <div class="relative my-4">
    <button onclick={() => copy(text, key)} class="absolute top-2 right-2 text-[11px] font-mono text-[#7a7a8a] hover:text-[#ff6a3d] border border-white/10 rounded px-2 py-1 transition-colors z-10">
      {copiedKey === key ? 'copied' : 'copy'}
    </button>
    <pre class="bg-black/40 border border-white/5 rounded-lg p-4 font-mono text-[12px] sm:text-[13px] text-[#d0d0e0] overflow-x-auto custom-scrollbar leading-relaxed">{text}</pre>
  </div>
{/snippet}

<style>
  .prose-doc :global(article:not(.is-active)) { display: none; }
  .prose-doc :global(article > .h2:first-child) {
    font-size: 2.1rem;
    letter-spacing: -0.01em;
    margin-bottom: 1.25rem;
  }
  .prose-doc :global(.diagram) {
    margin: 1.5rem 0;
    padding: 1.25rem 1rem 0.9rem;
    border: 1px solid rgba(255, 255, 255, 0.08);
    border-radius: 0.75rem;
    background: rgba(255, 255, 255, 0.02);
    overflow-x: auto;
  }
  .prose-doc :global(.diagram pre.mermaid) {
    display: flex;
    justify-content: center;
    margin: 0;
    font-family: var(--font-mono);
    font-size: 12px;
    color: #7a7a8a;
    white-space: pre;
  }
  .prose-doc :global(.diagram pre.mermaid:not([data-processed])) { justify-content: flex-start; }
  .prose-doc :global(.diagram figcaption) {
    margin-top: 0.75rem;
    text-align: center;
    font-size: 12.5px;
    color: #7a7a8a;
  }
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
  .prose-doc :global(.tbl-wrap) {
    overflow-x: auto;
    margin: 1.1rem 0;
    border: 1px solid rgba(255, 255, 255, 0.08);
    border-radius: 0.6rem;
  }
  .prose-doc :global(.tbl) {
    width: 100%;
    border-collapse: collapse;
    font-size: 14px;
    color: #b4b4c2;
  }
  .prose-doc :global(.tbl th) {
    text-align: left;
    font-weight: 600;
    color: #cfcfe0;
    font-size: 11px;
    text-transform: uppercase;
    letter-spacing: 0.05em;
    padding: 0.7rem 0.9rem;
    background: rgba(255, 255, 255, 0.03);
    border-bottom: 1px solid rgba(255, 255, 255, 0.1);
    white-space: nowrap;
  }
  .prose-doc :global(.tbl td) {
    padding: 0.6rem 0.9rem;
    border-bottom: 1px solid rgba(255, 255, 255, 0.05);
    vertical-align: top;
    line-height: 1.55;
  }
  .prose-doc :global(.tbl tr:last-child td) { border-bottom: none; }
  .prose-doc :global(.tbl td:first-child) { white-space: nowrap; }
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
