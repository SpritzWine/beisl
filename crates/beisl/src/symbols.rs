//! `beisl symbols <run-dir>` — the headless face of the trace-analysis pipeline.
//!
//! Builds the same [`artifact::TraceArtifact`] the wrapper writes at the end of a trace
//! (resolve `perf.data` via `symbol-resolver`, fold in the frametime CSV), prints its
//! report, and persists `artifact.json` + `report.txt` into the run dir via
//! [`artifact::TraceArtifact::persist`] - the same door the wrapper's post-run step and
//! the GUI's Analyze use, so this documented recovery command (`clean-run-analysis =
//! keep`'s default tells the user to run it later) leaves a run MCP can actually read
//! back. Binds to exactly the library the GUI/MCP will - same relationship as `beisl
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
            // A retry succeeded on the same perf.data — drop any stale failure verdict.
            session::library::clear_invalid(dir);
            out!("{}", a.render_report().trim_end());
            // The report already printed above - that's this command's whole point - so
            // a persistence failure (e.g. a read-only run dir) must not fail the command.
            if let Err(e) = a.persist(dir) {
                eprintln!("beisl: couldn't save artifact.json/report.txt in {run_dir} ({e})");
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            // Say so on disk too — same truth via `beisl-setup`'s ask, whichever door
            // hits the failure first.
            let _ = session::library::mark_invalid(dir, &e.to_string());
            eprintln!("beisl: symbol resolution failed ({e}) — is `perf` installed?");
            ExitCode::FAILURE
        }
    }
}
