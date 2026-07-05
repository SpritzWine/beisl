//! Orchestrates one trace: set up the environment, spawn the game, open the session
//! window, and manage the `perf` recorder across the game's lifetime — publishing live
//! state to `session.json` for the window to display.

use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use session::{Session, State};

use crate::mangohud::TOGGLE_KEY;
use crate::{cue, gpu, mangohud, perf};

/// Run `cmd` under tracing; returns the game's exit code.
pub fn trace(cmd: &[String]) -> io::Result<i32> {
    let run_dir = new_run_dir()?;
    let mut log = Log::open(&run_dir);
    log.line(&format!("launch argv = {cmd:?}"));

    // Steam runs launch options through a shell, and some %command% expansions carry
    // leading `VAR=value` env assignments; peel those off so we run the real program, not
    // a literal "VAR=value". A no-op when there are none (e.g. the usual reaper command).
    let (envs, prog) = split_env_prefix(cmd);
    if prog.is_empty() {
        log.line("error: nothing to run after env assignments");
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "no program to run"));
    }
    if !envs.is_empty() {
        log.line(&format!("env prefix = {envs:?}"));
    }
    log.line(&format!(
        "program = {} (exists: {}), args = {:?}",
        prog[0],
        Path::new(&prog[0]).exists(),
        &prog[1..]
    ));

    let mut child = Command::new(&prog[0]);
    child.args(&prog[1..]);
    for (k, v) in &envs {
        child.env(k, v);
    }
    mangohud::configure(&mut child, &run_dir); // frametime side-channel + toggle hotkey

    let mut recorder = perf::Recorder::new(&run_dir);
    // Watch for GPU hangs: xe keeps exactly ONE devcoredump, so capture-and-free
    // during the session or the next hang records nothing (see DESIGN.md case study).
    let mut gpuwatch = gpu::GpuWatch::new(&run_dir);

    // Publish the initial state so the window has something to show immediately.
    let mut sess = Session {
        game: game_name(&prog),
        trace_dir: run_dir.display().to_string(),
        hotkey: TOGGLE_KEY.to_string(),
        state: State::Starting,
        warnings: preflight_warnings(),
        ..Default::default()
    };
    let _ = sess.write(&run_dir);

    cue::play(cue::Cue::Ready);
    let mut proc = match child.spawn() {
        Ok(p) => p,
        Err(e) => {
            log.line(&format!("error: failed to launch {}: {e}", prog[0]));
            sess.state = State::Error;
            sess.message = format!("Failed to launch {}: {e}", prog[0]);
            let _ = sess.write(&run_dir);
            spawn_monitor(&run_dir); // open the window so the error is visible
            return Err(e);
        }
    };
    log.line(&format!("launched pid {}", proc.id()));

    // Open the session window alongside the game (best-effort; headless still works).
    spawn_monitor(&run_dir);

    // Record the whole session. If perf can't run, the game still launches un-traced and
    // the reason shows up as a live warning.
    match recorder.start() {
        Ok(()) => {
            sess.state = State::Recording;
            log.line("perf: recording started");
        }
        Err(e) => {
            sess.state = State::Running;
            sess.warnings.push(e.clone());
            log.line(&format!("perf: {e}"));
        }
    }
    // Stop perf cleanly if beisl is signalled (Steam's Stop button = SIGTERM, or Ctrl-C).
    perf::install_teardown();
    let _ = sess.write(&run_dir);

    let start = Instant::now();
    let mut stopped = false;
    loop {
        if let Some(status) = proc.try_wait()? {
            if !stopped {
                recorder.stop();
                cue::play(cue::Cue::RecordStop);
            }
            // A hang often coincides with the game dying — one last look.
            drain_gpu_events(&mut gpuwatch, &mut log, &mut sess, &run_dir);
            sess.state = State::Done;
            sess.elapsed_secs = start.elapsed().as_secs();
            sess.trace_bytes = trace_size(&run_dir);
            sess.message = if sess.trace_bytes > 0 {
                "Game exited — trace saved.".to_string()
            } else {
                "Game exited.".to_string()
            };
            let _ = sess.write(&run_dir);
            log.line(&format!("game exited: {status}, trace {} bytes", sess.trace_bytes));
            return Ok(status.code().unwrap_or(0));
        }

        sess.elapsed_secs = start.elapsed().as_secs();
        sess.trace_bytes = trace_size(&run_dir);
        drain_gpu_events(&mut gpuwatch, &mut log, &mut sess, &run_dir);

        // The window can ask us to stop the trace while the game keeps running. We must
        // keep living until the game exits (Steam waits on the launch-options process),
        // so we only stop the recorder, not the game.
        if !stopped {
            if let Some(disk) = Session::read(&run_dir) {
                if disk.stop_requested {
                    recorder.stop();
                    cue::play(cue::Cue::RecordStop);
                    stopped = true;
                    sess.stop_requested = true;
                    sess.trace_bytes = trace_size(&run_dir);
                    sess.state = State::Done;
                    sess.message = "Stopped — trace saved. Game still running.".to_string();
                }
            }
        }
        let _ = sess.write(&run_dir);
        std::thread::sleep(Duration::from_millis(800));
    }
}

/// Surface GPU-hang captures/warnings live: into the log and the session window.
///
/// User-facing wording is layered (DESIGN + Florian's spec): plain diagnosis first ("your
/// GPU, not the game or Wine"), the offer to help gather evidence second, the technical
/// detail (card, filename, reboot advice) last.
fn drain_gpu_events(watch: &mut gpu::GpuWatch, log: &mut Log, sess: &mut Session, run_dir: &Path) {
    for ev in watch.poll() {
        match ev {
            gpu::Event::Captured { card, dest } => {
                log.line(&format!("gpu: devcoredump on {card} captured -> {}", dest.display()));
                let file = dest
                    .file_name()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default();
                sess.warnings.push(format!(
                    "Something went wrong and your GPU was the cause — not the game's code and \
                     not Wine. Beisl saved the crash evidence for you ({file}). \
                     Heads-up: the GPU had to be reset, so reboot before your next comparison \
                     run — results until then can't be trusted. (GPU: {card})"
                ));
            }
            gpu::Event::Unreadable { card, node } => {
                log.line(&format!("gpu: devcoredump pending on {card} but root-only ({})", node.display()));
                // Offer the one-click enable only under the warn-only policy.
                let offer = preflight::Config::load().dump_access_policy
                    == preflight::DumpAccessPolicy::WarnOnly;
                sess.dump_capture_offer = offer;
                let bridge = if offer {
                    "Want to help get this fixed? Turn on crash-dump capture and Beisl will save \
                     the evidence — this crash's dump can still be rescued."
                } else {
                    "Beisl couldn't read the crash dump this time."
                };
                sess.warnings.push(format!(
                    "Something went wrong and your GPU was the cause — not the game's code and \
                     not Wine. {bridge} To grab it by hand instead: {} (GPU: {card})",
                    gpu::sudo_capture_hint(&node, run_dir, &card)
                ));
            }
        }
    }
}

/// Peel leading `VAR=value` tokens (Steam/user env assignments) off the front of the
/// command, returning them plus the remaining program + args. Stops at the first token
/// that isn't an assignment, so a normal `reaper …` command is left untouched.
fn split_env_prefix(cmd: &[String]) -> (Vec<(String, String)>, Vec<String>) {
    let mut envs = Vec::new();
    let mut i = 0;
    while i < cmd.len() {
        match cmd[i].split_once('=') {
            Some((k, v)) if is_env_name(k) => {
                envs.push((k.to_string(), v.to_string()));
                i += 1;
            }
            _ => break,
        }
    }
    (envs, cmd[i..].to_vec())
}

fn is_env_name(s: &str) -> bool {
    let mut chars = s.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// A small append-only diagnostic log in the trace dir. Steam swallows the wrapper's
/// stderr, so this is where a failed launch actually surfaces; mirrored to stderr too.
struct Log {
    file: Option<std::fs::File>,
}

impl Log {
    fn open(dir: &Path) -> Self {
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join("beisl.log"))
            .ok();
        Log { file }
    }

    fn line(&mut self, s: &str) {
        eprintln!("beisl: {s}");
        if let Some(f) = self.file.as_mut() {
            use std::io::Write;
            let _ = writeln!(f, "{s}");
        }
    }
}

/// Launch the session window: prefer `beisl-setup` on PATH, else a sibling of this binary.
fn spawn_monitor(dir: &Path) {
    // Headless / testing: `BEISL_NO_UI=1` runs the wrapper without the window.
    if std::env::var_os("BEISL_NO_UI").is_some() {
        return;
    }
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
