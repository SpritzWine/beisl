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
/// The compat-finding window's entry; basename = its APP_ID, for the same pairing.
/// Visible in the app menu like the setup app, but deliberately NOT a second
/// AppStream component: beisl is one listing in a software centre with several
/// entries behind it, so this one carries `X-AppStream-Ignore` to stop a distro's
/// metadata generator synthesising a component from the desktop file.
const COMPAT_DESKTOP_FILE: &str = "org.spritzwine.BeislCompat.desktop";

// Das Glasl — embedded via the assets crate, so `beisl install` works from a bare
// binary with no assets directory around it.
const ICON_APP: &str = assets::icons::APP;
const ICON_SYMBOLIC: &str = assets::icons::SYMBOLIC;
/// Icon names, per the freedesktop icon spec: the desktop entry's basename.
const ICON_NAME: &str = "org.spritzwine.Beisl";
const SESSION_ICON_NAME: &str = "org.spritzwine.BeislSession";

pub enum Action {
    /// `relax`: keep an AppImage where it is although others could replace it,
    /// and tell the shims not to refuse it for that. Only meaningful from an
    /// AppImage, and only has an effect when there is something to relax.
    Install {
        relax: bool,
    },
    Uninstall,
}

pub fn run(action: Action) -> ExitCode {
    let res = match action {
        Action::Uninstall => do_uninstall(),
        Action::Install { relax } => match preflight::appimage::running() {
            Some(image) => install_from_appimage(&image, relax),
            None => do_install(),
        },
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

fn data_home() -> PathBuf {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| home().join(".local/share"))
}

fn applications_dir() -> PathBuf {
    data_home().join("applications")
}

/// `~/.local/share/metainfo` - where a software centre looks for the AppStream
/// component that makes beisl a listed application rather than a loose binary.
fn metainfo_dir() -> PathBuf {
    data_home().join("metainfo")
}

/// `~/.local/share/man/man1` — on the default MANPATH, so `man beisl` works
/// after an unprivileged install.
fn man1_dir() -> PathBuf {
    data_home().join("man/man1")
}

const MAN_PAGES: [&str; 4] = ["beisl.1", "beisl-setup.1", "beisl-mcp.1", "beisl-compat.1"];

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
        // Only alongside the app it launches: a component whose desktop entry is
        // absent would have a software centre list something it cannot start.
        write_metainfo()?;
        extra.push_str("\n  + beisl-setup, and \"Beisl Setup\" added to your app menu");
    }
    // The compat-finding review window rides along the same way when the bundle has it.
    let compat_src = here.join("beisl-compat");
    if compat_src.is_file() {
        let compat_dst = bindir.join("beisl-compat");
        install_file(&compat_src, &compat_dst)?;
        write_compat_desktop(&compat_dst)?;
        extra.push_str("\n  + beisl-compat, and \"Beisl Compat Findings\" added to your app menu");
    }
    // The MCP server rides along when the bundle carries it — inert unless the user
    // wires `beisl-mcp` into an AI client's config (stdio only, no ports).
    let mcp_src = here.join("beisl-mcp");
    if mcp_src.is_file() {
        install_file(&mcp_src, &bindir.join("beisl-mcp"))?;
        extra.push_str("\n  + beisl-mcp (MCP server for AI clients; see the README)");
    }
    // Man pages ride along in the bundle's man/ (rendered from docs/*.1.md — the
    // same files the binaries embed for --help). Per-user manpath, no root.
    if copy_man_pages(&here.join("man"))? {
        extra.push_str("\n  + man pages (try `man beisl`)");
    }

    let path_note = path_note(&bindir);

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

fn path_note(bindir: &Path) -> String {
    if on_path(bindir) {
        String::new()
    } else {
        format!(
            "\n\nnote: {} is not on your PATH yet — add it (most desktops pick it up after a re-login).",
            bindir.display()
        )
    }
}

/// Copy whichever man pages `src` has into the per-user manpath. `true` if any did.
fn copy_man_pages(src: &Path) -> Result<bool, String> {
    if !src.is_dir() {
        return Ok(false);
    }
    let dst = man1_dir();
    fs::create_dir_all(&dst).map_err(|e| format!("mkdir {}: {e}", dst.display()))?;
    let mut copied = false;
    for page in MAN_PAGES {
        let from = src.join(page);
        if from.is_file() {
            fs::copy(&from, dst.join(page)).map_err(|e| format!("copy man page {page}: {e}"))?;
            copied = true;
        }
    }
    Ok(copied)
}

/// Set up from an AppImage: nothing is copied out of it. The file stays where the
/// user keeps it and is the application; what lands in `~/.local/bin` are scripts
/// that hand over to it at its actual path (DESIGN.md, "Agreed design
/// (2026-10-10)"). Running this again re-points them, which is how a moved or
/// replaced file is followed.
fn install_from_appimage(
    image: &preflight::appimage::Running,
    relax: bool,
) -> Result<String, String> {
    use preflight::appimage;

    let issues = appimage::inspect(&image.appimage);
    let blocking: Vec<&appimage::Issue> =
        issues.iter().filter(|i| i.blocks_strict_shim()).collect();
    if !blocking.is_empty() && !relax {
        let list: Vec<String> = blocking
            .iter()
            .map(|i| format!("  - {}", i.explain()))
            .collect();
        return Err(format!(
            "not setting up from {}:\n{}\n\
             A file others can replace would run as you the next time a game starts.\n\
             Move the AppImage somewhere only you can write and run it again, or keep it\n\
             there and say so: beisl install --relax-permission-check",
            image.appimage.display(),
            list.join("\n"),
        ));
    }
    // Relaxing only ever loosens what needs loosening: with nothing wrong, the
    // shims stay strict whatever was asked.
    let strict = blocking.is_empty();

    let bindir = bin_dir();
    fs::create_dir_all(&bindir).map_err(|e| format!("mkdir {}: {e}", bindir.display()))?;
    let mut written = Vec::new();
    for tool in appimage::TOOLS {
        if !image.appdir.join("usr/bin").join(tool).is_file() {
            continue;
        }
        let dst = bindir.join(tool);
        write_shim(&dst, &appimage::shim_script(tool, &image.appimage, strict))?;
        written.push(tool);
    }
    if written.contains(&"beisl-setup") {
        write_desktop(&bindir.join("beisl-setup"))?;
        write_metainfo()?;
    }
    if written.contains(&"beisl-compat") {
        write_compat_desktop(&bindir.join("beisl-compat"))?;
    }
    let man = copy_man_pages(&image.appdir.join("usr/share/man/man1"))?;

    let mut notes = String::new();
    if !strict {
        notes.push_str(
            "\n\nThe permission check is relaxed, as asked: the launchers will run this file \
             although others could replace it.",
        );
    }
    for issue in issues.iter().filter(|i| !i.blocks_strict_shim()) {
        notes.push_str(&format!("\n\nnote: {}", issue.explain()));
    }
    Ok(format!(
        "Set up beisl to run from {image}\n\
         {indent}launchers in {bin}: {tools}\n\
         {indent}app menu entries, icons{man}\n\n\
         The AppImage was not copied: it stays where you keep it. If you move or replace it,\n\
         open it once and setup points the launchers at it again. `beisl uninstall` removes\n\
         everything listed here and leaves the AppImage alone.\n\n\
         To trace a game, add this to its launch options (Steam: Properties -> Launch Options):\n\
         {indent}beisl %command%{notes}{path_note}",
        image = image.appimage.display(),
        bin = bindir.display(),
        tools = written.join(", "),
        man = if man { ", man pages" } else { "" },
        indent = "    ",
        path_note = path_note(&bindir),
    ))
}

/// Write a launcher script (0755) by staging and renaming, like `install_file`:
/// the destination may be a binary that is running right now.
fn write_shim(dst: &Path, script: &str) -> Result<(), String> {
    let tmp = dst.with_extension("tmp-install");
    let staged = fs::write(&tmp, script)
        .and_then(|_| fs::set_permissions(&tmp, fs::Permissions::from_mode(0o755)))
        .and_then(|_| fs::rename(&tmp, dst));
    staged.map_err(|e| {
        let _ = fs::remove_file(&tmp);
        format!("write {}: {e}", dst.display())
    })
}

fn do_uninstall() -> Result<String, String> {
    let mut removed = Vec::new();
    let icons = home().join(".local/share/icons/hicolor");
    let mut targets = vec![
        bin_dir().join("beisl"),
        bin_dir().join("beisl-setup"),
        bin_dir().join("beisl-mcp"),
        bin_dir().join("beisl-compat"),
        applications_dir().join(DESKTOP_FILE),
        applications_dir().join(COMPAT_DESKTOP_FILE),
        applications_dir().join(LEGACY_DESKTOP_FILE),
        applications_dir().join(SESSION_DESKTOP_FILE),
        icons.join(format!("scalable/apps/{ICON_NAME}.svg")),
        icons.join(format!("scalable/apps/{SESSION_ICON_NAME}.svg")),
        icons.join(format!("symbolic/apps/{ICON_NAME}-symbolic.svg")),
        metainfo_dir().join(format!("{}.metainfo.xml", assets::metainfo::ID)),
    ];
    targets.extend(MAN_PAGES.iter().map(|p| man1_dir().join(p)));
    for p in targets {
        if p.exists() {
            fs::remove_file(&p).map_err(|e| format!("rm {}: {e}", p.display()))?;
            removed.push(p.display().to_string());
        }
    }
    // The Steam launcher shim is root-owned (installed via pkexec in setup) —
    // uninstall runs unprivileged, so name the leftover instead of failing on it.
    let link_note = if fs::symlink_metadata("/usr/bin/beisl").is_ok() {
        "\n\nnote: the system-wide launcher /usr/bin/beisl (installed so Steam finds \
         beisl) remains — remove it with: sudo rm /usr/bin/beisl"
    } else {
        ""
    };
    if removed.is_empty() && link_note.is_empty() {
        Ok("Nothing to remove (beisl was not installed for this user).".into())
    } else if removed.is_empty() {
        Ok(format!("Nothing to remove for this user.{link_note}"))
    } else {
        Ok(format!("Removed:\n  {}{link_note}", removed.join("\n  ")))
    }
}

/// Copy `src` to `dst` (0755), unless they are already the same file. Staged as a
/// sibling temp file and renamed into place: copying straight over a *running* binary
/// fails with ETXTBSY — beisl-mcp typically is running (an AI session holds it open) —
/// while rename atomically swaps the path and lets the old inode live on.
fn install_file(src: &Path, dst: &Path) -> Result<(), String> {
    if same_file(src, dst) {
        return Ok(());
    }
    let mut tmp = dst.as_os_str().to_owned();
    tmp.push(".tmp-install");
    let tmp = PathBuf::from(tmp);
    let staged = (|| -> Result<(), String> {
        fs::copy(src, &tmp).map_err(|e| format!("copy to {}: {e}", tmp.display()))?;
        fs::set_permissions(&tmp, fs::Permissions::from_mode(0o755))
            .map_err(|e| format!("chmod {}: {e}", tmp.display()))?;
        fs::rename(&tmp, dst).map_err(|e| format!("rename to {}: {e}", dst.display()))
    })();
    if staged.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    staged
}

fn same_file(a: &Path, b: &Path) -> bool {
    matches!((fs::canonicalize(a), fs::canonicalize(b)), (Ok(x), Ok(y)) if x == y)
}

fn write_desktop(exec: &Path) -> Result<(), String> {
    let dir = applications_dir();
    fs::create_dir_all(&dir).map_err(|e| format!("mkdir {}: {e}", dir.display()))?;
    let exec = exec.display().to_string();
    fs::write(
        dir.join(DESKTOP_FILE),
        assets::desktop::render(assets::desktop::SETUP, &exec),
    )
    .map_err(|e| format!("write desktop entry: {e}"))?;
    let _ = fs::remove_file(dir.join(LEGACY_DESKTOP_FILE)); // pre-rename installs
    fs::write(
        dir.join(SESSION_DESKTOP_FILE),
        assets::desktop::render(assets::desktop::SESSION, &exec),
    )
    .map_err(|e| format!("write session desktop entry: {e}"))?;
    write_icons()
}

fn write_compat_desktop(exec: &Path) -> Result<(), String> {
    let dir = applications_dir();
    fs::create_dir_all(&dir).map_err(|e| format!("mkdir {}: {e}", dir.display()))?;
    fs::write(
        dir.join(COMPAT_DESKTOP_FILE),
        assets::desktop::render(assets::desktop::COMPAT, &exec.display().to_string()),
    )
    .map_err(|e| format!("write desktop entry: {e}"))
}

/// The AppStream component, beside the desktop entry it launches.
fn write_metainfo() -> Result<(), String> {
    let dir = metainfo_dir();
    fs::create_dir_all(&dir).map_err(|e| format!("mkdir {}: {e}", dir.display()))?;
    let path = dir.join(format!("{}.metainfo.xml", assets::metainfo::ID));
    fs::write(&path, assets::metainfo::XML).map_err(|e| format!("write {}: {e}", path.display()))
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

    #[test]
    fn install_file_replaces_an_existing_destination_without_leftovers() {
        let dir = std::env::temp_dir().join(format!("beisl-install-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let src = dir.join("src");
        let dst = dir.join("dst");
        fs::write(&src, b"new").unwrap();
        fs::write(&dst, b"old").unwrap();
        install_file(&src, &dst).unwrap();
        assert_eq!(fs::read(&dst).unwrap(), b"new");
        assert_eq!(
            fs::metadata(&dst).unwrap().permissions().mode() & 0o777,
            0o755
        );
        assert!(
            !dir.join("dst.tmp-install").exists(),
            "staging file left behind"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn install_file_failure_cleans_its_staging_file() {
        // dst is a directory: the copy to dst.tmp-install succeeds, the rename onto a
        // directory fails — the staged temp must not survive that late failure.
        let dir = std::env::temp_dir().join(format!("beisl-install-gone-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let src = dir.join("src");
        fs::write(&src, b"new").unwrap();
        let dst = dir.join("dst-is-a-dir");
        fs::create_dir_all(&dst).unwrap();
        assert!(install_file(&src, &dst).is_err());
        assert!(
            !dir.join("dst-is-a-dir.tmp-install").exists(),
            "staging file survived a failed rename"
        );
        let _ = fs::remove_dir_all(&dir);
    }
}
