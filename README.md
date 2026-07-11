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

Beisl ships bundled with SpritzWine (or build it standalone — it works with any wine).
From the extracted tree:

```sh
./beisl-setup          # GUI: offers Install/Update from this tree, then checks
                       # prerequisites (perf, symbols, permissions)
```

or headless:

```sh
./beisl install        # copies beisl onto your PATH (~/.local/bin) + adds "Beisl Setup" to the menu
beisl --check          # prerequisite checklist in the terminal
```

Then add Beisl to a game's launch options — **Steam → Properties → Launch Options**:

```
beisl %command%
```

Play, then close the game; the trace lands in `~/.local/share/beisl/traces/`. Works the
same way as `mangohud %command%`, and with any Proton/Wine — not just SpritzWine.
`beisl uninstall` removes it.

## Status

Early (**v0**). The libcosmic setup page and the preflight/install plumbing work today;
the wrapper sets up the environment and launches the game, while `perf` capture, symbol
resolution, the artifact export and the MCP server are stubbed with their design pinned in
DESIGN.md.

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
