# NAME

beisl — game perf tracing: perf CPU samples + MangoHud frametimes, wine-agnostic

# SYNOPSIS

    beisl [--] <command> [args...]   trace a game
                                     (Steam launch option: `beisl %command%`)
    beisl --check                    setup checklist in the terminal
                                     (aliases: --preflight, doctor)
    beisl symbols <trace-run-dir>    resolve + report a captured trace
    beisl install                    copy beisl into ~/.local/bin
                                     (+ "Beisl Setup" menu entry when bundled)
    beisl uninstall                  remove the per-user install
    beisl --version                  print the version
    beisl --help                     this help

# DESCRIPTION

Traces land in ~/.local/share/beisl/traces/run-*/ — frametime CSV, perf.data,
logs; artifact.json + report.txt once analyzed. `beisl-setup` is the GUI face:
the same checklist with one-click fixes, plus the trace library. Full docs:
`man beisl` or docs/ in the repository.

<!-- help-end -->

Beisl wraps an arbitrary game or command: it configures MangoHud to log
frametimes as a side-channel (always-on, flushed per frame — the log survives a
crash), records CPU samples with perf across the session, and merges both into
a symbolicated trace artifact that names where the time went — split by layer:
kernel, GPU driver, DXVK, wine, game.

It is wine-agnostic by design: SpritzWine, stock Proton, GE, Lutris-wine,
system wine, or a native Linux game — the same wrapper for all of them, so
an A/B comparison captures both halves the same way.

Recording runs at the best tier the system allows:

    full       system-wide, kernel + userspace (needs CAP_PERFMON on perf,
               or perf_event_paranoid <= 0)
    degraded   userspace-only — game, DXVK, wine and userspace driver time,
               no kernel layer (works unprivileged at paranoid <= 2)
    none       the game runs un-traced

`beisl --check` shows the current tier and the exact command to reach full.

# COMMANDS

**trace** (the default: everything after `beisl` is the wrapped command) —
launches the command, records, and analyzes on crash; a clean exit leaves the
raw trace in place for on-demand analysis from the trace library.

**--check** — the headless setup checklist: record tier, required tools,
advisory rows, and the exact fix command for anything missing. Exit status 1
when a required check needs setup.

**symbols <trace-run-dir>** — re-resolve a captured trace and print its
report (fetches GPU-driver debug symbols on demand via debuginfod when the
local perf supports it).

**install / uninstall** — per-user install into ~/.local/bin, desktop entry,
icons, and man pages; uninstall names anything root-owned it cannot remove.

# ENVIRONMENT

**BEISL_FORCE_RECORD_MODE** — `full`, `degraded`, or `none`: override the
detected record tier (testing aid).

**BEISL_NO_UI** — `1` disables the live session window (headless runs, CI).

**MANGOHUD_CONFIG** — honored and appended to, never replaced. Beisl adds its
logging keys last; a user `log_duration` is overridden to 0 (last-wins) so the
frametime log is never truncated mid-session, with a note in beisl.log.

**DEBUGINFOD_URLS** — honored when set; when unset and on-demand symbol fetch
is enabled (the default), the distro's default server is injected for the
analysis step only.

# FILES

    ~/.local/share/beisl/traces/run-*/    one directory per trace run
    $XDG_CONFIG_HOME/beisl/config.toml    persisted preferences
    /usr/bin/beisl                        optional launcher shim (see beisl-setup(1):
                                          Steam's containerized client needs it for
                                          bare `beisl %command%` launch options)

# EXIT STATUS

Tracing propagates the wrapped command's exit status (with launcher chains
this is the outermost launcher's status, not the game's). Usage errors exit 2.

# SEE ALSO

beisl-setup(1), beisl-mcp(1), mangohud(1), perf(1)
