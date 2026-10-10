# Packaging

Recipes for distributions that build from source. Both call `make install
DESTDIR=... PREFIX=/usr`, so the file layout lives in one place (the Makefile)
rather than being restated per distro.

| Path | For |
|---|---|
| `arch/PKGBUILD` | Arch Linux / AUR |
| `opensuse/beisl.spec`, `opensuse/_service` | openSUSE, via OBS |
| `*.desktop` | **generated** by `make generate`, not committed |

## The one awkward part

beisl builds two cargo workspaces. The main one is light and crates.io-only;
`crates/beisl-gui` is excluded from it because libcosmic pulls around 48
rev-pinned git dependencies. Everything is pinned by two committed lockfiles,
so builds are reproducible, but the git dependencies mean:

- **Arch** fetches them in `prepare()`, which is ordinary for a cargo package.
- **openSUSE** builds offline, so they must be vendored first. `_service` runs
  `cargo_vendor` against both workspaces; one run would miss the GUI's, since
  it has its own lockfile.

A distribution that forbids vendored git dependencies outright cannot package
the GUI today. The CLI half (`beisl`, `beisl-mcp`) has no such problem, and
splitting the package is the obvious answer if anyone needs it.

## What a package must not do

`beisl install` is the *tarball's* installer: it copies binaries out of an
extracted bundle into `~/.local`. A package is placed by the package manager
and must never run it. `make install` exists for exactly this reason.

perf is a recommendation, not a dependency: without it beisl records frametimes
and says what it cannot do. Nothing here needs setuid or capabilities at
install time - `beisl-setup` checks the system and explains the fixes, which
stay the user's to apply.
