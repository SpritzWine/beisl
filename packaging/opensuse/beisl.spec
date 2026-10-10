#
# spec file for package beisl
#
# OBS builds offline, so the crates are vendored (Source1) rather than fetched.
# Both sources come from upstream's `make srpm SPEC=packaging/opensuse/beisl.spec`
# (or `make sdist vendor` for the two tarballs alone); OBS's own cargo_vendor
# service is not used, because it would miss the GUI workspace's separate
# lockfile and the store list cargo vendor drops from gamebus-coupler.
#
# All build logic is upstream's Makefile; this file is metadata. See
# packaging/README.md.
#

# cargo strips the release binaries itself, so there is no debuginfo to extract.
%global debug_package %{nil}

Name:           beisl
Version:        0.3.0
Release:        0
Summary:        Find out what made a game stutter
License:        LGPL-2.1-or-later
Group:          Development/Tools/Debuggers
URL:            https://codeberg.org/SpritzWine/beisl
Source0:        %{name}-%{version}.tar.gz
Source1:        %{name}-%{version}-vendor.tar.zst

BuildRequires:  cargo >= 1.93
BuildRequires:  rust >= 1.93
BuildRequires:  make
BuildRequires:  zstd
BuildRequires:  pkgconfig(xkbcommon)
# make check-packaging: a component a software centre would reject is a
# packaging bug worth failing the build over.
BuildRequires:  AppStream
BuildRequires:  desktop-file-utils

Requires:       libxkbcommon0
# perf does the CPU sampling. Without it beisl still records frametimes and says
# so, which is why this is a recommendation rather than a requirement.
Recommends:     perf
Recommends:     mangohud

%description
Beisl records what a game was actually doing when it stuttered. It wraps a
launch, captures frametimes alongside CPU samples, and attributes each slow
frame to a layer: the game itself, Wine, DXVK or vkd3d, the graphics driver,
or the kernel.

It is wine-agnostic - the same wrapper works under Steam with Proton, under
Lutris or Heroic, or on a bare wine prefix - and it reads the environment
rather than changing it. It also notices when a game cannot run at all, and
hands the evidence to you to review and file upstream yourself.

%prep
%autosetup
# Unpacked over the source: vendor/ and .cargo/config.toml for both workspaces.
tar --zstd -xf %{SOURCE1}

%build
export CARGO_NET_OFFLINE=true
make build generate

%install
export CARGO_NET_OFFLINE=true
# MANIFEST makes upstream's install list the package's file list, so the two
# cannot drift: a file added to `make install` ships without touching this spec.
make install DESTDIR=%{buildroot} PREFIX=%{_prefix} \
    MANIFEST=%{_builddir}/beisl-filelist

%check
export CARGO_NET_OFFLINE=true
cargo test --workspace --locked
make check-packaging

%files -f %{_builddir}/beisl-filelist
%license LICENSE
%doc README.md

%changelog
