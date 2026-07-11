//! `beisl symbols <run-dir>` — the headless face of the trace-analysis pipeline.
//!
//! Builds the same [`artifact::TraceArtifact`] the wrapper writes at the end of a trace
//! (resolve `perf.data` via `symbol-resolver`, fold in the frametime CSV) and prints its
//! report. Binds to exactly the library the GUI/MCP will — same relationship as `beisl
//! --check` and the setup page both binding to `preflight`.

use std::path::Path;
use std::process::ExitCode;

use crate::out::out;

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

    match artifact::TraceArtifact::build(dir, &game, duration) {
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
