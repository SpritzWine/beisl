//! `beisl --check` — the headless face of the preflight checklist.
//!
//! Renders exactly the state the libcosmic setup page will bind to: each check's
//! status, its `why` line, and the `pkexec` fix command it would run. Runs nothing
//! privileged itself.

use std::process::ExitCode;

use preflight::{
    all_checks, detect_distro, disk_check, driver_debuginfo_check, fix_command, gpu_dump_check,
    multi_gpu_check, Status,
};

/// `println!` that shrugs off a closed stdout (`beisl --check | head`): the reader
/// hanging up mid-listing is not an error worth a panic — std's `println!` aborts with
/// a broken-pipe panic there. Drop the remaining output, keep the exit code honest.
macro_rules! out {
    ($($t:tt)*) => {{
        use std::io::Write as _;
        let _ = writeln!(std::io::stdout(), $($t)*);
    }};
}

pub fn run() -> ExitCode {
    let distro = detect_distro();
    out!("Beisl preflight  (distro family: {distro:?})\n");

    let mut checks = all_checks();
    for c in checks.iter_mut() {
        c.detect();
    }
    // The disk check needs the trace directory, so it's built (and detected) here.
    checks.push(disk_check(&crate::run::data_root().join("traces")));
    // The GPU dump-access row is optional and opt-in — shown, but never swept into a
    // blocking fix (it's non-blocking, and the GUI drives its own enable button).
    checks.push(gpu_dump_check());
    // The multi-GPU/dual-ICD row is optional and informational — its remedy is picking a
    // pin in the GUI's Advanced section, not a privileged fix, so it never gates readiness.
    checks.push(multi_gpu_check());
    // The driver debug-symbols row is optional and informational — the remedy is a
    // manual, distro-specific install named in the why, never a one-click fix.
    checks.push(driver_debuginfo_check());

    let mut blocking_missing = false;

    for c in &checks {
        let mark = match &c.status {
            Status::Ok => "[x]",
            Status::Failed(_) => "[!]",
            _ => "[ ]",
        };
        let state = match &c.status {
            Status::Ok => "ok".to_string(),
            Status::NeedsFix => "needs setup".to_string(),
            Status::Failed(e) => format!("failed: {e}"),
            other => format!("{other:?}"),
        };
        out!("{mark} {:<24} {}", c.label, state);
        out!("       {}", c.why);

        if c.status == Status::NeedsFix {
            if let Some(fix) = &c.fix {
                match fix_command(fix, distro) {
                    cmd if cmd.is_empty() => {
                        out!("       fix:  (install manually — unknown for this distro)")
                    }
                    // Multi-line elevated scripts (the dump-access rule) are unreadable as a
                    // single line — summarise instead of dumping the whole heredoc.
                    cmd if cmd.iter().any(|a| a.contains('\n')) => {
                        out!("       fix:  enable in beisl-setup (installs a udev rule + tmpfiles entry via pkexec)")
                    }
                    cmd => out!("       fix:  {}", cmd.join(" ")),
                }
            }
            if c.blocking {
                blocking_missing = true;
            }
        }
        out!();
    }

    if blocking_missing {
        out!("Some required checks need setup — the GUI runs each `fix:` line via pkexec.");
        ExitCode::from(1)
    } else {
        out!("All required checks pass — ready to trace.");
        ExitCode::SUCCESS
    }
}
