#!/bin/sh
# Move every place that names the release version, in one go.
#
#   packaging/bump-version.sh <version> "<one sentence for the software centre>"
#
# The version is written in more syntaxes than cargo's: Cargo.toml and both
# lockfiles, the AppStream release list, the Arch PKGBUILD, the two rpm specs
# and the Nix recipe. Tests fail on any of them left behind (assets and
# docs-gen), but only after the tag is cut is too late to find out, so this
# runs them at the end.
#
# What it cannot do is listed when it finishes: the checksums that exist only
# once the release is published, and the pages branch.
#
# Changes the working tree only. Commit and tag yourself.
set -eu

new="${1:?usage: bump-version.sh <version> \"<summary sentence>\"}"
summary="${2:?usage: bump-version.sh <version> \"<summary sentence>\"}"
root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"

die() { echo "bump-version: $*" >&2; exit 1; }

# Full releases only. rpm forbids a hyphen in Version and Arch in pkgver, so a
# prerelease cannot be written into the recipes, and the docs-gen guard
# requires them to name the version being built. Cut one by hand if needed.
echo "$new" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+$' \
    || die "'$new' is not a version like 1.2.3 (prereleases are not handled, see the comment)"

old=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n1)
[ -n "$old" ] || die "no version in Cargo.toml"

# The release workflow slices this section out for the release page and fails
# without it; fail here instead, before anything is changed.
grep -qxF "# beisl $new" RELEASENOTES.md \
    || die "RELEASENOTES.md has no '# beisl $new' section; write it first"

for f in Cargo.lock crates/beisl-gui/Cargo.lock; do
    git diff --quiet -- "$f" || die "$f has uncommitted changes; the lockfile check below needs a clean one"
done

today=$(date +%Y-%m-%d)

# --- cargo -----------------------------------------------------------------
# Line-anchored on the old value, so only [workspace.package] can match.
sed -i "s/^version = \"$old\"\$/version = \"$new\"/" Cargo.toml

# Lockfiles through cargo, offline: nothing may be resolved anew by a bump.
# The GUI workspace sees the root crates as path dependencies, so resolving
# its manifest is what rewrites their entries.
cargo update --workspace --offline --quiet
cargo metadata --offline --format-version 1 \
    --manifest-path crates/beisl-gui/Cargo.toml >/dev/null

# A bump that changed anything but our own version lines pulled in something
# unreviewed (see the lockfile rule in CLAUDE.md).
for f in Cargo.lock crates/beisl-gui/Cargo.lock; do
    stray=$(git diff -U0 -- "$f" | grep '^[-+][^-+]' \
        | grep -vxF -e "-version = \"$old\"" -e "+version = \"$new\"" || true)
    [ -z "$stray" ] || die "$f changed beyond the version lines:
$stray"
done

# --- AppStream ---------------------------------------------------------------
meta=crates/assets/data/metainfo/org.spritzwine.Beisl.metainfo.xml
esc=$(printf '%s' "$summary" | sed -e 's/&/\&amp;/g' -e 's/</\&lt;/g' -e 's/>/\&gt;/g')
if grep -q "<release version=\"$new\"" "$meta"; then
    # Run again for the same version: the entry exists, only its date moves.
    sed -i "s|<release version=\"$new\" date=\"[^\"]*\"|<release version=\"$new\" date=\"$today\"|" "$meta"
else
    awk -v v="$new" -v d="$today" -v s="$esc" '
        { print }
        /^  <releases>$/ {
            print "    <release version=\"" v "\" date=\"" d "\">"
            print "      <description>"
            print "        <p>"
            print "          " s
            print "        </p>"
            print "      </description>"
            print "    </release>"
            done = 1
        }
        END { if (!done) exit 1 }
    ' "$meta" > "$meta.new" || { rm -f "$meta.new"; die "no <releases> in $meta"; }
    mv "$meta.new" "$meta"
fi

# --- distro recipes ----------------------------------------------------------
sed -i -e "s/^pkgver=.*/pkgver=$new/" -e 's/^pkgrel=.*/pkgrel=1/' \
    -e "s/^sha256sums=.*/sha256sums=('SKIP')/" packaging/arch/PKGBUILD

sed -i "s/^Version:\( *\).*/Version:\1$new/" \
    packaging/fedora/beisl.spec packaging/opensuse/beisl.spec

# Fedora keeps its changelog in the spec (openSUSE's lives in OBS).
spec=packaging/fedora/beisl.spec
if ! grep -q " - $new-1\$" "$spec"; then
    who=$(sed -n 's/^# Maintainer: //p' packaging/arch/PKGBUILD)
    stamp=$(LC_ALL=C date '+%a %b %d %Y')
    awk -v h="* $stamp $who - $new-1" -v l="- Update to $new" '
        { print }
        /^%changelog$/ { print h; print l; print "" }
    ' "$spec" > "$spec.new" && mv "$spec.new" "$spec"
fi

# Both hashes are of artifacts this version has not published yet.
sed -i -e "s/^  version = \".*\";/  version = \"$new\";/" \
    -e 's/hash = "sha256-[^"]*";/hash = lib.fakeHash;/' packaging/nix/package.nix

# --- prove it ----------------------------------------------------------------
# The two guards: metainfo's newest release, and the recipes' versions.
cargo test --offline --quiet -p assets -p docs-gen

cat <<NOTE

bump-version: $old -> $new. Review with git diff, then by hand:

  1. commit, then: git tag v$new && git push origin main v$new
  2. once the release is published:
       packaging/arch/PKGBUILD     sha256sums (updpkgsums)
       packaging/nix/package.nix   both hashes (the first build prints them)
  3. the pages branch: git show main:RELEASENOTES.md > releasenotes.md
NOTE
