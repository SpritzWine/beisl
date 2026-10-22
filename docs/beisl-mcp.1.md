# NAME

beisl-mcp - MCP server over recorded beisl traces, read-only unless asked (JSON-RPC on stdio)

# SYNOPSIS

    beisl-mcp             serve on stdin/stdout (an AI client starts this; it
                          opens no ports and touches nothing without a client)
    beisl-mcp --version   print the version
    beisl-mcp --help      this help

    beisl-mcp --traces-dir DIR
                          serve a trace library other than the default
                          (~/.local/share/beisl/traces); for testing a
                          consumer against a fixture tree without moving
                          XDG_DATA_HOME for every other tool
    beisl-mcp --allow-labels
                          also offer label_spike, the one tool that writes:
                          the assistant may record what a spike really was

# DESCRIPTION

Wire it into an AI client's config:

    { "mcpServers": { "beisl": { "command": "beisl-mcp" } } }

Full docs: `man beisl-mcp` or docs/ in the repository.

<!-- help-end -->

The server is entirely opt-in: the transport is the process's own stdin and
stdout — no ports, no sockets — and nothing runs unless an AI client is
configured to start the binary. Every tool reads, except label_spike, which
exists only when the client's config passes `--allow-labels`; analyzing and
deleting traces stay in the app, where a human confirms them.

# TOOLS

    list_traces         every run, newest first: id, game, start, duration,
                        analyzed?
    get_setup_status    the preflight checklist (same checks as `beisl --check`)
    get_artifact        a run's analyzed trace as JSON: frame stats and spikes
                        (each against the second before it, with a verdict:
                        its likely cause if one is sure enough, the candidates
                        and their evidence, and whether it wants a confirmation
                        or a checklist; and its label, once it has one), the
                        per-layer CPU split, hottest
                        functions, diagnostics
    get_report          the same analysis as human-readable prose
    get_log             the tail of a run's beisl, game, perf, or proton log
    get_crash_summary   pre-chewed crash forensics for a run that "just died"
    list_events         crash/gpu/compat-wall/log-signal events from every run
                        since a since_unix cursor - "anything crash since we
                        last talked?" as one cheap call, without walking
                        list_traces yourself. A compat-wall says the game
                        structurally cannot run here (a vendor kernel-mode
                        anti-cheat driver, a missing Wine export, or a
                        volume-GUID path bug) and fires even on a clean exit
    label_spike         with --allow-labels only: record what spike N of a run
                        was (shader-compile, cpu-contention, throttle, blocked,
                        gpu-bound, memory-pressure, cpu-work, gc-pause,
                        jit-compile, loading, shutdown, or unknown when it
                        cannot be told: an answer, never trained on). Stored as the
                        agent's label, beside beisl label's in
                        ~/.local/share/beisl/labels.jsonl: it counts a third of
                        the user's own until the user confirms it, and never
                        decides on its own whether a learned layer may speak

# SEE ALSO

beisl(1), beisl-setup(1)
