# Beisl — design

> The source of truth for the architecture. Code stubs point here.

## What it is

Beisl wraps a game and produces a **symbolicated CPU trace** of where *wine* spends
its time, with MangoHud's frametime log as a **side-channel that bookmarks the
low-performance peaks**. The point is to answer, with real numbers, questions like
"which wine subsystem is hot enough to be worth rewriting in Rust?" — and to let
creators hand us that data instead of a vague "it feels slow."

A *Beisl* is a Viennese corner pub — the place where everyone loudly holds forth on
what everyone else did wrong. Fitting that this one vets the **wine's** performance.
(Spritz, Prater, Beisl.)

## Two modes — only one is the tool

- **Mode A — "is it smooth":** frametimes, 1%/0.1% lows, stutter. MangoHud already
  does this. We do **not** rebuild it.
- **Mode B — "where does wine burn CPU":** symbol-level `perf` sampling. Nothing
  off-the-shelf does this *for wine*. **This is Beisl.**

MangoHud is not the product; it is a **side-channel**. Its frametime CSV gives
timestamps of hitches, which become **markers into the perf trace** — so instead of
staring at a whole-session flamegraph, you (or Claude, over MCP) jump straight to
"the 5 worst hitches and what wine was doing during each."

## The headline view: culprit attribution

The most valuable thing Beisl tells a non-expert is *who ate the frame* — by **layer**,
not by symbol. The artifact carries the per-DSO breakdown; the GUI's headline turns it
into a layer split the user can feel at a glance:

> kernel · GPU driver · game code + its JIT (Mono/CoreCLR) · DXVK/vkd3d · **wine**

First real capture (Blood West — a robust title picked *because* it runs great): 45%
kernel, 32% the game's own JIT, ~3% DXVK, **wine < 0.5%**. The GUI should make "it's the
game/driver, not wine or DXVK" obvious — a colorful stacked bar, or at minimum an ordered
table. **Confirming a good game is good is a first-class result, not a null one**; the
tool should say so with conviction, so a reviewer trusts it when it *does* finger wine.

## Surfacing it: the ask, then the results view

The engine below produces the data; this is how the **session window**
(`beisl-setup --session <dir>`) turns it into "here's what dragged performance down."
It builds on two things that already exist: the post-run **policy** (the wrapper decides,
per outcome, whether to resolve — see below) and the self-describing **`artifact.json`**
(`artifact::TraceArtifact`: `frame_stats`, `spikes`, `hot_symbols`, `diagnostics`, all
tier-stamped). The window does *no* analysis logic itself — it binds to the serialized
artifact, exactly as the setup page binds to `preflight`.

**Post-run policy (built).** Analyzing every session is presumptuous — a clean run
doesn't warrant the seconds to resolve or the tens of MB kept. So:
- **Abnormal exit** — non-zero code, *or* a GPU hang the watcher saw (launcher chains can
  launder the real exit code to 0, so the hang signal catches the "why did it crash"
  case) — is **always analyzed**; the wrapper writes `artifact.json` + `report.txt` on the
  spot.
- **Clean exit** follows the `clean-run-analysis` preference: **`keep`** (default — the
  wrapper is non-destructive: leave the raw trace in place, resolve nothing, and set
  `session.analysis_offer`), `always` (resolve now), or `discard` (drop `perf.data`).
  The default deliberately does *no* automatic work and throws *nothing* away — the
  decision is the user's, made in the window.

**The ask** (the default GUI surface — window sees `analysis_offer` and no
`artifact.json`): a plain line ("Clean run — not analyzed yet") + two buttons. **Analyze**
runs `TraceArtifact::build` on a background thread (the setup app's worker + `Tick`-poll
pattern - resolve is ~seconds and must never block the event loop), writes the artifact
and report, and flips to the results view. **Discard** deletes the run dir. Because the
wrapper keeps the trace by default, this ask is the normal path for a clean run - the
window is where "analyze or throw away" is decided, not the launch-options wrapper. A
crash never shows the ask: it's already analyzed, so the window opens straight on the
results.
*(2026-07-11)* Stop-&-save mid-game gets the same ask, under a **`Saved`** state whose
caption says the game is still running — a complete trace never masquerades as a
finished session.

**The results view** (an `artifact.json` exists), top to bottom — the plain diagnosis
first, the caveats last, in the layered wording the rest of the UI uses:

1. **Header** — game · wine flavor · fidelity tier, with the `Exports`-tier caveat inline
   so a partial trace is never read as a full one (the same rule that stamps every trace).
2. **Frametime health** — avg fps · 1% low · worst, then the worst **spikes** as bookmarks
   ("221 ms at 7.1s"): where it hitched.
3. **The culprit** — *The headline view* above, rendered: the **layer split**
   (kernel · GPU driver · game/JIT · DXVK/vkd3d · **wine**) as a stacked bar, then the top
   hot symbols within the hot layers. This is the "who ate the frame" answer.
4. **Issues** — the anonymous hot modules tied to their fix (`libvulkan_intel.so` 17% →
   install mesa debuginfo; a wine PE → the `Full` tier), i.e. the *same* signals the
   preflight `driver debug symbols` / `kernel symbols` checks predict, now confirmed
   against the real trace. This is the "indicate about issues" surface.

**The layer classification (built).** The resolver buckets *every* sample by its DSO into a
`Layer` (`kernel · driver · dxvk/vkd3d · wine · game`) — keyed on module *kind*, not build
tree, so it's wine-agnostic (the translation DLLs and driver libs are named the same under any
wine; wine's own builtins are the PEs under a `/lib/wine/` path). It covers the whole trace,
so kernel and *unnamed* samples are still placed by their module even when the function can't
be named. The artifact carries the rollup as a `layers` field and the headline bar reads it
directly; `beisl symbols` renders it as a text bar ("who ate the frame").

Each layer also carries its own **symbol coverage** (`symbol_pct` — the share of *its* samples
we could name), so the bar says not just "driver was 18%" but "…and we could name 98% of it"
vs. a big `game` layer at 22% named. That points the user straight at where fidelity is worth
buying: a large layer with low coverage is where installing that layer's debuginfo, or the
`Full` (retained-DWARF) tier for a wine PE, would actually reveal what's inside — the same
remedy the preflight debuginfo checks predict, now measured against the real trace.

**Coverage, not a fake layer.** Samples perf can't back to any module (`[unknown]`) are a
*measurement gap*, not a stack layer — so they are **not** shown as a peer bucket. Doing that
would both shrink the real layers and dress the gap up as an answer, when in truth it could be
graphics/driver code we merely couldn't place. Instead the layer shares are normalized over
the *attributed* samples, and the gap is disclosed separately as `attributed_pct` ("of 66%
attributed … 34% unmapped — could belong to any layer"). The gap is never redistributed into a
named layer — that would be fabrication.

**Honest gap:** spikes and hot-symbols are shown *side by side*, not correlated — telling
*which* symbols were hot *during* a given spike needs timestamped samples plus
MangoHud-vs-`perf` clock alignment, a later refinement. v0 answers "what was the worst
frametime" and "where did CPU go overall" honestly, if separately.

**Reuse (built, 2026-07-11):** the identical results view opens from the trace library —
the library spawns the session window per run, so an un-analyzed past run lands on the
same ask (analyze-on-demand); A/B compare (two artifacts side by side, tiers labeled)
is v2.

## Wine-agnostic — a hard requirement, not a nice-to-have

A reviewer's whole point is "SpritzWine **vs** stock Proton," so Beisl must run on
both — otherwise you can never capture the baseline half of the comparison. The
design splits along exactly that line:

- **Capture is wine-agnostic by construction.** The wrapper + MangoHud + `perf`
  sample *whatever process runs*: SpritzWine/Prater, stock Proton, GE, Lutris-wine,
  system wine, or a native Linux game. Nothing in the capture path touches ours.
- **Symbolication is tiered, and the generic tier is the default.** The universal
  mechanism needs zero cooperation from wine:
  `/proc/<pid>/maps` (module bases, the kernel's view) ⋈ each module's on-disk
  **export table**. SpritzWine merely *upgrades* the fidelity on top.

| Tier | Wine | Mechanism | Fidelity |
|------|------|-----------|----------|
| `Full` | SpritzWine / Prater | our build + **retained** debug (PDB/DWARF — see *Symbols delivery*) | internal statics, not just exports |
| `Exports` | stock Proton / GE (stripped) | `/proc/maps` ⋈ export tables | exported fns only — degraded but comparable |
| `Distro` | system wine | installed debuginfo packages | often good |

**Rule:** every trace is stamped with its tier (see `artifact::TraceMeta`), so an A/B
is never silently comparing full symbols against partial ones.

### Symbols delivery — the `-devel` / debuginfo model

The `Full` tier needs the PE debug info our Prater build currently strips **and
discards** (`file` says "stripped to external PDB"; the PDB is thrown away). Retaining
it is easy — our *main* SpritzWine build already keeps it (`ntdll.dll` 3.2 MB there vs
Prater's stripped 964 KB) — but the **full set is not small**: the stripped PE tree is
~440 MB (both arches); with symbols it roughly triples to **~1 GB+**. So we do NOT
bundle symbols; we follow the distro split-debuginfo pattern (`*-debuginfo` /
`*-dbgsym` / debuginfod):

1. **Build change:** add `--build-id` to the PE link — today perf reports a *blank*
   build-id for our PE DLLs, which breaks both binary-matching and keyed lookup — and
   collect the PDBs/DWARF into a build-id-keyed `debug/.build-id/ab/cd….debug` subtree
   instead of discarding them.
2. **Default ship = stripped runtime** + a tiny **symbol manifest** always present in
   the prefix (build version + commit + per-module build-ids + a fetch base URL). A few
   KB, and it's what guarantees symbols match the *exact* binary that was mapped — plus
   it's the persisted "where to fetch" reference even when the store is offline.
3. **Symbols = a separate `-devel` artifact** (release asset / package / debuginfod
   endpoint) carrying that build-id tree.
4. **Beisl fetches on demand, per-module, cached:** for each unsymbolicated module it
   reads the build-id, looks it up in the manifest, pulls just that `.debug` (a trace
   touches ~5–15 wine modules = tens of MB, not the whole GB), caches it, and lets
   `llvm-symbolizer` resolve. `debuginfod-find` + llvm-symbolizer already do this
   natively; the host even has `DEBUGINFOD_URLS` set, which covers the ELF side for free.

**PE wrinkle:** debuginfod is ELF/build-id native, so the unix `.so` side is free; PE
DLLs need the `--build-id` above (or keying by the CodeView PDB GUID) for the same
build-id-keyed lookup to work uniformly.

## Architecture

```
crates/beisl            wrapper bin (`beisl %command%`):
   src/run.rs             orchestrate: env -> spawn game -> manage perf -> post-run policy
   src/mangohud.rs        drive MangoHud (frametime CSV + toggle hotkey); compose, don't vendor
   src/perf.rs            perf recorder (system-wide; cgroup scoping still to come)
   src/cue.rs             audible feedback (v0 stand-in for the in-game indicator)
   src/symbols.rs         `beisl symbols <run-dir>` — headless face of the analysis
crates/symbol-resolver  perf.data -> wine symbols (`Exports` tier built; `Full` to come)
crates/artifact         frametimes ⋈ symbols + diagnostics -> artifact.json + report.txt
--- v1 (built, 2026-07-11) ---
crates/beisl-gui        libcosmic `beisl-setup`: preflight checklist + session window
                        (the ask + results view) + trace library (the product surface)
crates/beisl-mcp        `beisl-mcp` — read-only MCP server over the trace library
                        (see *The MCP server* below)
```

### The MCP server (2026-07-11)

The flywheel needs an AI that can *read* the trace, so v1 ships `beisl-mcp` — and its
design is mostly a list of refusals:

- **A separate, opt-in binary.** The wrapper stays a wrapper for everyone who never
  uses this. Nothing runs — or listens anywhere; the transport is the process's own
  stdio, no ports, no sockets — until the user wires `beisl-mcp` into their AI
  client's config. Inert by default is our consent stance, applied to AI access.
- **Hand-rolled JSON-RPC 2.0 on `serde_json`, not the official SDK.** The SDK's async
  runtime exists to schedule many concurrent sockets; this server has exactly one
  pipe, one client, and microsecond file reads — all weight, no lift. The tipping
  point is server-push notifications (which is also where rmcp starts earning its
  keep): the day we want to *tell* the client "a run just finished" instead of being
  asked, we revisit.
- **Six tools, all read-only:** `list_traces`, `get_setup_status` (the preflight
  checklist as advice — fix commands come back as text, never executed here),
  `get_artifact`, `get_report`, `get_log` (tail-capped: launcher chains spam
  megabytes and the useful lines cluster at the tail), and `get_crash_summary`
  (pre-chewed forensics — see the hotkey-crash case study it automates). **Delete
  and analyze deliberately have no MCP surface**: destroying a user's trace, or
  spending seconds of CPU on it, stays a human click in the GUI/CLI.
- **Next, per the vetted user conversations:** a poll-shaped `list_events(since)`
  first, push maybe after. Spike slicing (`query_spike` — "which symbols were hot
  *during* this hitch") is the real differentiator, but it needs time-bucketed
  samples in the artifact first; it's awaiting a design pass, not a weekend.

### perf scoping

The game is deep in a process tree (`proton -> wine -> game`). To capture the tree
and nothing else, put the spawned child in a fresh **cgroup v2** and
`perf record --cgroup <cg> -g`. System-wide `-a` + filter is the fallback.

### The gate — verified (2026-07-03)

Everything downstream rested on one question: **can `perf` resolve wine's symbols from a
real game inside the Steam `pressure-vessel` container?** First capture — Blood West under
our Prater, via Heroic — answered it:

- **`perf` sees every module** and attributes samples with stable offsets, from the host,
  system-wide (`-a` sidesteps the container's PID namespace). PMU-interrupt sampling, not
  ptrace, so anti-tamper titles tolerate it.
- **ELF (`ntdll.so`) symbolicates natively.** ✓
- **PE (`ntdll.dll`, …) gives module + raw offset only** — perf's symbolizer is ELF-only.
  The `Exports` tier fixes this (`/proc/maps` ⋈ export table), proven live:
  `ntdll.dll+0x593e0 → RtlUnwindEx`. It misattributes internal statics to the nearest
  export (giveaway offsets like `+0x998`), which is exactly why `Full` needs retained
  debug info.

So the engine works; the fidelity ceiling is a build-config change (retain the PDBs +
`--build-id`, see *Symbols delivery*). Frame pointers (`-fno-omit-frame-pointer`; Rust
`-C force-frame-pointers=yes`) still matter for cheap call-graph unwinding. Capturing
needed `perf_event_paranoid` lowered — handled by *Onboarding* below.

## Feedback: audio cue in v0

Drawing into a running game = becoming a Vulkan/GL layer = reimplementing MangoHud.
We refuse that. So v0 confirms "your hotkey worked / you are recording" with a short
**audible blip** (distinct start/stop tones) — audible even over a fullscreen game.
v2 nicety: piggyback MangoHud's own logging indicator in its overlay, and/or a
libnotify toast on start/stop. Never our own in-game overlay.

## Onboarding / preflight (the setup page)

For a non-technical user the first screen is a **preflight checklist** — read-only
checkboxes that show what Beisl needs, what it's doing right now, and why, and drive any
fixes. The check *logic* lives in a UI-agnostic `preflight` crate, so the libcosmic GUI, a
headless `beisl --check`, and the installer's control panel all share it; the GUI is a
thin view over its state.

**Checks — each is detect → status → optional fix:**

| Check | Detect | Fix (privileged via `pkexec`) | Blocking? |
|-------|--------|-------------------------------|-----------|
| `perf` present | `which perf` | install distro pkg (`perf` / `linux-perf`) | yes |
| `llvm-symbolizer` | `which llvm-symbolizer` | install `llvm` | yes (for symbols) |
| perf can record | CAP_PERFMON on perf **or** `perf_event_paranoid ≤ 0` | `setcap cap_perfmon+ep perf` (ELF perf) → else `sysctl …=-1` | yes |
| MangoHud present | `which mangohud` | install `mangohud` | no (optional side-channel) |
| disk headroom | `df` on the trace dir (≥ 5 GiB) | free space / pick a roomier folder (advisory) | warn |

*This table predates the kernel-symbols, GPU dump-access, multi-GPU, and driver
debug-symbols rows — `preflight` is the current roster.*

**Status model:** `Pending → Checking → Ok | NeedsFix → Fixing → Ok | Failed`. Fixes that
need root run through **`pkexec`** (a graphical PolicyKit prompt — no terminal password
entry), wrapped in a progress bar. Package names are distro-mapped (apt / dnf / pacman /
zypper).

**Perf access — prefer CAP_PERFMON, sysctl as fallback.** Rather than globally lowering
`perf_event_paranoid`, we grant the perf binary **CAP_PERFMON** (`setcap cap_perfmon+ep`):
surgical (no system-wide weakening), output stays user-owned, no per-capture prompt. It
sticks where `perf` is a real ELF binary (Fedora/Arch/SUSE); on **Debian's `perf` wrapper
script** it doesn't, so we fall back to the sysctl there. One caveat: a `perf` package
update clears the cap — harmless, since preflight re-detects it every run. (Detection is
`getcap` **or** the sysctl value, so either remedy satisfies the check.)

**Disk, before and during.** Traces default to the roomiest writable location; the
preflight warns below ~5 GiB, and the recorder samples free space *while capturing* and
stops cleanly before it can fill the disk — call-graph captures grow by the second.

**Entertain the wait.** Installs and first-launch bootstrap are slow, so the setup page
spends that dead time *teaching* — a rotating panel bound to the current step:

- what & why: "Installing `perf` — your distro didn't ship it; we use it to sample the CPU."
- how to use it: "Press **&lt;F-key&gt;** to start/stop a trace."
- where it lands: "Your trace will be saved to `~/.local/share/beisl/traces/…`."
- what's launching: "Launching **&lt;game&gt;** under **&lt;wine flavor&gt;**."

Each check carries a `why` string for exactly this. Turning a tedious wait into onboarding
is a feature, not filler.

## Packaging & entry points

Beisl is a **separate project**, but **bundled with SpritzWine and on by default** —
a 10 MB toolkit next to a 100 GB game is free, and having it at hand beats the bytes.
Entry points, easy → portable:

| Context | How the user starts a trace |
|---------|-----------------------------|
| SpritzWine / Prater | a **"Record performance" toggle** in the libcosmic control panel — wires Beisl into the Proton script for the next launch. No launch-option injection. |
| Steam + stock Proton/GE | `beisl %command%` in the game's Launch Options |
| Lutris | `beisl` as a pre-launch / wrapper command |
| bare wine | `beisl wine game.exe` |

Owning the compat tool is what lets our own users skip the scary launch-option step
entirely; the wrapper is the universal fallback for everyone else.

### Flatpak plan (2026-07-11) — reaching the Flatpak Lutris/Steam userbase

Investigated against a real Flatpak Lutris install. The findings:

- **Lutris declares no add-on extension point.** Steam's "GE-Proton in Discover"
  works because Valve's manifest declares `com.valvesoftware.Steam.CompatibilityTool`
  / `.Utility`; `net.lutris.Lutris` has nothing equivalent, so a first-class
  `net.lutris.Lutris.beisl` add-on needs an upstream manifest PR first.
- **The proven side door is a freedesktop-runtime extension** à la
  `org.freedesktop.Platform.VulkanLayer.gamescope` / `.MangoHud`: both ship *binaries*
  (not just layers), get mounted at `/usr/lib/extensions/vulkan/<name>/` inside every
  runtime-based app, show up in Discover as add-ons, and Lutris already wires their
  `bin/` dirs into `PATH`. Shipping `…VulkanLayer.beisl` the same way works with
  today's Lutris *and* Steam flatpaks; the user points Lutris's command prefix (or
  Steam's launch options) at the mounted binary.
- **The real blockers are perf's, not packaging's:**
  - Flatpak's seccomp filter blocks `perf_event_open` unless the app sandbox has
    `--allow=devel`. The inspected Lutris install has `features=devel` set (whether
    stock or a Flatseal override — verify before relying on it; if not stock it's a
    documented one-liner: `flatpak override --allow=devel net.lutris.Lutris`).
  - **File capabilities are dead inside the sandbox** (bubblewrap sets no-new-privs),
    so the `cap_perfmon`-on-perf-binary model preflight checks today cannot work
    there. In-sandbox recording needs host `kernel.perf_event_paranoid` lowered via
    sysctl (fixable through `flatpak-spawn --host pkexec …` — the session-bus talk
    permission for `org.freedesktop.Flatpak` exists on the inspected install), or a
    fallback to child-tree-only recording (works at paranoid ≤ 2, loses kernel
    samples and the system-wide view). This child-tree fallback is the
    implemented **degraded mode** (`perf record -- <game>`, perf as the game's
    parent); its one blind spot is a launcher that double-forks/reparents the
    game *out* of perf's subtree — Full `-a` is immune to that escape, degraded
    is not, so a real Proton/umu title on a paranoid ≤ 2 box is the check that
    matters before trusting it.
  - The runtime ships no `perf`; the extension must bundle it.
  - Alternative worth prototyping first: **keep perf on the host entirely** —
    beisl-in-sandbox only orchestrates (env, MangoHud, session.json) and starts the
    recorder on the host via `flatpak-spawn --host`, where cap_perfmon keeps working.
    Simpler privilege story, one binary in two contexts.
- **The file-based `session.json` bridge survives the sandbox boundary for free** —
  a host-side GUI/monitor watches the trace dir; no IPC needed. This is now a design
  constraint to preserve.
- Sandbox awareness = detect `/.flatpak-info`, then: preflight switches to
  sandbox-mode checks (paranoid sysctl instead of cap_perfmon, fixes via
  `flatpak-spawn --host`), and symbol resolution must look under `/run/host` for
  host libraries.

#### Agreed direction (2026-10-10): a Flatpak that sets up the host, plus the extension

For a later feature branch; nothing here is built. Research behind it is in the
vault (`Tasks/Flatpak - host setup app plus runtime extension`).

What was ruled out first: the Flatpak as the launch wrapper. A host shim calling
`flatpak run` starts the game inside beisl's sandbox, and the only way out,
`flatpak-spawn --host`, hands the game the session helper's environment instead
of the caller's and makes it a child of that helper rather than of the launch
option. That breaks the unchanged-environment rule and takes the game out of
Steam's process tree. The AppImage's "shim execs the package" does not transfer.

What was agreed instead is two pieces that work together:

- **`org.spritzwine.Beisl`, the Flatpak app, is the informed installer and the
  GUI.** It carries the setup page, the trace library, the compat window and the
  MCP server. It does not wrap games. What it does for wrapping is explain what
  has to exist on the real filesystem - the `beisl` wrapper itself in
  `~/.local/bin`, and a host that passes the perf checks - and then put it there
  and verify it, step by step, as the user confirms each one. Opt-in throughout:
  the same stance as the AppImage's setup page, and the reason dropping a binary
  onto the host is acceptable here when unpacking the AppImage was not. Here it
  is the only way a sandboxed app can give a host launcher something to run, and
  it happens in front of the user rather than behind an install command.
- **`org.freedesktop.Platform.VulkanLayer.beisl`, the runtime extension, covers
  launchers that are themselves Flatpaks** (Steam, Lutris, Heroic). Their launch
  options run inside their own sandbox, where neither a host `~/.local/bin` nor
  `flatpak run` exists, so beisl has to be mounted in (see the findings above).
  Recording there is the degraded child-tree mode.
- **Interoperable by construction.** Both write the same trace directory and the
  same `session.json`, which already survives the sandbox boundary, so a trace
  recorded by the extension inside Flatpak Steam opens in the Flatpak GUI, and so
  does one recorded by the host wrapper. The app should also recognise the other
  two: tell the user when a Flatpak launcher is installed and the extension would
  serve it, and when a host `beisl` is already present from a distro package or
  an AppImage and nothing needs installing.

Constraints the branch will have to respect:

- **The binary placed on the host must be the portable build.** One compiled
  inside the Flatpak SDK links the runtime's glibc, which can be newer than the
  host's. It needs the same older-glibc build the AppImage and tarball need.
- **Writing to `~/.local/bin` needs a way in**: a narrow `--filesystem` grant, or
  the file-chooser portal so the user picks the folder and no static permission
  is needed. Host checks and fixes need `--talk-name=org.freedesktop.Flatpak`,
  which Flathub grants only with an explanation. "Inspect and fix the host's perf
  setup, on request" is a far better case than a wrapper that escapes at once.
- **Traces must live where the host wrapper writes them**
  (`--filesystem=xdg-data/beisl:create`), not under `~/.var/app/...`.
- **Shared with the AppImage work**: the record of what setup wrote and the
  page that names and removes it, and the rule that a missing piece never stops
  a game.

### AppImage (2026-07-11) — low-hanging fruit for single-file distribution

Unlike Flatpak, AppImage is a natural fit: no sandbox means the wrapper semantics
survive untouched — Steam launch options see the real process tree, host `perf`,
`pkexec` and MangoHud work as-is, traces land in the real `$HOME`. Findings from the
dist work:

- **Almost nothing to bundle.** `beisl` links only glibc; `beisl-setup` dlopens its
  display/Vulkan libs (winit/wgpu style) from the host — which is standard AppImage
  practice anyway. `linuxdeploy` can consume the `make package` dist tree directly.
- **Chores, not blockers:** build against an older glibc baseline (CI image) for
  portability; AppImage has a single entrypoint, so an `AppRun` dispatch is needed
  (`Beisl.AppImage setup` → GUI, or match `$ARGV0`; everything else → the wrapper);
  `beisl install` should learn to copy the AppImage itself + a symlink instead of
  the raw binary.

#### Agreed design (2026-10-10): the AppImage stays the app, shims point at it

Decided against the obvious alternative of having the AppImage unpack its
binaries into `~/.local/bin`. That would use the format as a self-extracting
archive, which is what the tarball's `beisl install` already is, and would throw
away the one thing an AppImage is: a single file that is the application.

- **One file, wherever the user keeps it.** The AppImage is not copied or moved.
  Setting up from it writes four small shim scripts,
  `~/.local/bin/{beisl,beisl-setup,beisl-compat,beisl-mcp}`, each carrying the
  AppImage's ACTUAL path (the runtime's `$APPIMAGE`) and exec-ing it with the
  tool's name. The desktop entries point at the shims. Owner's correction to an
  earlier draft that parked the file at a fixed location: where the file lives is
  the user's business, and the shims are told, not the other way round.
- **Setup rechecks the path every time it runs from an AppImage.** If the shims
  name a different path than the one it was just started from - the file was
  moved, renamed, or replaced by a newer download elsewhere - the setup page says
  so and re-points them. A path that is still correct is confirmed, not rewritten.
- **`AppRun` dispatches** on the invoked name first (the shims and any symlink),
  then on a leading `setup`/`compat`/`mcp` word, and hands everything else to the
  wrapper. Started with no arguments, as from a double-click, it opens
  `beisl-setup`.
- **A missing AppImage must never stop a game.** Between two runs of setup the
  recorded path can go stale, so the shim has to cope on its own. The `beisl`
  shim, like the
  existing `/usr/bin/beisl` Steam shim it sits underneath, says on stderr that
  its target is gone and then runs the wrapped command untraced. The GUI shims
  fail with the expected path. Shims do NOT delete themselves: a script that
  removes itself in the middle of a game launch is a surprise, and it would leave
  the desktop entries and icons behind anyway.
- **The setup page says so.** When `beisl-setup` runs from an AppImage it tells
  the user that it is, what installing will write and where, and offers a way to
  remove all of it again. Owner's condition for the shim design: nothing lands on
  the system that the application has not named, and everything it names can be
  taken back from the same page.
- **The game's environment stays the game's.** The AppImage runtime exports
  `APPDIR`, `APPIMAGE`, `ARGV0` and `OWD`; the wrapper removes them before it
  spawns anything, and `AppRun` sets neither `PATH` nor `LD_LIBRARY_PATH`.
- **Nothing is bundled**, `appimagetool` is used directly, and `make appimage` is
  a shim over `make install DESTDIR=AppDir PREFIX=/usr` like every other recipe.
- **Portability is the build's doing, not the format's.** The binaries must be
  built against an older glibc (a Debian 12 class image, glibc 2.36); a build on
  the development machine needs 2.43. The tarball needs the same build to be
  portable, so the AppImage adds a double-clickable file and catalogue reach on
  top of it, not a technical gain.

Known code gaps it will need: `preflight::find_tool` looks on `PATH` and in
`~/.local/bin` but not beside the running executable, so a GUI inside the mount
would not find its sibling tools without the shims; and `beisl install` looks for
man pages next to the binary, which is not where an AppDir keeps them.

To be tested on hardware before it is called done: whether the runtime works
with only `fusermount3` present (the classic one needs libfuse2, which current
Fedora and Ubuntu do not install), a full Steam launch through the shim with the
mount living as long as the game, and what happens to the mount when the game is
killed.

## Case study: the Saints Row GPU hang (2026-07-05)

What a wrong-layer diagnosis costs, measured on our own kitchen table. Saints Row
(2022) with ray tracing on an Arc A770 (anv, xe KMD) produced hard GPU hangs — xe
"Timedout job", devcoredump, engine reset. Everything about it *looked* like a Mesa
bug: a fresh anv BVH-encoder commit was a plausible suspect, a real encoding
inconsistency was even found nearby, and a full afternoon went into commit
bisection (revert + surgical patch, both rebuilt and installed), decoding three
devcoredumps with `aubinator_error_decode` down to the exact stalled `PIPE_CONTROL`
and the spinning BVH-build kernels, and a Fedora-Mesa vs git-Mesa A/B. Result:
*every* driver hung identically. The next discriminator took two minutes:
relaunching on the **same driver** under a different runtime (Proton-CachyOS
instead of Prater) — clean. For the rest of the afternoon the culprit looked like
our layer, not the driver's — until reboot-disciplined retesting overturned that
verdict too: the "clean" runtime had been session luck on a GPU whose state was
degraded by the day's accumulated engine resets, and once every configuration was
retested from a fresh boot, **everything** hung with the identical signature — on
both the xe and i915 kernel drivers, under Valve's wine and ours, with every sync
mechanism. Final verdict: a driver-class bug after all (anv's GRL/BVH build
spinning on the game's raced per-frame skinning data), reported upstream as a
comment on mesa#11557 with the cross-KMD dump pair.

Lessons, each of which should become a Beisl behavior:

- **The layer-swap A/B is the killer diagnosis move.** "Same game, same driver,
  different runtime" (and its inverse) settled in minutes what forensics couldn't in
  hours. The v2 A/B view should treat *correctness outcomes* (hang / no hang, device
  loss) as first-class results next to frametimes, with the swapped layer stamped in
  the artifact.
- **GPU hangs need capture discipline.** The xe driver retains exactly **one**
  devcoredump; while it sits unread, new hangs record nothing. The wrapper should
  watch dmesg/devcoredump during a session and auto-capture + clear + attach dumps
  to the artifact (v0/v1 material).
- **Preflight must catch environment traps.** Two Mesa installs (system + /usr/local)
  expose the same GPU as two Vulkan devices — which driver a game gets is enumeration
  roulette that invalidates any A/B. Detect dual ICDs, warn, and offer
  `VK_DRIVER_FILES` pinning in the setup page.
- **Record identity, not just samples.** The hung process was only visible as the
  kernel's 15-char comm `-game_ignores_f…` (the game's own thread name — the engine
  admitting it ignores fences). Artifacts should carry full cmdline, runtime
  name/version, and vkd3d-proton/DXVK versions so attribution never rests on a
  truncated string.
- **One hang per boot.** Repeated GPU engine resets can leave the driver/GPU in a
  degraded state that silently flips subsequent test outcomes — half a day of A/B
  attributions were voided by exactly this. Beisl should stamp "GPU resets since
  boot" into every artifact and flag any A/B comparison recorded on a dirty state.

Resolution (2026-07-05, same day): the SpritzWine-side "open item" dissolved — with
reboot discipline and a cross-KMD retest (xe *and* i915, identical stalled-
`PIPE_CONTROL` signature), the hang proved wine-independent and kernel-driver-
independent: an anv GRL/BVH class bug (mesa#11557 cluster), triggered by the game's
raced per-frame skinning uploads, unfixable game-side (Volition is gone). Filed
upstream as a comment on mesa#11557 with the dump pair; evidence archived at
`/media/Data/xe-dump/` (xe devcoredumps + i915 error state + Mesa issue draft).

## Case study: our own layers are suspects too (2026-07-11)

Two incidents from the trace-library review session (The Settlers), both with the same
moral: the tooling between us and the game can be the culprit.

- **The MangoHud hotkey crash.** Stopping the frametime log via Shift+F2 with the HUD
  visible aborted the game — an ImGui "cannot have an empty ID" assert *inside the
  MangoHud layer* (upstream #2053; 0.8.2 affected). The measurement side-channel
  crashed the thing it was measuring, and the whole story was one assert line buried
  under hundreds of lines of loader spam in `game.log`. That digging is now a tool:
  `get_crash_summary` automates exactly the forensics that diagnosed this — the
  wrapper's verdict lines, the session's warnings, and the error-shaped lines from
  the log tail.
- **Version skew froze the window.** A fresh wrapper spawned the *installed*
  `beisl-setup` from PATH — an older build that didn't know the new `Saved` state, so
  the window sat frozen at "Recording" while the trace was long saved. `spawn_monitor`
  now prefers the sibling binary next to the wrapper (same build by construction —
  one bundle, one dist tree), PATH only as fallback: skew-proofing beats
  install-preferring.

## The flywheel (why it's worth building)

A creator who hits a stutter exports a **symbolicated, spike-annotated trace** and
hands it to us. Over MCP that is directly actionable: Claude reads it, names the hot
wine symbol during the worst hitches, and it feeds straight into the Rustify-the-
hot-path roadmap. Creators generate exactly the profiling data the roadmap needs.

## Roadmap

- **v0 (engine):** wrapper + MangoHud env + cgroup-scoped perf + symbol-resolver +
  raw artifact export + audio cue. **Gate cleared (2026-07-11):** the `Exports` tier is
  built (`perf script` ⋈ PE export tables, proven on real captures), `artifact` merges
  frametimes + symbols + diagnostics to `artifact.json`/`report.txt`, and the wrapper's
  post-run **policy** (crash → analyze; clean → offer/always/discard) drives it. `Full`
  tier (retained-DWARF build) and cgroup scoping still open.
- **v1 (product):** libcosmic GUI + MCP server. **Shipped (2026-07-12):** the GUI's
  centrepiece, the **session-window analysis UI** (see *Surfacing it*), is built — the
  Analyze/Discard **ask** for a kept clean run (and for a mid-game `Saved` stop), then
  the **results view**: the layer-split culprit bar + frametime chart, both with hover
  readouts, + issues, all read from `artifact.json`. The **trace library** (list / open /
  analyze-on-demand / export / delete) reuses that exact view by spawning the session
  window per run. And the **MCP server** shipped as its own `beisl-mcp` binary (see *The
  MCP server*) — six read-only tools, not a stub. Still open from this milestone: the
  installer's "Record performance" toggle (it lives in the SpritzWine control panel, not
  this repo) — plus the v0 debts, the `Full`/DWARF tier and cgroup scoping.
- **v2 (polish):** in-game indicator via MangoHud overlay / libnotify; flamegraph;
  A/B compare view (SpritzWine vs Proton, side by side, tiers labeled).
- **Case-study-driven additions (2026-07-05, see above):** devcoredump watcher in the
  wrapper (**built 2026-07-11** — captures + clears dumps live, policy-gated for the
  root-only case); dual-ICD detection + `VK_DRIVER_FILES` pinning in preflight
  (**built 2026-07-11** — multi-GPU check + a GPU pin picker in the GUI);
  hang/device-loss as first-class A/B outcomes (v2).
- **Flatpak (2026-07-11, see Packaging):** prototype the host-side-perf split via
  `flatpak-spawn --host` first, then a `…VulkanLayer.beisl` runtime extension (MVP,
  covers Flatpak Lutris *and* Steam); upstream a Lutris extension point later for the
  first-class Discover add-on story.
- **AppImage (2026-07-11, see Packaging):** low-hanging fruit — unsandboxed, so the
  wrapper works unchanged; both binaries are host-lib-light. `AppRun` dispatch +
  older-glibc build baseline on top of `make package`, that's it.
