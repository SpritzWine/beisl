//! Orchestrates one trace: set up the environment, spawn the game, and manage the
//! `perf` recorder around its lifetime.

use std::io;
use std::path::PathBuf;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::{cue, mangohud, perf};

/// Run `cmd` under tracing; returns the game's exit code.
pub fn trace(cmd: &[String]) -> io::Result<i32> {
    let run_dir = new_run_dir()?;
    eprintln!("beisl: trace dir = {}", run_dir.display());

    let mut child = Command::new(&cmd[0]);
    child.args(&cmd[1..]);

    // MangoHud frametime CSV = the side-channel that bookmarks low-perf peaks.
    mangohud::configure(&mut child, &run_dir);

    let mut recorder = perf::Recorder::new(&run_dir);

    // v0 feedback: an audible cue instead of an in-game overlay (see DESIGN.md).
    cue::play(cue::Cue::Ready);

    let mut proc = child.spawn()?;

    // TODO: gate perf start/stop on MangoHud's "logging on/off" edge (file-watch on
    //   run_dir), so the user's toggle hotkey drives the CPU capture too. For v0 we
    //   attach for the whole child lifetime.
    recorder.attach(proc.id());
    cue::play(cue::Cue::RecordStart);

    let status = proc.wait()?;

    recorder.finish();
    cue::play(cue::Cue::RecordStop);

    // TODO(artifact): symbol-resolver::resolve(perf.data, /proc/<pid>/maps) ⋈ the
    //   MangoHud CSV -> artifact::TraceArtifact, stamped with the detected fidelity
    //   tier, written into run_dir for the GUI / MCP / export.

    Ok(status.code().unwrap_or(0))
}

/// `$XDG_DATA_HOME/beisl/traces/run-<unix_ts>/` (falls back to `~/.local/share`).
/// The GUI surfaces these as cards; users never touch the directory directly.
fn new_run_dir() -> io::Result<PathBuf> {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let dir = data_root().join("traces").join(format!("run-{ts}"));
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

pub(crate) fn data_root() -> PathBuf {
    if let Ok(x) = std::env::var("XDG_DATA_HOME") {
        if !x.is_empty() {
            return PathBuf::from(x).join("beisl");
        }
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(".local/share/beisl")
}
