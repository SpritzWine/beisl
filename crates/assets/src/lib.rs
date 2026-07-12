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
}
