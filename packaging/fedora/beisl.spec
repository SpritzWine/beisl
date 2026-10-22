# beisl for Fedora, built in COPR.
#
# COPR rather than Fedora proper, and for the same reason COSMIC itself lives in
# one: the GUI links libcosmic, whose rev-pinned git dependencies cannot come
# from crates.io packages. The crates are vendored (Source1) and the build runs
# with the network off. A submission to Fedora proper would additionally need
# the bundled(crate(...)) provides and the combined licence of everything
# vendored, which this spec does not claim to have.
#
# All build logic is upstream's Makefile; this file is metadata. See
# packaging/README.md.

# No debug_package override: %%build's RUSTFLAGS (Fedora's %%build_rustflags)
# add debuginfo and turn off the profile's strip, so rpm splits a -debuginfo
# package as for any C program. Disabling it left binaries with a symbol table
# and threw the debug info away (rpmlint: unstripped-binary-or-object).

Name:           beisl
Version:        0.4.0
Release:        1%{?dist}
Summary:        Find out what made a game stutter
License:        LGPL-2.1-or-later
URL:            https://codeberg.org/SpritzWine/beisl
# Both published with each release and produced by upstream's `make srpm`: the
# source at the tag, and the vendored dependencies of both cargo workspaces
# with their cargo config. rpmbuild uses the local copies when present.
Source0:        %{url}/releases/download/v%{version}/%{name}-%{version}.tar.gz
Source1:        %{url}/releases/download/v%{version}/%{name}-%{version}-vendor.tar.zst

# The only architecture upstream builds and tests.
ExclusiveArch:  x86_64

BuildRequires:  cargo >= 1.93
BuildRequires:  rust >= 1.93
BuildRequires:  make
BuildRequires:  zstd
BuildRequires:  pkgconfig(xkbcommon)
# make check-packaging: a component a software centre would reject is a
# packaging bug worth failing the build over.
BuildRequires:  appstream
BuildRequires:  desktop-file-utils

# perf does the CPU sampling. Without it beisl still records frametimes and
# says so, which is why these are recommendations rather than requirements.
Recommends:     perf
Recommends:     mangohud
# The GUI loads these at runtime, so rpm cannot see them: a display connection
# (Wayland, or X11) and the GPU renderer, which falls back to software without
# Vulkan or EGL. Weak because the CLI needs none of them and a desktop has all.
Recommends:     libwayland-client.so.0()(64bit)
Recommends:     libwayland-egl.so.1()(64bit)
Recommends:     libX11.so.6()(64bit)
Recommends:     libX11-xcb.so.1()(64bit)
Recommends:     libXi.so.6()(64bit)
Recommends:     libxcb.so.1()(64bit)
Recommends:     libxkbcommon-x11.so.0()(64bit)
Recommends:     libvulkan.so.1()(64bit)
Recommends:     libEGL.so.1()(64bit)

%description
Beisl records what a game was actually doing when it stuttered. It wraps a
launch, captures frame times alongside CPU samples, and attributes each slow
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
# MANIFEST makes upstream's install list the package's file list.
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
* Sat Oct 10 2026 fschaupp <spritzwine.absently488@passinbox.com> - 0.4.0-1
- Update to 0.4.0

* Sat Oct 10 2026 fschaupp <spritzwine.absently488@passinbox.com> - 0.3.0-1
- Initial package, built from upstream's make install
