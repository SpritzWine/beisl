#!/bin/sh
# Build the portable AppImage: in the older-glibc container, from the source and
# vendor tarballs, with the network off.
#
#   packaging/appimage/build-in-container.sh        (run from the repo root)
#
# Three steps, and only the first two may touch the network:
#   1. `make sdist vendor`  - the two tarballs, from HEAD and the lockfiles
#   2. build the container image, and fetch the pinned appimagetool + runtime
#   3. unpack, build and wrap inside the container with --network none
#
# The result is dist/Beisl-<version>-<arch>.AppImage, owned by the caller.
# Set CONTAINER=podman to use podman instead of docker.
set -eu

engine="${CONTAINER:-docker}"
root=$(cd "$(dirname "$0")/../.." && pwd)
cd "$root"
version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n1)
arch=$(uname -m)
image=beisl-build:bookworm

make sdist vendor

"$engine" build -t "$image" -f packaging/appimage/Containerfile packaging/appimage

# The tool downloads are pinned by checksum in tools.sha256; fetch them out here
# so the container never needs to.
mkdir -p dist/tools
grep -v '^#' packaging/appimage/tools.sha256 | while read -r sum name url; do
    [ -f "dist/tools/$name" ] || curl -fsSL -o "dist/tools/$name" "$url"
    echo "$sum  dist/tools/$name" | sha256sum -c --quiet -
done

# Target dirs live under dist/ so a second run is incremental. They are this
# container's own: cargo does not track which glibc an artifact was built
# against, so sharing them with a host build would mix the two.
mkdir -p dist/portable
"$engine" run --rm --network none \
    --user "$(id -u):$(id -g)" \
    -e HOME=/work/home -e CARGO_HOME=/work/cargo-home -e CARGO_NET_OFFLINE=true \
    -v "$root/dist:/dist" -v "$root/dist/portable:/work" \
    "$image" sh -eu -c "
        rm -rf /work/src && mkdir -p /work/src /work/home /work/cargo-home
        tar -xzf /dist/beisl-$version.tar.gz -C /work/src --strip-components=1
        tar --zstd -xf /dist/beisl-$version-vendor.tar.zst -C /work/src
        cd /work/src
        make appimage DIST=/work/out TOOL_CACHE=/dist/tools \\
            CLI_TARGET=/work/target-cli GUI_TARGET=/work/target-gui
        make check-packaging
    "
cp "dist/portable/out/Beisl-$version-$arch.AppImage" dist/
echo "portable: dist/Beisl-$version-$arch.AppImage"
