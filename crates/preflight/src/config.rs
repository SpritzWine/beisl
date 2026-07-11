//! Beisl's persisted configuration — `$XDG_CONFIG_HOME/beisl/config.toml`
//! (falling back to `~/.config/beisl/config.toml`).
//!
//! Two decisions live here:
//!
//! * The **GPU crash-dump access policy**. Reading a GPU devcoredump / i915 error node is
//!   root-only by default, so the user chooses how Beisl behaves when a hang leaves a
//!   root-only dump pending:
//!   * [`DumpAccessPolicy::WarnOnly`] (default) — detect it, warn, and hand over a `sudo`
//!     one-liner. No standing privilege, no system change.
//!   * [`DumpAccessPolicy::RuleInstalled`] — a udev rule + tmpfiles entry grant the
//!     `render` group read access, so dumps auto-capture without root.
//!
//!   A privilege change (installing the rule) must NEVER ride along with the ordinary
//!   "set everything up" flow — it is always its own labelled, explicit click. This file
//!   only records the *intent*; `preflight`'s `Fix`/`fix_command` describe the privileged
//!   action and the caller runs it through `pkexec`.
//!
//! * The **GPU pin** ([`GpuPin`], via [`Config::gpu_pin`]) — which GPU (if any) the wrapper
//!   should force Vulkan to enumerate, countering the "enumeration roulette" trap from
//!   DESIGN.md's Saints Row case study. Unlike the dump-access rule this is a *plain* config
//!   write with no privilege involved anywhere — picking a pin in the GUI just saves this
//!   file, and the wrapper reads it at launch to set `VK_DRIVER_FILES`/`MESA_VK_DEVICE_SELECT`.

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

/// A pinned GPU: the exact ICD manifests (+ optional device selector) that make Vulkan
/// enumeration land on one specific card instead of whichever the loader happens to pick
/// first — the "enumeration roulette" trap from DESIGN.md's Saints Row case study. `None`
/// (the default) means no pinning: use the system's normal Vulkan enumeration.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct GpuPin {
    /// Human label for the picker / session window, e.g.
    /// `"Intel — intel_icd (/usr/share/vulkan/icd.d)"`.
    pub name: String,
    /// The exact ICD manifest paths to force via `VK_DRIVER_FILES` (the wrapper joins them
    /// with `:`). Usually one x86_64 manifest plus its paired i686 one — wine/Proton needs
    /// both arches.
    pub icd_paths: Vec<String>,
    /// `MESA_VK_DEVICE_SELECT` value, e.g. `"8086:56a0!"` — set only when preflight could
    /// resolve the exact PCI vendor:device id of the matching physical GPU.
    pub device_select: Option<String>,
}

/// The whole of Beisl's config file. Kept tiny and forward-compatible: every field is
/// `#[serde(default)]`, so an older file (or a newer one with unknown keys) still loads.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct Config {
    #[serde(default)]
    pub dump_access_policy: DumpAccessPolicy,
    /// The GPU to pin to, if any. `None` (default) = no pinning.
    #[serde(default)]
    pub gpu_pin: Option<GpuPin>,
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
    use std::sync::Mutex;

    // `Config::path()` reads the process-global `XDG_CONFIG_HOME` env var, so any two tests
    // that set/unset it concurrently (cargo test runs them on separate threads by default)
    // race each other. Serialize the tests that touch it.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn round_trips_through_a_toml_file() {
        let _guard = ENV_LOCK.lock().unwrap();
        // Isolate HOME/XDG so the test never touches the real user config.
        let tmp = std::env::temp_dir().join(format!("beisl-cfg-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        // SAFETY: single-threaded test; we set the env for the duration of this test only.
        unsafe {
            std::env::set_var("XDG_CONFIG_HOME", &tmp);
        }

        // Default when nothing is written yet.
        assert_eq!(
            Config::load().dump_access_policy,
            DumpAccessPolicy::WarnOnly
        );

        let cfg = Config {
            dump_access_policy: DumpAccessPolicy::RuleInstalled,
            ..Default::default()
        };
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

    #[test]
    fn gpu_pin_round_trips_and_defaults_to_none() {
        let _guard = ENV_LOCK.lock().unwrap();
        let tmp = std::env::temp_dir().join(format!("beisl-cfg-gpu-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        unsafe {
            std::env::set_var("XDG_CONFIG_HOME", &tmp);
        }

        assert_eq!(Config::load().gpu_pin, None);

        let cfg = Config {
            gpu_pin: Some(GpuPin {
                name: "Intel — intel_icd (/usr/share/vulkan/icd.d)".into(),
                icd_paths: vec![
                    "/usr/share/vulkan/icd.d/intel_icd.x86_64.json".into(),
                    "/usr/share/vulkan/icd.d/intel_icd.i686.json".into(),
                ],
                device_select: Some("8086:56a0!".into()),
            }),
            ..Default::default()
        };
        cfg.save().unwrap();
        assert_eq!(Config::load(), cfg);

        std::fs::remove_dir_all(&tmp).ok();
        unsafe {
            std::env::remove_var("XDG_CONFIG_HOME");
        }
    }
}
