# COPR's "make srpm" build method runs this in a chroot WITH network, then
# builds the resulting source rpm in one WITHOUT. That is exactly the split
# upstream wants: fetch everything first, build airgapped. All the logic is the
# top-level Makefile's; this only installs what it needs and forwards.
#
# COPR calls:  make -f .copr/Makefile srpm outdir=<dir> spec=<path>
srpm:
	dnf -y install cargo git-core make rpm-build tar zstd
	$(MAKE) srpm SPEC=packaging/fedora/beisl.spec DIST=$(outdir)
