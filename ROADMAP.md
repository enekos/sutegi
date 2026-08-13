# sutegi — roadmap

*"Laravel for Rust": a batteries-included web framework built entirely on `std`, with an
AI agent as a first-class user.*

**Updated** 2026-08-13 · **Version** v0.9.0, 27 crates, 23 published · **~47k LOC, 711 tests**

---

## Where this actually is

- **The framework works.** ORM with a real `Backend` seam (SQLite + a hand-rolled pure-std
  Postgres driver), migrations with deterministic shadow-replay diffing, auth, mail,
  templates, event sourcing, a durable queue that runs on either backend, WebSockets on a
  kqueue/epoll reactor (80k sockets at ~0% idle CPU), channels/presence/pubsub, actors and
  supervision trees, FTS + hybrid search, reactive `watch()` queries. Phoenix parity is
  substantially real, not aspirational.
- **Four real consumers, all mine.** horma, bildu, bai, segurit. bildu is the serious one —
  68k LOC in production on a 2 GB box, and it has driven three framework releases (v0.7 FTS
  and `watch()`, v0.8 the user system, v0.9 the backend-portable queue). Dogfooding is
  genuinely working as a design forcing function.
- **Zero external users.** 23 crates on crates.io and, as far as anyone can tell, nobody
  outside this laptop has ever built anything on them. Every quality signal this project
  has is self-generated.
- **No TLS, and that is a tax every consumer pays** — though the shape of the tax just
  improved. The framework still cannot terminate TLS, and bildu routes *every* outbound call
  through a shelled-out `curl`. But `sutegi-storage`'s S3 work (2026-08-10, PR #14)
  introduced the right abstraction: a one-method **`HttpTransport` seam** with `SystemCurl`
  (delegates the handshake and certificate verification to system curl — the crypto that must
  not be hand-rolled is not) and `PlainHttp` (pure std, and it *refuses* https rather than
  pretending). Server-side termination is still a proxy's job, and there is no in-tree TLS
  implementation.
- **Two headline features are weaker than they read.** Release profile is `panic = "abort"`,
  so supervision-tree crash-restart is dev/test semantics only — in production you are
  relying on the pod restarting. And `cross_pod()` on SQLite reports the weaker guarantee
  honestly, which is good, but means "durable queue" means two different things depending
  on backend.
- **The perf gate cries wolf.** `make bench-compare` fails on unmodified master; two runs
  on the same code disagreed (1 vs 5 regressions, 16–19 "improvements" in untouched code).
  A gate that is routinely bypassed with `--no-verify` is not a gate.

## The one thing that decides this project

**Does anyone other than me ever ship on sutegi?**

Everything else is a matter of continuing to build well, which is demonstrably not the
bottleneck. 27 crates and a security audit and Phoenix parity do not answer this question;
one stranger's production deployment does. If the answer is no, that is a legitimate
outcome — but then sutegi is *my application framework*, its roadmap should stop looking
like a public framework's, and the crates.io publishing ritual should stop.

Decide it deliberately rather than by drift.

---

## M1 — Make the claims survive a stranger reading them ← next

The single highest-leverage work is not code. Someone evaluating sutegi today reads
"zero dependencies" and "supervision trees" and "durable queue" and forms beliefs that
production will not honour.

- [ ] **Re-record `benches/baselines/local.json`** and make the pre-commit gate either
      trustworthy or gone. Pick one. A gate bypassed by habit trains you to ignore CI.
- [ ] **A `LIMITATIONS.md` that leads with the hard parts**, linked from the README's first
      screen: no TLS in either direction and what that means operationally (a proxy is
      mandatory; outbound HTTPS needs `curl` or a sidecar); `panic = "abort"` vs supervision
      restart; per-backend queue guarantees; the blocking-thread concurrency model and where
      it stops scaling.
- [ ] **GH Pages docs refresh.** The published docs still describe the pre-realtime story.
      Stale docs on a public framework are worse than no docs — they are a promise you
      already broke.
- [ ] **`cargo doc` warning-free on every published crate**, and a getting-started that a
      stranger can complete without reading source.

**Done when:** someone can go from `cargo add sutegi` to a deployed, TLS-served CRUD app
following only published docs, and every surprise they hit is one the docs warned them about.

## M2 — TLS, decided for real

The posture ("don't hand-roll; add `rustls` behind a feature when a real consumer needs it")
was correct when written. bildu is that consumer, and it is paying in `curl` subprocesses.
The S3 work already built the seam this plugs into — a third `HttpTransport` impl is now a
much smaller job than it was a week ago.

- [ ] Add `rustls` behind an off-by-default `tls` feature: a `RustlsTransport` implementing
      the existing `HttpTransport`, server-side termination, and PG/SMTP transport encryption
      through the adapter seams that already accommodate it.
- [ ] Keep `--no-default-features` genuinely dependency-free and prove it in CI, so
      "zero-dep" narrows to a true, defensible claim instead of an absolute one that
      quietly costs users.
- [ ] Port bildu's outbound path off `curl` as the acceptance test. The transport seam means
      this is a constructor change in bildu rather than a rewrite.
- [ ] Generalize the seam beyond storage: fetch, webhooks, and the LLM calls should all take
      an `HttpTransport` rather than each reinventing a `curl` boundary.

**Done when:** bildu makes an HTTPS request with no subprocess, and the zero-dep build still
compiles with an empty dependency tree.

## M3 — One external user

Bold, and the only milestone that changes what sutegi *is*.

- [ ] Write the comparison nobody has written: sutegi vs axum+sqlx, honestly, including
      where sutegi loses (ecosystem, async, TLS, employability of the skill).
- [ ] Publish the thing that is actually novel and has no competitor: **the agent-native
      contract**. `/__introspect` + `/__tools` + `/__channels` let an LLM drive an arbitrary
      app with no SDK and no source access. That is a genuinely new idea and it is currently
      buried under framework-parity work that axum users do not care about.
- [ ] Ship an MCP server over the agent contract. Every sutegi app becomes an MCP server for
      free — that is a one-sentence pitch, and it lands in the one market that is growing.
- [ ] Find one person, help them ship one thing, write down every place they got stuck.

**Done when:** a repository that is not mine has sutegi in its `Cargo.toml` and a URL.

## M4 — Close the honest gaps

Only after M1–M3. These are known, specced, and not urgent.

- [ ] Supervision under `panic = "unwind"`: measure the binary-size and perf cost, then
      either offer it as a profile or document the limitation permanently.
- [ ] P6 single-binary deploy — productize horma's proven `include_str!` embed as a scaffold
      option.
- [ ] Deferred from the Postgres-parity SDD: GIN-index emission from `#[model(searchable)]`,
      FTS language knob, row-level invalidation v2.
- [ ] Router: `PATCH` verb (bildu hit this and used `PUT`), and mount `__`-prefixed routes
      before user routes so a root `static_dir("/")` cannot shadow `/__tools`.
- [ ] Timeout knob on bridge server-side effects — a 60s call currently holds a worker.

---

## Not doing

- **`sutegi-live`.** The zumar bridge is the LiveView analog. Two answers to one question is
  worse than one.
- **An async runtime.** Blocking threads plus a kqueue/epoll reactor for sockets is the
  design. Revisit only with a measurement that shows it losing on a workload that matters.
- **Chasing Rails/Laravel feature parity for its own sake.** Parity was a useful compass
  while the framework was thin. It is now an infinite backlog that generates no users.
- **More crates.** 27 is past the point where the count is a liability. New capability goes
  into an existing crate or behind a feature flag.

## Risks worth naming

- **Bus factor 1 with a hand-rolled Postgres wire driver, crypto, and an epoll reactor.**
  Every one of these is a correctness-critical component that no second pair of eyes has
  read. The security audit (1 Critical, 10 High) found that the hand-rolled *primitives*
  held up and the bugs clustered in new integration code — reassuring, and not a substitute
  for external review of the driver and the reactor.
- **The agent surface is the attack surface.** `/__tools` was invocable unauthenticated with
  full DB authority, and `/__introspect` was un-gatable. Both fixed. The lesson generalizes:
  insecure-by-default on the most novel feature is the failure mode to keep auditing.
- **Credentials in a framework are a recurring hazard.** The S3 work shipped a real fix for a
  real leak — `S3Store` derived `Debug` over its access and secret keys, so printing a store
  put credentials in the logs. `SystemCurl` now keeps credentials out of `argv`, pins
  `--proto =https`, refuses redirects so a 3xx cannot replay a signature elsewhere, and floors
  TLS at 1.2. That is the right standard; the lesson is that it took an audit-grade pass to
  find a one-line derive.
- **Attention.** sutegi competes with 16 other active projects, and it is currently keeping
  up — but it is the dependency under four of them, so any month it lags is a month its
  consumers grow private workarounds.
