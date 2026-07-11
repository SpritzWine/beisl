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

const DESKTOP_FILE: &str = "org.spritzwine.beisl-setup.desktop";

pub fn run(uninstall: bool) -> ExitCode {
    let res = if uninstall {
        do_uninstall()
    } else {
        do_install()
    };
    match res {
        Ok(msg) => {
            println!("{msg}");
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
    for p in [
        bin_dir().join("beisl"),
        bin_dir().join("beisl-setup"),
        applications_dir().join(DESKTOP_FILE),
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
         Icon=utilities-system-monitor\n\
         Terminal=false\n\
         Categories=Utility;System;\n",
        exec.display()
    );
    fs::write(dir.join(DESKTOP_FILE), contents).map_err(|e| format!("write desktop entry: {e}"))?;
    Ok(())
}

fn on_path(dir: &Path) -> bool {
    std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).any(|d| d == dir))
        .unwrap_or(false)
}
