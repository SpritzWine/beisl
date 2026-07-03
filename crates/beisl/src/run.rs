//! Orchestrates one trace: set up the environment, spawn the game, open the session
//! window, and manage the `perf` recorder across the game's lifetime — publishing live
//! state to `session.json` for the window to display.

use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use session::{Session, State};

use crate::mangohud::TOGGLE_KEY;
use crate::{cue, mangohud, perf};

/// Run `cmd` under tracing; returns the game's exit code.
pub fn trace(cmd: &[String]) -> io::Result<i32> {
    let run_dir = new_run_dir()?;
    eprintln!("beisl: trace dir = {}", run_dir.display());

    let mut child = Command::new(&cmd[0]);
    child.args(&cmd[1..]);
    mangohud::configure(&mut child, &run_dir); // frametime side-channel + toggle hotkey

    let mut recorder = perf::Recorder::new(&run_dir);

    // Publish the initial state so the window has something to show immediately.
    let mut sess = Session {
        game: game_name(cmd),
        trace_dir: run_dir.display().to_string(),
        hotkey: TOGGLE_KEY.to_string(),
        state: State::Starting,
        warnings: preflight_warnings(),
        ..Default::default()
    };
    let _ = sess.write(&run_dir);

    cue::play(cue::Cue::Ready);
    let mut proc = child.spawn()?;

    // Open the session window alongside the game (best-effort; headless still works).
    spawn_monitor(&run_dir);

    recorder.attach(proc.id());
    cue::play(cue::Cue::RecordStart);
    sess.state = State::Recording;
    let _ = sess.write(&run_dir);

    let start = Instant::now();
    let mut stopped = false;
    loop {
        if let Some(status) = proc.try_wait()? {
            if !stopped {
                recorder.finish();
                cue::play(cue::Cue::RecordStop);
            }
            sess.state = State::Done;
            sess.elapsed_secs = start.elapsed().as_secs();
            sess.trace_bytes = trace_size(&run_dir);
            sess.message = "Game exited — trace saved.".to_string();
            let _ = sess.write(&run_dir);
            return Ok(status.code().unwrap_or(0));
        }

        sess.elapsed_secs = start.elapsed().as_secs();
        sess.trace_bytes = trace_size(&run_dir);

        // The window can ask us to stop the trace while the game keeps running. We must
        // keep living until the game exits (Steam waits on the launch-options process),
        // so we only stop the recorder, not the game.
        if !stopped {
            if let Some(disk) = Session::read(&run_dir) {
                if disk.stop_requested {
                    recorder.finish();
                    cue::play(cue::Cue::RecordStop);
                    stopped = true;
                    sess.stop_requested = true;
                    sess.state = State::Done;
                    sess.message = "Stopped — trace saved. Game still running.".to_string();
                }
            }
        }
        let _ = sess.write(&run_dir);
        std::thread::sleep(Duration::from_millis(800));
    }
}

/// Launch the session window: prefer `beisl-setup` on PATH, else a sibling of this binary.
fn spawn_monitor(dir: &Path) {
    if Command::new("beisl-setup")
        .arg("--session")
        .arg(dir)
        .spawn()
        .is_ok()
    {
        return;
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(sibling) = exe.parent().map(|p| p.join("beisl-setup")) {
            let _ = Command::new(sibling).arg("--session").arg(dir).spawn();
        }
    }
}

/// Blocking preflight checks that still need setup — surfaced live as "what's wrong".
fn preflight_warnings() -> Vec<String> {
    let mut warns = Vec::new();
    let mut checks = preflight::all_checks();
    for c in checks.iter_mut() {
        c.detect();
        if c.blocking && c.status == preflight::Status::NeedsFix {
            warns.push(format!("{} needs setup — run beisl-setup", c.label));
        }
    }
    warns
}

fn trace_size(dir: &Path) -> u64 {
    std::fs::metadata(dir.join("perf.data"))
        .map(|m| m.len())
        .unwrap_or(0)
}

/// Best-effort human name for the game: the last `.exe` argument, else the command's
/// basename.
fn game_name(cmd: &[String]) -> String {
    cmd.iter()
        .rev()
        .find(|a| a.to_ascii_lowercase().ends_with(".exe"))
        .or_else(|| cmd.first())
        .map(|a| {
            Path::new(a)
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| a.clone())
        })
        .unwrap_or_default()
}

/// `$XDG_DATA_HOME/beisl/traces/run-<unix_ts>/` (falls back to `~/.local/share`).
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
