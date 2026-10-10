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
| `opensuse/beisl.spec` | openSUSE, via OBS | airgapped |
| `nix/package.nix` | Nix / NixOS | airgapped; built in the Nix sandbox, not yet run on a NixOS desktop |
| `appimage/` | one runnable file, any distro | airgapped, in a container |
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
rpm rebuilds inside `unshare -n`, a namespace with no interfaces.

- **COPR** uses its "make srpm" method. `/.copr/Makefile` runs `make srpm`
  where COPR allows network, and the rpm is then built where it does not.
- **OBS**: run `make srpm SPEC=packaging/opensuse/beisl.spec` and upload the
  spec and the two tarballs. OBS's `cargo_vendor` service is deliberately not
  used; it would miss the GUI's separate lockfile and the dropped file.
- **Nix** fetches the same two artifacts as fixed-output sources, so it needs
  two hashes rather than one per git dependency. Built in a `nixos/nix`
  container with the sandbox on and the network off, tests included, from the
  local tarballs; its header shows how. The two hashes wait for the first
  release that carries this packaging and the vendor tarball. Unchecked on a
  real NixOS desktop: opening a window, whether a system-wide `beisl` is
  visible inside NixOS's Steam environment, and whether libcosmic's own icons
  render without a COSMIC icon theme.

Fedora means COPR here, not Fedora proper, for the reason COSMIC itself lives
in one: libcosmic's git dependencies cannot be satisfied from crates.io
packages. A submission to Fedora proper would need `bundled(crate(...))`
provides and the combined licence of everything vendored, which the spec does
not claim.

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
"Agreed design (2026-10-10)". One trap for anyone working on it: `APPDIR` and
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
