<!-- Das Glasl, theme-aware: the -dark variant is drawn for dark grounds. Forgejo/
     GitHub render the <picture> media query; anything plainer falls back to the img. -->
<picture>
  <source media="(prefers-color-scheme: dark)" srcset="crates/assets/data/icons/beisl-glass-dark.svg">
  <img src="crates/assets/data/icons/beisl-glass-light.svg" alt="Das Glasl — the Beisl mark" width="72" align="right">
</picture>

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

## Install & use

Beisl ships bundled with SpritzWine, or grab it standalone from
[Codeberg releases](https://codeberg.org/SpritzWine/beisl/releases): download
`beisl-<version>-x86_64.tar.gz` (and `SHA256SUMS` to verify it), extract it, and run
`./beisl-setup` or `./beisl install` from the extracted tree — same flow as below.
(Or build it yourself — see **Build**.)

From the extracted tree:

```sh
./beisl-setup          # GUI: offers Install/Update from this tree, then checks
                       # prerequisites (perf, symbols, permissions)
```

or headless:

```sh
./beisl install        # copies beisl onto your PATH (~/.local/bin) + adds "Beisl Setup" to the menu
beisl --check          # prerequisite checklist in the terminal (alias: `beisl doctor`)
```

Then add Beisl to a game's launch options — **Steam → Properties → Launch Options**:

```
beisl %command%
```

Play, then close the game; the trace lands in `~/.local/share/beisl/traces/`.
Usage documentation lives in [`docs/`](docs/) — one markdown file per binary, and the
single source of truth: the same files render each binary's `--help` and the man pages
(`man beisl` works after install). Works the
same way as `mangohud %command%`, and with any Proton/Wine — not just SpritzWine. Mid-game
you can also hit "Stop & save" in the session window: the recorder stops and the trace is
saved while the game keeps running, so you don't have to quit to keep a capture.
`beisl-setup`'s **Traces** page lists every past run (newest first) — open one to see its
results, reveal its folder, or delete it. `beisl uninstall` removes the install. Every
binary takes `--version` (`beisl --version`, `beisl-setup --version`, `beisl-mcp --version`).

## Status

Early (**v0**), but the core pipeline runs end to end: the wrapper sets up the
environment, launches the game, records `perf` (system-wide for now — scoping the
capture to just the game's cgroup is a later refinement), and resolves + merges it into
`artifact.json`/`report.txt` (symbol fidelity depends on what the traced wine build
carries — a build with debug info retained resolves more than a stripped one). The
libcosmic setup app covers preflight/install, a trace library (browse past runs,
analyze/discard, reveal the folder, delete), and the session window (live capture,
"Stop & save" mid-game, and the results view). `beisl-mcp` serves that same library
read-only over MCP. See DESIGN.md for the architecture and what's still ahead — an
in-game overlay indicator, flamegraphs, and an A/B compare view.

## Build

Everything at once — both workspaces, packaged into a release tarball under `dist/`:

```sh
make                    # = make package; needs a recent stable Rust
```

Or piece by piece:

```sh
cargo build --release   # host-side tool; needs a recent stable Rust
```

The GUI is a separate workspace (`crates/beisl-gui`, heavy libcosmic/wgpu tree — kept out
of the main lockfile on purpose):

```sh
cd crates/beisl-gui && cargo build --release
```

## Asking an AI about your traces (MCP)

The bundle ships `beisl-mcp`, a read-only [MCP](https://modelcontextprotocol.io)
server over your recorded traces. It is entirely opt-in: it opens no ports (the
transport is its own stdin/stdout) and nothing runs unless you wire it into an AI
client's configuration, e.g.:

```json
{ "mcpServers": { "beisl": { "command": "beisl-mcp" } } }
```

It exposes six read-only tools:

- `list_traces` — every run, newest first: id, game, start time, duration, analyzed?
- `get_setup_status` — the preflight checklist (the same checks as `beisl --check`)
- `get_artifact` — a run's analyzed trace as JSON: frame stats and spikes, the
  per-layer CPU split (who ate the frame: kernel/driver/dxvk/wine/game), hottest
  functions, diagnostics
- `get_report` — the same analysis, human-readable prose
- `get_log` — the tail of a run's `beisl`, `game`, or `perf` log
- `get_crash_summary` — pre-chewed crash forensics for a run that "just died"

Put together, "which layer ate the frame in last night's session?" becomes a question
you can literally ask. Deleting and analyzing stay in the app, where a human
confirms them.

**A note on git dependencies:** the main workspace is crates.io-only. `beisl-gui` pulls
~48 git dependencies — all of them the [libcosmic](https://github.com/pop-os/libcosmic)
stack, which System76 does not publish to crates.io yet (the whole COSMIC ecosystem ships
as rev-pinned git, including their forks of `winit` etc.). We pin an exact revision and
`Cargo.lock` pins every transitive commit, so builds are reproducible; the pin matches the
SpritzWine installer's. Worth migrating when COSMIC lands on crates.io.

## License

Copyright © 2026 Florian Schaupp.

Beisl is free software, licensed under the GNU Lesser General Public License,
version 2.1 or (at your option) any later version — see [LICENSE](LICENSE).
