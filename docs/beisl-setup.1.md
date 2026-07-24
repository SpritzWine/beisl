# NAME

beisl-setup — GUI: setup checklist with one-click fixes, install/update, trace library

# SYNOPSIS

    beisl-setup                    open the setup window
    beisl-setup --session <dir>    open the live session window for a trace run
    beisl-setup --payload <dir>    (dev) install page against an extracted dist tree
    beisl-setup --version          print the version
    beisl-setup --help             this help

# DESCRIPTION

The headless twin is `beisl --check`; traces live in
~/.local/share/beisl/traces/. Full docs: `man beisl-setup` or docs/ in the
repository.

<!-- help-end -->

The setup page renders the same preflight state as `beisl --check`: the perf
record tier (full / degraded / none, with the exact command to reach full),
required tools with one-click pkexec fixes, and advisory rows. System-level
changes are explicit opt-in buttons, never part of the one-click sweep:

**Steam launcher** — Steam's containerized client starts launch options with a
bare system PATH, so `beisl %command%` cannot find a per-user install. The
button installs a small root-owned forwarder at /usr/bin/beisl that runs each
user's own beisl (one pkexec prompt; remove with `sudo rm /usr/bin/beisl`).

**GPU crash-dump capture** — a udev rule + tmpfiles entry granting the render
group read access to GPU crash dumps, so the evidence of a GPU-side crash can
be saved the moment it happens.

The Advanced section holds the poweruser switches: GPU pin for multi-GPU
boxes, on-demand debug-symbol fetch, dump-access policy, and Frosted
background — whether beisl-setup follows the system theme's Frosted Glass
setting (on by default). Outside COSMIC's Settings app, where there's no
dialog for that theme flag, a button in the same section offers to write it
directly; it only appears while the theme has no saved preference yet.

When started from an extracted dist tree, an install/update page comes first;
the Traces page lists every past run — open one for its results, reveal its
folder, analyze a raw trace, or delete it.

# FILES

    ~/.local/share/beisl/traces/run-*/    one directory per trace run
    $XDG_CONFIG_HOME/beisl/config.toml    persisted preferences

# SEE ALSO

beisl(1), beisl-mcp(1), mangohud(1)
