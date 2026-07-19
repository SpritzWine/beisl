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
/// Thin forward to `preflight::debuginfo::fetch_urls` — the URL policy moved there so the
/// GUI's per-module "Install debuginfo" work (a `preflight`-only concern) could sit next
/// to it; see that function's doc comment for the three-way policy.
pub(crate) fn debuginfod_urls(cfg: &Config) -> Option<String> {
    preflight::debuginfo::fetch_urls(cfg)
}

pub fn run(run_dir: &str) -> ExitCode {
    let dir = Path::new(run_dir);
    if !dir.join("perf.data").is_file() {
        eprintln!("beisl: no perf.data in {run_dir} (expected a trace run directory)");
        return ExitCode::from(2);
    }
    // The game name, duration, and record mode are best-effort from the run's session.json
    // (record_mode empty when unreadable — same as a legacy run that predates the field, so
    // the report simply omits the degraded note rather than guessing).
    let (game, duration, record_mode) = session::Session::read(dir)
        .map(|s| (s.game, s.elapsed_secs as f64, s.record_mode))
        .unwrap_or_else(|| ("unknown".to_string(), 0.0, String::new()));

    let urls = debuginfod_urls(&Config::load());
    match artifact::TraceArtifact::build_with_debuginfod(
        dir,
        &game,
        duration,
        &record_mode,
        urls.as_deref(),
    ) {
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
