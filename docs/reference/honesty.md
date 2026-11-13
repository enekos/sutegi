<!-- Generated from landing/src/Docs.svelte. Run: cd landing && pnpm docs:export -->

[← Philosophy & the bet](philosophy.md) · [Documentation index](../README.md) · [The workspace map →](crates.md)

# Is it production-ready?

The honest answer is: **it depends on what you are asking sutegi to do, and how much you are willing to own yourself.** Rather than a marketing yes, here is the real picture.

## What is solid

The core is small enough to read in an afternoon and is exercised hard. Beyond the unit suite, a deterministic, pure-`std` **fuzz and differential harness** hammers every hand-rolled surface — JSON, HTTP, the crypto primitives, the Postgres wire protocol, the RFC 6455 WebSocket codec, templating, SigV4 — and runs in CI as a required gate. Building that harness caught and fixed several real bugs (a JSON stack-overflow DoS, an HTTP unbounded line-read, an unchecked PG frame panic, a SCRAM iteration-count DoS, and more), and the JSON parser is checked round-trip against `serde_json` on hundreds of thousands of cases. The query builder guards identifiers against SQL injection, and the search grammar sanitizes input before it can reach engine query syntax — both matter precisely because an AI tool argument can reach a column, a sort slot or a match expression.

The concurrency claims are tested against real servers, not asserted: queue claim exclusivity under six concurrent workers, crash recovery through an expired lease, a lost-update race surfacing as `40001` under `Serializable`, a rolled-back write never waking a watcher, and two OS processes chatting through one PostgreSQL over channels.

> [!WARNING]
> **The hand-rolled surface is the thing to weigh**
>
> Zero dependencies means sutegi implements its own cryptography and protocols. That surface is tested against RFC vectors, live servers, and the fuzz harness — but it has **not had an independent security audit**, and the constant-time defenses in the auth path are implemented and reasoned about, not yet *measured*. For a framework whose selling point is hand-built crypto, that distinction matters.

## The one real gap: TLS

sutegi does not ship TLS. The intended posture is to terminate HTTPS at a load balancer or service mesh — standard, and fine for the front door. The genuine limitation is **in-cluster Postgres and SMTP**, often expected encrypted; today those connections must stay inside a trusted network boundary. (Object storage is already solved: the [storage transport seam](storage.md) borrows the system `curl` for `https`, so S3/R2 works today without a TLS stack in the tree.) TLS is the one primitive we will *not* hand-roll; the plan is a single curated, audited dependency (`rustls`) behind an opt-in `tls` feature, added when a real consumer needs it.

## It is a coherent solo bet, not a mature ecosystem

Laravel and Phoenix took years and large communities. sutegi is a broad, coherent framework built quickly by one maintainer — its breadth currently runs ahead of its battle-tested depth, and no real production workload has yet run on it under load. That is not a defect to paper over; it is the honest framing.

## Three tiers you can actually claim

- **Run it yourself, in-cluster, eyes open.** Internal services and agent tool servers where PG/SMTP stay on a trusted network. Well within reach today.
- **Run it for a real workload.** Add TLS if your topology needs encrypted PG/SMTP, and a deployed consumer under real traffic to convert “one maintainer’s correctness” into evidence.
- **1.0 / recommend to others.** Requires measured timing on the auth path and an external security review. Do not cross this line on trust alone.

> [!NOTE]
> **In short**
>
> Reach for sutegi when small, legible, and agent-friendly matter and you can keep your data plane on a trusted network. Do not reach for it as a drop-in replacement for a mature, audited stack fielding hostile traffic with hand-rolled crypto on the open internet — not yet.

---

[← Philosophy & the bet](philosophy.md) · [Documentation index](../README.md) · [The workspace map →](crates.md)
