//! Beisl's embedded data payloads, compiled in so a bare installed binary needs no
//! assets directory around it. One home instead of per-crate `include_str!` path
//! climbs; the files themselves live in this crate's `data/` (see
//! `data/icons/README.md` for the brand marks' copy-outward flow).

/// The brand marks (das Glasl — see `data/icons/README.md`).
pub mod icons {
    /// The app icon: the glass on its tile background, for hicolor scalable + .desktop.
    pub const APP: &str = include_str!("../data/icons/icon-glass-pour.svg");
    /// Monochrome variant for tray/menu/symbolic contexts.
    pub const SYMBOLIC: &str = include_str!("../data/icons/icon-symbolic.svg");

    /// Fallback for the details-pane toggle. The named `pane-show-symbolic` is
    /// preferred and is what COSMIC's file manager uses; libcosmic resolves it through
    /// the `Cosmic` theme, which inherits Pop. This copy covers a system where those
    /// icons are absent.
    pub const PANE_RIGHT: &[u8] = include_bytes!("../data/icons/pane-right-symbolic.svg");
}

/// AppStream metadata: what a software centre reads to list beisl as an application.
/// Embedded like the rest, so `beisl install` stays one self-contained binary.
pub mod metainfo {
    /// The component id, equal to the desktop entry's basename by requirement.
    pub const ID: &str = "org.spritzwine.Beisl";
    pub const XML: &str = include_str!("../data/metainfo/org.spritzwine.Beisl.metainfo.xml");
}

/// The desktop entries, with `{exec}` left for the caller to fill.
///
/// One source for two very different installs: `beisl install` writes them with an
/// absolute path out of an extracted bundle, while a distro package writes them with
/// a bare command name because the package manager put the binary on PATH. Keeping
/// the bodies here is what stops those two drifting apart.
pub mod desktop {
    pub const SETUP_FILE: &str = "org.spritzwine.Beisl.desktop";
    pub const SESSION_FILE: &str = "org.spritzwine.BeislSession.desktop";
    pub const COMPAT_FILE: &str = "org.spritzwine.BeislCompat.desktop";

    pub const SETUP: &str = "[Desktop Entry]\n\
                             Type=Application\n\
                             Name=Beisl Setup\n\
                             Comment=Check prerequisites for perf tracing\n\
                             Exec={exec}\n\
                             Icon=org.spritzwine.Beisl\n\
                             Terminal=false\n\
                             Categories=Utility;System;\n";

    /// Menu-hidden: windows land on it by app_id, nobody launches it by hand.
    pub const SESSION: &str = "[Desktop Entry]\n\
                               Type=Application\n\
                               Name=Beisl Session\n\
                               Exec={exec} --session %f\n\
                               Icon=org.spritzwine.BeislSession\n\
                               Terminal=false\n\
                               NoDisplay=true\n\
                               Categories=Utility;System;\n";

    /// Visible like the setup app, but deliberately NOT a second AppStream
    /// component: beisl is one listing in a software centre with several entries
    /// behind it, so `X-AppStream-Ignore` stops a distro's metadata generator
    /// synthesising a component from this file.
    pub const COMPAT: &str = "[Desktop Entry]\n\
                              Type=Application\n\
                              Name=Beisl Compat Findings\n\
                              Comment=Review the compat walls beisl found and file them upstream\n\
                              Exec={exec}\n\
                              Icon=org.spritzwine.Beisl\n\
                              Terminal=false\n\
                              Categories=Utility;System;\n\
                              X-AppStream-Ignore=true\n";

    /// Fill `{exec}`. A placeholder rather than `format!` so the bodies stay plain
    /// data that a packaging tool can render without linking the installer.
    pub fn render(template: &str, exec: &str) -> String {
        template.replace("{exec}", exec)
    }
}

/// The opt-in GPU dump-access rule pair (installed via pkexec by the caller — never
/// from library code; see preflight's consent stance).
pub mod gpu_dump {
    pub const UDEV_RULES: &str = include_str!("../data/udev/99-beisl-gpu-dump.rules");
    pub const TMPFILES_CONF: &str = include_str!("../data/tmpfiles/beisl-gpu-dump.conf");
}

/// The `/usr/bin/beisl` launcher shim for Steam's PATH-scrubbed launch options
/// (installed via pkexec by the caller). A root-owned forwarder to the *invoking*
/// user's `~/.local/bin/beisl` — deliberately NOT a symlink into one user's home:
/// per-user resolution keeps multi-user systems correct, nothing user-writable sits
/// behind a system path, and updates flow through without touching the shim.
pub mod steam_shim {
    pub const SHIM: &str = include_str!("../data/shim/beisl-steam-shim.sh");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payloads_are_what_they_claim() {
        assert!(icons::APP.trim_start().starts_with("<svg"));
        assert!(icons::SYMBOLIC.trim_start().starts_with("<svg"));
        assert!(gpu_dump::UDEV_RULES.contains("devcoredump"));
        assert!(!gpu_dump::TMPFILES_CONF.trim().is_empty());
        assert!(steam_shim::SHIM.starts_with("#!/bin/sh"));
        // The multi-user guarantee: the shim resolves $HOME at run time and must
        // never bake in the installing user's path.
        assert!(!steam_shim::SHIM.contains("/home/"));
    }

    /// A software centre shows the newest `<release>` as the current version, so a
    /// release that forgot to add one advertises the previous one forever. The
    /// release notes live in RELEASENOTES.md; this only pins that the newest entry
    /// here is the version actually being built.
    #[test]
    fn the_metainfo_announces_the_version_being_built() {
        let want = format!("version=\"{}\"", env!("CARGO_PKG_VERSION"));
        let first = metainfo::XML
            .split("<release ")
            .nth(1)
            .expect("at least one release entry");
        assert!(
            first.starts_with(&want),
            "newest <release> is not {}: {}",
            env!("CARGO_PKG_VERSION"),
            first.lines().next().unwrap_or_default()
        );
        assert!(metainfo::XML.contains(&format!("<id>{}</id>", metainfo::ID)));
    }
}
