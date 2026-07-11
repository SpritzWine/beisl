//! Orchestrates one trace: set up the environment, spawn the game, open the session
//! window, and manage the `perf` recorder across the game's lifetime — publishing live
//! state to `session.json` for the window to display.

use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
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
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "no program to run",
        ));
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

    // Force Vulkan enumeration onto a specific GPU, if the user pinned one —
    // countering the "enumeration roulette" trap from DESIGN.md's Saints Row case study,
    // where two Vulkan-visible drivers silently invalidated an A/B. Plain config, no
    // privilege involved.
    let cfg = preflight::Config::load();
    if let Some(pin) = &cfg.gpu_pin {
        for (k, v) in gpu_pin_envs(pin) {
            log.line(&format!("gpu pin: {k}={v}"));
            child.env(k, v);
        }
    }

    // Frametime side-channel + toggle hotkey. Also aim MangoHud's GPU sampler at the card
    // the game actually renders on (the pinned GPU, else the sole GPU) — left to itself
    // the sampler picks a card independently of Vulkan device selection, and on a
    // multi-GPU box it happily logs an idle iGPU's stats as the game's.
    let mango_pci_dev = mangohud::sample_pci_dev(cfg.gpu_pin.as_ref());
    if let Some(addr) = &mango_pci_dev {
        log.line(&format!("mangohud: sample GPU pci_dev={addr}"));
    }
    mangohud::configure(&mut child, &run_dir, mango_pci_dev.as_deref());

    // Tee the child's stdout+stderr into game.log — Wine/vkd3d/DXVK diagnostics
    // (err: lines, device-lost messages) land on stderr, and inherited stdio alone is
    // gone once the launcher window closes. Tee, don't redirect: our own stdio IS the
    // Lutris log window / Steam console, and a beisl injected into launch options must
    // not swallow the game's output. Two handles onto the same file; interleaving of
    // the two streams is acceptable.
    let game_log = match std::fs::File::create(run_dir.join("game.log")) {
        Ok(f) => {
            child.stdout(Stdio::piped());
            child.stderr(Stdio::piped());
            Some(f)
        }
        Err(e) => {
            // Inherited stdio stays in place — the game still runs, just uncaptured.
            log.line(&format!(
                "game.log: couldn't create ({e}) — child output not captured"
            ));
            None
        }
    };

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
        gpu_pin: cfg.gpu_pin.as_ref().map(|p| p.name.clone()),
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

    // Wire up the tees now that the pipes exist. Detached threads: they end on pipe EOF
    // (child and any launcher descendants holding it gone), and at latest with us.
    if let Some(f) = game_log {
        let err_file = f.try_clone();
        if let Some(out) = proc.stdout.take() {
            tee(out, io::stdout(), f);
        }
        if let Some(err) = proc.stderr.take() {
            match err_file {
                Ok(f) => tee(err, io::stderr(), f),
                // Clone failed — keep at least the launcher-window side alive.
                Err(_) => tee_discard_file(err, io::stderr()),
            }
        }
    }

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
                // Decide what the trace is worth: a crash (non-zero code, or a GPU hang the
                // watcher saw) is always analyzed for forensics; a clean exit follows the
                // user's `clean-run-analysis` preference so a boring session doesn't burn
                // seconds resolving and keep tens of MB for nothing.
                let crashed = status.code() != Some(0) || gpuwatch.hang_seen();
                match decide_post_run(crashed, cfg.clean_run_analysis) {
                    PostRun::Analyze => {
                        let why = if crashed {
                            "abnormal exit"
                        } else {
                            "clean exit"
                        };
                        log.line(&format!("post-run: analyzing ({why})"));
                        match write_report(&run_dir, &sess.game, sess.elapsed_secs, &mut log) {
                            Some(headline) => format!("Game exited — trace saved. {headline}"),
                            None => "Game exited — trace saved.".to_string(),
                        }
                    }
                    PostRun::Keep => {
                        log.line("post-run: clean exit — leaving raw trace in place");
                        sess.analysis_offer = true;
                        "Game exited — trace saved (not analyzed). Analyze it any time in \
                         the window, or run `beisl symbols`."
                            .to_string()
                    }
                    PostRun::Discard => {
                        let _ = std::fs::remove_file(run_dir.join("perf.data"));
                        sess.trace_bytes = 0;
                        log.line("post-run: clean exit — perf trace discarded (per preference)");
                        "Game exited — clean run, perf trace discarded (per your setting)."
                            .to_string()
                    }
                }
            } else {
                "Game exited.".to_string()
            };
            let _ = sess.write(&run_dir);
            // With launcher chains (umu-run → launcher → game) this is the *wrapper's*
            // status — it can be 0 even when the real game process died.
            log.line(&format!(
                "launched command exited: {status} (with launcher chains this is the \
                 wrapper's status, not the game's), trace {} bytes",
                sess.trace_bytes
            ));
            return Ok(status.code().unwrap_or(0));
        }

        sess.elapsed_secs = start.elapsed().as_secs();
        sess.trace_bytes = trace_size(&run_dir);
        drain_gpu_events(&mut gpuwatch, &mut log, &mut sess, &run_dir);

        // MangoHud names its CSV after the *real* game process; with launcher chains
        // that beats the argv guess (which names the wrapper, e.g. "upc.exe").
        if let Some(game) = mangohud_game_name(&run_dir) {
            if sess.game != game {
                log.line(&format!(
                    "game name from MangoHud csv: {game} (was: {})",
                    sess.game
                ));
                sess.game = game;
            }
        }

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
                log.line(&format!(
                    "gpu: devcoredump on {card} captured -> {}",
                    dest.display()
                ));
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
                log.line(&format!(
                    "gpu: devcoredump pending on {card} but root-only ({})",
                    node.display()
                ));
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

/// The child-process env vars a GPU pin translates to: `VK_DRIVER_FILES` (the pin's ICD
/// manifest paths, colon-joined — the format Vulkan's loader expects) and, only when we
/// could resolve it, `MESA_VK_DEVICE_SELECT`. Split out from `trace` so it's testable
/// without spawning a real process.
fn gpu_pin_envs(pin: &preflight::GpuPin) -> Vec<(String, String)> {
    let mut envs = vec![("VK_DRIVER_FILES".to_string(), pin.icd_paths.join(":"))];
    if let Some(sel) = &pin.device_select {
        envs.push(("MESA_VK_DEVICE_SELECT".to_string(), sel.clone()));
    }
    envs
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

/// What to do with a finished trace: [`decide_post_run`] picks it from the exit status
/// and the user's clean-run preference.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PostRun {
    /// Resolve now and write the report (crash, or the `always` preference).
    Analyze,
    /// Leave the raw trace in place, untouched — the default. Analyze later via the window
    /// or `beisl symbols`; nothing is discarded.
    Keep,
    /// Drop the perf trace to reclaim space (clean exit, `discard` preference).
    Discard,
}

/// Crash → always analyze (that's the whole point of the tool); clean exit → the user's
/// preference. `crashed` folds the game's exit code together with a detected GPU hang,
/// because with launcher chains (umu-run → … → game) the code we see can be 0 even when
/// the game died — the hang signal catches that.
fn decide_post_run(crashed: bool, policy: preflight::CleanRunAnalysis) -> PostRun {
    if crashed {
        return PostRun::Analyze;
    }
    match policy {
        preflight::CleanRunAnalysis::Always => PostRun::Analyze,
        preflight::CleanRunAnalysis::Keep => PostRun::Keep,
        preflight::CleanRunAnalysis::Discard => PostRun::Discard,
    }
}

/// Resolve the finished trace and drop `artifact.json` (the exportable, GUI/MCP-ready
/// merge) + `report.txt` (human-readable) into the run dir. Best-effort — a resolution
/// failure (no `perf` at analysis time, unreadable data) must never turn a captured trace
/// into a wrapper error. Returns the one-line headline for the session message.
fn write_report(run_dir: &Path, game: &str, duration_s: u64, log: &mut Log) -> Option<String> {
    log.line("post-run: resolving symbols");
    let art = match artifact::TraceArtifact::build(run_dir, game, duration_s as f64) {
        Ok(a) => a,
        Err(e) => {
            log.line(&format!("post-run: resolution failed ({e})"));
            return None;
        }
    };
    let _ = art.write_json(&run_dir.join("artifact.json"));
    let _ = std::fs::write(run_dir.join("report.txt"), art.render_report());
    let headline = art.headline();
    log.line(&format!("post-run: {headline}"));
    Some(headline)
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

/// Pump `src` into both `term` and `file` until EOF, on a detached thread. Raw chunks,
/// flushed to the terminal side per read so the game's output reaches the Lutris/Steam
/// log view as promptly as it would without us in between.
fn tee<R, W>(mut src: R, mut term: W, mut file: std::fs::File)
where
    R: io::Read + Send + 'static,
    W: io::Write + Send + 'static,
{
    use io::Write as _;
    std::thread::spawn(move || {
        let mut buf = [0u8; 8192];
        loop {
            match src.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    let _ = term.write_all(&buf[..n]);
                    let _ = term.flush();
                    let _ = file.write_all(&buf[..n]);
                }
            }
        }
    });
}

/// [`tee`] without the file sink, for when cloning the game.log handle failed: the
/// launcher-window pass-through is the part we must never lose.
fn tee_discard_file<R, W>(mut src: R, mut term: W)
where
    R: io::Read + Send + 'static,
    W: io::Write + Send + 'static,
{
    std::thread::spawn(move || {
        let _ = io::copy(&mut src, &mut term);
    });
}

/// The real game's name from a MangoHud frametime CSV in the run dir, if one has
/// appeared yet. MangoHud writes it mid-session, named after the game process.
fn mangohud_game_name(run_dir: &Path) -> Option<String> {
    std::fs::read_dir(run_dir)
        .ok()?
        .flatten()
        .find_map(|e| csv_game_name(&e.file_name().to_string_lossy()))
}

/// `"Outlaws_2026-07-11_07-15-40.csv"` → `Some("Outlaws")`. The suffix is MangoHud's
/// `_%Y-%m-%d_%H-%M-%S` timestamp; filenames that don't match are ignored.
fn csv_game_name(file_name: &str) -> Option<String> {
    let stem = file_name.strip_suffix(".csv")?;
    let (rest, time) = stem.rsplit_once('_')?;
    let (game, date) = rest.rsplit_once('_')?;
    fn is_stamp(s: &str, lens: &[usize]) -> bool {
        let parts: Vec<&str> = s.split('-').collect();
        parts.len() == lens.len()
            && parts
                .iter()
                .zip(lens)
                .all(|(p, l)| p.len() == *l && p.bytes().all(|c| c.is_ascii_digit()))
    }
    (!game.is_empty() && is_stamp(date, &[4, 2, 2]) && is_stamp(time, &[2, 2, 2]))
        .then(|| game.to_string())
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

#[cfg(test)]
mod tests {
    use super::*;
    use preflight::{CleanRunAnalysis, GpuPin};

    // The Arc A770 from the case-study machine as a MESA_VK_DEVICE_SELECT selector
    // (same fixture naming as the mangohud/preflight test modules).
    const ARC_SELECT: &str = "8086:56a0!";

    #[test]
    fn a_crash_is_always_analyzed_whatever_the_preference() {
        for policy in [
            CleanRunAnalysis::Keep,
            CleanRunAnalysis::Always,
            CleanRunAnalysis::Discard,
        ] {
            assert_eq!(
                decide_post_run(true, policy),
                PostRun::Analyze,
                "{policy:?}"
            );
        }
    }

    #[test]
    fn a_clean_exit_follows_the_preference() {
        // The default keeps the trace in place — a clean run is never discarded by default.
        assert_eq!(
            decide_post_run(false, CleanRunAnalysis::default()),
            PostRun::Keep
        );
        assert_eq!(
            decide_post_run(false, CleanRunAnalysis::Keep),
            PostRun::Keep
        );
        assert_eq!(
            decide_post_run(false, CleanRunAnalysis::Always),
            PostRun::Analyze
        );
        assert_eq!(
            decide_post_run(false, CleanRunAnalysis::Discard),
            PostRun::Discard
        );
    }

    #[test]
    fn gpu_pin_envs_joins_icd_paths_and_sets_device_select() {
        let pin = GpuPin {
            name: "Intel — intel_icd (/usr/share/vulkan/icd.d)".into(),
            icd_paths: vec![
                "/usr/share/vulkan/icd.d/intel_icd.x86_64.json".into(),
                "/usr/share/vulkan/icd.d/intel_icd.i686.json".into(),
            ],
            device_select: Some(ARC_SELECT.into()),
        };
        let envs = gpu_pin_envs(&pin);
        assert_eq!(
            envs,
            vec![
                (
                    "VK_DRIVER_FILES".to_string(),
                    "/usr/share/vulkan/icd.d/intel_icd.x86_64.json:\
                     /usr/share/vulkan/icd.d/intel_icd.i686.json"
                        .to_string()
                ),
                ("MESA_VK_DEVICE_SELECT".to_string(), ARC_SELECT.to_string()),
            ]
        );
    }

    #[test]
    fn gpu_pin_envs_omits_device_select_when_unresolved() {
        let pin = GpuPin {
            name: "nouveau_icd (/usr/share/vulkan/icd.d)".into(),
            icd_paths: vec!["/usr/share/vulkan/icd.d/nouveau_icd.x86_64.json".into()],
            device_select: None,
        };
        let envs = gpu_pin_envs(&pin);
        assert_eq!(envs.len(), 1);
        assert_eq!(envs[0].0, "VK_DRIVER_FILES");
    }

    #[test]
    fn csv_game_name_parses_mangohud_filenames() {
        assert_eq!(
            csv_game_name("Outlaws_2026-07-11_07-15-40.csv").as_deref(),
            Some("Outlaws")
        );
        // Underscores in the game name survive — only the timestamp suffix is peeled.
        assert_eq!(
            csv_game_name("Blood_West_2026-07-11_07-15-40.csv").as_deref(),
            Some("Blood_West")
        );
    }

    #[test]
    fn csv_game_name_rejects_non_mangohud_files() {
        for name in [
            "perf.data",
            "beisl.log",
            "game.log",
            "notes.csv",                       // no timestamp suffix
            "Outlaws_2026-07-11.csv",          // date only
            "_2026-07-11_07-15-40.csv",        // empty game name
            "Outlaws_2026-7-11_07-15-40.csv",  // malformed date
            "Outlaws_2026-07-11_07-15-4x.csv", // non-digit in time
        ] {
            assert_eq!(csv_game_name(name), None, "{name}");
        }
    }

    /// A configured pin must always survive into the session's human-readable label
    /// (`session::Session::gpu_pin`), the same field the session window shows. This locks
    /// down the mapping `trace()` does inline (`cfg.gpu_pin.as_ref().map(|p| p.name...)`).
    #[test]
    fn session_gpu_pin_label_mirrors_the_configured_pins_name() {
        let pin = GpuPin {
            name: "Intel — intel_icd (/usr/share/vulkan/icd.d)".into(),
            icd_paths: vec!["/a.json".into()],
            device_select: None,
        };
        let cfg = preflight::Config {
            gpu_pin: Some(pin.clone()),
            ..Default::default()
        };
        let label = cfg.gpu_pin.as_ref().map(|p| p.name.clone());
        assert_eq!(label.as_deref(), Some(pin.name.as_str()));
    }
}
