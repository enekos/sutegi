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

## Structure (cloned from Marrow)

```
index.html          Vite entry (meta, fonts, #app)
src/main.ts         mounts the app
src/App.svelte      renders <Landing/>
src/app.css         Tailwind import + theme tokens + animations
src/Landing.svelte  the whole page (hero, what, how, features, use-cases,
                    quickstart, extensive docs accordions, live introspect, CTA)
```

The **Introspect a live app** panel fetches `/__introspect` from a running
sutegi app (default `http://localhost:8080`) — start one with
`cargo run -p todo-example` (CORS enabled) to see its live surface.
