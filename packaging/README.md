# Packaging

Recipes for distributions that build from source.

**The Makefile is the build system; a recipe is metadata plus four calls.** No
packaging logic lives here, so adding a distro does not mean reimplementing
anything, and changing what beisl installs does not mean editing every recipe.

| target | what a recipe uses it for |
|---|---|
| `make fetch` | download both workspaces' dependencies (network). **Arch only** |
| `make vendor` | the same, for a build that must then be offline. **Everything else** |
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

| Path | For |
|---|---|
| `arch/PKGBUILD` | Arch Linux / AUR |
| `opensuse/beisl.spec`, `opensuse/_service` | openSUSE, via OBS |
| `*.desktop` | **generated** by `make generate`, not committed |

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
because cargo clones git dependencies with their submodules. So the vendor
step caches it back, and the build then finds it without reaching for the
network:

    make vendor

`make vendor` does both steps. The vendored crate sits at `vendor/gamebus-coupler/`, so the relative path its
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
