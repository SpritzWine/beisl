# Packaging

Recipes for distributions that build from source.

**The Makefile is the build system; a recipe is metadata plus four calls.** No
packaging logic lives here, so adding a distro does not mean reimplementing
anything, and changing what beisl installs does not mean editing every recipe.

| target | what a recipe uses it for |
|---|---|
| `make fetch` | download both workspaces' dependencies (network). **Arch only** |
| `make vendor` | one tarball that makes the build offline. **Everything else** |
| `make sdist` | the source tarball that goes with it (HEAD) |
| `make srpm SPEC=...` | both, wrapped as a source rpm for COPR or OBS |
| `make build generate` | compile, and render the man pages and desktop entries |
| `make install DESTDIR=... PREFIX=/usr` | stage everything, in FHS layout |
| `make check-packaging` | validate what a software centre and a linter read |

`make install MANIFEST=<file>` additionally writes every installed path, one
per line. The rpm spec feeds that to `%files -f`, so the package's file list
*is* upstream's install list: a file added to `make install` ships without
anyone touching the spec. That replaced fifteen hand-maintained paths, which
is exactly the kind of list that silently rots.

What stays per-distro is only what genuinely differs: dependency names and the
metadata syntax.

## Vendor by default; Arch is the exception

**Every recipe but Arch builds airgapped**, from `make vendor`. Not because
each distro demands it, but so the path most people's package is built on is
the one that is proven to work without a network, with the dependency tree
pinned by content rather than by a revision someone could move.

**Arch keeps `make fetch` and builds online.** The AUR fetches sources per
user build, and vendoring would hand every one of them a 71 MB tarball to
produce an 18 MB program (measured 2026-10-10: 657 crates, 949 MB unpacked).
Arch's own convention is `cargo fetch`, and an Arch user who hits a break
there is equipped to diagnose it and file something useful. Owner's call,
deliberately: the exception goes where the audience can absorb it.

The cost of two paths is that they can diverge, and they already did once -
`cargo vendor` dropped gamebus-coupler's store list while `cargo fetch` was
fine, so the bug lived only in the path nobody ran (see the blocker above).
Making the airgapped path the default is what keeps that from recurring: it
is now the common path, not the rare one. A new recipe uses `make vendor`
unless there is a reason in writing not to.

| Path | For | Builds |
|---|---|---|
| `arch/PKGBUILD` | Arch Linux / AUR | online (`make fetch`) |
| `fedora/beisl.spec`, `/.copr/Makefile` | Fedora, via COPR | airgapped |
| `fedora/build-in-container.sh` | COPR's rebuild, reproduced in a clean local container | airgapped |
| `debian/` | Debian 12+ and Ubuntu 22.04+, as a third-party .deb | airgapped, in a container (`debian/build-in-container.sh`) |
| `opensuse/beisl.spec` | openSUSE, via OBS | airgapped |
| `nix/package.nix` | Nix / NixOS | airgapped; built in the Nix sandbox, not yet run on a NixOS desktop |
| `appimage/` | one runnable file, any distro | airgapped, in a container |
| `smoke-test.sh` | installs a built rpm or .deb into a fresh container and checks it | |
| `vendor.sh`, `cache-coupler-data.sh` | what `make vendor` runs | |
| `*.desktop` | **generated** by `make generate`, not committed | |

## An airgapped build is two tarballs

`make vendor` writes `dist/beisl-<version>-vendor.tar.zst`. Unpacked over the
source tree it adds a `vendor/` directory and a `.cargo/config.toml` for
**both** workspaces, plus the one file cargo vendor drops (below). A recipe
therefore needs no vendoring knowledge of its own:

    unpack beisl-<version>.tar.gz
    unpack beisl-<version>-vendor.tar.zst over it
    CARGO_NET_OFFLINE=true make build generate
    make install DESTDIR=... PREFIX=/usr

Verified with the network actually gone, not merely unused: the Fedora source
rpm rebuilds inside `unshare -n`, a namespace with no interfaces, and every
build under "What was built where" ran in a container started with
`--network none`.

- **COPR** uses its "make srpm" method. `/.copr/Makefile` runs `make srpm`
  where COPR allows network, and the rpm is then built where it does not.
- **OBS**: run `make srpm SPEC=packaging/opensuse/beisl.spec` and upload the
  spec and the two tarballs. OBS's `cargo_vendor` service is deliberately not
  used; it would miss the GUI's separate lockfile and the dropped file.
- **Nix** fetches the same two artifacts as fixed-output sources, so it needs
  two hashes rather than one per git dependency. Built in a `nixos/nix`
  container with the sandbox on, tests included, from the published release.
  A version bump resets the two hashes and they are filled in again once that
  release is out; the header shows how to build from local tarballs until
  then. Unchecked on a
  real NixOS desktop: opening a window, whether a system-wide `beisl` is
  visible inside NixOS's Steam environment, and whether libcosmic's own icons
  render without a COSMIC icon theme.

Fedora means COPR here, not Fedora proper, for the reason COSMIC itself lives
in one: libcosmic's git dependencies cannot be satisfied from crates.io
packages. A submission to Fedora proper would need `bundled(crate(...))`
provides and the combined licence of everything vendored, which the spec does
not claim.

## Fedora

Every current Fedora ships a new enough rust (looked up
2026-10-22: 1.99.0 on 43 and 44, 1.99.1 on 45 in updates-testing and on
rawhide; the spec needs 1.93, libcosmic's rust-version). So the COPR chroots
are `fedora-43-x86_64` and `fedora-44-x86_64`, then `fedora-45-x86_64` once it
is released, and `fedora-rawhide-x86_64` if wanted. 43 and 44 are built below;
45 and rawhide are not.

Fedora's `%build` exports its own `RUSTFLAGS` (opt-level 3, full debuginfo,
no strip, frame pointers), and they win over the cargo profile. So rpm splits
a `beisl-debuginfo` and `beisl-debugsource` as it does for C, and the rpm's
binaries are built differently from the release tarball's and the .deb's,
which follow the profile (size-optimised, stripped). The spec used to disable
debuginfo on the belief that cargo had stripped already; it had not, and the
debug info was thrown away while the symbol table shipped.

The GUI loads its display and GPU libraries with `dlopen`, which rpm's
dependency generator cannot see: Wayland or X11 to open a window, and Vulkan
or EGL to render, falling back to software without them. Found by reading the
binaries (some names are assembled from two immediates and do not show up in
`strings`). The spec recommends them by soname, weakly, because the CLI needs
none of them and a desktop has them all.

## Debian and Ubuntu: a third-party .deb

**This is not Debian-policy packaging.** Both workspaces build from the
vendored crates, not from Debian's `librust-*-dev` packages, which have no
counterpart for libcosmic's rev-pinned git dependencies; `debian/copyright`
names beisl's licence, not those of the hundreds of crates linked into it.
What does come from the distribution's archive is everything else: the
toolchain, debhelper, and every library and tool the build touches. No
rustup, no upstream toolchain.

The obstacle is the GUI's rustc >= 1.93. What each archive offers, looked up
in containers on 2026-10-22:

| release | default rustc | newer, still from the archive | builds? |
|---|---|---|---|
| Debian 12 | 1.63 | `rustc-web` 1.96.0 (bookworm-security) | yes |
| Debian 13 | 1.85.1 | `rustc-web` 1.96.0 (trixie); `rustc` 1.96.1 in trixie-backports | yes |
| Debian testing | 1.97.1 | | yes |
| Ubuntu 24.04 | 1.75 | `rustc-1.91` at most | **no** |
| Ubuntu 26.04 | 1.93.1 | | yes |

`rustc-web` and `cargo-web` are the toolchain Debian keeps current in its
stable releases for building Firefox and Chromium. They provide `rustc` and
`cargo` at their own version, so the plain `rustc (>= 1.93)` Build-Depends
selects them on Debian 12 and 13 with no extra repository or pinning.

**Where a .deb installs is decided by where it was built**, as for the
AppImage. Built on Debian 12 it depends on `libc6 (>= 2.35)`, `libgcc-s1` and
`libxkbcommon0` only, and passes the smoke test on Debian 12, 13 and testing
and Ubuntu 22.04, 24.04 and 26.04. That is how Ubuntu 24.04 (and 22.04) get
beisl at all: they cannot build it, and what they install is the Debian 12
build, which is a Debian package on Ubuntu rather than Ubuntu packaging.

Mechanics, all in `debian/rules`: it builds through the Makefile like every
other recipe, installs straight into the package so the file list is
upstream's install list, keeps `CARGO_HOME` inside the tree, and stops
`dh_clean` from deleting the vendored crates' `Cargo.toml.orig` files, which
cargo checksums (found by the first container build). The recommendations
mirror the rpm's: `linux-perf | linux-tools-generic` (Ubuntu 24.04 has no
`linux-perf`), `mangohud`, and the libraries the GUI loads at runtime.

Not done: there is no source package, only binary builds
(`dpkg-buildpackage -b`). A `.dsc` for Launchpad or OBS would need the vendor
tarball as a second orig component, whose layout does not match what
dpkg-source expects of one. Every build is versioned `0.4.0-1` whatever its
base, which is fine for downloading one file and not for an apt repository
carrying builds for several releases side by side.

## What was built where

On 2026-10-22, x86_64 only. Each build started from `make sdist vendor`'s two
tarballs in a fresh container holding only the recipe's build dependencies
(plus zstd for the .deb, to unpack the vendor tarball), ran with
`--network none`, tests included, and as an unprivileged user. Each
package was then installed into a fresh container of every release listed,
with hard dependencies only, and `smoke-test.sh` checked: `beisl --version`,
`beisl --check` exiting 0 or 1, `beisl-mcp` answering `initialize`,
`beisl-setup --version` and `beisl-compat --version` with no display, `ldd`
resolving all four binaries, the shipped desktop entries, metainfo, icon and
man page, and that installing the Recommends makes every library the GUI loads
at runtime resolvable.

| recipe, build base | toolchain | installed and smoke-tested on | linter |
|---|---|---|---|
| `fedora/beisl.spec`, fedora:44 | rust 1.99.0 | fedora:44 | rpmlint 2.8.0: clean with `beisl.rpmlintrc` |
| `fedora/beisl.spec`, fedora:43 | rust 1.99.0 | fedora:43 | rpmlint 2.8.0: two duplicate-file findings, below |
| `debian/`, debian:12 | rustc-web 1.96.0 | debian:12, debian:13, debian:testing, ubuntu:22.04, ubuntu:24.04, ubuntu:26.04 | lintian 2.116.3 (bookworm) and 2.142.0 (testing): one W, below |
| `debian/`, debian:13 | rustc-web 1.96.0 | debian:13 | lintian 2.122.0: the same W |
| `debian/`, debian:testing | rustc 1.97.1 | debian:testing | lintian 2.142.0: the same W |
| `debian/`, ubuntu:26.04 | rustc 1.93.1 | ubuntu:26.04 | lintian 2.129.0ubuntu2.1: the same W |
| `debian/`, ubuntu:24.04 | none new enough | stops at Build-Depends | |

The native builds depend on their own base's glibc, which is the argument for
shipping the oldest one: `libc6 (>= 2.39)` built on Debian 13, `(>= 2.43)` on
testing and Ubuntu 26.04, against `(>= 2.35)` on Debian 12. On Ubuntu the
script names the base's series in the changelog instead of `unstable`, which
Ubuntu's lintian rejects as an error.

To reproduce, from the repository root:

    packaging/fedora/build-in-container.sh 44      # or 43; dist/rpm/fc44/
    packaging/debian/build-in-container.sh debian:12   # or debian:13,
                                                   # debian:testing, ubuntu:26.04
    packaging/smoke-test.sh fedora:44 dist/rpm/fc44/RPMS/x86_64/beisl-*.fc44.x86_64.rpm
    packaging/smoke-test.sh ubuntu:24.04 dist/deb/debian-12/beisl_*_amd64.deb

What the linters still report, and why it stays:

- rpmlint `incorrect-fsf-address` (E): `LICENSE` carries the FSF's postal
  address from before its 2024 move. A licence text is upstream's to update;
  filtered in `fedora/beisl.rpmlintrc` so that anything new fails the build.
- rpmlint `inconsistent-file-extension` on the vendor tarball (W): rpmlint
  expects libmagic to say "ZSTD compressed", and it says "Zstandard". Filtered.
- Fedora 43 only, rpmlint `files-duplicated-waste` (E, in debugsource) and
  `files-duplicate` (W, the two identical icons): 43's rpm-build does not
  hardlink identical files, 44's does. The debugsource duplicates are crates
  vendored by both workspaces. Cosmetic, and gone with 43.
- lintian `initial-upload-closes-no-bugs` (W): a first changelog entry for
  Debian's archive closes an ITP bug, and this package is not in Debian. Gone
  with the next changelog entry.
- lintian, info only: `desktop-entry-lacks-keywords-entry` for two desktop
  entries (upstream metadata, generated by docs-gen, not added here); the
  bookworm lintian also lists `hardening-no-fortify-functions` (Rust code has
  no `_FORTIFY_SOURCE` calls) and `spelling-error-in-binary` false positives.

Not verified:

- **COPR itself.** No COPR project exists yet. The container reproduces its
  split (source rpm first, then BuildRequires only, offline, unprivileged),
  not its infrastructure.
- Fedora 45 and rawhide, and the openSUSE spec: not built here.
- **A window.** No GUI was opened anywhere. The runtime-loaded libraries were
  found by reading the binaries and checked to be installable, not watched
  being loaded.
- **A trace.** The test containers have no perf, so `beisl --check` reports
  the record tier as none there; nothing recorded a game on these packages.

## The AppImage

`make appimage` stages with `make install` like every other recipe and adds
only what the format wants: `AppRun`, and a desktop entry and icon at the top
level. Nothing is bundled. `appimage/build.sh` fetches the two things it needs,
appimagetool and the runtime, pinned by checksum in `appimage/tools.sha256`.

**Where it runs is decided by where it was built**, not by the format: a binary
needs a glibc at least as new as the one it was linked against. So `build.sh`
refuses binaries that need more than glibc 2.36 (`ALLOW_NEW_GLIBC=1` makes a
local test image), and the image that ships comes from

    packaging/appimage/build-in-container.sh

which builds in Debian 12 from the two tarballs with the network off. Measured:
the result needs glibc 2.35 at most and starts on Ubuntu 22.04 and Debian 12,
where a binary built on Fedora 44 (glibc 2.43) refuses to. The same is true of
the release tarball: built on a current distro, it runs only there. The GUI
additionally needs the host's libxkbcommon, which every desktop has.

What the AppImage does once it is on a user's machine - launchers instead of
copied binaries, what they check, what setup shows - is in `DESIGN.md`,
the "AppImage" section. One trap for anyone working on it: `APPDIR` and
`APPIMAGE` are inherited by every shell opened from a terminal that is itself
an AppImage. Do not name a variable after them, and do not believe them
without checking (`preflight::appimage::running` does).

## Two workspaces, and one real blocker

beisl builds two cargo workspaces. The main one is light and crates.io-only;
`crates/beisl-gui` is excluded from it because libcosmic pulls around 48
rev-pinned git dependencies. Everything is pinned by two committed lockfiles.

**The git dependencies are not a problem.** `cargo vendor --locked` handles
them like any other source: measured here, it vendored 657 crates including
every `cosmic-*` and `iced` dependency, emitted the `replace-with` stanzas for
13 git sources, and the whole libcosmic tree then compiled with `--offline`.
Network happens when the source service runs, not when the package builds,
which is how openSUSE ships COSMIC itself.

**What does break an offline build is `gamebus-coupler`.** Its `build.rs`
reads `../gamedb/stores.toml`, a file outside its own package directory, so
`cargo vendor` extracts the crate without it and the build panics asking for
a git submodule that a vendored tree does not have. Arch is unaffected, since
`cargo fetch` does a real clone with submodules.

The file is not missing, only unvendored: cargo's git checkout still has it,
because cargo clones git dependencies with their submodules. So `make vendor`
caches it back into the tarball, and the build then finds it without reaching
for the network. The vendored crate sits at `vendor/gamebus-coupler/`, so the relative path its
`build.rs` already uses resolves to `vendor/gamedb/stores.toml`. Verified: the
whole GUI workspace then compiles with `--offline`. No upstream change needed
to package today, and the script removes itself from the picture once the
coupler carries its own copy (reported upstream 2026-10-10).

The CLI half (`beisl`, `beisl-mcp`) does not link the coupler and has no such
problem, so splitting the package is the fallback if the fix takes a while.

## What a package must not do

`beisl install` is the *tarball's* installer: it copies binaries out of an
extracted bundle into `~/.local`. A package is placed by the package manager
and must never run it. `make install` exists for exactly this reason.

perf is a recommendation, not a dependency: without it beisl records frametimes
and says what it cannot do. Nothing here needs setuid or capabilities at
install time - `beisl-setup` checks the system and explains the fixes, which
stay the user's to apply.
