# beisl

Game perf-tracing tool (Rust, early v0): wraps a game launch (`beisl %command%` in Steam),
sets up the environment (MangoHud frametime CSV side-channel, optional GPU pin), records
`perf` + logs into a per-run trace dir under `~/.local/share/beisl/traces/run-*/`
(`session.json`, `beisl.log`, `perf.log`, `perf.data`, MangoHud CSV), and publishes live
state for a session window. Wine-agnostic by design. `DESIGN.md` has the architecture,
case studies, and roadmap. Symbol resolution (Exports tier), artifact export, the trace
library, and the MCP server are built; the Full/DWARF symbol tier is still open.

## Mandatory rules (all projects)

Identical in every SpritzWine, Prater, beisl and gamebus repo and vault. Change
them everywhere or nowhere. They apply to everything an agent writes: code,
comments, commit messages, docs, notes.

1. Attribution: the only AI trailer is
   `Assisted-By: Claude <model> <noreply@anthropic.com>`, one line, last.
   Never `Co-Authored-By`. Never a `Claude-Session:` line or any session or
   tracking URL. Commits that only package the maintainer's own edits carry no
   AI trailer at all.
2. Plain ASCII punctuation: no em, en or figure dashes, no Unicode minus or
   non-breaking hyphen, no non-breaking spaces, no zero-width or bidi
   characters. Write `-` or `--`, or restructure the sentence. Before
   committing, run the unicode-hygiene skill
   (`python3 ~/.claude/skills/unicode-hygiene/scripts/scan_unicode.py`) on
   what you touched: hidden characters fail the run, dashes are reported.
   Vendored third-party files (SDK headers, LICENSE texts) are left as they are.
3. Comments: concise and load-bearing. Say why, not what. No narration of
   obvious code, no essays, no restating the commit message. Match the
   surrounding density; when in doubt, fewer.

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
- `crates/docs-gen` — packaging tool: renders `docs/*.1.md` into man pages (mandown).
  `docs/*.1.md` are the SINGLE SOURCE for user-facing usage text: each binary embeds
  its file via `include_str!` and prints everything above the `<!-- help-end -->`
  marker as `--help`; `make man` renders the full files for the bundle.
- `crates/beisl-gui` — `beisl-setup` binary (libcosmic): preflight checklist, plus an
  install/update page shown first when run from an extracted dist tree (a tiny fork of
  the SpritzWine installer wizard over `beisl install`). **Excluded** from the main
  workspace: it is its own workspace with its own `Cargo.lock` because libcosmic pulls
  ~48 rev-pinned git deps (wgpu/winit tree). Build it separately, only when touching it.

Dependency direction: `beisl`, `beisl-gui`, and `beisl-mcp` all depend on `preflight` +
`session`; `beisl` and `beisl-gui` also depend on `artifact` (which itself depends on
`symbol-resolver` + `session` - the latter so `TraceArtifact::persist` can name
`artifact.json`/`report.txt` from the one place that owns those constants); nothing
depends on `beisl`.

## Commands (verified)

```sh
cargo check --workspace          # main workspace, fast (excludes beisl-gui)
cargo test --workspace           # inline unit tests (all main-workspace crates)
cargo clippy --workspace
cargo fmt
cargo check -p beisl             # or -p preflight, -p session, ...
make man                         # docs/*.1.md -> man/*.1 (cargo run -p docs-gen)
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
  Run `cargo audit` (cargo-audit is installed) before and after touching a lockfile, and
  read the diff of a `cargo update` rather than committing it blind - `Cargo.lock` pinning
  is what kept us out of the crates.io `arrayref` build-script attack of 2026-08-20, and
  the light-deps rule is why the root workspace was never even exposed (only `beisl-gui`
  pulls `arrayref`, transitively via `tiny-skia`). See the vault note "arrayref
  Supply-Chain Attack 2026-08-20 - Not Affected".
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
