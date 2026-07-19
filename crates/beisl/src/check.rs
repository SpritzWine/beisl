//! `beisl --check` — the headless face of the preflight checklist.
//!
//! Renders exactly the state the libcosmic setup page will bind to: each check's
//! status, its `why` line, and the `pkexec` fix command it would run. Runs nothing
//! privileged itself.

use std::process::ExitCode;

use crate::out::out;
use preflight::{
    all_checks, detect_distro, detect_record_mode, detect_system_kind, disk_check,
    driver_debuginfo_check, fix_command, gpu_dump_check, locked_system_message,
    mangohud_hotkey_check, multi_gpu_check, ostree_install_note, perf_debuginfod_check,
    perf_record_fix, steam_visible_beisl_check, CheckId, RecordMode, Status, SystemKind,
};

pub fn run() -> ExitCode {
    let distro = detect_distro();
    let system = detect_system_kind();

    // A locked immutable system (SteamOS) can't get a host `perf` — say so plainly and point
    // the user at a box where beisl works, instead of walking a checklist that can't pass.
    if system == SystemKind::Locked {
        out!("{}", locked_system_message());
        return ExitCode::from(1);
    }

    out!("Beisl preflight  (distro family: {distro:?})\n");
    // What a trace captures on this box, as a fidelity ladder with the current tier marked —
    // the "perf can record" check folds into this: declining CAP_PERFMON isn't "broken", it's
    // the degraded tier. Only a genuine `none` (perf missing / paranoid too high) can't trace.
    let mode = detect_record_mode();
    out!("Perf record tier — what a trace captures here:");
    for tier in [RecordMode::Full, RecordMode::Degraded, RecordMode::None] {
        let mark = if tier == mode { "[x]" } else { "[ ]" };
        out!("  {mark} {:<9} {}", tier.as_str(), tier.describe());
    }
    // Below Full: show how to reach it (grant CAP_PERFMON — described here, never run).
    if mode != RecordMode::Full {
        let cmd = fix_command(&perf_record_fix(), distro, system);
        if !cmd.is_empty() {
            out!("      \u{2192} for full: {}", cmd.join(" "));
        }
    }
    out!();
    // On an ostree box the install fixes below are layered and need a reboot — flag it once.
    if system == SystemKind::OstreeAtomic {
        out!("{}\n", ostree_install_note());
    }

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
    // Advisory companion: whether `perf` can fetch those symbols on demand at all (built
    // with debuginfod support). Beisl injects the URL itself, so this is the only piece it
    // can't paper over — informational, never a one-click fix.
    checks.push(perf_debuginfod_check());
    // Advisory: the MangoHud #2053 hotkey crash — warned about, never "fixed" by us.
    checks.push(mangohud_hotkey_check());
    // Steam's containerized client scrubs the launch-option PATH — without the /usr/bin
    // launcher, `beisl %command%` (the tool's whole entry point) silently fails. Renders
    // first, mirroring the setup page's main-checklist placement: most actionable row
    // while unfixed. Once a Steam-launched trace confirms it's fixed the check itself
    // turns purely informational (see `Check::advanced`), but this flat CLI list has no
    // separate "advanced" section to move it to, so it stays first either way.
    checks.insert(0, steam_visible_beisl_check());

    // A genuine RecordMode::None (perf missing, or paranoid too high for even a degraded
    // trace) means the box can't record at all — that gates readiness. A below-Full tier
    // (degraded) does not: a userspace-only trace is still a trace.
    let mut blocking_missing = mode == RecordMode::None;

    for c in &checks {
        // The perf-record capability is shown as the tier ladder above, not a binary row.
        if c.id == CheckId::PerfParanoid {
            continue;
        }
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
                match fix_command(fix, distro, system) {
                    cmd if cmd.is_empty() => {
                        out!("       fix:  (install manually — unknown for this distro)")
                    }
                    // Multi-line elevated scripts are unreadable as a single line —
                    // summarise per check instead of dumping the whole heredoc.
                    cmd if cmd.iter().any(|a| a.contains('\n')) => match c.id {
                        CheckId::SteamVisibleBeisl => {
                            out!(
                                "       fix:  opt in via beisl-setup (installs a small /usr/bin/beisl launcher via pkexec)"
                            );
                            // Terminals linkify URLs — the bug reference stays clickable here too.
                            out!("       bug:  {}", preflight::STEAM_PATH_ISSUE_URL);
                        }
                        _ => out!(
                            "       fix:  enable in beisl-setup (installs a udev rule + tmpfiles entry via pkexec)"
                        ),
                    },
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
