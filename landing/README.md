# sutegi landing / docs page

A single, self-contained `index.html` — no build step, no dependencies. Open it
directly, or host it statically (e.g. GitHub Pages), mirroring the style of the
[Marrow](https://github.com/enekos/marrow) and Tartalo landing pages (dark theme,
JetBrains Mono + Space Grotesk, scramble animations) with a forge-ember accent.

## View locally

```bash
open landing/index.html          # macOS
# or serve it:
python3 -m http.server -d landing 8000   # then visit http://localhost:8000
```

## Deploy (GitHub Pages)

Publish the `landing/` directory (or copy `index.html` to the Pages root). The
page is fully static — fonts load from Google Fonts; everything else is inline.

Sections: hero · what · features · quickstart · agent contract · architecture · docs.
