# One-stop build & package for the whole of beisl, both workspaces
# (the main one and the excluded beisl-gui — see Cargo.toml for why they're split).
#
#   make            # build everything + produce dist/beisl-<version>-<arch>.tar.gz
#   make build      # just compile both workspaces (release)
#   make clean      # remove dist/ only (cargo targets kept — cheap; incremental
#                   #   rebuilds still pick up source changes on the current branch)
#   make fetch      # download both workspaces' dependencies (needs network).
#                   #   The Arch recipe's path, and only that one
#   make vendor     # one tarball that makes the build OFFLINE: unpacked over
#                   #   the source it adds both workspaces' vendor/ and cargo
#                   #   config. Every other distro's path, so the common one is
#                   #   the one proven to need no network
#   make sdist      # the source tarball that goes with it (git archive of HEAD)
#   make srpm SPEC=packaging/<distro>/beisl.spec
#                   # sdist + vendor wrapped as a source rpm; what COPR and OBS
#                   #   build from, with the network off
#   make install    # stage a system install into $(DESTDIR)$(PREFIX); set
#                   #   MANIFEST=<file> to also write the list of installed paths
#   make check-packaging
#                   # validate what a software centre and a packager will read
#
# Those four are the packaging contract: a distro recipe supplies metadata and
# calls them, so the build logic lives here once instead of in every recipe.
# See packaging/README.md.
#   make distclean  # clean + drop BOTH cargo target dirs, forcing a from-scratch
#                   #   rebuild. Use after a branch switch if you don't trust the
#                   #   incremental state — `make distclean && make`. Slower: the
#                   #   ~48-crate beisl-gui git tree recompiles.
#
# The tarball is the "extracted tree" the README's install section talks about:
# beisl and beisl-setup side by side, which is the layout `beisl install` expects.
# The udev/tmpfiles data files are include_str!-embedded in preflight, so they
# don't need to ship separately.

VERSION := $(shell sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n1)
ARCH    := $(shell uname -m)
DIST    := dist
BUNDLE  := beisl-$(VERSION)

# Target dirs, overridable so a build can put them somewhere persistent -- e.g. CI on
# the self-hosted runner points them at a bind-mounted cache disk (ci/runner/), which
# is what makes a release build after the first one incremental instead of recompiling
# the ~630-crate libcosmic/wgpu tree from cold. Two dirs because the GUI is its own
# workspace (own lockfile) and must not drag that tree into the light CLI build.
# Defaults are the in-tree paths, so a plain `make` behaves exactly as before.
CLI_TARGET ?= target
GUI_TARGET ?= crates/beisl-gui/target

# Where `make install` stages a system install. A distro package sets DESTDIR to
# its build root and leaves PREFIX at /usr; `make install PREFIX=$HOME/.local` is
# the same layout for one user. This is NOT what `beisl install` does: that is the
# tarball's own user-level install, which copies binaries out of an extracted
# bundle. A packaged beisl is placed by the package manager and must never write
# into the system itself.
DESTDIR ?=
PREFIX  ?= /usr/local

# Set to a path and `make install` also writes every installed file there, one
# per line, relative to DESTDIR. An rpm spec feeds it to `%files -f`, which is
# what stops a package's file list drifting from what upstream installs.
MANIFEST ?=

.PHONY: all build build-cli build-gui man generate package install fetch vendor \
        sdist srpm check-packaging clean distclean

all: package

build: build-cli build-gui

build-cli:
	cargo build --release --workspace --target-dir $(CLI_TARGET)

# --target-dir is resolved relative to the cwd cargo runs in, so pass it absolute:
# this recipe has already cd'd into the GUI workspace.
build-gui:
	cd crates/beisl-gui && cargo build --release --target-dir $(abspath $(GUI_TARGET))

# docs/*.1.md -> man/*.1 — pure cargo (the docs-gen crate), so CI needs no
# distro markdown tool. The same files feed the binaries' --help via include_str!.
# Man pages and the packaging desktop entries, both rendered from the sources the
# binaries themselves embed. `man` stays as the name people already type.
generate man:
	cargo run --release -p docs-gen --target-dir $(CLI_TARGET)

package: build generate
	rm -rf $(DIST)/$(BUNDLE)
	mkdir -p $(DIST)/$(BUNDLE)/man
	install -m755 $(CLI_TARGET)/release/beisl $(DIST)/$(BUNDLE)/
	install -m755 $(CLI_TARGET)/release/beisl-mcp $(DIST)/$(BUNDLE)/
	install -m755 $(GUI_TARGET)/release/beisl-setup $(DIST)/$(BUNDLE)/
	install -m755 $(GUI_TARGET)/release/beisl-compat $(DIST)/$(BUNDLE)/
	install -m644 README.md LICENSE $(DIST)/$(BUNDLE)/
	install -m644 man/*.1 $(DIST)/$(BUNDLE)/man/
	tar -C $(DIST) -czf $(DIST)/$(BUNDLE)-$(ARCH).tar.gz $(BUNDLE)
	@echo "packaged: $(DIST)/$(BUNDLE)-$(ARCH).tar.gz"

# Everything a distro package ships, in the layout the FHS and the freedesktop
# specs expect. Depends on `man` for the rendered pages and on the release build.
install: build generate
	install -Dm755 $(CLI_TARGET)/release/beisl        $(DESTDIR)$(PREFIX)/bin/beisl
	install -Dm755 $(CLI_TARGET)/release/beisl-mcp    $(DESTDIR)$(PREFIX)/bin/beisl-mcp
	install -Dm755 $(GUI_TARGET)/release/beisl-setup  $(DESTDIR)$(PREFIX)/bin/beisl-setup
	install -Dm755 $(GUI_TARGET)/release/beisl-compat $(DESTDIR)$(PREFIX)/bin/beisl-compat
	for m in man/*.1; do \
	    install -Dm644 "$$m" "$(DESTDIR)$(PREFIX)/share/man/man1/$$(basename $$m)"; \
	done
	install -Dm644 crates/assets/data/metainfo/org.spritzwine.Beisl.metainfo.xml \
	    $(DESTDIR)$(PREFIX)/share/metainfo/org.spritzwine.Beisl.metainfo.xml
	install -Dm644 crates/assets/data/icons/icon-glass-pour.svg \
	    $(DESTDIR)$(PREFIX)/share/icons/hicolor/scalable/apps/org.spritzwine.Beisl.svg
	install -Dm644 crates/assets/data/icons/icon-glass-pour.svg \
	    $(DESTDIR)$(PREFIX)/share/icons/hicolor/scalable/apps/org.spritzwine.BeislSession.svg
	install -Dm644 crates/assets/data/icons/icon-symbolic.svg \
	    $(DESTDIR)$(PREFIX)/share/icons/hicolor/symbolic/apps/org.spritzwine.Beisl-symbolic.svg
	install -Dm644 packaging/org.spritzwine.Beisl.desktop \
	    $(DESTDIR)$(PREFIX)/share/applications/org.spritzwine.Beisl.desktop
	install -Dm644 packaging/org.spritzwine.BeislCompat.desktop \
	    $(DESTDIR)$(PREFIX)/share/applications/org.spritzwine.BeislCompat.desktop
	install -Dm644 packaging/org.spritzwine.BeislSession.desktop \
	    $(DESTDIR)$(PREFIX)/share/applications/org.spritzwine.BeislSession.desktop
	install -Dm644 LICENSE $(DESTDIR)$(PREFIX)/share/licenses/beisl/LICENSE
	@if [ -n "$(MANIFEST)" ]; then \
	    find "$(DESTDIR)$(PREFIX)" -type f \
	        | sed -e "s|^$(DESTDIR)||" \
	        | grep -v '/share/licenses/' \
	        | sort > "$(MANIFEST)"; \
	    echo "manifest: $(MANIFEST) ($$(wc -l < "$(MANIFEST)") paths)"; \
	fi
	@echo "installed into $(DESTDIR)$(PREFIX)"

# Both workspaces, because the GUI is excluded from the root one and carries
# its own lockfile: one fetch would miss libcosmic's rev-pinned git tree.
fetch:
	cargo fetch --locked
	cargo fetch --locked --manifest-path crates/beisl-gui/Cargo.toml

# The same, for a build that may not touch the network afterwards. The second
# step puts back what cargo vendor drops: gamebus-coupler reads a store list
# from a sibling submodule, outside its own package, so a vendored tree has to
# have it restored or the build panics. Nothing is downloaded by that step.
vendor:
	packaging/vendor.sh $(DIST)/$(BUNDLE)-vendor.tar.zst

# HEAD, not the working tree: a source tarball has to be something a tag can
# reproduce. Prefixed the way rpm's %autosetup and most recipes expect.
sdist:
	mkdir -p $(DIST)
	git archive --format=tar.gz --prefix=$(BUNDLE)/ -o $(DIST)/$(BUNDLE).tar.gz HEAD
	@echo "source: $(DIST)/$(BUNDLE).tar.gz"

# The fetch-everything-first half of an airgapped build, as one artifact. The
# rpm build that consumes it never needs the network. SPEC picks the distro's
# metadata; the sources are the same two tarballs for all of them.
SPEC ?= packaging/fedora/beisl.spec
srpm: sdist vendor
	rpmbuild -bs $(SPEC) \
	    --define "_sourcedir $(abspath $(DIST))" \
	    --define "_srcrpmdir $(abspath $(DIST))"

# What a software centre and a packaging linter will read. Worth failing the
# build over: a component a centre rejects is found before release this way,
# and after it otherwise.
check-packaging: generate
	appstreamcli validate --no-net crates/assets/data/metainfo/org.spritzwine.Beisl.metainfo.xml
	desktop-file-validate packaging/*.desktop

clean:
	rm -rf $(DIST) man packaging

# From-scratch: also drop both cargo target dirs so nothing stale survives a
# branch switch. Slower — the beisl-gui git-dep tree recompiles from zero.
distclean: clean
	cargo clean
	cd crates/beisl-gui && cargo clean
