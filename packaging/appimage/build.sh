#!/bin/sh
# Turn a staged install into an AppImage.
#
#   build.sh <AppDir> <output.AppImage> <tool-cache-dir>
#
# <AppDir> is what `make install DESTDIR=<AppDir> PREFIX=/usr` produced. This
# adds only what the format asks for on top of that: the entry point, and a
# desktop entry and icon at the top level. Nothing is bundled; beisl's binaries
# need only glibc and libxkbcommon, and everything else is the host's.
#
# Environment:
#   GLIBC_MAX   highest glibc version the binaries may require (default 2.36).
#               An AppImage built against a newer one starts only on systems
#               at least that new, which defeats the point of shipping one.
#   ALLOW_NEW_GLIBC=1   build anyway, for a local test image.
set -eu

appdir="${1:?usage: build.sh <AppDir> <output.AppImage> <tool-cache-dir>}"
out="${2:?usage: build.sh <AppDir> <output.AppImage> <tool-cache-dir>}"
cache="${3:?usage: build.sh <AppDir> <output.AppImage> <tool-cache-dir>}"
here=$(cd "$(dirname "$0")" && pwd)
id=org.spritzwine.Beisl
arch=$(uname -m)

# --- the highest glibc symbol version any binary asks for ----------------------
glibc_max="${GLIBC_MAX:-2.36}"
needed=$(for b in "$appdir"/usr/bin/*; do
    objdump -T "$b" 2>/dev/null | grep -o 'GLIBC_[0-9.]*' || true
done | sed 's/GLIBC_//' | sort -t. -k1,1n -k2,2n -k3,3n | tail -n1)
newest=$(printf '%s\n%s\n' "$needed" "$glibc_max" | sort -t. -k1,1n -k2,2n -k3,3n | tail -n1)
if [ -n "$needed" ] && [ "$newest" != "$glibc_max" ]; then
    echo "build.sh: the binaries need glibc $needed, newer than the $glibc_max ceiling." >&2
    if [ "${ALLOW_NEW_GLIBC:-}" = 1 ]; then
        echo "build.sh: ALLOW_NEW_GLIBC=1, building a NON-PORTABLE image anyway." >&2
    else
        echo "build.sh: build on an older base (see packaging/README.md), or set" >&2
        echo "build.sh: ALLOW_NEW_GLIBC=1 for an image that only runs on systems this new." >&2
        exit 1
    fi
fi

# --- what the format wants at the top of the AppDir ---------------------------
install -m755 "$here/AppRun" "$appdir/AppRun"
cp "$appdir/usr/share/applications/$id.desktop" "$appdir/$id.desktop"
cp "$appdir/usr/share/icons/hicolor/scalable/apps/$id.svg" "$appdir/$id.svg"
ln -sf "$id.svg" "$appdir/.DirIcon"

# --- the two pinned downloads -------------------------------------------------
mkdir -p "$cache"
fetch() { # <file name> -> path, verified
    line=$(grep -v '^#' "$here/tools.sha256" | grep "  $1  ") || {
        echo "build.sh: $1 is not pinned in tools.sha256" >&2
        exit 1
    }
    sum=${line%%  *}
    url=${line##*  }
    [ -f "$cache/$1" ] || curl -fsSL -o "$cache/$1" "$url"
    echo "$sum  $cache/$1" | sha256sum -c --quiet - >&2 || {
        echo "build.sh: $cache/$1 does not match its pinned checksum; not using it" >&2
        exit 1
    }
    echo "$cache/$1"
}
tool=$(fetch "appimagetool-1.9.1-$arch.AppImage")
runtime=$(fetch "runtime-20251108-$arch")
chmod +x "$tool"

# Run the tool unpacked: build containers have no FUSE. The runtime is passed
# explicitly so the tool does not download one of its own choosing.
mkdir -p "$(dirname "$out")"
ARCH=$arch "$tool" --appimage-extract-and-run \
    --runtime-file "$runtime" "$appdir" "$out" >&2
chmod +x "$out"
echo "appimage: $out ($(du -h "$out" | cut -f1))"
