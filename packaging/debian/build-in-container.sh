#!/bin/sh
# Build the .deb in a clean container from the two tarballs, with the network
# off, then run lintian on it.
#
#   packaging/debian/build-in-container.sh [image]     (default: debian:12)
#
# The toolchain is the image's own archive's: rustc/cargo >= 1.93 come from
# rustc-web/cargo-web on Debian 12 and 13, from rustc/cargo on Debian testing
# and Ubuntu 26.04. Ubuntu 24.04 has nothing new enough, so the build stops at
# its Build-Depends there.
#
# Where the .deb installs is decided by the build base, as for the AppImage: a
# binary needs a glibc at least as new as the one it was linked against. Built
# on Debian 12 it needs glibc 2.35 and installs on Debian 12 and Ubuntu 22.04
# and everything newer; built on testing it needs 2.43 (measured 2026-10-22).
#
# This is a third-party .deb, not Debian-policy packaging: the crates are
# vendored, not Debian's (see rules). Output: dist/deb/<image>/, owned by the
# caller. Set CONTAINER=podman to use podman instead of docker.
set -eu

engine="${CONTAINER:-docker}"
base="${1:-debian:12}"
root=$(cd "$(dirname "$0")/../.." && pwd)
cd "$root"
version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n1)
tag=$(echo "$base" | tr ':/' '--')
image="beisl-debbuild:$tag"
out="dist/deb/$tag"

make sdist vendor

# Build-Depends as apt resolves them from debian/control, which pulls in
# build-essential too, plus zstd to unpack the vendor tarball. The builder has
# the caller's uid (Ubuntu images already have one: uid 1000 is "ubuntu"),
# so what it writes is the caller's.
"$engine" build -t "$image" -f - packaging/debian <<EOF
FROM docker.io/library/$base
COPY control changelog /tmp/beisl/debian/
RUN apt-get update \
 && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends zstd \
 && DEBIAN_FRONTEND=noninteractive apt-get build-dep -y --no-install-recommends /tmp/beisl \
 && rm -rf /var/lib/apt/lists/* /tmp/beisl \
 && (getent passwd $(id -u) >/dev/null || useradd -m -u $(id -u) builder)
EOF

rm -rf "$out" && mkdir -p "$out"
"$engine" run --rm --network none --user "$(id -u)" \
    -v "$root/dist:/dist:ro" -v "$root/packaging/debian:/debian:ro" -v "$root/$out:/out" \
    "$image" sh -eu -c "
        mkdir /out/build && cd /out/build
        tar -xzf /dist/beisl-$version.tar.gz
        tar --zstd -xf /dist/beisl-$version-vendor.tar.zst -C beisl-$version
        cp -r /debian beisl-$version/debian && rm beisl-$version/debian/build-in-container.sh
        cd beisl-$version
        # The changelog names Debian's unstable; Ubuntu's tools want one of its series.
        . /etc/os-release
        [ \"\$ID\" != ubuntu ] || sed -i \"1s/) unstable;/) \$VERSION_CODENAME;/\" debian/changelog
        dpkg-buildpackage -b -uc -us
        cd /out && mv build/*.deb build/*.buildinfo build/*.changes . && rm -rf build
    "

# Not in the build image: lintian pulls in tools the build must not see.
"$engine" run --rm -v "$root/$out:/out:ro" "docker.io/library/$base" sh -c '
    apt-get update >/dev/null && apt-get install -y lintian >/dev/null 2>&1 \
        && lintian --version && lintian -I /out/*.changes'
echo "debs: $out/"
