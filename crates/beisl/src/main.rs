//! Beisl — the perf-tracing wrapper.
//!
//! Usage: `beisl [--] <command> [args...]`
//!   (Steam launch option: `beisl %command%`; or one toggle in the SpritzWine panel.)
//!
//! It wraps an arbitrary game/command: configures MangoHud to log frametimes as a
//! side-channel *spike index*, records CPU samples with `perf` scoped to the game's
//! process tree, and (TODO) merges them into a symbolicated trace artifact.
//!
//! Wine-agnostic by design — SpritzWine/Prater, stock Proton, GE, Lutris-wine,
//! system wine, or a native Linux game. See DESIGN.md.

use std::env;
use std::process::ExitCode;

mod check;
mod cue;
mod install;
mod mangohud;
mod gpu;
mod perf;
mod run;

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();

    // Subcommands. `--check` runs the setup preflight (the GUI binds to the same
    // `preflight` crate; this is its headless face); `install`/`uninstall` put `beisl`
    // on PATH so it works in launch options.
    match args.first().map(String::as_str) {
        Some("--check" | "--preflight" | "doctor") => return check::run(),
        Some("install") => return install::run(false),
        Some("uninstall") => return install::run(true),
        _ => {}
    }

    let cmd = match parse_command(&args) {
        Some(c) if !c.is_empty() => c,
        _ => {
            eprintln!(
                "beisl: no command given\n\n\
                 usage: beisl [--] <command> [args...]\n       \
                 (Steam launch option: `beisl %command%`)"
            );
            return ExitCode::from(2);
        }
    };

    match run::trace(&cmd) {
        Ok(code) => ExitCode::from(code as u8),
        Err(e) => {
            eprintln!("beisl: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Everything after the binary name is the wrapped command; an optional leading
/// `--` separator is tolerated (`beisl -- %command%`).
fn parse_command(args: &[String]) -> Option<Vec<String>> {
    if args.is_empty() {
        return None;
    }
    let start = if args[0] == "--" { 1 } else { 0 };
    Some(args[start..].to_vec())
}
