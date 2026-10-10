#
# spec file for package beisl
#
# Unlike the Arch recipe, OBS builds offline, so the dependencies cannot be
# fetched during the build. beisl's GUI workspace pulls ~48 rev-pinned git
# dependencies (libcosmic), and `cargo vendor` handles those - but it must run
# for BOTH workspaces, since crates/beisl-gui is excluded from the main one and
# carries its own lockfile. The _service file beside this spec does that.
#
Name:           beisl
Version:        0.3.0
Release:        0
Summary:        Find out what made a game stutter
License:        LGPL-2.1-or-later
Group:          Development/Tools/Debuggers
URL:            https://codeberg.org/SpritzWine/beisl
Source0:        %{name}-%{version}.tar.gz
Source1:        vendor.tar.zst
Source2:        cargo_config

BuildRequires:  cargo >= 1.80
BuildRequires:  rust >= 1.80
BuildRequires:  make
BuildRequires:  pkgconfig(xkbcommon)
# The AppStream component is validated at build time; a component a software
# centre would reject is a packaging bug, not something to find after release.
BuildRequires:  appstream-glib

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
%autosetup -a1
mkdir -p .cargo
cp %{SOURCE2} .cargo/config.toml
# The GUI is its own workspace and needs the same vendored source replacement.
mkdir -p crates/beisl-gui/.cargo
cp %{SOURCE2} crates/beisl-gui/.cargo/config.toml

%build
make build generate CLI_TARGET=target GUI_TARGET=crates/beisl-gui/target

%install
make install DESTDIR=%{buildroot} PREFIX=%{_prefix}

%check
cargo test --workspace --offline
appstream-util validate-relax --nonet \
    %{buildroot}%{_datadir}/metainfo/org.spritzwine.Beisl.metainfo.xml

%files
%license LICENSE
%doc README.md
%{_bindir}/beisl
%{_bindir}/beisl-mcp
%{_bindir}/beisl-setup
%{_bindir}/beisl-compat
%{_mandir}/man1/beisl.1%{?ext_man}
%{_mandir}/man1/beisl-mcp.1%{?ext_man}
%{_mandir}/man1/beisl-setup.1%{?ext_man}
%{_mandir}/man1/beisl-compat.1%{?ext_man}
%{_datadir}/applications/org.spritzwine.Beisl.desktop
%{_datadir}/applications/org.spritzwine.BeislCompat.desktop
%{_datadir}/applications/org.spritzwine.BeislSession.desktop
%{_datadir}/icons/hicolor/scalable/apps/org.spritzwine.Beisl.svg
%{_datadir}/icons/hicolor/scalable/apps/org.spritzwine.BeislSession.svg
%{_datadir}/icons/hicolor/symbolic/apps/org.spritzwine.Beisl-symbolic.svg
%{_datadir}/metainfo/org.spritzwine.Beisl.metainfo.xml

%changelog
