//! Beisl — the perf-tracing wrapper.
//!
//! Usage: `beisl [--] <command> [args...]`
//!   (Steam launch option: `beisl %command%`; or one toggle in the SpritzWine panel.)
//!
//! It wraps an arbitrary game/command: configures MangoHud to log frametimes as a
//! side-channel *spike index*, records CPU samples with `perf` across the session
//! (system-wide; cgroup scoping is a roadmap item), and merges them into a symbolicated
//! trace artifact (artifact.json + report.txt).
//!
//! Wine-agnostic by design — SpritzWine/Prater, stock Proton, GE, Lutris-wine,
//! system wine, or a native Linux game. See DESIGN.md.

use std::env;
use std::process::ExitCode;

mod check;
mod cue;
mod gpu;
mod install;
mod mangohud;
mod out;
mod perf;
mod run;
mod symbols;

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();

    // Subcommands. `--check` runs the setup preflight (the GUI binds to the same
    // `preflight` crate; this is its headless face); `install`/`uninstall` put `beisl`
    // on PATH so it works in launch options; `symbols` resolves a captured trace.
    match args.first().map(String::as_str) {
        Some("--version") => {
            println!("beisl {}", preflight::VERSION);
            return ExitCode::SUCCESS;
        }
        Some("--help" | "-h" | "help") => {
            println!("{HELP}");
            return ExitCode::SUCCESS;
        }
        Some("--check" | "--preflight" | "doctor") => return check::run(),
        Some("install") => return install::run(false),
        Some("uninstall") => return install::run(true),
        Some("symbols") => {
            let Some(dir) = args.get(1) else {
                eprintln!("beisl: usage: beisl symbols <trace-run-dir>");
                return ExitCode::from(2);
            };
            return symbols::run(dir);
        }
        _ => {}
    }

    let cmd = match parse_command(&args) {
        Some(c) if !c.is_empty() => c,
        _ => {
            eprintln!(
                "beisl: no command given\n\n\
                 usage: beisl [--] <command> [args...]\n       \
                 (Steam launch option: `beisl %command%` — see `beisl --help`)"
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

/// `beisl --help` — one screen, plain language first; the manpage is the roadmap's
/// place for depth. (`beisl -- help` still traces a program named `help`.)
const HELP: &str = "\
beisl — game perf tracing: perf CPU samples + MangoHud frametimes, wine-agnostic

usage:
  beisl [--] <command> [args...]   trace a game
                                   (Steam launch option: `beisl %command%`)
  beisl --check                    setup checklist in the terminal
                                   (aliases: --preflight, doctor)
  beisl symbols <trace-run-dir>    resolve + report a captured trace
  beisl install                    copy beisl into ~/.local/bin
                                   (+ \"Beisl Setup\" menu entry when bundled)
  beisl uninstall                  remove the per-user install
  beisl --version                  print the version
  beisl --help                     this help

Traces land in ~/.local/share/beisl/traces/run-*/ — frametime CSV, perf.data,
logs; artifact.json + report.txt once analyzed. `beisl-setup` is the GUI face:
the same checklist with one-click fixes, plus the trace library.";

/// Everything after the binary name is the wrapped command; an optional leading
/// `--` separator is tolerated (`beisl -- %command%`).
fn parse_command(args: &[String]) -> Option<Vec<String>> {
    if args.is_empty() {
        return None;
    }
    let start = if args[0] == "--" { 1 } else { 0 };
    Some(args[start..].to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn help_names_every_subcommand() {
        // The dispatch above and this text must not drift apart.
        for needle in [
            "--check",
            "doctor",
            "symbols",
            "install",
            "uninstall",
            "--version",
            "%command%",
        ] {
            assert!(HELP.contains(needle), "help lost {needle}");
        }
    }
}
