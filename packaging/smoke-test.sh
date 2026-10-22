#!/bin/sh
# Install a built package into a fresh container and check that what it ships
# starts and finds its libraries.
#
#   packaging/smoke-test.sh <image> <package file>
#     e.g. packaging/smoke-test.sh fedora:44 dist/rpm/fc44/RPMS/x86_64/beisl-*.rpm
#          packaging/smoke-test.sh ubuntu:24.04 dist/deb/debian-12/beisl_*.deb
#
# Hard dependencies only at first, no Recommends: a library the package forgot
# to declare then fails here rather than on a machine where something else
# happened to pull it in. The Recommends are added at the end, to check they
# cover what the GUI loads at runtime. No window is opened; the container has
# no display and the GUI binaries are only asked for --version. The network is
# used for the distro's packages.
# Set CONTAINER=podman to use podman instead of docker.
set -eu

if [ "${1:-}" != --inside ]; then
    usage="usage: smoke-test.sh <image> <package file>"
    image="${1:?$usage}"
    file="${2:?$usage}"
    [ -f "$file" ] || { echo "smoke-test: no such file: $file" >&2; exit 1; }
    dir=$(cd "$(dirname "$file")" && pwd)
    name=$(basename "$file")
    self="$(cd "$(dirname "$0")" && pwd)/smoke-test.sh"
    exec "${CONTAINER:-docker}" run --rm \
        -v "$dir/$name:/pkg/$name:ro" -v "$self:/smoke-test.sh:ro" \
        "$image" sh /smoke-test.sh --inside "/pkg/$name"
fi

pkg="$2"
fail() { echo "smoke-test: FAIL: $*" >&2; exit 1; }
step() { echo "--- $*"; }

. /etc/os-release
step "install into $PRETTY_NAME"
case "$pkg" in
*.rpm)
    dnf -y -q --setopt=install_weak_deps=False install "$pkg"
    version=$(rpm -q --qf '%{VERSION}' beisl)
    files=$(rpm -ql beisl)
    ;;
*.deb)
    export DEBIAN_FRONTEND=noninteractive
    apt-get update -qq
    apt-get install -y -qq --no-install-recommends "$pkg" >/dev/null
    version=$(dpkg-query -W -f '${Version}' beisl)
    version=${version%-*}
    files=$(dpkg -L beisl)
    ;;
*) fail "not an rpm or a deb: $pkg" ;;
esac

step "beisl --version"
out=$(beisl --version)
echo "$out"
[ "$out" = "beisl $version" ] || fail "expected 'beisl $version'"

# 1 means a required check needs setup (no perf in a bare container), which is
# the honest answer here; anything else is a crash.
step "beisl --check"
s=0; beisl --check || s=$?
[ "$s" -le 1 ] || fail "beisl --check exited $s"

step "beisl-mcp answers initialize"
req='{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"smoke-test","version":"0"}}}'
out=$(echo "$req" | timeout 10 beisl-mcp)
echo "$out"
echo "$out" | grep -q '"serverInfo"' && echo "$out" | grep -q "\"version\":\"$version\"" \
    || fail "no serverInfo for beisl $version"

for gui in beisl-setup beisl-compat; do
    step "$gui --version"
    out=$(env -u DISPLAY -u WAYLAND_DISPLAY "$gui" --version)
    echo "$out"
    echo "$out" | grep -q "$version" || fail "$gui does not report $version"
done

step "ldd resolves every shipped binary"
for b in beisl beisl-mcp beisl-setup beisl-compat; do
    p=$(command -v "$b")
    ldd "$p" | sed "s|^|$b: |"
    ! ldd "$p" | grep -q 'not found' || fail "$b has unresolved libraries"
done

step "shipped files"
for f in /usr/share/applications/org.spritzwine.Beisl.desktop \
         /usr/share/applications/org.spritzwine.BeislCompat.desktop \
         /usr/share/applications/org.spritzwine.BeislSession.desktop \
         /usr/share/metainfo/org.spritzwine.Beisl.metainfo.xml \
         /usr/share/icons/hicolor/scalable/apps/org.spritzwine.Beisl.svg \
         /usr/share/man/man1/beisl.1; do
    echo "$files" | grep -q "^$f" || fail "package does not ship $f"
done
echo "$files" | wc -l | sed 's/$/ paths in the package/'

# What the GUI dlopens, found in the binaries (2026-10-22): no package manager
# sees these, so the recipes declare them as Recommends. Installing those must
# make every one resolvable, or the declaration has drifted from the binaries.
step "the Recommends cover what the GUI loads at runtime"
case "$pkg" in
*.rpm)
    rpm -q --recommends beisl | grep '\.so' | xargs -d '\n' dnf -y -q install ;;
*.deb)
    dpkg-query -W -f '${Recommends}' beisl | tr ',' '\n' \
        | grep -v -e perf -e mangohud | sed 's/ //g' \
        | xargs apt-get install -y -qq --no-install-recommends >/dev/null ;;
esac
for so in libwayland-client.so.0 libwayland-egl.so.1 libX11.so.6 libX11-xcb.so.1 \
          libXi.so.6 libxcb.so.1 libxkbcommon-x11.so.0 libvulkan.so.1 libEGL.so.1; do
    ldconfig -p | grep -qF "$so (" || fail "$so is not provided by the Recommends"
    echo "$so: found"
done

echo "smoke-test: PASS ($PRETTY_NAME, beisl $version)"
