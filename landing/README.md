# sutegi landing / docs site

The marketing + docs site for sutegi. Same stack and style as the
[Marrow](https://github.com/enekos/marrow) landing page — **Svelte 5 + Vite +
Tailwind 4**, dark theme, JetBrains Mono + Space Grotesk, scramble/hover
animations — re-themed with a forge-ember accent.

Builds to a single self-contained HTML via `vite-plugin-singlefile`.

## Develop

```bash
cd landing
npm install
npm run dev      # http://localhost:5173
```

## Build

```bash
npm run build    # -> dist/index.html (single self-contained file)
npm run preview
```

`vite.config.ts` sets `base: '/sutegi/'` for GitHub Pages at
`enekos.github.io/sutegi/`. Publish `dist/`.

## Markdown documentation

All authored landing and reference content is also available in the
[GitHub documentation index](../docs/README.md). Regenerate it after editing the
Svelte content:

```bash
pnpm docs:export
pnpm docs:check
```

The exporter uses the existing Svelte parser to retain prose, examples, tables,
and callouts. It rewrites website routes to relative Markdown links and adds
previous/next navigation. CI checks that the committed export stays current.
The existing in-depth guides in `docs/` remain hand-maintained.

## Structure

```
index.html          Vite entry (meta, fonts, #app)
src/main.ts         mounts the app
src/App.svelte      routes between <Landing/> and <Docs/>
src/app.css         Tailwind import + theme tokens + animations
src/Landing.svelte  hero, features, use-cases, quickstart, tutorial, live introspect
src/Docs.svelte     grouped documentation reference
scripts/export-docs.mjs  exports both pages to ../docs/ as Markdown
```

The **Introspect a live app** panel fetches `/__introspect` from a running
sutegi app (default `http://localhost:8080`) — start one with
`cargo run -p todo-example` (CORS enabled) to see its live surface.
