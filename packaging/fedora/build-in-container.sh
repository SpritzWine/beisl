#!/bin/sh
# Rebuild the Fedora rpm the way COPR does, in a clean container: a source rpm
# from the two tarballs, only its BuildRequires installed, then rpmbuild with
# the network off, as an unprivileged user like mock's. rpmlint runs after.
#
#   packaging/fedora/build-in-container.sh [release]      (default: 44)
#
# The source rpm and the rpms are made under --network none; only the images
# and the linter are fetched. Output lands in dist/rpm/fc<release>/.
# Set CONTAINER=podman to use podman instead of docker.
set -eu

engine="${CONTAINER:-docker}"
release="${1:-44}"
root=$(cd "$(dirname "$0")/../.." && pwd)
cd "$root"
version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n1)
image="beisl-rpmbuild:f$release"
out="dist/rpm/fc$release"

make sdist vendor

# The builder has the caller's uid, so what it writes is the caller's.
"$engine" build -t "$image" - <<EOF
FROM docker.io/library/fedora:$release
RUN dnf -y install rpm-build 'dnf5-command(builddep)' shadow-utils && dnf clean all \
 && useradd -m -u $(id -u) builder
EOF

rm -rf "$out" && mkdir -p "$out/SOURCES"
cp "dist/beisl-$version.tar.gz" "dist/beisl-$version-vendor.tar.zst" "$out/SOURCES/"
"$engine" run --rm --network none --user builder \
    -v "$root:/src:ro" -v "$root/$out:/work" "$image" \
    rpmbuild -bs /src/packaging/fedora/beisl.spec --define '_topdir /work'
srpm=$(cd "$out/SRPMS" && ls beisl-*.src.rpm)

# Exactly the source rpm's BuildRequires, as COPR resolves them, so the build
# cannot lean on anything the spec does not declare.
"$engine" build -t "$image-deps" -f - "$out/SRPMS" <<EOF
FROM $image
COPY $srpm /tmp/
RUN dnf -y builddep /tmp/$srpm && dnf clean all
EOF

"$engine" run --rm --network none --user builder -v "$root/$out:/work" "$image-deps" \
    rpmbuild --rebuild "/work/SRPMS/$srpm" --define '_topdir /work'

# Not in the build image: rpmlint pulls in tools the build must not see.
"$engine" run --rm -v "$root/$out:/work:ro" \
    -v "$root/packaging/fedora/beisl.rpmlintrc:/beisl.rpmlintrc:ro" \
    "docker.io/library/fedora:$release" sh -c 'dnf -y install rpmlint >/dev/null 2>&1 \
        && rpmlint -r /beisl.rpmlintrc /work/SRPMS/*.rpm /work/RPMS/*/*.rpm'
echo "rpms: $out/RPMS/"
