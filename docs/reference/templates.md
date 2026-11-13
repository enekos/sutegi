<!-- Generated from landing/src/Docs.svelte. Run: cd landing && pnpm docs:export -->

[← Event sourcing](events.md) · [Documentation index](../README.md) · [Collections →](collections.md)

# Templates

The `template` feature is a Blade-lite engine over `Json` contexts: `{{ escaped }}` and `{!! raw !!}` dot-path interpolation, `@if`/`@else`, `@foreach … as …` (with `loop.index`/`first`/`last`), and `@include` for partials.

```rust
let mut views = Templates::new();
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
// line-numbered errors. The mail layer's themed HTML renders through this.
```

Templates compile once to an AST and report line-numbered errors. It also powers the themed HTML in the [mail layer](mail.md).

---

[← Event sourcing](events.md) · [Documentation index](../README.md) · [Collections →](collections.md)
