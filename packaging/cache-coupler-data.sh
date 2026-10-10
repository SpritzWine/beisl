#!/bin/sh
# Put back what `cargo vendor` drops.
#
# gamebus-coupler's build.rs reads ../gamedb/stores.toml, a file outside its own
# package directory. cargo vendor extracts only the package, so a vendored tree
# has no gamedb/ and the build panics asking for a submodule it cannot have.
#
# The file is not missing, only unvendored: cargo's git checkout of
# gamebus-presenced still has it, because cargo clones git dependencies with
# their submodules. This copies it next to the vendored crate, where the
# relative path build.rs already uses resolves to it. Nothing is downloaded,
# so an offline build after this succeeds.
#
# Run against the vendor directory before it is packed into the tarball:
#   cargo vendor --locked vendor && ./cache-coupler-data.sh vendor
#
# Remove this once gamebus-coupler carries its own copy of the store list
# (reported upstream 2026-10-10).
set -eu

vendor="${1:?usage: cache-coupler-data.sh <vendor-dir>}"
crate="$vendor/gamebus-coupler"

[ -d "$crate" ] || { echo "no gamebus-coupler in $vendor; nothing to do"; exit 0; }
[ -f "$vendor/gamedb/stores.toml" ] && { echo "already cached"; exit 0; }

# Match the submodule path exactly. A looser pattern also finds
# .scripts/gamedb-lint-fixtures/stores.toml, which is a TEST FIXTURE: it happens
# to match the real list today and is meant to diverge from it, so picking it
# would quietly ship fixture data in a package.
src=$(find "${CARGO_HOME:-$HOME/.cargo}/git/checkouts" \
        -path '*gamebus-presenced*/gamedb/stores.toml' -print -quit 2>/dev/null || true)

if [ -z "$src" ]; then
    echo "cache-coupler-data: no stores.toml in cargo's git checkouts." >&2
    echo "Run 'cargo vendor' first, from a tree whose lockfile pins the coupler." >&2
    exit 1
fi

mkdir -p "$vendor/gamedb"
cp "$src" "$vendor/gamedb/stores.toml"
echo "cached $src -> $vendor/gamedb/stores.toml"
