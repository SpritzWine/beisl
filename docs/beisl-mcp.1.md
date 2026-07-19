# NAME

beisl-mcp — read-only MCP server over recorded beisl traces (JSON-RPC on stdio)

# SYNOPSIS

    beisl-mcp             serve on stdin/stdout (an AI client starts this; it
                          opens no ports and touches nothing without a client)
    beisl-mcp --version   print the version
    beisl-mcp --help      this help

# DESCRIPTION

Wire it into an AI client's config:

    { "mcpServers": { "beisl": { "command": "beisl-mcp" } } }

Full docs: `man beisl-mcp` or docs/ in the repository.

<!-- help-end -->

The server is entirely opt-in: the transport is the process's own stdin and
stdout — no ports, no sockets — and nothing runs unless an AI client is
configured to start the binary. All tools are read-only; analyzing and
deleting traces stay in the app, where a human confirms them.

# TOOLS

    list_traces         every run, newest first: id, game, start, duration,
                        analyzed?
    get_setup_status    the preflight checklist (same checks as `beisl --check`)
    get_artifact        a run's analyzed trace as JSON: frame stats and spikes,
                        the per-layer CPU split, hottest functions, diagnostics
    get_report          the same analysis as human-readable prose
    get_log             the tail of a run's beisl, game, or perf log
    get_crash_summary   pre-chewed crash forensics for a run that "just died"
    list_events         crash/gpu/log-signal events from every run since a
                        since_unix cursor — "anything crash since we last
                        talked?" as one cheap call, without walking
                        list_traces yourself

# SEE ALSO

beisl(1), beisl-setup(1)
