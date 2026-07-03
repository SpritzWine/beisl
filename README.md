# Beisl

> Perf-tracing made easy — joining `perf` and MangoHud for actual numbers that matter.

A *Beisl* is a Viennese corner pub: the place where, over a Spritz, everyone loudly
holds forth on what everyone else did wrong. Fitting, then, that this one sits and
vets the **wine's** performance.

Beisl wraps a game — Steam launch option `beisl %command%`, or one toggle in the
SpritzWine control panel — and produces a **symbolicated CPU trace** of where wine
spends its time, with MangoHud's frametime log as a side-channel that bookmarks the
low-performance peaks. Export it, open it in the GUI, or let it feed straight into
analysis over MCP.

Works with **any** wine — SpritzWine/Prater, stock Proton, GE, Lutris-wine, system
wine — not just ours, so a SpritzWine-vs-Proton comparison captures both halves.

See **[DESIGN.md](DESIGN.md)** for the architecture and roadmap.

## Status

Early scaffold (**v0**). The wrapper sets up the environment and runs the game today;
`perf` capture, symbol resolution, the artifact export, the GUI and the MCP server
are stubbed with their design pinned in DESIGN.md.

## Build

```sh
cargo build --release   # host-side tool; needs a recent stable Rust
```

## License

Copyright © 2026 Florian Schaupp.

Beisl is free software, licensed under the GNU Lesser General Public License,
version 2.1 or (at your option) any later version — see [LICENSE](LICENSE).
