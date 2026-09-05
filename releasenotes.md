# Unreleased

Still in the cellar, on the current development line: beisl learns the
game's real name from Steam, and learns to say "this one never had a
chance" while the game is still up.

## New

- **The game's name, not the launcher's.** A trace is filed under the game
  it traced. beisl reads the Steam appid from the launch environment
  (`SteamAppId` / `SteamGameId`, or the `AppId=` token in reaper's argv) and
  the product name out of the library's `appmanifest`, both at spawn: a run
  that used to be recorded as `WardogsLauncher-Shipping.exe` now reads
  *WARDOGS Playtest*. Name sources are ranked (the argv guess names the
  launcher, MangoHud the process, Steam's manifest the product, gamebus the
  product as Steam and Discord together call it), and only a better source
  may replace a name, so a good one never degrades into a worse one. The
  appid and the title are read out of Steam's own bookkeeping; nothing is
  asked of the game to get them.
- **Compat walls, called while the game is still up.** Some games do not
  stutter, they simply cannot run: an anti-cheat's kernel-mode driver wine
  cannot load, a missing wine export, a volume-GUID path bug. beisl now
  recognizes the shape of those walls as they happen and says so in the
  session window, instead of reporting "No problems detected" next to the
  game's own error dialog.
- **A Compatibility section on the trace.** A trace's detail page gains the
  wall in plain words with the log line that is the evidence for it, plus
  every kernel driver the game asked wine to load, whether wine ships that
  driver ("Wine builtin" or "NOT shipped by Wine"), and how many times it was
  tried. The report file carries the same. There is no baked-in roster of
  wine's drivers: beisl reads them out of the wine tree that actually ran,
  and if it finds none it says "unknown" rather than guess.
- **The Proton log, preserved.** A default Proton launch suppresses wine's
  diagnostics (`WINEDEBUG=-all`), so the driver wall leaves nothing to find.
  If you turn `PROTON_LOG` on yourself, beisl reads that log too and keeps a
  copy beside the trace as `proton.log`, because Proton deletes it at the
  next launch. beisl does not switch wine's logging on for you, then or
  ever: the switch is yours, the reading is ours.
- **`beisl-compat`, a review window over gamebus.** beisl detects compat
  findings; the gamebus project houses and files them. The new window
  reviews what was found, drafts a ProtonDB report or an AreWeAntiCheatYet
  issue, and marks what was filed. Every action is a `gamebus-setup compat`
  command run for you: none of it is reimplemented here, and nothing submits
  anything by itself.
- **A wall reaches gamebus even if gamebus was not listening.** A detected
  wall is handed over as a JSON file dropped in gamebus's inbox at exit, so
  the finding survives whether or not gamebus-presenced happened to be
  running when the game stopped. Only walls with a Steam appid are handed
  over, because an appid is what a finding can be filed under.
- **Four binaries in the bundle.** `beisl`, `beisl-setup`, `beisl-compat`
  and `beisl-mcp`, each with a man page.

## Heads-up

- **The anti-cheat wall needs your own `PROTON_LOG`.** Without it, wine's
  diagnostic lines never reach a file and the driver wall stays invisible.
  beisl will not switch it on for you.
- **`beisl-compat` has not been through a design review yet.** It works, and
  it is deliberately thin over `gamebus-setup compat`, but the shape of the
  window is not settled.
- **gamebus-presenced stays optional.** Without it, Steam launches still get
  their identity from the Steam manifest, and games from other launchers
  keep the process name. A new row in the `beisl-setup` checklist says which
  of the two you are in.

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
