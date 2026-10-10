# beisl 0.4.0

The Packaging release: beisl stops being only a tarball with its own
installer. It is an AppStream component a software centre can list, it has a
build a distro can call, and it ships as an AppImage that stays the app
instead of unpacking itself. Meanwhile beisl-compat learns the second half of
gamebus: the launches it could not name.

## New

- **An AppImage that stays the app.** One file, kept wherever you like.
  Nothing is copied out of it: `beisl install`, run from the AppImage (or the
  setup window, which is what a double-click opens), writes four small
  launchers into `~/.local/bin` that hand over to the file at its actual
  path, plus the menu entries, icons, man pages and metainfo. Steam's
  `beisl %command%` and an MCP client's `beisl-mcp` then work as for any
  other install. Move or replace the file, open it once, and the launchers
  follow.
- **Launchers that check before they execute.** A launcher is a place someone
  could redirect, so it verifies its target first: still there, still an
  AppImage, owned by you, and neither the file nor any directory above it
  writable by anyone else. If not, it says which, and `beisl <game>` starts
  the game untraced, because a missing profiler must never stop a game.
  `beisl install` refuses such a location with the reasons;
  `--relax-permission-check` keeps the file there anyway and writes that
  choice into the launchers, where it can be read.
- **The setup page says it runs from an AppImage.** Started from one,
  beisl-setup names the file, lists what setting up writes and where, shows
  what is wrong with where the file is kept, and removes all of it again
  from the same page, leaving the AppImage alone. It rechecks on every start
  and steps aside for the checklist when the setup still holds.
- **Identity misses in beisl-compat.** The window gains a second tab beside
  the findings: the launches gamebus could not name authoritatively, with
  their store, codename and review state. Dismiss, restore, promote to umu
  and take back need no typing; a details pane folds in from the side for
  the corrections that do (a title, a store). It talks to gamebus's own MCP
  server, and which stores a picker may offer or whether a pick clears an
  override stays gamebus's answer, not a copy of its rule.
- **A second party for gamebus's ledger.** beisl-compat identifies itself to
  gamebus with its own Ed25519 key (created 0600 beside beisl's config,
  never in a trace dir), remembers the ledger head, and asks on the next
  connection whether that head is still in the chain. A reset ledger and an
  ordinary edit say nothing; only a head that vanished from its own chain is
  an alarm. The key is enrolled at the first edit, not on connect: someone
  who only looks is not registered.
- **A way in without a wall.** The gamebus row on the setup checklist grows
  a *Review* button whenever both tools are installed, since identity misses
  exist whether or not a game ever failed to run.
- **One listing in a software centre.** beisl is now an AppStream component,
  `org.spritzwine.Beisl`: a centre lists it once while the app menu keeps its
  separate entries. Screenshots are still missing, so no store lists it yet.
- **A build a packager can call.** `make install DESTDIR=... PREFIX=/usr`
  stages the FHS layout and can write a manifest of every installed path, so
  a package's file list cannot drift from upstream's. Recipes for Arch,
  Fedora (COPR), openSUSE (OBS) and Nix live in `packaging/`; each is
  metadata over the Makefile and holds no build logic of its own. They are
  recipes: no distro package is published.
- **Airgapped by default.** Everything but Arch builds with the network off
  from two tarballs: the source, and `make vendor`'s, which carries both
  cargo workspaces' dependencies. Arch fetches online, the written exception.
  The Fedora source rpm was rebuilt in a namespace with no network
  interfaces, and the Nix recipe in its sandbox, tests included.
- **More on the release page.** Beside the bundle (`beisl-<v>-<arch>.tar.gz`)
  a release now carries `Beisl-<v>-<arch>.AppImage`, the source as
  `beisl-<v>.tar.gz` and the dependencies as `beisl-<v>-vendor.tar.zst`,
  all in `SHA256SUMS`.

## Fixed

- **Releases start on older distros.** The release is now built on Debian 12
  instead of a moving base: the binaries need glibc 2.35 at most and start
  on Ubuntu 22.04 and Debian 12, where earlier releases did not.
  `make appimage` refuses anything above the ceiling, so moving the build
  image forward fails the release rather than quietly shrinking who can run
  it.
- **beisl-compat reads gamebus's answer, not its files.** The findings come
  from `gamebus-setup compat --json` instead of gamebus's private stash
  file, and the drain, the scan tally and the drafts are no longer read out
  of human prose. A finding shows the GPU vendor beisl recorded where it
  used to read "unknown GPU", and a draft lists the facts it still needs
  before filing.
- **A refusal shows its reason.** When gamebus refuses something, the window
  shows why, not only the closing "Nothing was written."
- **NixOS hosts.** Preflight finds Vulkan ICD manifests under
  `/run/opengl-driver`, and no longer offers `setcap` on a perf binary in
  the read-only store, where it cannot work; the sysctl is offered instead.
- `make clean` no longer deletes the tracked recipes in `packaging/`.

## Heads-up

- **The recipes wait for this release.** The PKGBUILD checksum and the two
  Nix hashes can only be filled in from published 0.4.0 tarballs, so they
  are placeholders until then. Nothing has opened a window on a NixOS
  desktop yet.
- **The AppImage's untested corners.** A full Steam session through a
  launcher, and a host that has only `fusermount3`, have not been run.
  Signed releases checked by the launcher are planned, not built. The image
  is x86_64 and bundles nothing: the windows need the host's libxkbcommon.
- **Flatpak is designed, not built.** `DESIGN.md` says what it will be and
  why it cannot be the game wrapper.

## Compatibility

- beisl-compat needs `gamebus-setup` from gamebus-presenced 0.4.0 or newer
  for `compat --json`; an older one is reported as version skew, never as an
  empty stash. The identity-misses tab is built against gamebus-presenced
  0.5.0.
- The trace format did not move: `artifact.json` stays schema v1 and the MCP
  surface is unchanged. An existing tarball install keeps working and
  updates the same way, `./beisl install` from the new bundle.

---

# beisl 0.3.0

The Compat-Finding release: traces now know when a game hits a wall, the
session window says so live, and a new beisl-compat window hands every wall
to gamebus so it can be reported — all while the COSMIC toolkit pin marches
forward and the MCP server learns to serve any trace library.

## New

- **Compat-finding review window.** A new beisl-compat window surfaces every
  compat wall detected during a trace, shows the wall and every driver the
  game tried, and hands the finding to gamebus through its inbox so it can
  be reported.
- **Live compat walls in the session window.** The session window now says it
  while it happens: compat walls appear as they are detected, not just after
  the fact.
- **beisl-compat.** A dedicated window over gamebus's compat report flow:
  open a trace, see its walls, and send them upstream without leaving beisl.
- **COSMIC 1.3 alignment.** The setup app's toolkit pin moves forward in step
  with the installer, keeping the frosted-glass look consistent across the
  suite.
- **MCP serves any trace library.** beisl-mcp can now serve any trace
  library, not just the current run.

## Fixed

- Traces are named after the game, not the launcher.
- The game's identity is taken from Steam before presence can override it.
- beisl-mcp now says so when gamebus-presenced is missing.
- Private keys are kept out of the repo for good.
- A cargo audit is required around any lockfile change.

## Compatibility

- `artifact.json` remains schema v1; new fields are additive with defaults.
  The MCP surface gains no breaking changes.

---

# beisl 0.2.0

The Frosted Glass release: the trace library learns to be honest about
broken runs, analysis reports the median you actually felt, an AI can ask
"anything crash since we last talked?" — and the setup app joins COSMIC 1.3,
frost included.

## New

- **Frosted Glass.** The setup app moves to the COSMIC 1.3 era of libcosmic
  and renders the frosted-window look under any compositor that serves the
  standardized blur protocol (cosmic-comp, KWin, …). It follows the system
  theme: an Advanced toggle lets beisl-setup decline the frost for itself,
  a link opens COSMIC Settings straight on the Appearance page where the
  theme's own switch lives, and on systems without a COSMIC Settings UI a
  consent-style button can write the theme flag for you (it says exactly
  what it touches and how to revert). Content sections now sit on the
  standard container panels, so everything stays readable over the blur.
- **The median you experienced.** Frametime stats add a time-weighted
  median alongside the per-frame one: the frame the *session's wall-clock*
  spent its typical moment in, so a stutter-heavy minute no longer hides
  behind thousands of fast frames. The results header shows it front and
  center.
- **One-click debuginfo install.** Issues-panel rows whose module maps to a
  known distro debuginfo package (Mesa Vulkan drivers, deliberately narrow)
  grow an *Install* button — pkexec-confirmed, followed by an automatic
  re-analyze so the freshly resolved symbols land in the report you're
  looking at.
- **Re-analyze on demand.** An already-analyzed run's window offers a
  fresh analysis pass, so old traces pick up resolver improvements without
  hand-deleting anything.
- **An honest trace library.** Runs whose recorder died pre-boot read
  "stale — never finished" instead of pretending to be live (and become
  deletable again); a trace that can't be analyzed says so — with the
  reason — instead of failing quietly on every attempt; the Traces page
  gains selection and bulk delete.
- **MCP: `list_events`.** One cheap poll answers "anything crash since we
  last talked?" — crashes, GPU hangs and corroborated log signals newer
  than your cursor, deduplicated, with the next cursor in the answer. Built
  for an AI client (or a cron'd agent) checking in at session start.
- **Steam PATH self-observation.** Every Steam-launched trace records
  whether the client still scrubs launch-option PATH (upstream
  [steam-for-linux #13389](https://github.com/ValveSoftware/steam-for-linux/issues/13389)):
  the day Steam ships the fix, the launcher-shim checklist row retires
  itself — no beisl update needed.

## Fixed

- **Native games no longer borrow the launcher's wine.** Wine-flavor
  detection is scoped to the traced game's own process instead of the whole
  capture, so a native Linux title launched through a wine-based launcher
  reads "none — native game" rather than inheriting Proton's name.
- **Updating over a running install.** `beisl install` stages and renames
  instead of copying over live binaries — updating while `beisl-mcp` (or a
  session) is running no longer dies with "text file busy", and a running
  reader keeps its old binary until restart.
- **A re-analysis can't eat the artifact.** Analysis results are written
  via a staged temp file and atomic rename, so a mid-write crash leaves the
  previous good `artifact.json` untouched.
- The traces-page scrollbar keeps its distance from the row buttons.

## Compatibility

- `artifact.json` stays schema v1; new fields are additive with defaults,
  and old artifacts render their old readings (absent data reports
  "unknown", never a fresh claim). Config files gain keys with
  behavior-preserving defaults. The MCP surface grows `list_events`; the
  existing six tools are unchanged.

---

# beisl 0.1.0

The first release without the alpha tag: beisl now traces on locked-down
systems, survives Steam's containerized client, refuses to be lied to by its
own side-channel — and documents itself.

## New

- **Degraded recording tier.** When system-wide sampling isn't allowed
  (no CAP_PERFMON, `perf_event_paranoid` up to 2 — most stock distros),
  beisl now records a userspace-only trace instead of nothing: game, DXVK,
  wine and userspace-driver time, just no kernel layer. The checklist shows
  the tier ladder (full / degraded / none) with the exact command to reach
  full, and a degraded trace's report says why its kernel bar is empty.
- **Steam launcher shim.** Steam's containerized client starts launch options
  with a bare system PATH, so `beisl %command%` stopped resolving (upstream
  [steam-for-linux #13389](https://github.com/ValveSoftware/steam-for-linux/issues/13389)).
  The setup offers a one-click, opt-in fix: a small root-owned launcher at
  `/usr/bin/beisl` that runs each user's own install — multi-user correct,
  update-proof, and it launches the game untraced rather than break anything
  if no install exists.
- **On-demand driver symbols.** Analysis fetches GPU-driver debug symbols via
  debuginfod when the local `perf` supports it (default on; Advanced toggle to
  keep resolves offline), with distro-aware advisories when it can't.
- **Distro- and system-aware setup.** Install fixes now speak the local
  dialect — apt/dnf/pacman/zypper, Ubuntu's `linux-tools` split, `rpm-ostree`
  layering on Fedora Atomic (Bazzite & friends) — and SteamOS gets an honest
  redirect instead of a checklist that can't pass.
- **Single-source docs.** `docs/*.1.md` in the repository render three ways:
  each binary's `--help`, real man pages (`man beisl` works after install —
  generated by the new `docs-gen` crate, pure cargo, CI-friendly), and
  browsable markdown.

## Fixed

- **Frametime stats immune to MangoHud's clock leak.** MangoHud logs the
  machine's *time since boot* as the frametime of every new swapchain's first
  frame (unreported upstream; diagnosed here — the garbage rows are 99.8 % of
  uptime). One such row inflated a 4-minute session to "32 hours logged,
  0.34 avg fps". Analysis now drops rows above a generous 10 s ceiling *or*
  matching the time-since-boot signature, counts them, and says so in the
  report — real multi-second shader stalls survive.
- The checklist scrollbar no longer crowds the rows or their buttons, and the
  file-manager/browser helpers no longer spill KDE's harmless env-var chatter
  into the terminal.
- `make distclean` for a guaranteed-fresh rebuild after switching branches.

## Compatibility

- Trace artifacts (`artifact.json`, schema v1) remain readable across this
  release; new fields are additive with defaults. The MCP tool surface and
  its identifiers are now pinned by tests — from 0.1.0 on they only change
  deliberately.

---

# beisl 0.0.1-alpha.2

The trace library release: past runs stop being invisible, and an AI can
finally read your traces.

## New

- **Trace library.** `beisl-setup` grows a *Traces* page: every past run,
  newest first — open it in the results view, analyze it on demand, reveal
  its folder, or delete it (also straight from the results view). One
  containment-checked deletion path everywhere.
- **beisl-mcp.** A read-only [MCP](https://modelcontextprotocol.io) server
  over your traces, shipped as its own opt-in binary (stdio only, no ports;
  inert until you wire it into an AI client). Six tools: list runs, read a
  run's analysis artifact / report / logs, get pre-chewed crash forensics,
  and read the preflight checklist as setup advice.
- **Stop & save.** Stopping the recorder mid-game now says what actually
  happened — trace saved, game still running — and offers analysis right
  there instead of masquerading as a finished session.
- **Das Glasl.** The app icon lands in your app menu and taskbar; the
  session window stops wearing a generic gear.

## Fixed

- A user's own MangoHud `log_duration=N` no longer silently truncates the
  session frametime CSV (beisl appends a last-wins `log_duration=0`, with a
  note in the run log when it overrides).
- A freshly built wrapper no longer opens a stale installed session window:
  the sibling `beisl-setup` (same build) is preferred over PATH.
- The results view's delete confirm lands under the pointer instead of
  across the window.

## Heads-up

- **MangoHud ≤ 0.8.x hotkey crash:** stopping the frametime log while the
  HUD is visible can crash the game (upstream
  [MangoHud #2053](https://github.com/flightlessmango/MangoHud/issues/2053)).
  The preflight checklist now warns about it; until upstream ships a fix,
  hide the HUD before using the pause hotkey — or just leave the log
  running.

Licensing housekeeping rounds it out: the LGPL text, copyright note, and
Cargo authors ship correctly from the very first commit of the repository's
(rewritten) history.
