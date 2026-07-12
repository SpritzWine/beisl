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

.PHONY: all build build-cli build-gui package clean distclean

all: package

build: build-cli build-gui

build-cli:
	cargo build --release --workspace

build-gui:
	cd crates/beisl-gui && cargo build --release

package: build
	rm -rf $(DIST)/$(BUNDLE)
	mkdir -p $(DIST)/$(BUNDLE)
	install -m755 target/release/beisl $(DIST)/$(BUNDLE)/
	install -m755 target/release/beisl-mcp $(DIST)/$(BUNDLE)/
	install -m755 crates/beisl-gui/target/release/beisl-setup $(DIST)/$(BUNDLE)/
	install -m644 README.md LICENSE $(DIST)/$(BUNDLE)/
	tar -C $(DIST) -czf $(DIST)/$(BUNDLE)-$(ARCH).tar.gz $(BUNDLE)
	@echo "packaged: $(DIST)/$(BUNDLE)-$(ARCH).tar.gz"

clean:
	rm -rf $(DIST)

# From-scratch: also drop both cargo target dirs so nothing stale survives a
# branch switch. Slower — the beisl-gui git-dep tree recompiles from zero.
distclean: clean
	cargo clean
	cd crates/beisl-gui && cargo clean
