#!/bin/sh
# beisl launcher shim, installed to /usr/bin/beisl by beisl-setup (pkexec).
#
# Steam's containerized client starts launch options with a bare
# PATH=/usr/bin:/bin, so `beisl %command%` can't see per-user installs.
# This root-owned forwarder resolves the *invoking* user's own install at
# run time — correct on multi-user systems, and never stale: updates
# rewrite ~/.local/bin/beisl, not this file.
home="${HOME:-$(getent passwd "$(id -u)" | cut -d: -f6)}"
beisl="$home/.local/bin/beisl"
if [ -x "$beisl" ]; then
    # Only run a beisl the invoking user owns: a foreign-HOME context (sudo -E,
    # a mis-set service) must never make root — or anyone else — execute a file
    # writable by a different user. (-O: supported by bash/dash/busybox sh.)
    if [ -O "$beisl" ]; then
        exec "$beisl" "$@"
    fi
    echo "beisl shim: $beisl is not owned by the invoking user - not running it" >&2
else
    echo "beisl shim: no install at $beisl" >&2
fi
# No usable install for this user: never break a game launch — run it untraced.
if [ "$#" -gt 0 ]; then
    echo "beisl shim: launching untraced" >&2
    exec "$@"
fi
echo "beisl shim: run 'beisl install' first" >&2
exit 127
