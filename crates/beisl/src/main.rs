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
mod handoff;
mod install;
mod mangohud;
mod out;
mod perf;
mod presence;
mod run;
mod steam;
mod symbols;
mod walls;

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
            println!("{}", help_text());
            return ExitCode::SUCCESS;
        }
        Some("--check" | "--preflight" | "doctor") => return check::run(),
        // Everything except these two hands the environment on to a game or a tool,
        // and the AppImage runtime's variables are not theirs to see. Install and
        // uninstall are the two that need to know where the image is.
        Some("install") => {
            let relax = args.iter().any(|a| a == "--relax-permission-check");
            return install::run(install::Action::Install { relax });
        }
        Some("uninstall") => return install::run(install::Action::Uninstall),
        Some("symbols") => {
            let Some(dir) = args.get(1) else {
                eprintln!("beisl: usage: beisl symbols <trace-run-dir>");
                return ExitCode::from(2);
            };
            return symbols::run(dir);
        }
        _ => {}
    }

    // The game's environment is the game's: started from its own AppImage, beisl
    // carries the runtime's APPIMAGE/APPDIR/ARGV0/OWD, and nothing it spawns should.
    // Only when they are provably beisl's. The same variables are inherited from a
    // terminal that is an AppImage, and then they are part of the environment the
    // user launched the game in: removing them would be changing it.
    if preflight::appimage::running().is_some() {
        preflight::appimage::strip_runtime_env();
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

/// The user docs are the single source of truth: this is the man-page source
/// (rendered by `docs-gen` into `man beisl`), and `--help` prints its top — cut at
/// the help-end marker, `# ` section markers stripped for the terminal.
/// (`beisl -- help` still traces a program named `help`.)
const DOC: &str = include_str!("../../../docs/beisl.1.md");

fn help_text() -> String {
    let top = DOC.split("<!-- help-end -->").next().unwrap_or(DOC);
    let lines: Vec<&str> = top
        .lines()
        .map(|l| l.strip_prefix("# ").unwrap_or(l))
        .collect();
    lines.join("\n").trim().to_string()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn help_names_every_subcommand() {
        // The dispatch above and the doc must not drift apart.
        let help = help_text();
        for needle in [
            "--check",
            "doctor",
            "symbols",
            "install",
            "uninstall",
            "--version",
            "%command%",
        ] {
            assert!(help.contains(needle), "help lost {needle}");
        }
    }

    #[test]
    fn help_is_the_docs_top_not_the_whole_man_page() {
        assert!(
            DOC.contains("<!-- help-end -->"),
            "marker gone — --help would dump the entire man page"
        );
        let help = help_text();
        assert!(help.starts_with("NAME"), "section markers stripped");
        assert!(
            !help.contains("EXIT STATUS"),
            "man-only depth leaked into --help"
        );
    }
}
