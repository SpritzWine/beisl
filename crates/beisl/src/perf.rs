//! CPU sampling via `perf`.
//!
//! v0 records the **whole game session** system-wide (`perf record -a`) — the exact shape
//! we validated by hand; symbolication filters to the wine DSOs afterward. Scoping to the
//! game's cgroup and driving start/stop from the MangoHud toggle are later refinements.
//!
//! The capture is wine-agnostic — perf samples whatever runs. What differs by wine flavor
//! is symbolication (see the `symbol-resolver` crate + DESIGN.md fidelity tiers).

use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicI32, Ordering};

/// PID of the running `perf`, so the async-signal-safe teardown handler can reach it.
static PERF_PID: AtomicI32 = AtomicI32::new(0);

extern "C" fn handle_term(_sig: libc::c_int) {
    let pid = PERF_PID.load(Ordering::SeqCst);
    if pid > 0 {
        // async-signal-safe: tell perf to flush + exit, then leave ourselves.
        unsafe { libc::kill(pid, libc::SIGINT) };
    }
    unsafe { libc::_exit(130) };
}

/// Install teardown handlers so a SIGTERM/SIGINT/SIGHUP to beisl (e.g. Steam's Stop
/// button, or Ctrl-C) stops perf and lets it flush before we exit. SIGKILL can't be
/// caught — the `PR_SET_PDEATHSIG` set in [`Recorder::start`] is the backstop for that.
pub fn install_teardown() {
    unsafe {
        for sig in [libc::SIGTERM, libc::SIGINT, libc::SIGHUP] {
            libc::signal(sig, handle_term as *const () as libc::sighandler_t);
        }
    }
}

pub struct Recorder {
    perf_data: PathBuf,
    log_path: PathBuf,
    child: Option<Child>,
}

impl Recorder {
    pub fn new(run_dir: &Path) -> Self {
        Self {
            perf_data: run_dir.join("perf.data"),
            log_path: run_dir.join("perf.log"),
            child: None,
        }
    }

    /// Start recording. Returns an error (without aborting the launch) if perf can't run —
    /// e.g. not installed, or `perf_event_paranoid`/CAP_PERFMON not set up. The caller
    /// surfaces that as a session warning and lets the game run un-traced.
    pub fn start(&mut self) -> Result<(), String> {
        let stderr = std::fs::File::create(&self.log_path)
            .map(Stdio::from)
            .unwrap_or_else(|_| Stdio::null());
        // Flat, system-wide, ~499 Hz — a good resolution/size trade-off for a whole session.
        let mut cmd = Command::new("perf");
        cmd.args(["record", "-F", "499", "-a", "-o"])
            .arg(&self.perf_data)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(stderr);
        // Make perf die with us: if beisl is killed — even SIGKILL by Steam — the kernel
        // sends SIGINT to perf, so it flushes perf.data instead of recording forever.
        unsafe {
            cmd.pre_exec(|| {
                if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGINT as libc::c_ulong) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                // Close the fork/exec race: if beisl already exited, don't start recording.
                if libc::getppid() == 1 {
                    libc::_exit(0);
                }
                Ok(())
            });
        }
        let child = cmd
            .spawn()
            .map_err(|e| format!("perf couldn't start ({e}) — run beisl-setup"))?;
        PERF_PID.store(child.id() as i32, Ordering::SeqCst);
        self.child = Some(child);
        Ok(())
    }

    /// Stop recording and flush `perf.data`. Uses SIGINT so perf finalises the file
    /// (SIGKILL would discard it), then waits for it to exit.
    pub fn stop(&mut self) {
        if let Some(mut child) = self.child.take() {
            PERF_PID.store(0, Ordering::SeqCst);
            let _ = Command::new("kill")
                .args(["-INT", &child.id().to_string()])
                .status();
            let _ = child.wait();
        }
    }
}
