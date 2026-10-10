# Packaging

Recipes for distributions that build from source. Both call `make install
DESTDIR=... PREFIX=/usr`, so the file layout lives in one place (the Makefile)
rather than being restated per distro.

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
`cargo fetch` does a real clone with submodules. Reported upstream
(2026-10-10); until it is fixed, the openSUSE recipe needs the coupler's
`gamedb/stores.toml` restored into the vendor tree by hand, or the GUI left
out of the package.

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
