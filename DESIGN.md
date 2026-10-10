# Beisl - design

> The design as it stands. Code comments point here by section name.
> This file describes what is true now and why; how it got here is in its git
> history (the decision-log form ends with the commit before this rewrite).

## What it is

Beisl wraps a game launch and produces a **symbolicated CPU trace** of where the
time went, with MangoHud's frametime log as a **side-channel that bookmarks the
slow moments**. It answers, with numbers, "what made this stutter: the game, Wine,
the translation layer, the driver or the kernel?" - and lets a player hand over
that data instead of "it feels slow".

A *Beisl* is a Viennese corner pub, the place where everyone loudly holds forth
on what everyone else did wrong. Fitting for the thing that vets the **wine's**
performance. (Spritz, Prater, Beisl.)

Frametimes, 1% lows and stutter graphs are MangoHud's job and are not rebuilt
here. Symbol-level `perf` sampling *of a game running under Wine* is what nothing
off the shelf does, and that is Beisl. MangoHud is not the product; it is the
side-channel whose timestamps turn a whole-session profile into "the worst
hitches, and where the CPU went".

## Rules the design does not bend

Each of these has decided an argument at least once.

- **Wine-agnostic.** The point is "SpritzWine *versus* stock Proton", so capture
  touches nothing of ours: the wrapper, MangoHud and `perf` sample whatever
  process runs - SpritzWine, Prater, Proton, GE, Lutris wine, system wine, or a
  native game. Our own builds only raise the symbol fidelity on top.
- **The game's environment is the game's.** Beisl reads the environment and
  hands it on. It sets what its own side-channel needs (MangoHud's variables, an
  optional GPU pin) and nothing else: no `LD_LIBRARY_PATH`, no `PROTON_LOG`, no
  variables describing how Beisl itself was packaged. Where the user has turned
  a log on, Beisl reads it; it never turns one on.
- **A tracer that cannot trace must never stop a game.** No perf, no permission,
  a missing AppImage behind a launcher, a wall in the game itself: the game still
  starts, untraced if need be, and Beisl says why.
- **Nothing is done for the user that the user did not ask for.** No report is
  submitted, no trace deleted or analyzed over MCP, no privileged change folded
  into a batch. Privileged fixes are described by library code and executed by
  the caller through `pkexec`, each on its own click where they are standing
  changes.
- **Say how good the answer is.** Every trace is stamped with its symbol tier and
  record mode; samples that could not be placed are disclosed as a gap, never
  spread over the layers; a clean run of a healthy game is reported as a result,
  with conviction, so the tool is believed when it does blame Wine.
- **No overlay of our own.** Drawing into a running game means becoming a Vulkan
  layer, which is reimplementing MangoHud. Feedback during a run is an audio cue
  and the session window.

## A trace

`beisl <command>` (in Steam: `beisl %command%`) creates
`~/.local/share/beisl/traces/run-<unix>/` and fills it:

| File | What |
|---|---|
| `session.json` | live state for the session window: game, Steam appid, state, warnings, record mode |
| `beisl.log` | the wrapper's own timestamped account |
| `game.log` | the game's stdout and stderr, teed |
| `perf.data`, `perf.log` | the samples, and perf's own output |
| MangoHud CSV | frametimes, flushed per frame so they survive a crash |
| `proton.log` | the user's own Proton log, preserved, when they had one on |
| `artifact.json`, `report.txt` | the analysis, once made |

**Record modes** (`preflight::RecordMode`), detected per run and stamped into the
trace:

- **Full** - `perf record -F 499 -a`: system-wide, kernel included. Needs
  `CAP_PERFMON` on the perf binary or `perf_event_paranoid <= 0`.
- **Degraded** - `perf record -e cycles:u -F 499 -- <game>`: the game's own
  process tree, userspace only, no privilege needed at the common
  `perf_event_paranoid <= 2`. Keeps the game, translation, Wine and
  userspace-driver layers; loses the kernel bar. Blind to a launcher that
  reparents the game out of perf's subtree.
- **None** - no perf, or paranoia too high. The game runs untraced, frametimes
  only.

**After the run** the wrapper decides whether to analyze. An abnormal exit - a
non-zero code, or a GPU hang the watcher saw, since launcher chains launder exit
codes to 0 - is always analyzed on the spot. A clean exit follows the
`clean-run-analysis` setting: `keep` (default: leave the raw trace, analyze
nothing, offer it in the window), `always`, or `discard`. The default does no
work and throws nothing away; the decision is the user's.

**Who the game is** is resolved from the strongest source available, and only a
stronger one replaces a name: launcher argv, then MangoHud's CSV, then Steam's
own manifest and environment at spawn (`steam.rs`), then gamebus presence.

## The answer: who ate the frame

The most valuable thing Beisl tells a non-expert is *who ate the frame*, by
**layer** rather than by symbol:

> kernel - GPU driver - DXVK/vkd3d - wine - the game and its JIT

The resolver buckets every sample by its module into a `Layer`, keyed on the
module's *kind* (translation DLLs and driver libraries are named the same under
any Wine; Wine's builtins are the PEs under a `/lib/wine/` path), so the split is
Wine-agnostic and covers samples whose function cannot be named. The artifact
carries it as `layers`; the session window draws it as a stacked bar and
`beisl symbols` as a text bar.

Each layer carries its own **symbol coverage**: not just "driver was 18%" but
"and 98% of it could be named". A large layer with low coverage is where buying
fidelity pays - that layer's debuginfo, or the `Full` tier for a Wine PE.

**Coverage is not a layer.** Samples perf cannot back to any module are a
measurement gap. They are not shown as a peer bucket and never redistributed:
shares are normalized over the attributed samples, and the gap is stated
separately as `attributed_pct`.

**What it does not do yet:** spikes and hot symbols are shown side by side, not
correlated. "Which symbols were hot during this hitch" needs time-bucketed
samples and MangoHud-to-perf clock alignment.

## Symbols

Symbolication is tiered, and the tier that needs no cooperation from Wine is the
default: `/proc/<pid>/maps` for module bases, joined with each module's on-disk
**export table**.

| Tier | Wine | Mechanism | Fidelity |
|---|---|---|---|
| `Full` | SpritzWine / Prater | our build with **retained** debug info | internal statics - *not built yet* |
| `Exports` | stock Proton / GE (stripped) | maps joined with export tables | exported functions only; comparable |
| `Distro` | system wine | installed debuginfo packages | often good |

Every trace carries its tier, so an A/B never silently compares full symbols with
partial ones.

### The gate

Everything rests on one question: can `perf` resolve Wine's symbols from a real
game inside Steam's pressure-vessel container? Proven on real captures:

- `perf` sees every module and attributes samples with stable offsets, from the
  host, system-wide. It samples by PMU interrupt, not ptrace, so anti-tamper
  titles tolerate it.
- ELF modules (`ntdll.so`) symbolicate natively.
- PE modules give module plus raw offset; perf's symbolizer is ELF-only. The
  `Exports` tier resolves them (`ntdll.dll+0x593e0 -> RtlUnwindEx`), and
  misattributes internal statics to the nearest export, which is exactly why
  `Full` needs retained debug info.

Driver and kernel symbols come from the distro: preflight checks each hardware
ICD's build-id against installed debuginfo (debuginfod counts as covered), and
analysis fetches driver symbols on demand through debuginfod when the local perf
supports it (on by default, a setting).

### Symbols for our own builds (open)

The `Full` tier needs the PE debug info the Prater build currently strips and
discards. The full set roughly triples a ~440 MB tree, so it is not bundled; the
model is the distro split-debuginfo one:

1. Link PEs with `--build-id` (today perf reports a blank build-id for them) and
   collect the debug info into a build-id-keyed tree instead of discarding it.
2. Ship the stripped runtime plus a few-KB **symbol manifest**: build version,
   per-module build-ids, a fetch URL. It is what guarantees symbols match the
   exact binary that was mapped.
3. Publish the symbols as a separate artifact or debuginfod endpoint.
4. Beisl fetches per module, on demand, cached: a trace touches 5 to 15 Wine
   modules, tens of MB, not the whole tree.

## Surfaces

All of them are thin views. The check logic is the `preflight` crate, the
analysis is `artifact`; no window computes anything of its own.

**The session window** (`beisl-setup --session <dir>`) follows `session.json`
while the game runs: state, elapsed time, warnings as they happen, including a
compatibility wall. Afterwards it shows either **the ask** - "clean run, not
analyzed yet", with Analyze and Discard, also for a trace stopped and saved
mid-game - or **the results**: game, Wine flavor and tier with its caveat;
frametime health and the worst spikes; the layer split and hot symbols; then
issues, each tied to its remedy, and the compatibility section when the logs
showed a wall.

**The setup page** (`beisl-setup`) is the preflight checklist: read-only
checkboxes saying what Beisl needs, what is the case, and why, with one-click
fixes. `preflight` holds the roster: perf, llvm-symbolizer, permission to record,
MangoHud and its hotkey, disk headroom, kernel and driver symbols, debuginfod,
GPU dump access, more than one GPU, whether Steam can find `beisl`, and gamebus
presence. Perf access prefers `CAP_PERFMON` on the perf binary over lowering
`perf_event_paranoid`: surgical, and nothing system-wide is weakened. The same
checklist is `beisl --check` in a terminal. Run from a bundle or an AppImage, an
install page comes first (see *Packaging*).

**The trace library** lists past runs; each opens in the same session window, so
an unanalyzed run lands on the same ask.

**The MCP server** (`beisl-mcp`) is a separate, opt-in binary: stdio only, no
port, inert until the user wires it into an AI client. Hand-rolled JSON-RPC on
`serde_json`; one pipe and microsecond file reads do not need an async runtime.
Seven tools, all read-only: `list_traces`, `get_setup_status`, `get_artifact`,
`get_report`, `get_log`, `get_crash_summary`, `list_events`. Deleting and
analyzing have no MCP surface on purpose.

**The audio cue** confirms a hotkey with distinct tones, audible
over a fullscreen game.

## Compatibility walls, and gamebus

Some games cannot run at all, and a profiler is the first thing to see it.
`crates/compat` knows what a log line means: a vendor **kernel driver** Wine
cannot load (kernel anti-cheat), an **unimplemented Wine function**, and the
**volume-GUID path** bug. The wrapper watches the logs for these while the game
runs and warns in the session window; `beisl-mcp` reports the same as a
`compat-wall` event afterwards. The driver wall is only visible when the user
has a Proton log on, since Proton silences that line by default; Beisl reads the
log and preserves it, and does not enable it.

**Beisl detects and enriches; gamebus houses and reports.** At exit the wrapper
drops the finding into gamebus-setup's inbox, a spool directory, and has nothing
further to do with it. There is no store and no report generator in Beisl.

**`beisl-compat`** is a second front-end over gamebus's review, next to gamebus's
own terminal UI:

- *Compat findings*: scan, read the evidence, draft a ProtonDB or
  AreWeAntiCheatYet report, mark what was filed. Drives `gamebus-setup compat
  --json`; opening it drains the inbox.
- *Identity misses*: the launches gamebus could not name authoritatively.
  Dismiss, promote, correct a title or a store, in a details pane. Driven over
  gamebus's MCP server.

It holds gamebus's records rather than copies of their fields: the
`gamebus-coupler` crate carries the record types, the judgements neither side
may recompute (which targets a finding still wants, which stores a picker
offers) and the edit verbs as data. Beisl links that crate and nothing else of
gamebus, so no network or database code arrives with it.

Over MCP, `beisl-compat` is one identified client: it signs gamebus's challenge
with its own key (`~/.config/beisl/`), only when it first edits, because that
enrols it for good. It is also the **second party for gamebus's audit chain** -
it remembers the last ledger head and asks on the next connection whether that
head is still in the chain. A reset ledger re-anchors quietly and an ordinary
edit says nothing; only a head that has vanished from its own chain is an alarm.
None of this is a security boundary against software running as the same user,
and it does not claim to be.

## Architecture

```
main workspace (crates.io only, light)
  crates/beisl            the wrapper: run.rs orchestrates a trace; mangohud, perf,
                          gpu, cue; steam + presence (who the game is); walls
                          (live wall watch); handoff (the gamebus drop); install
  crates/preflight        setup checks + persisted config; appimage (see Packaging)
  crates/session          session.json: the wrapper writes, the window reads
  crates/symbol-resolver  perf.data addresses -> symbols and layers, by tier
  crates/artifact         frametimes + samples + diagnostics -> artifact.json, report.txt
  crates/compat           what a log line says about a game's ability to run
  crates/beisl-mcp        the read-only MCP server
  crates/assets           embedded payloads: icons, desktop entries, AppStream
                          component, launcher and shim scripts, udev rule
  crates/docs-gen         renders docs/*.1.md to man pages, and the desktop entries

own workspace (libcosmic and its git dependencies, own lockfile)
  crates/beisl-gui        beisl-setup (setup page, session window, trace library)
                          and beisl-compat (the gamebus review window)
```

`beisl`, `beisl-gui` and `beisl-mcp` depend on `preflight` and `session`; nothing
depends on `beisl`. The file-based `session.json` is the only bridge between the
wrapper and the window, which is why a window can be a different process, a
different build, or on the other side of a sandbox.

## Packaging

How a user starts a trace:

| Context | Entry point |
|---|---|
| SpritzWine / Prater | a "Record performance" toggle in the installer's control panel (lives in that repo) |
| Steam with stock Proton / GE | `beisl %command%` in Launch Options |
| Lutris, Heroic | `beisl` as the wrapper command |
| bare wine | `beisl wine game.exe` |

Steam's own client scrubs `PATH` to the system directories, so a per-user install
is invisible to it; setup offers a small root-owned `/usr/bin/beisl` that runs
each user's own install and starts the game untraced when there is none.

**The build system is the Makefile, and every package is a shim over it.**
`make install DESTDIR=... PREFIX=...` stages binaries, man pages, icons, desktop
entries and the AppStream component; a recipe adds metadata. Beisl is one listing
in a software centre (`org.spritzwine.Beisl`) with several menu entries behind
it. Everything but Arch builds **airgapped** from two tarballs, the source and
`make vendor`'s, so the common path is the one proven to need no network.
`packaging/README.md` is the reference.

| Form | State |
|---|---|
| Release tarball + `beisl install` | per-user copy into `~/.local/bin` |
| Arch (`PKGBUILD`) | online build, the written exception |
| Fedora (COPR), openSUSE (OBS) | airgapped; the Fedora rpm proven with no network |
| Nix | airgapped, same two sources; built in the Nix sandbox, not yet run on a NixOS desktop |
| AppImage | below |
| Flatpak | designed, not built; below |

Portability is the build's doing: a binary starts only where glibc is at least as
new as it was built against. Releases are built on Debian 12, which is enforced
by the AppImage build's glibc ceiling.

### AppImage

The AppImage *is* the application, one file kept wherever the user likes. So
nothing is copied out of it.

- **Launchers, not copies.** Setting up writes four small scripts into
  `~/.local/bin` (`beisl`, `beisl-setup`, `beisl-compat`, `beisl-mcp`) that hand
  over to the file at its actual path, plus menu entries, icons and man pages.
  Moved or replaced, the file is followed by running it once.
- **`AppRun` picks the tool** from the launcher's `--beisl-tool=` argument, else
  from the name it was invoked under; with no arguments it opens the setup
  window, otherwise everything goes to the wrapper untouched.
- **A launcher checks its target before running it**: a regular, executable file
  that starts like an AppImage, owned by the user, with nobody else able to
  replace it on the way there. If not, it says which, and `beisl <game>` starts
  the game untraced. The magic check keeps a same-named script from standing in;
  it is not proof of origin.
- **Setup can loosen the permission check on request.** It examines the same
  location first, shows what it found and why it matters, and either the file is
  moved or the user keeps it there and says so; that choice is written into the
  launchers, where it can be read.
- **Setup believes it is in an AppImage only when it can show it**: its own
  executable inside the mounted directory, and Beisl's AppStream component in
  that directory. The runtime's variables are inherited by anything started from
  a terminal that is itself an AppImage, so on their own they prove nothing, and
  Beisl removes them from a game's environment only when they are provably its
  own.
- **The setup page says so.** Run from an AppImage it names the file, lists what
  setting up writes, shows the state of the launchers, and removes all of it
  again, leaving the AppImage alone. It rechecks on every start and steps aside
  when the setup still holds.

Not built: **signed releases**, checked by the launcher with `ssh-keygen` under
signer continuity, so an update passes without rerunning setup. Not yet tested
on hardware: a host with only `fusermount3`, and a full Steam session through a
launcher.

### Flatpak (not built)

A Flatpak cannot be the wrapper. A host launcher calling `flatpak run` starts the
game inside Beisl's sandbox, and the only way out, `flatpak-spawn --host`, gives
the game the session helper's environment instead of the caller's and makes it
that helper's child rather than the launch option's: the environment rule broken
and the game gone from Steam's process tree. Inside a sandbox, perf also loses
its file capabilities (bubblewrap sets no-new-privs) and needs `--allow=devel`.

The design is two pieces that work together:

- **The app, `org.spritzwine.Beisl`: the GUI and an informed installer.** Setup
  page, trace library, review window and MCP server. For wrapping, it explains
  what has to exist on the real filesystem - the `beisl` wrapper in
  `~/.local/bin` and a host that passes the perf checks - and puts it there and
  verifies it step by step, as the user confirms each one. It also recognises
  what is already present: a host `beisl` from a package or an AppImage, and
  Flatpak launchers the extension would serve.
- **The runtime extension, `org.freedesktop.Platform.VulkanLayer.beisl`**, for
  launchers that are Flatpaks themselves (Steam, Lutris, Heroic), whose launch
  options run inside their own sandbox. Mounted in like MangoHud and gamescope
  are; records in degraded mode.

Both write the same trace directory and `session.json`, so a trace recorded by
either opens in the same window. Constraints: the binary placed on the host must
be the portable build; writing `~/.local/bin` needs a narrow permission or the
file-chooser portal; host checks need the Flatpak talk permission, which Flathub
grants on explanation. Flathub also checks that the developer controls the
domain in the app id, and `spritzwine.org` is not ours - to be settled before a
submission.

## Case studies

Two incidents the design answers to. Code points at them by name.

### The Saints Row GPU hang

Saints Row with ray tracing on an Arc A770 produced hard GPU hangs that looked in
every way like a fresh Mesa bug. An afternoon went into bisecting the driver and
decoding devcoredumps; every driver hung identically. Swapping the *runtime* on
the same driver came back clean within two minutes and pointed at our layer -
until retesting each configuration from a fresh boot showed that "clean" had been
luck on a GPU degraded by the day's resets. Everything hung, on both kernel
drivers, under Valve's Wine and ours: a driver-class bug after all, reported
upstream (mesa#11557).

What it made Beisl do:

- **Capture GPU hangs properly.** The kernel keeps exactly one devcoredump, and
  while it sits unread new hangs record nothing. The wrapper watches for dumps
  during a session, captures and clears them, and treats a hang as an abnormal
  exit. (Built.)
- **Catch environment traps in preflight.** Two Mesa installs expose one GPU as
  two Vulkan devices, and which one a game gets is roulette. Preflight detects
  more than one GPU or ICD and offers a pin. (Built.)
- **Record identity, not just samples.** The hung process was visible only as a
  15-character thread name. Traces carry the game, the runtime and the Steam
  appid from sources that are not a truncated string. (Built.)
- **Swap one layer, compare outcomes.** "Same game, same driver, different
  runtime" settles in minutes what forensics cannot in hours. An A/B view should
  treat hang or no hang as a first-class result. (Open.)
- **One hang per boot.** Repeated engine resets silently flip later results. A
  trace should say how many GPU resets preceded it, and an A/B on a dirty state
  should be flagged. (Open.)

### Our own layers are suspects too

- **The MangoHud hotkey crash.** Stopping the frametime log by hotkey with the
  HUD visible aborted the game, an assert inside the MangoHud layer (upstream
  #2053). The side-channel crashed what it measured, and the story was one line
  under hundreds of lines of loader noise. `get_crash_summary` automates that
  digging; logging is always on, and the hotkey pauses rather than arms.
- **Version skew froze the window.** A new wrapper started an older installed
  `beisl-setup` that did not know a new state. Tools now prefer the sibling next
  to the running executable, the same build by construction, over `PATH`.

## The flywheel

A player who hits a stutter hands over a symbolicated, spike-annotated trace.
Over MCP that is directly actionable: an assistant reads it, names what was hot,
and it feeds the work on the hot path. Players generate exactly the profiling
data that work needs - and a trace nobody can read is a trace nobody fixes.

## Open

- **The `Full` tier**: retained debug info for our own builds, as under
  *Symbols*.
- **Tighter perf scoping**: the game's process tree in its own cgroup instead of
  system-wide sampling plus filtering. Call graphs are not recorded either.
- **Spike correlation**: which symbols were hot during a given hitch.
- **A/B compare**: two traces side by side, tiers and outcomes labeled.
- **In degraded mode perf alters the game's environment**: it prepends its own
  directory to `PATH`, sets `PREFIX` and `PERF_BUILDID_DIR`, and blanks
  `DEBUGINFOD_URLS`. That is perf's doing as the game's parent, and it is against
  the environment rule above. Found 2026-10-10, not addressed.
- **Feedback polish**: MangoHud's own logging indicator or a notification on
  start and stop; a notification when a launcher behind a menu entry fails, which
  today only reaches stderr.
- **Flatpak**, and **signed AppImage releases**, as designed above.
- **A store listing**: screenshots, and the app-id domain question.
- **The "Record performance" toggle** in the SpritzWine control panel, which
  belongs to that repository.
