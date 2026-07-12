//! `beisl symbols <run-dir>` — the headless face of the trace-analysis pipeline.
//!
//! Builds the same [`artifact::TraceArtifact`] the wrapper writes at the end of a trace
//! (resolve `perf.data` via `symbol-resolver`, fold in the frametime CSV) and prints its
//! report. Binds to exactly the library the GUI/MCP will — same relationship as `beisl
//! --check` and the setup page both binding to `preflight`.

use std::path::Path;
use std::process::ExitCode;

use preflight::Config;

use crate::out::out;

/// The `DEBUGINFOD_URLS` to hand the perf-side resolve, honoring the user's preference.
///
/// `perf` fetches missing ELF driver debug symbols on demand only when this var is set for
/// its process. A trace resolved inside Steam's game-launch session doesn't inherit the
/// interactive shell's value (see the artifact `build_with_debuginfod` note), so we set it
/// explicitly — otherwise the driver layer silently resolves to anonymous addresses in
/// exactly the auto-analyzed crash traces the tool exists for.
///
/// * fetch **off** (poweruser Advanced toggle): return `Some("")` — force-disable even if
///   the launching shell exported a server, so an offline/hermetic resolve stays offline.
/// * fetch **on**, env already set: return `None` — inherit the user's own value verbatim.
/// * fetch **on**, env unset (the common Steam/Arch/SteamOS case): inject the distro
///   default (which appends the federated elfutils aggregator), so resolution is
///   reproducible regardless of what launched it.
pub(crate) fn debuginfod_urls(cfg: &Config) -> Option<String> {
    if !cfg.debuginfod_fetch {
        return Some(String::new());
    }
    if std::env::var_os("DEBUGINFOD_URLS").is_some_and(|v| !v.is_empty()) {
        return None;
    }
    preflight::debuginfo::distro_debuginfod_url(preflight::detect_distro())
}

pub fn run(run_dir: &str) -> ExitCode {
    let dir = Path::new(run_dir);
    if !dir.join("perf.data").is_file() {
        eprintln!("beisl: no perf.data in {run_dir} (expected a trace run directory)");
        return ExitCode::from(2);
    }
    // The game name is best-effort from the run's session.json; duration likewise.
    let (game, duration) = session::Session::read(dir)
        .map(|s| (s.game, s.elapsed_secs as f64))
        .unwrap_or_else(|| ("unknown".to_string(), 0.0));

    let urls = debuginfod_urls(&Config::load());
    match artifact::TraceArtifact::build_with_debuginfod(dir, &game, duration, urls.as_deref()) {
        Ok(a) => {
            out!("{}", a.render_report().trim_end());
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("beisl: symbol resolution failed ({e}) — is `perf` installed?");
            ExitCode::FAILURE
        }
    }
}
