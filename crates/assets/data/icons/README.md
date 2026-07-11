# beisl brand marks

Das Glasl — a Spritzer glass whose pour is a frametime trace (the layer
bands are chart.rs's own data colours; the blip crests the open rim).
Clip-free construction so every rasterizer renders it identically; the
full concept board lives on the `pages` branch under `reference/`.

- `icon-glass-pour.svg` — the app icon (tile background), for
  hicolor/scalable and the .desktop entry.
- `icon-symbolic.svg` — monochrome, for tray/menu/symbolic contexts.
- `beisl-glass-{dark,light}.svg` — the bare glass on nothing, for docs
  and pages that bring their own background.

These are the canonical copies (embedded via the `assets` crate). The `pages` branch carries deliberate
duplicates (`page/assets/` — a static site must self-host; `reference/marks/`
— the concept archive): if a mark ever changes, edit here first, then copy
outward to the pages branch.
