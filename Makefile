# One-stop build & package for the whole of beisl, both workspaces
# (the main one and the excluded beisl-gui — see Cargo.toml for why they're split).
#
#   make            # build everything + produce dist/beisl-<version>-<arch>.tar.gz
#   make build      # just compile both workspaces (release)
#   make clean      # remove dist/ only (cargo targets kept — cheap; incremental
#                   #   rebuilds still pick up source changes on the current branch)
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

.PHONY: all build build-cli build-gui man package clean distclean

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
man:
	cargo run --release -p docs-gen --target-dir $(CLI_TARGET)

package: build man
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

clean:
	rm -rf $(DIST) man

# From-scratch: also drop both cargo target dirs so nothing stale survives a
# branch switch. Slower — the beisl-gui git-dep tree recompiles from zero.
distclean: clean
	cargo clean
	cd crates/beisl-gui && cargo clean
