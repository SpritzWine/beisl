# beisl

Game perf-tracing tool (Rust, early v0): wraps a game launch (`beisl %command%` in Steam),
sets up the environment (MangoHud frametime CSV side-channel, optional GPU pin), records
`perf` + logs into a per-run trace dir under `~/.local/share/beisl/traces/run-*/`
(`session.json`, `beisl.log`, `perf.log`, `perf.data`, MangoHud CSV), and publishes live
state for a session window. Wine-agnostic by design. `DESIGN.md` has the architecture,
case studies, and roadmap. Symbol resolution (Exports tier), artifact export, the trace
library, and the MCP server are built; the Full/DWARF symbol tier is still open.

## Workspace layout

Main workspace (`Cargo.toml`, crates.io deps only, mostly std):

- `crates/assets` — embedded data payloads (brand icons, udev rule, tmpfiles conf);
  the `data/` files live inside this crate so `include_str!` paths stay short.
- `crates/beisl` — the wrapper binary; subcommands: trace (default via `%command%`),
  `--check` (aliases `--preflight`, `doctor`), `install`/`uninstall`, `symbols <dir>`
  (resolve a captured trace), `--version`. `run.rs` orchestrates a trace; `mangohud.rs`,
  `perf.rs`, `gpu.rs` handle the pieces.
- `crates/preflight` — UI-agnostic setup checks (perf/llvm/paranoid/mangohud/GPU) +
  persisted config. Detection only; privileged fixes are *described* (`Fix` +
  `fix_command()`) and executed by the caller via pkexec, never from inside the crate.
- `crates/session` — small serde JSON state file (`session.json`) bridging the CLI
  writer and the GUI session window reader.
- `crates/symbol-resolver` — perf.data addresses → wine symbols, tiered fidelity.
- `crates/artifact` — merged exportable trace, frametimes ⋈ samples.
- `crates/beisl-mcp` — `beisl-mcp` binary: read-only MCP server (JSON-RPC over stdio,
  hand-rolled — no async runtime) over the trace library; opt-in, ships in the bundle.
- `crates/beisl-gui` — `beisl-setup` binary (libcosmic): preflight checklist, plus an
  install/update page shown first when run from an extracted dist tree (a tiny fork of
  the SpritzWine installer wizard over `beisl install`). **Excluded** from the main
  workspace: it is its own workspace with its own `Cargo.lock` because libcosmic pulls
  ~48 rev-pinned git deps (wgpu/winit tree). Build it separately, only when touching it.

Dependency direction: `beisl`, `beisl-gui`, and `beisl-mcp` all depend on `preflight` +
`session`; `beisl` and `beisl-gui` also depend on `artifact` (which itself depends on
`symbol-resolver`); nothing depends on `beisl`.

## Commands (verified)

```sh
cargo check --workspace          # main workspace, fast (excludes beisl-gui)
cargo test --workspace           # inline unit tests (all main-workspace crates)
cargo clippy --workspace
cargo fmt
cargo check -p beisl             # or -p preflight, -p session, ...
(cd crates/beisl-gui && cargo check)   # GUI: separate workspace; heavy git deps,
                                       # slow first build — skip unless GUI changed
make                             # release-build BOTH workspaces + package the bundle
                                 # tarball (beisl + beisl-mcp + beisl-setup + README/LICENSE)
                                 # into dist/
```

## Conventions

- **Errors:** std only — `io::Result` / `io::Error` (no anyhow/thiserror). Top level
  prints `beisl: {e}` to stderr and maps to an exit code in `main.rs`.
- **Logging:** no log crate. `eprintln!("beisl: ...")` for the terminal; during a trace,
  a `Log` helper appends timestamped lines to `beisl.log` in the run dir (perf output
  goes to `perf.log`). Failures that shouldn't abort the run use `let _ = ...`.
- **Tests:** inline `#[cfg(test)] mod tests` at the bottom of the source file, no
  `tests/` dirs.
- **Deps:** main workspace stays light and crates.io-only; planned deps are noted in
  Cargo.toml comments before being added. Heavy GUI deps live only in `beisl-gui`.
- **Comments** explain the *why* (often referencing DESIGN.md case studies) — keep that
  style when editing.
- No privilege escalation inside library code; preflight fixes run via pkexec from the
  GUI/caller.

## Known shortcomings (from real trace analysis, 2026-07-11: Star Wars Outlaws via umu-run silently exited; trace run-1783746830)

Fixed on 2026-07-11 (see git history for the details): game stdout/stderr now teed
into `game.log` in the run dir; MangoHud gets `pci_dev=` (from the GPU pin, or the
sole GPU — with ≥2 GPUs and no pin it still guesses nothing, so *pin the GPU* on
multi-GPU boxes); the exit-status log line is labeled as the wrapper's, not the
game's; `session.json`'s `game` is upgraded from the launcher argv guess to the real
name once the MangoHud CSV appears; MangoHud logging is always-on (`autostart_log=1`
— the CSV is flushed per frame, so frametimes survive up to a crash; the hotkey now
pauses/resumes instead of arming); a user's own `log_duration=N` no longer truncates
the CSV (we append `log_duration=0` last — MangoHud resolves duplicate keys
last-wins and 0 disables the auto-stop — with a beisl.log note when overriding).

Still open: nothing at the moment.
(The driver-hotspots half of the Outlaws follow-ups is handled too: the advisory
"driver debug symbols" check probes each hardware ICD's build-id against
`/usr/lib/debug/.build-id/` — debuginfod counts as covered — and names the
distro-specific install in its why; no auto-fix, mirroring the kernel-symbols
check's consent stance.)
