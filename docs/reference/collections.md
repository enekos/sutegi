<!-- Generated from landing/src/Docs.svelte. Run: cd landing && pnpm docs:export -->

[← Templates](templates.md) · [Documentation index](../README.md) · [Crypto primitives →](crypto.md)

# Collections

`collect(..)` wraps any iterable in a `Collection<T>` — a fluent, chainable API for the everyday shaping that raw `Iterator` makes verbose. It is part of the facade, no feature needed.

```rust
use sutegi::collect;

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
// nothing over doing the work by hand.
```

It is a thin layer over `Vec<T>`: it `Deref`s to `[T]` and round-trips through `Vec` and iterators, so it adds no allocation over doing the work by hand — and you can drop back to plain iterator code at any point in a chain.

---

[← Templates](templates.md) · [Documentation index](../README.md) · [Crypto primitives →](crypto.md)
