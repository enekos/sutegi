<!-- Generated from landing/src/Docs.svelte. Run: cd landing && pnpm docs:export -->

[← Introduction](introduction.md) · [Documentation index](../README.md) · [Is it production-ready? →](honesty.md)

# Philosophy & the bet

Frameworks usually force a trade: batteries-included but heavy, or tiny but bare. sutegi bets you can refuse the trade if you build every layer on `std` and make each one an *opt-in compile-time feature*. Compile in only the HTTP core and you get a ~480 KB binary with no async runtime; switch on `sqlite` and `graceful` and you get an ergonomic, agent-native service — with nothing else along for the ride. The full `todo` example, every pillar plus bundled SQLite, is ~1.31 MB.

The second bet is agent-native design. Because every route, model, and tool registers its own metadata, the framework assembles a complete, machine-readable description of your app for free. An LLM points at `/__introspect`, reads the surface — including what the *store* can do — and calls `/__tools`. The same application you built for humans is drivable by a model without a line of glue.

The third principle is that **the type you hold is the only thing that changes when you scale**. Handlers, models, validation, the queue, the event store, file storage and channel broadcasts are written once against traits; moving from a single SQLite file to a fleet of Postgres-backed pods swaps a constructor, not your code. Where the engines genuinely differ, a [capability bit](backend.md) says so out loud and the gated call fails with a named error instead of a dialect SQL surprise.

The cost of these bets is a large hand-rolled surface and a young ecosystem — which the next page addresses head-on.

---

[← Introduction](introduction.md) · [Documentation index](../README.md) · [Is it production-ready? →](honesty.md)
