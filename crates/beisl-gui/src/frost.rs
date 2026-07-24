//! Non-COSMIC assist for the theme half of Frosted Glass.
//!
//! libcosmic only frosts a window when BOTH halves agree: the app's own allow-mask
//! (`Config::frosted_background`, applied via `Core::set_auto_blur` in `main.rs`/
//! `window.rs`) AND the COSMIC theme's `frosted_windows` flag. Inside COSMIC that flag has
//! a Settings toggle; outside it (or on an older COSMIC) there's no dialog for it at all,
//! so this offers to write the per-key theme file directly — the same file
//! `cosmic-config` itself would produce.

use std::io;
use std::path::{Path, PathBuf};

/// The two theme variants whose `frosted_windows` key gates Frosted Glass.
const THEME_IDS: [&str; 2] = [
    "com.system76.CosmicTheme.Dark",
    "com.system76.CosmicTheme.Light",
];

/// The `frosted_windows` key file for one theme variant, under a `~/.config`-rooted base.
fn key_path(config_dir: &Path, theme_id: &str) -> PathBuf {
    config_dir
        .join("cosmic")
        .join(theme_id)
        .join("v1")
        .join("frosted_windows")
}

/// Both `frosted_windows` key files (Dark, Light).
fn key_paths(config_dir: &Path) -> [PathBuf; 2] {
    THEME_IDS.map(|id| key_path(config_dir, id))
}

/// Whether to offer the assist: true only when NEITHER key file exists yet. Once either
/// does — any value — the theme (or the user, via COSMIC Settings) has already spoken, and
/// we never second-guess an explicit `false` or force past the theme.
pub fn frosted_flag_offered(config_dir: &Path) -> bool {
    key_paths(config_dir).iter().all(|p| !p.is_file())
}

/// Write the RON literal `true` into both theme variants' `frosted_windows` key, creating
/// each `<theme-id>/v1/` directory as needed. Reverting is just deleting the two files.
///
/// All-or-nothing: a Dark-then-Light write order means a Light failure after Dark already
/// landed would otherwise leave a half-written state that also self-seals — the very
/// presence of the Dark file makes `frosted_flag_offered` false, so the row (and its retry)
/// would vanish while Light never got the flag. On failure this rolls the partial write
/// back (best-effort) so `frosted_flag_offered` still reads true and the assist stays
/// available.
pub fn write_frosted_flag(config_dir: &Path) -> io::Result<()> {
    let mut written = Vec::with_capacity(THEME_IDS.len());
    for path in key_paths(config_dir) {
        let result = path
            .parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|()| std::fs::write(&path, "true"));
        match result {
            Ok(()) => written.push(path),
            Err(e) => {
                for p in &written {
                    let _ = std::fs::remove_file(p);
                }
                return Err(e);
            }
        }
    }
    Ok(())
}

/// The real COSMIC config base: `$XDG_CONFIG_HOME`, else `~/.config` — same resolution as
/// `preflight::Config::path()`.
pub fn real_config_dir() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".config")
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offered_when_absent() {
        let tmp = std::env::temp_dir().join(format!("beisl-frost-absent-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();

        assert!(frosted_flag_offered(&tmp));

        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn hidden_once_either_file_exists_true() {
        let tmp = std::env::temp_dir().join(format!("beisl-frost-true-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();

        let dark = key_path(&tmp, "com.system76.CosmicTheme.Dark");
        std::fs::create_dir_all(dark.parent().unwrap()).unwrap();
        std::fs::write(&dark, "true").unwrap();
        assert!(!frosted_flag_offered(&tmp));

        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn hidden_when_existing_value_is_false() {
        // We never offer to flip an explicit `false` — its mere presence hides the row,
        // regardless of which way it reads.
        let tmp = std::env::temp_dir().join(format!("beisl-frost-false-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();

        let light = key_path(&tmp, "com.system76.CosmicTheme.Light");
        std::fs::create_dir_all(light.parent().unwrap()).unwrap();
        std::fs::write(&light, "false").unwrap();
        assert!(!frosted_flag_offered(&tmp));

        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn write_creates_both_files_with_true() {
        let tmp = std::env::temp_dir().join(format!("beisl-frost-write-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();

        write_frosted_flag(&tmp).unwrap();
        for p in key_paths(&tmp) {
            assert_eq!(std::fs::read_to_string(&p).unwrap(), "true");
        }
        assert!(!frosted_flag_offered(&tmp));

        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn a_failed_light_write_rolls_back_dark_and_stays_offered() {
        // Dark writes first and would succeed; pre-occupy Light's key path with a
        // directory so its write fails with EISDIR partway through. Without the
        // rollback, the already-landed Dark file would self-seal `frosted_flag_offered`
        // (see the doc comment on `write_frosted_flag`) even though Light never got it.
        let tmp = std::env::temp_dir().join(format!("beisl-frost-rollback-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();

        let light = key_path(&tmp, "com.system76.CosmicTheme.Light");
        std::fs::create_dir_all(&light).unwrap();

        assert!(write_frosted_flag(&tmp).is_err());

        let dark = key_path(&tmp, "com.system76.CosmicTheme.Dark");
        assert!(!dark.exists(), "Dark write should have been rolled back");
        assert!(
            frosted_flag_offered(&tmp),
            "a failed write must leave the assist offered, not self-sealed"
        );

        std::fs::remove_dir_all(&tmp).ok();
    }
}
