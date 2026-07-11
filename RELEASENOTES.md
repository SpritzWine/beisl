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
