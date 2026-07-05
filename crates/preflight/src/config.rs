//! Beisl's persisted configuration — `$XDG_CONFIG_HOME/beisl/config.toml`
//! (falling back to `~/.config/beisl/config.toml`).
//!
//! Today it holds exactly one decision: the **GPU crash-dump access policy**. Reading a
//! GPU devcoredump / i915 error node is root-only by default, so the user chooses how
//! Beisl behaves when a hang leaves a root-only dump pending:
//!
//! * [`DumpAccessPolicy::WarnOnly`] (default) — detect it, warn, and hand over a `sudo`
//!   one-liner. No standing privilege, no system change.
//! * [`DumpAccessPolicy::RuleInstalled`] — a udev rule + tmpfiles entry grant the
//!   `render` group read access, so dumps auto-capture without root.
//!
//! A privilege change (installing the rule) must NEVER ride along with the ordinary
//! "set everything up" flow — it is always its own labelled, explicit click. This file
//! only records the *intent*; `preflight`'s `Fix`/`fix_command` describe the privileged
//! action and the caller runs it through `pkexec`.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// How Beisl treats a root-only GPU crash dump.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum DumpAccessPolicy {
    /// Default, zero-privilege: warn and hand over a `sudo` capture one-liner.
    #[default]
    WarnOnly,
    /// The udev rule + tmpfiles entry are installed; dumps auto-capture without root.
    RuleInstalled,
    // Future option (intentionally NOT implemented yet): `AskAtExit` — after a hang left
    // a dump we couldn't read, offer to install the rule at session end. Left as a note so
    // the enum + UI can grow into it without a config migration.
}

/// The whole of Beisl's config file. Kept tiny and forward-compatible: every field is
/// `#[serde(default)]`, so an older file (or a newer one with unknown keys) still loads.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct Config {
    #[serde(default)]
    pub dump_access_policy: DumpAccessPolicy,
}

impl Config {
    /// `$XDG_CONFIG_HOME/beisl/config.toml`, else `~/.config/beisl/config.toml`.
    pub fn path() -> PathBuf {
        let base = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| {
                PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".config")
            });
        base.join("beisl").join("config.toml")
    }

    /// Load the config, or the default on any error (missing file, parse failure). Config
    /// is advisory, never fatal — a broken file must not stop a trace.
    pub fn load() -> Config {
        std::fs::read_to_string(Self::path())
            .ok()
            .and_then(|t| toml::from_str(&t).ok())
            .unwrap_or_default()
    }

    /// Persist the config, creating the directory if needed.
    pub fn save(&self) -> std::io::Result<()> {
        let path = Self::path();
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let text = toml::to_string_pretty(self)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        std::fs::write(path, text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_a_toml_file() {
        // Isolate HOME/XDG so the test never touches the real user config.
        let tmp = std::env::temp_dir().join(format!("beisl-cfg-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        // SAFETY: single-threaded test; we set the env for the duration of this test only.
        unsafe {
            std::env::set_var("XDG_CONFIG_HOME", &tmp);
        }

        // Default when nothing is written yet.
        assert_eq!(Config::load().dump_access_policy, DumpAccessPolicy::WarnOnly);

        let cfg = Config { dump_access_policy: DumpAccessPolicy::RuleInstalled };
        cfg.save().unwrap();
        assert_eq!(Config::load(), cfg);

        // The on-disk form is the kebab-case string, not a bare enum.
        let text = std::fs::read_to_string(Config::path()).unwrap();
        assert!(text.contains("rule-installed"), "got: {text}");

        std::fs::remove_dir_all(&tmp).ok();
        unsafe {
            std::env::remove_var("XDG_CONFIG_HOME");
        }
    }
}
