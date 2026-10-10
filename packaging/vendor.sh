#!/bin/sh
# Build the one tarball an airgapped build needs besides the source itself.
#
# Unpacked over the source tree it adds, for BOTH workspaces (the GUI is its own,
# with its own lockfile), a vendor/ directory and the .cargo/config.toml that
# points cargo at it. A recipe then needs no vendoring knowledge at all:
# unpack source, unpack this on top, build with the network off.
#
#   packaging/vendor.sh <output.tar.zst>
#
# Staged outside the tree on purpose: a .cargo/config.toml dropped into a
# developer's checkout would silently switch their own builds to vendored
# sources.
set -eu

out="${1:?usage: vendor.sh <output.tar.zst>}"
root=$(cd "$(dirname "$0")/.." && pwd)
case "$out" in /*) ;; *) out="$PWD/$out" ;; esac

stage=$(mktemp -d "${TMPDIR:-/tmp}/beisl-vendor.XXXXXX")
trap 'rm -rf "$stage"' EXIT

# cargo prints the config for the directory it was given, as an absolute path.
# In the tarball each workspace's vendor/ sits beside its .cargo/, and a
# relative `directory` is resolved from the parent of .cargo, so "vendor" is
# correct for both and for wherever the tree is later unpacked.
vendor_one() { # <manifest> <subdir of the tree, or .>
    mkdir -p "$stage/$2/.cargo"
    cargo vendor --locked --manifest-path "$1" "$stage/$2/vendor" \
        | sed -e 's|^directory = ".*"$|directory = "vendor"|' \
        > "$stage/$2/.cargo/config.toml"
    grep -q '^directory = "vendor"$' "$stage/$2/.cargo/config.toml" || {
        echo "vendor.sh: cargo printed no source directory for $1" >&2
        exit 1
    }
}

vendor_one "$root/Cargo.toml" .
vendor_one "$root/crates/beisl-gui/Cargo.toml" crates/beisl-gui

# Put back what cargo vendor drops (see the script for the why).
"$root/packaging/cache-coupler-data.sh" "$stage/crates/beisl-gui/vendor"

# Stable order, owner and mtime, so the same lockfiles give the same tarball.
mkdir -p "$(dirname "$out")"
tar -C "$stage" --sort=name --owner=0 --group=0 --numeric-owner \
    --mtime='2000-01-01 00:00Z' -cf - . | zstd -q -19 -T0 -o "$out" -f
echo "vendored: $out ($(du -h "$out" | cut -f1))"
