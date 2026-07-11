//! `beisl install` / `beisl uninstall`.
//!
//! Puts `beisl` on the user's PATH (`~/.local/bin`) so it works in Steam launch options
//! (`beisl %command%`), copies the setup GUI alongside it when the bundle ships one, and
//! registers a desktop entry so "Beisl Setup" appears in the app menu. Copies rather than
//! symlinks, so a relocatable SpritzWine bundle can move without breaking the install.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use crate::out::out;

// Must equal the setup app's Wayland APP_ID ("org.spritzwine.Beisl", beisl-gui
// main.rs) — compositors pair a window to its icon via app_id ↔ desktop-file
// basename, and a mismatch is a generic gear on the running window.
const DESKTOP_FILE: &str = "org.spritzwine.Beisl.desktop";
/// The pre-rename entry name older installs wrote; upgrades and uninstalls sweep it.
const LEGACY_DESKTOP_FILE: &str = "org.spritzwine.beisl-setup.desktop";
/// A second, menu-hidden desktop entry for the session window's own APP_ID, so
/// compositors can pair its windows with the icon and a human name (a window whose
/// app_id has no desktop file gets a generic gear and the raw id in the taskbar).
const SESSION_DESKTOP_FILE: &str = "org.spritzwine.BeislSession.desktop";

// Das Glasl — embedded via the assets crate, so `beisl install` works from a bare
// binary with no assets directory around it.
const ICON_APP: &str = assets::icons::APP;
const ICON_SYMBOLIC: &str = assets::icons::SYMBOLIC;
/// Icon names, per the freedesktop icon spec: the desktop entry's basename.
const ICON_NAME: &str = "org.spritzwine.Beisl";
const SESSION_ICON_NAME: &str = "org.spritzwine.BeislSession";

pub fn run(uninstall: bool) -> ExitCode {
    let res = if uninstall {
        do_uninstall()
    } else {
        do_install()
    };
    match res {
        Ok(msg) => {
            out!("{msg}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("beisl: {e}");
            ExitCode::FAILURE
        }
    }
}

fn home() -> PathBuf {
    PathBuf::from(std::env::var_os("HOME").unwrap_or_default())
}

fn bin_dir() -> PathBuf {
    home().join(".local/bin")
}

fn applications_dir() -> PathBuf {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| home().join(".local/share"))
        .join("applications")
}

fn do_install() -> Result<String, String> {
    let exe = std::env::current_exe().map_err(|e| format!("cannot find own path: {e}"))?;
    let here = exe.parent().unwrap_or_else(|| Path::new("."));
    let bindir = bin_dir();
    fs::create_dir_all(&bindir).map_err(|e| format!("mkdir {}: {e}", bindir.display()))?;

    let beisl_dst = bindir.join("beisl");
    install_file(&exe, &beisl_dst)?;

    let mut extra = String::new();
    // A bundle ships beisl-setup next to beisl; register it when present.
    let setup_src = here.join("beisl-setup");
    if setup_src.is_file() {
        let setup_dst = bindir.join("beisl-setup");
        install_file(&setup_src, &setup_dst)?;
        write_desktop(&setup_dst)?;
        extra.push_str("\n  + beisl-setup, and \"Beisl Setup\" added to your app menu");
    }
    // The MCP server rides along when the bundle carries it — inert unless the user
    // wires `beisl-mcp` into an AI client's config (stdio only, no ports).
    let mcp_src = here.join("beisl-mcp");
    if mcp_src.is_file() {
        install_file(&mcp_src, &bindir.join("beisl-mcp"))?;
        extra.push_str("\n  + beisl-mcp (MCP server for AI clients; see the README)");
    }

    let path_note = if on_path(&bindir) {
        String::new()
    } else {
        format!(
            "\n\nnote: {} is not on your PATH yet — add it (most desktops pick it up after a re-login).",
            bindir.display()
        )
    };

    Ok(format!(
        "Installed beisl -> {beisl}{extra}\n\n\
         To trace a game, add this to its launch options (Steam: Properties -> Launch Options):\n\
         {indent}beisl %command%\n\
         Then play and close the game; the trace lands in ~/.local/share/beisl/traces/.\n\
         Run `beisl-setup` first to check prerequisites (perf, symbols, permissions).{path_note}",
        beisl = beisl_dst.display(),
        indent = "    ",
    ))
}

fn do_uninstall() -> Result<String, String> {
    let mut removed = Vec::new();
    let icons = home().join(".local/share/icons/hicolor");
    for p in [
        bin_dir().join("beisl"),
        bin_dir().join("beisl-setup"),
        bin_dir().join("beisl-mcp"),
        applications_dir().join(DESKTOP_FILE),
        applications_dir().join(LEGACY_DESKTOP_FILE),
        applications_dir().join(SESSION_DESKTOP_FILE),
        icons.join(format!("scalable/apps/{ICON_NAME}.svg")),
        icons.join(format!("scalable/apps/{SESSION_ICON_NAME}.svg")),
        icons.join(format!("symbolic/apps/{ICON_NAME}-symbolic.svg")),
    ] {
        if p.exists() {
            fs::remove_file(&p).map_err(|e| format!("rm {}: {e}", p.display()))?;
            removed.push(p.display().to_string());
        }
    }
    if removed.is_empty() {
        Ok("Nothing to remove (beisl was not installed for this user).".into())
    } else {
        Ok(format!("Removed:\n  {}", removed.join("\n  ")))
    }
}

/// Copy `src` to `dst` (0755), unless they are already the same file.
fn install_file(src: &Path, dst: &Path) -> Result<(), String> {
    if same_file(src, dst) {
        return Ok(());
    }
    fs::copy(src, dst).map_err(|e| format!("copy to {}: {e}", dst.display()))?;
    fs::set_permissions(dst, fs::Permissions::from_mode(0o755))
        .map_err(|e| format!("chmod {}: {e}", dst.display()))?;
    Ok(())
}

fn same_file(a: &Path, b: &Path) -> bool {
    matches!((fs::canonicalize(a), fs::canonicalize(b)), (Ok(x), Ok(y)) if x == y)
}

fn write_desktop(exec: &Path) -> Result<(), String> {
    let dir = applications_dir();
    fs::create_dir_all(&dir).map_err(|e| format!("mkdir {}: {e}", dir.display()))?;
    let contents = format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name=Beisl Setup\n\
         Comment=Check prerequisites for perf tracing\n\
         Exec={}\n\
         Icon={icon}\n\
         Terminal=false\n\
         Categories=Utility;System;\n",
        exec.display(),
        icon = ICON_NAME,
    );
    fs::write(dir.join(DESKTOP_FILE), contents).map_err(|e| format!("write desktop entry: {e}"))?;
    let _ = fs::remove_file(dir.join(LEGACY_DESKTOP_FILE)); // pre-rename installs
                                                            // The session window's app_id gets its own (menu-hidden) entry: same artwork,
                                                            // no Exec anyone should launch by hand — windows land on it via app_id matching.
    let session = format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name=Beisl Session\n\
         Exec={} --session %f\n\
         Icon={}\n\
         Terminal=false\n\
         NoDisplay=true\n\
         Categories=Utility;System;\n",
        exec.display(),
        SESSION_ICON_NAME,
    );
    fs::write(dir.join(SESSION_DESKTOP_FILE), session)
        .map_err(|e| format!("write session desktop entry: {e}"))?;
    write_icons()?;
    Ok(())
}

/// Put das Glasl where the icon theme looks for it. Same artwork under both names
/// (the setup entry's and the session app_id's); the symbolic variant rides along
/// for shells that prefer it (tray/menu contexts).
fn write_icons() -> Result<(), String> {
    let icons = home().join(".local/share/icons/hicolor");
    let scalable = icons.join("scalable/apps");
    let symbolic = icons.join("symbolic/apps");
    for (dir, name, body) in [
        (&scalable, format!("{ICON_NAME}.svg"), ICON_APP),
        (&scalable, format!("{SESSION_ICON_NAME}.svg"), ICON_APP),
        (
            &symbolic,
            format!("{ICON_NAME}-symbolic.svg"),
            ICON_SYMBOLIC,
        ),
    ] {
        fs::create_dir_all(dir).map_err(|e| format!("mkdir {}: {e}", dir.display()))?;
        fs::write(dir.join(&name), body).map_err(|e| format!("write icon {name}: {e}"))?;
    }
    // Nudge icon-theme caches: the spec keys on the theme dir's mtime.
    let _ = std::process::Command::new("touch").arg(&icons).status();
    Ok(())
}

fn on_path(dir: &Path) -> bool {
    std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).any(|d| d == dir))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_icons_are_svgs() {
        assert!(ICON_APP.trim_start().starts_with("<svg"));
        assert!(ICON_SYMBOLIC.trim_start().starts_with("<svg"));
    }

    #[test]
    fn icon_names_match_the_desktop_entries() {
        // The freedesktop icon spec pairs Icon= with the entry basename; drift here
        // means a generic gear in the app menu.
        assert_eq!(format!("{ICON_NAME}.desktop"), DESKTOP_FILE);
        assert_eq!(format!("{SESSION_ICON_NAME}.desktop"), SESSION_DESKTOP_FILE);
    }
}
