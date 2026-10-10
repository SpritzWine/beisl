# beisl for Nix.
#
# Built and run once, 2026-10-10, in a nixos/nix container (Nix 2.35.2,
# nixpkgs-unstable 26.05pre, rustc 1.95.0) with the sandbox on and the
# container's network off: evaluation, the offline cargo build of both
# workspaces, `cargo test`, `make check-packaging`, install, and all four
# binaries answering --version from the store path. The two GUI binaries got
# the rpath below, and every library name found in them exists on it (checked
# by name, not by opening a window); `beisl` and `beisl-mcp` kept the plain
# glibc rpath. NOT verified: the two hashes (no
# release carries the vendor tarball yet, and the v0.3.0 tag predates the
# Makefile targets this calls, so the published sources cannot be built as
# named today), and opening a window on a NixOS desktop.
#
# That run used the local artifacts instead of the published ones, which is
# how to build from a checkout until a release ships both:
#
#   (callPackage ./packaging/nix/package.nix { }).overrideAttrs (_: {
#     src = ./dist/beisl-0.3.0.tar.gz;             # make sdist
#     vendor = ./dist/beisl-0.3.0-vendor.tar.zst;  # make vendor
#   })
#
# Once a release carries both, with the hashes filled in:
#
#   nix-build -E 'with import <nixpkgs> { }; callPackage ./packaging/nix/package.nix { }'
#
# Deliberately not rustPlatform.buildRustPackage. beisl is two cargo workspaces
# with two lockfiles, and the GUI one pins libcosmic's git dependencies, which
# would mean an outputHashes entry per git source, kept in step by hand on
# every libcosmic bump. It would also hit the same wall cargo vendor does:
# gamebus-coupler reads a store list from outside its own package, and
# importCargoLock extracts packages one by one, exactly like cargo vendor.
#
# Upstream already solves all of that once, in `make vendor`: one tarball that
# is unpacked over the source and makes the build need no network. So this
# derivation takes two fixed sources and calls the Makefile, the same as every
# other recipe (see packaging/README.md). Two hashes instead of fourteen.
{
  lib,
  stdenv,
  fetchFromGitea,
  fetchurl,
  cargo,
  rustc,
  pkg-config,
  zstd,
  patchelf,
  libxkbcommon,
  wayland,
  vulkan-loader,
  libGL,
  libx11,
  libxcb,
  libxi,
  appstream,
  desktop-file-utils,
}:

stdenv.mkDerivation (finalAttrs: {
  pname = "beisl";
  version = "0.3.0";

  src = fetchFromGitea {
    domain = "codeberg.org";
    owner = "SpritzWine";
    repo = "beisl";
    rev = "v${finalAttrs.version}";
    hash = lib.fakeHash; # fill in on the first build
  };

  # Published beside the release by upstream's `make vendor`. Not yet attached
  # to a release as of 0.3.0; until it is, build it locally with `make vendor`
  # and point this at the file (overrideAttrs, see the top of the file).
  vendor = fetchurl {
    url = "https://codeberg.org/SpritzWine/beisl/releases/download/v${finalAttrs.version}/beisl-${finalAttrs.version}-vendor.tar.zst";
    hash = lib.fakeHash; # fill in on the first build
  };

  nativeBuildInputs = [
    cargo
    rustc
    pkg-config
    zstd
    patchelf
  ];

  buildInputs = [ libxkbcommon ];

  nativeCheckInputs = [
    appstream
    desktop-file-utils
  ];

  # vendor/ and .cargo/config.toml for both workspaces, over the source.
  prePatch = ''
    tar --zstd -xf ${finalAttrs.vendor}
  '';

  env.CARGO_NET_OFFLINE = "true";

  buildPhase = ''
    runHook preBuild
    export HOME=$TMPDIR
    make build generate
    runHook postBuild
  '';

  doCheck = true;
  checkPhase = ''
    runHook preCheck
    cargo test --workspace --locked
    make check-packaging
    runHook postCheck
  '';

  # The layout is upstream's: DESTDIR is the store path, PREFIX is empty.
  installPhase = ''
    runHook preInstall
    make install DESTDIR=$out PREFIX=
    runHook postInstall
  '';

  # libcosmic dlopens these, and a NixOS system has no global library path to
  # find them on. Added to the two GUI binaries' rpath ONLY, and as rpath, not
  # as a wrapper exporting LD_LIBRARY_PATH: `beisl` wraps the game, and its
  # environment is the game's environment. It must reach the game unchanged.
  #
  # The list is what `strings` finds in the built binaries: winit carries both
  # its Wayland and its X11 backend (libX11, libX11-xcb, libxcb, libXi, and
  # libxkbcommon-x11 from the libxkbcommon package), wgpu wants libvulkan and
  # libEGL (from libglvnd). Nothing names libXcursor or libwayland-cursor.
  postFixup = ''
    for gui in beisl-setup beisl-compat; do
      patchelf --add-rpath ${
        lib.makeLibraryPath [
          wayland
          libxkbcommon
          vulkan-loader
          libGL
          libx11
          libxcb
          libxi
        ]
      } $out/bin/$gui
    done
  '';

  meta = {
    description = "Find out what made a game stutter";
    longDescription = ''
      Beisl wraps a game launch, records frametimes alongside CPU samples, and
      attributes each slow frame to a layer: the game, Wine, DXVK or vkd3d, the
      graphics driver, or the kernel. perf does the sampling and is found on
      PATH at run time; without it beisl records frametimes only and says so.
    '';
    homepage = "https://codeberg.org/SpritzWine/beisl";
    license = lib.licenses.lgpl21Plus;
    platforms = [ "x86_64-linux" ];
    mainProgram = "beisl";
  };
})
