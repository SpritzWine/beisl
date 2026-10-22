//! CPU sampling via `perf`.
//!
//! v0 records the **whole game session** system-wide (`perf record -a`) — the exact shape
//! we validated by hand; symbolication filters to the wine DSOs afterward. Scoping to the
//! game's cgroup and driving start/stop from the MangoHud toggle are later refinements.
//!
//! The capture is wine-agnostic — perf samples whatever runs. What differs by wine flavor
//! is symbolication (see the `symbol-resolver` crate + DESIGN.md fidelity tiers).
//!
//! Degraded mode (no `-a`) attaches perf to the game instead of letting perf start it:
//! [`spawn_held`] stops the game just before its exec until perf is recording. perf as
//! the game's parent changed its environment and ended the recording on a stop signal.

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::fd::FromRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicI32, Ordering};
use std::time::{Duration, Instant};

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
/// caught — the `PR_SET_PDEATHSIG` set in [`arm_pdeathsig`] is the backstop for that.
pub fn install_teardown() {
    unsafe {
        for sig in [libc::SIGTERM, libc::SIGINT, libc::SIGHUP] {
            libc::signal(sig, handle_term as *const () as libc::sighandler_t);
        }
    }
}

/// Make `cmd`'s child die with us: if beisl is killed — even SIGKILL by Steam — the
/// kernel sends SIGINT to it, so a `perf` in that slot flushes `perf.data` instead of
/// recording forever.
pub fn arm_pdeathsig(cmd: &mut Command) {
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
}

/// Publish `pid` as the running `perf`'s PID, so the async-signal-safe teardown handler
/// ([`install_teardown`]) can reach it. `0` clears it (no perf to signal).
pub fn set_perf_pid(pid: u32) {
    PERF_PID.store(pid as i32, Ordering::SeqCst);
}

/// The `perf record` args for a degraded (userspace-only) capture of `pid` and everything
/// it forks: its cycles at ~499 Hz, no `-a` (that needs the privilege Degraded mode
/// lacks). Started disabled (`-D -1`) and enabled over the control fifo, whose ack is
/// what tells us sampling is live.
pub fn degraded_args(perf_data: &Path, pid: u32, ctl: &Path, ack: &Path) -> Vec<String> {
    let mut args: Vec<String> = ["record", "-q", "-e", "cycles:u", "-F", "499"]
        .iter()
        .chain(&CLOCK_ARGS)
        .map(|a| a.to_string())
        .collect();
    args.push("--control".to_string());
    args.push(format!("fifo:{},{}", ctl.display(), ack.display()));
    args.extend(["-D", "-1", "-p"].map(String::from));
    args.push(pid.to_string());
    args.push("-o".to_string());
    args.push(perf_data.display().to_string());
    args
}

/// How long perf gets to attach and confirm before the game is let go untraced.
const ATTACH_TIMEOUT: Duration = Duration::from_secs(10);

/// Spawn `cmd`, held just before its exec, call `attach` with its pid, then let it exec.
/// Held there it has forked nothing, so a perf attached now follows the exec and every
/// process it starts. A byte on the release pipe lets it go, and so does EOF, so if beisl
/// dies first the game runs untraced rather than hanging. The byte is what normally
/// releases it: anything else forked meanwhile holds a copy of the pipe until its own
/// exec, which would keep EOF from ever arriving (two held spawns did exactly that).
///
/// `spawn` itself waits for the exec, so it runs on a helper thread while this one
/// attaches; an exec failure still comes back from `spawn` as usual.
pub fn spawn_held(mut cmd: Command, attach: impl FnOnce(u32)) -> io::Result<Child> {
    let [pid_r, pid_w] = cloexec_pipe()?;
    let [go_r, go_w] = match cloexec_pipe() {
        Ok(p) => p,
        Err(e) => {
            unsafe {
                libc::close(pid_r);
                libc::close(pid_w);
            }
            return Err(e);
        }
    };
    // Async-signal-safe calls only: this runs in the forked child.
    unsafe {
        cmd.pre_exec(move || {
            libc::close(pid_r);
            libc::close(go_w);
            let pid = libc::getpid().to_ne_bytes();
            libc::write(pid_w, pid.as_ptr().cast(), pid.len());
            libc::close(pid_w);
            let mut b = 0u8;
            while libc::read(go_r, (&mut b as *mut u8).cast(), 1) < 0
                && io::Error::last_os_error().kind() == io::ErrorKind::Interrupted
            {}
            libc::close(go_r);
            Ok(())
        });
    }
    let spawner = std::thread::spawn(move || {
        let r = cmd.spawn();
        // Not before: the fork has to inherit them. Closing `pid_w` also ends the read
        // below when the fork itself failed.
        unsafe {
            libc::close(pid_w);
            libc::close(go_r);
        }
        r
    });
    let mut pid_r = unsafe { File::from_raw_fd(pid_r) };
    let mut release = unsafe { File::from_raw_fd(go_w) };
    let mut pid = [0u8; 4];
    if pid_r.read_exact(&mut pid).is_ok() {
        attach(i32::from_ne_bytes(pid) as u32);
    }
    let _ = release.write_all(&[1]);
    drop(release);
    spawner
        .join()
        .unwrap_or_else(|_| Err(io::Error::other("the launch thread panicked")))
}

fn cloexec_pipe() -> io::Result<[i32; 2]> {
    let mut fds = [0; 2];
    if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(fds)
}

/// Enable a perf started with `-D -1` and wait for its ack, so the caller knows sampling
/// is live. perf opens both fifos read-write at startup; until it has, a non-blocking open
/// for writing fails, which is how its arrival shows.
fn enable_and_wait(perf: &mut Child, ctl: &Path, ack: &Path) -> Result<(), String> {
    let deadline = Instant::now() + ATTACH_TIMEOUT;
    let gone = |perf: &mut Child| -> Option<String> {
        match perf.try_wait() {
            Ok(Some(status)) => Some(format!("perf exited ({status}) before recording")),
            _ if Instant::now() > deadline => Some("perf didn't confirm recording in time".into()),
            _ => None,
        }
    };
    let mut ctl_w = loop {
        match OpenOptions::new()
            .write(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(ctl)
        {
            Ok(f) => break f,
            Err(_) => {
                if let Some(why) = gone(perf) {
                    return Err(why);
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    };
    ctl_w
        .write_all(b"enable\n")
        .map_err(|e| format!("perf control: {e}"))?;
    let mut ack_r = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(ack)
        .map_err(|e| format!("perf ack: {e}"))?;
    let mut got = Vec::new();
    loop {
        let mut buf = [0u8; 16];
        match ack_r.read(&mut buf) {
            Ok(n) => got.extend_from_slice(&buf[..n]),
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
            Err(e) => return Err(format!("perf ack: {e}")),
        }
        if got.starts_with(b"ack") {
            return Ok(());
        }
        if let Some(why) = gone(perf) {
            return Err(why);
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Sample times on CLOCK_MONOTONIC instead of perf's own clock, which nothing else
/// shares: the wrapper anchors the frametime CSV on the same clock, so a spike frame can
/// be found in perf.data (measured to about a millisecond, see artifact's spike windows).
const CLOCK_ARGS: [&str; 2] = ["-k", "CLOCK_MONOTONIC"];

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
        // Flat, system-wide, ~499 Hz: a good resolution/size trade-off for a whole session.
        let mut args: Vec<String> = ["record", "-F", "499", "-a"]
            .iter()
            .chain(&CLOCK_ARGS)
            .map(|a| a.to_string())
            .collect();
        args.push("-o".to_string());
        args.push(self.perf_data.display().to_string());
        let child = self.spawn_perf(&args)?;
        set_perf_pid(child.id());
        self.child = Some(child);
        Ok(())
    }

    /// Degraded mode: attach to `pid`, held by [`spawn_held`], and return once perf
    /// confirms it is sampling. On an error perf is gone and the game should be let go
    /// untraced.
    pub fn start_attached(&mut self, pid: u32) -> Result<(), String> {
        let dir = self.perf_data.parent().unwrap_or(Path::new("."));
        let (ctl, ack) = (dir.join("perf.ctl"), dir.join("perf.ack"));
        for fifo in [&ctl, &ack] {
            let _ = std::fs::remove_file(fifo);
            let c = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes())
                .map_err(|e| format!("perf control fifo: {e}"))?;
            if unsafe { libc::mkfifo(c.as_ptr(), 0o600) } != 0 {
                return Err(format!("perf control fifo: {}", io::Error::last_os_error()));
            }
        }
        let result = self
            .spawn_perf(&degraded_args(&self.perf_data, pid, &ctl, &ack))
            .and_then(|mut child| match enable_and_wait(&mut child, &ctl, &ack) {
                Ok(()) => Ok(child),
                Err(e) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    Err(format!("{e} (see perf.log); the game runs untraced"))
                }
            });
        let _ = std::fs::remove_file(&ctl);
        let _ = std::fs::remove_file(&ack);
        let child = result?;
        set_perf_pid(child.id());
        self.child = Some(child);
        Ok(())
    }

    fn spawn_perf(&self, args: &[String]) -> Result<Child, String> {
        let stderr = File::create(&self.log_path)
            .map(Stdio::from)
            .unwrap_or_else(|_| Stdio::null());
        let mut cmd = Command::new("perf");
        cmd.args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(stderr);
        // Make perf die with us: if beisl is killed — even SIGKILL by Steam — the kernel
        // sends SIGINT to perf, so it flushes perf.data instead of recording forever.
        arm_pdeathsig(&mut cmd);
        cmd.spawn()
            .map_err(|e| format!("perf couldn't start ({e}), run beisl-setup"))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn degraded_args_attach_userspace_only_and_start_disabled() {
        let path = Path::new("/tmp/beisl-test/perf.data");
        let (ctl, ack) = (Path::new("/r/perf.ctl"), Path::new("/r/perf.ack"));
        let args = degraded_args(path, 4242, ctl, ack);
        assert!(args.contains(&"-q".to_string()));
        assert!(args.windows(2).any(|w| w == ["-e", "cycles:u"]));
        assert!(args.windows(2).any(|w| w == ["-F", "499"]));
        assert!(args.windows(2).any(|w| w == CLOCK_ARGS));
        assert!(args.windows(2).any(|w| w == ["-p", "4242"]));
        assert!(args.windows(2).any(|w| w == ["-D", "-1"]));
        assert!(args
            .windows(2)
            .any(|w| w == ["--control", "fifo:/r/perf.ctl,/r/perf.ack"]));
        assert!(args
            .windows(2)
            .any(|w| w[0] == "-o" && w[1] == path.display().to_string()));
        // Degraded mode must NOT ask for system-wide capture — that's the privileged bit
        // it's specifically working around. Nor run a program: the game is ours.
        assert!(!args.contains(&"-a".to_string()));
        assert!(!args.contains(&"--".to_string()));
    }

    #[test]
    fn a_held_child_is_attached_before_it_execs() {
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "echo $$"]).stdout(Stdio::piped());
        let mut seen = None;
        let child = spawn_held(cmd, |pid| {
            // Still this test binary, not yet sh: nothing of the game has run.
            let exe = std::fs::read_link(format!("/proc/{pid}/exe")).unwrap();
            seen = Some((pid, exe));
        })
        .unwrap();
        let out = child.wait_with_output().unwrap();
        let (pid, exe) = seen.expect("attach ran");
        assert_eq!(exe, std::env::current_exe().unwrap());
        // The same process went on to exec the program.
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), pid.to_string());
    }

    #[test]
    fn a_held_child_that_cannot_exec_still_reports_why() {
        let mut attached = false;
        let err = spawn_held(Command::new("/nonexistent/beisl-test-game"), |_| {
            attached = true
        })
        .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
        assert!(attached);
    }
}
