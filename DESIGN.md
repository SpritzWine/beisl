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
   src/run.rs             orchestrate: env -> spawn game -> manage perf -> (artifact)
   src/mangohud.rs        drive MangoHud (frametime CSV + toggle hotkey); compose, don't vendor
   src/perf.rs            perf recorder, cgroup-scoped to the game tree
   src/cue.rs             audible feedback (v0 stand-in for the in-game indicator)
crates/symbol-resolver  perf.data addresses -> wine symbols (the tiers above)   ← THE GATE
crates/artifact         frametime side-channel ⋈ resolved samples -> one export file
--- v1 ---
crates/beisl-gui        libcosmic control panel + results view (the product surface)
crates/beisl-mcp        serve the artifact to Claude
```

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
    samples and the system-wide view).
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

## The flywheel (why it's worth building)

A creator who hits a stutter exports a **symbolicated, spike-annotated trace** and
hands it to us. Over MCP that is directly actionable: Claude reads it, names the hot
wine symbol during the worst hitches, and it feeds straight into the Rustify-the-
hot-path roadmap. Creators generate exactly the profiling data the roadmap needs.

## Roadmap

- **v0 (engine):** wrapper + MangoHud env + cgroup-scoped perf + symbol-resolver +
  raw artifact export + audio cue. Gated on the symbolication spike. No GUI/MCP yet.
- **v1 (product):** libcosmic GUI (trace library: list / open / export / delete;
  setup helper; spike → hot-symbols view) + MCP server. Installer integration + the
  "Record performance" toggle.
- **v2 (polish):** in-game indicator via MangoHud overlay / libnotify; flamegraph;
  A/B compare view (SpritzWine vs Proton, side by side, tiers labeled).
- **Case-study-driven additions (2026-07-05, see above):** devcoredump watcher in the
  wrapper (v0/v1); dual-ICD detection + `VK_DRIVER_FILES` pinning in preflight (v1);
  hang/device-loss as first-class A/B outcomes (v2).
- **Flatpak (2026-07-11, see Packaging):** prototype the host-side-perf split via
  `flatpak-spawn --host` first, then a `…VulkanLayer.beisl` runtime extension (MVP,
  covers Flatpak Lutris *and* Steam); upstream a Lutris extension point later for the
  first-class Discover add-on story.
- **AppImage (2026-07-11, see Packaging):** low-hanging fruit — unsandboxed, so the
  wrapper works unchanged; both binaries are host-lib-light. `AppRun` dispatch +
  older-glibc build baseline on top of `make package`, that's it.
