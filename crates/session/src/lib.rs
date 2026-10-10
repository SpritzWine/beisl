//! Live session state, shared between the `beisl` wrapper (the writer) and the
//! `beisl-setup --session` window (the reader), via a small JSON file in the trace
//! directory. The wrapper owns the game + perf lifecycle and publishes state here; the
//! window polls it to show settings, recording state, and "what's wrong" while you play.
//! One reverse channel: the window sets `stop_requested` to ask the wrapper to stop.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub const FILE: &str = "session.json";

/// The wrapper's beisl.log marker for "the launched command exited" (see
/// `beisl/src/run.rs`'s post-run logging). Shared here — rather than each side owning
/// its own copy of the string — so `beisl-mcp`'s crash judgment
/// (`wrapper_exit_crashed`/`crash_detail`) can't silently drift out of sync with the
/// wrapper's actual wording: a rewording on either side now breaks the build instead of
/// quietly reading every crash as clean.
pub const EXIT_LOG_MARKER: &str = "launched command exited:";

#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum State {
    #[default]
    Starting,
    /// Game is up; not capturing (capture is toggle-driven / recorder not yet active).
    Running,
    Recording,
    Stopping,
    /// "Stop & save": the recorder is stopped and the trace is complete, but the game
    /// is still running (Steam waits on us, so only the recorder stops). Distinct from
    /// `Done` so the window can offer analysis without hiding that the game lives on.
    Saved,
    Done,
    Error,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct Session {
    /// Human name for the game (best-effort from the launch command).
    pub game: String,
    /// Where the trace is written.
    pub trace_dir: String,
    /// The MangoHud hotkey that pauses/resumes the frametime log (logging itself is
    /// always-on — auto-started shortly after launch).
    pub hotkey: String,
    pub state: State,
    pub elapsed_secs: u64,
    /// Size of the growing perf.data (0 until the recorder writes).
    pub trace_bytes: u64,
    /// "What's wrong" — preflight/runtime issues to surface live.
    pub warnings: Vec<String>,
    /// A short human status line.
    pub message: String,
    /// Reader -> writer: the window asked to stop the trace.
    pub stop_requested: bool,
    /// Writer -> reader: a GPU crash left a dump we could NOT read, and the policy is
    /// warn-only — so the window should offer a one-click "Enable auto-capture". The
    /// wrapper keeps re-attempting the read every poll, so once the rule is installed the
    /// still-pending dump is captured without any further hand-off.
    #[serde(default)]
    pub dump_capture_offer: bool,
    /// Human label of the pinned GPU (`preflight::config::GpuPin::name`), if the wrapper
    /// launched with a pin configured — so the session window can show "GPU: <name>
    /// (pinned)". `None` when no pin is configured (the common case).
    #[serde(default)]
    pub gpu_pin: Option<String>,
    /// Writer -> reader: a clean-exit trace was left un-analyzed (the default
    /// `clean-run-analysis = keep` — the wrapper kept it in place without resolving), so
    /// the window should present the "Analyze" / "Discard" ask by default. The raw
    /// `perf.data` is still in the trace dir; analysis is `artifact::TraceArtifact::build`
    /// (what `beisl symbols` runs). Crashes are auto-analyzed and never set this.
    #[serde(default)]
    pub analysis_offer: bool,
    /// Which perf record mode this run used ("full" | "degraded" | "none"); empty on
    /// legacy runs. A downstream artifact step will read it to mark a degraded trace.
    #[serde(default)]
    pub record_mode: String,
    /// The wrapper deleted `perf.data` itself, per the `discard` clean-run preference
    /// — a HEALTHY run, not a recorder failure. Distinguishes this from a genuine
    /// zero-byte-write failure, which reads `raw_bytes == 0` identically otherwise.
    #[serde(default)]
    pub trace_discarded: bool,
    /// Steam appid for the game, from gamebus-presenced's correlated presence record
    /// (`app_ids["steam"]`) when that service is running. The id ProtonDB and
    /// gamebus-gamedb both key on, which no other source in a trace carries: beisl only
    /// ever sees process names. Empty when gamebus isn't installed, when the launch
    /// wasn't a Steam one, or on a legacy run - all normal, none of them an error.
    #[serde(default)]
    pub steam_appid: String,
    /// The wrapper's own process identity, captured at launch - lets a reader probe
    /// whether it's still alive (`WrapperProc::verdict`) instead of only trusting the
    /// last state it managed to write. `#[serde(default)]`/`None` on legacy runs and
    /// whenever `WrapperProc::current()` couldn't read `/proc` for some part of it -
    /// those degrade to the pre-boot-only staleness rule, same as before this field
    /// existed.
    #[serde(default)]
    pub wrapper: Option<WrapperProc>,
}

/// The wrapper process's identity, recorded once at launch so a later reader -
/// possibly outside this process, possibly outside its PID namespace entirely - can
/// tell whether it's still running. See `verdict` for the actual liveness check.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct WrapperProc {
    pub pid: u32,
    /// `/proc/<pid>/stat` field 22 (start time in clock ticks since boot). Paired with
    /// `pid` so a recycled pid can't be mistaken for the same process: an unrelated
    /// process reusing this pid will almost certainly have a different starttime.
    pub starttime: u64,
    /// `/proc/self/ns/pid`'s target (e.g. "pid:[4026531836]") at launch - see
    /// `verdict`'s doc comment for why this matters.
    pub pid_ns: String,
}

impl WrapperProc {
    /// The calling process's own identity. Meant to be called once, by the wrapper
    /// itself, right after it starts. `None` if any part is unreadable - never guess
    /// at a liveness fact we can't back up; a `None` here just means the run degrades
    /// to the pre-boot-only staleness rule later.
    ///
    /// Both `pid` and `starttime` come from the SAME read of `/proc/self/stat`,
    /// rather than `std::process::id()` + a second `/proc/<pid>/stat` open - the pid
    /// field there is `getpid()`'s own answer, but pairing it with a `/proc` lookup
    /// keyed by a value from a different syscall leaves a seam if `/proc` were ever
    /// mounted from a different namespace than the caller's `getpid()` (the same
    /// exotic case the `pid_ns` guard in `verdict` exists for). One read closes it.
    pub fn current() -> Option<Self> {
        let stat = std::fs::read_to_string("/proc/self/stat").ok()?;
        let pid = parse_pid(&stat)?;
        let starttime = parse_starttime(&stat)?;
        let pid_ns = std::fs::read_link("/proc/self/ns/pid")
            .ok()?
            .to_str()?
            .to_string();
        Some(Self {
            pid,
            starttime,
            pid_ns,
        })
    }

    /// Whether the process recorded here is still the one running: `Some(true)`
    /// alive, `Some(false)` gone (exited, or its pid recycled by something else),
    /// `None` inconclusive. Every rule favors "inconclusive" over a wrong "gone" - a
    /// bad verdict here would tell a reader to delete or hide a still-recording run.
    ///
    /// The PID-namespace check is the important one: beisl traces are routinely
    /// launched inside Steam's pressure-vessel container, which has its own PID
    /// namespace. A desktop-side reader (the Traces page) probing a container-local
    /// pid from outside would hit an unrelated process - or none - and could declare
    /// a genuinely live recording dead. A mismatched namespace means this pid isn't
    /// ours to interpret at all, so it's `None`, not a guess either way.
    pub fn verdict(&self) -> Option<bool> {
        let our_ns = std::fs::read_link("/proc/self/ns/pid").ok()?;
        if our_ns.to_str() != Some(self.pid_ns.as_str()) {
            return None;
        }
        let stat = match std::fs::read_to_string(proc_stat_path(self.pid)) {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Some(false),
            Err(_) => return None,
        };
        parse_starttime(&stat).map(|st| st == self.starttime)
    }
}

fn proc_stat_path(pid: u32) -> String {
    format!("/proc/{pid}/stat")
}

/// Pull field 1 (pid) out of a `/proc/<pid>/stat` line - pure and /proc-free like
/// `parse_starttime`, and safe for the same reason `comm` can't confuse it: pid is
/// purely numeric, so the first `(` in the line is unambiguously where `comm` starts,
/// no matter what `comm` itself contains.
fn parse_pid(stat: &str) -> Option<u32> {
    stat.split('(').next()?.trim().parse().ok()
}

/// Pull field 22 (starttime) out of a `/proc/<pid>/stat` line. Pure and /proc-free so
/// it's unit-testable directly. `comm` (field 2) is the process name in parentheses,
/// unquoted, and may itself contain spaces and even parentheses - so the only safe
/// split point is the LAST `)` in the line, not the first whitespace. Everything after
/// it is fields 3.. as whitespace-separated tokens, so field 22 sits at index 19
/// (field 3 is index 0). `None` on anything short of that shape.
fn parse_starttime(stat: &str) -> Option<u64> {
    let after_comm = stat.rsplit_once(')')?.1;
    after_comm.split_whitespace().nth(19)?.parse().ok()
}

impl Session {
    pub fn file_in(dir: &Path) -> PathBuf {
        dir.join(FILE)
    }

    /// Atomically replace the session file (write-temp-then-rename), so a reader never
    /// sees a half-written file.
    pub fn write(&self, dir: &Path) -> std::io::Result<()> {
        let json = serde_json::to_vec_pretty(self)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        let tmp = dir.join(".session.json.tmp");
        std::fs::write(&tmp, json)?;
        std::fs::rename(&tmp, Self::file_in(dir))
    }

    /// Read the session file, or `None` if it's missing/unparseable.
    pub fn read(dir: &Path) -> Option<Session> {
        let data = std::fs::read(Self::file_in(dir)).ok()?;
        serde_json::from_slice(&data).ok()
    }
}

/// The trace library: enumerating past runs on disk.
///
/// The wrapper writes each trace to `<data>/beisl/traces/run-<unix_ts>/`; this module is
/// the UI-agnostic "list past runs" answer the trace library (DESIGN.md, "Surfaces") builds on.
/// It lives here because `session.json` is already this crate's bridge between the
/// wrapper and the GUI — the library is the same bridge, across time instead of live.
pub mod library {
    use std::path::{Path, PathBuf};

    use serde::{Deserialize, Serialize};

    use super::Session;

    /// Written by the wrapper's post-run analysis next to `session.json`.
    pub const ARTIFACT_FILE: &str = "artifact.json";
    pub const REPORT_FILE: &str = "report.txt";
    /// Written by `mark_invalid` next to `session.json` when analysis was attempted
    /// and failed.
    pub const INVALID_FILE: &str = "invalid.json";

    /// One past run under the traces root.
    #[derive(Clone, Debug)]
    pub struct RunEntry {
        /// The run directory itself.
        pub dir: PathBuf,
        /// Directory name (`run-<unix_ts>`), the run's stable id.
        pub id: String,
        /// Launch time: the unix timestamp from the dir name (0 if unparseable, which
        /// sorts such strays to the end rather than dropping them).
        pub started: u64,
        /// Best-effort session state; `None` when `session.json` is missing or corrupt
        /// (the run is still listed — the trace data may be intact).
        pub session: Option<Session>,
        /// `artifact.json` exists — the run is analyzed and openable in the results view.
        pub has_artifact: bool,
        pub has_report: bool,
        /// `perf.data`'s size in bytes; 0 when missing or empty (the recorder died
        /// before writing anything). See `raw_bytes()`.
        pub raw_bytes: u64,
        /// A prior failed analysis attempt's error, still valid against the current
        /// `perf.data` — see `invalid_of()`. `None` if never attempted, or the data
        /// changed since.
        pub invalid: Option<String>,
    }

    impl RunEntry {
        /// Human name for a list row: the game name when the session file survived,
        /// otherwise the run id.
        pub fn title(&self) -> &str {
            match &self.session {
                Some(s) if !s.game.is_empty() => &s.game,
                _ => &self.id,
            }
        }

        /// Whether the session state is one where the recorder or the game itself
        /// *could* still own the directory — the shared predicate behind both
        /// `is_live_at` and `is_stale_at` below. `session: None` means an old dir with
        /// a missing/corrupt `session.json` — the wrapper writes that file at launch,
        /// so an unreadable one is a damaged relic of a run that's long over. The
        /// state-match itself lives in the free function `is_live_state` (see there),
        /// shared with `stale_session_at` for callers that only have a `Session`, not
        /// a scanned `RunEntry`.
        fn live_state(&self) -> bool {
            self.session
                .as_ref()
                .is_some_and(|s| is_live_state(s.state))
        }

        /// Whether the recorder or the game itself might still own this run's directory.
        /// `delete_run` itself stays policy-free — it only ever checks containment (a
        /// run-* dir directly under the root) — because the session window's own
        /// single-run Discard legitimately removes a `Saved` run's dir while the game is
        /// still up (that IS its "stop capturing this session" flow). It's the CALLER —
        /// any bulk or list surface that presents "you can delete this" as a standing
        /// option, like the traces library page — that must gate its own selection/delete
        /// UI on this.
        ///
        /// A run whose session is stuck in a live state but which started before the
        /// current boot is excluded (see `is_stale`) — nothing can still own a
        /// directory from a boot that's gone, and treating it as live would caption it
        /// "recording" forever.
        ///
        /// "Might still own" is deliberately loose even once the wrapper is confirmed
        /// gone (`is_stale`'s probe half): `perf` is started with `PDEATHSIG` so it
        /// exits with the wrapper, but MangoHud is the GAME's overlay, not the
        /// wrapper's child - it keeps appending frametimes to its CSV in this same
        /// directory for as long as the game runs. Consequences are mild (a dead
        /// wrapper means the trace is unrecoverable anyway, and MangoHud is just
        /// writing on to an fd whose directory entry a bulk-delete already removed),
        /// but it means "not live" here is a statement about the WRAPPER, not a
        /// guarantee that nothing is still writing into the directory.
        pub fn is_live(&self) -> bool {
            self.is_live_at(boot_time())
        }

        /// A live-state session that cannot actually be live: it started before the
        /// current boot, so whatever wrapper process was recording it died with that
        /// boot, without ever writing `Done`/`Error`/`Saved`->done. Real data, 2026-07-19:
        /// 5 runs on this machine sat in `Recording`/`Saved` from a previous boot —
        /// `is_live`'s new gating would otherwise protect them from deletion and
        /// caption them "recording" forever.
        ///
        /// The converse - a wrapper killed within the *current* boot - is caught too,
        /// via `Session::wrapper`: a recorded pid/starttime that no longer matches
        /// anything in `/proc` (or is simply gone) reads as stale immediately, rather
        /// than waiting for the next reboot. See `stale_session_at` for how the two
        /// rules compose.
        pub fn is_stale(&self) -> bool {
            self.is_stale_at(boot_time())
        }

        /// Core of `is_live`, boot time injectable — same pattern as
        /// `artifact::frames_from_csv_gated` — so tests don't depend on the real
        /// kernel clock.
        pub(crate) fn is_live_at(&self, boot: Option<u64>) -> bool {
            self.live_state() && !self.is_stale_at(boot)
        }

        /// Core of `is_stale`, boot time injectable. Delegates the state-match +
        /// pre-boot check to `stale_session_at` (shared with the standalone
        /// `stale_session`/`stale_session_at` below) — `session: None` short-circuits
        /// via `live_state`-style `is_some_and` so a damaged/missing session file is
        /// never "stale" (it's just not live).
        pub(crate) fn is_stale_at(&self, boot: Option<u64>) -> bool {
            self.session
                .as_ref()
                .is_some_and(|s| stale_session_at(s, self.started, boot))
        }
    }

    /// Whether a session state is one where the recorder or the game itself *could*
    /// still own the run directory: `Starting..=Stopping`, and `Saved` (its own doc
    /// comment: the game is STILL RUNNING even though the recorder finished). Only
    /// `Done`/`Error` are settled. The shared state-match behind `RunEntry::live_state`
    /// and `stale_session_at` — kept as one function so the two can never drift apart.
    fn is_live_state(state: super::State) -> bool {
        matches!(
            state,
            super::State::Starting
                | super::State::Running
                | super::State::Recording
                | super::State::Stopping
                | super::State::Saved
        )
    }

    /// Core of `stale_session`, boot/verdict both injectable - same pattern as
    /// `RunEntry::is_stale_at`, and in fact the function it now delegates to. Reads
    /// the wrapper's own liveness verdict off `session.wrapper` (see
    /// `super::WrapperProc::verdict`) and ORs it with the pre-boot rule via
    /// `stale_core`, which is where the actual conservative logic lives - kept
    /// separate so tests can inject the verdict without touching real `/proc`.
    ///
    /// The probe only runs for a live-state session: `stale_core` would ignore its
    /// verdict for a settled `Done`/`Error` row anyway (`is_live_state` gates the
    /// whole rule there), so skipping it here saves a `readlink` + a `/proc/<pid>/stat`
    /// read per settled row - and the Traces page calls this multiple times per row,
    /// per render.
    pub(crate) fn stale_session_at(
        session: &super::Session,
        started: u64,
        boot: Option<u64>,
    ) -> bool {
        let verdict = is_live_state(session.state)
            .then(|| {
                session
                    .wrapper
                    .as_ref()
                    .and_then(super::WrapperProc::verdict)
            })
            .flatten();
        stale_core(session.state, started, boot, verdict)
    }

    /// The actual staleness rule, with both the boot time and the wrapper-liveness
    /// verdict injected - the innermost core, so unit tests exercise it directly
    /// without ever touching `/proc` or the real kernel clock. A live state
    /// (`is_live_state`) is stale when EITHER of two independent, both-conservative
    /// rules fires:
    /// * pre-boot: `started` predates `boot` - whatever wrapper was recording it died
    ///   with that boot. Stands alone, unconditionally: even a `Some(true)` verdict
    ///   never rescues a pre-boot run (a same pid + starttime recurring across a
    ///   reboot would otherwise be a false "still alive").
    /// * probe-dead: `verdict == Some(false)` - the recorded wrapper pid/starttime no
    ///   longer matches anything in `/proc`, so it died within the *current* boot.
    ///
    /// Boot unknown, an unparseable/zero `started`, and `verdict == None`
    /// (inconclusive, e.g. a legacy run with no `Session::wrapper`, or a probe run
    /// from a different PID namespace) all favor "not stale" - a false "live" only
    /// keeps a row around a little longer, where a false "stale" could let someone
    /// delete or hide an in-progress recording. Settled states (`Done`/`Error`) are
    /// excluded by `is_live_state` itself: settled is settled, not "stale".
    pub(crate) fn stale_core(
        state: super::State,
        started: u64,
        boot: Option<u64>,
        verdict: Option<bool>,
    ) -> bool {
        let pre_boot = started > 0 && boot.is_some_and(|b| started < b);
        is_live_state(state) && (pre_boot || verdict == Some(false))
    }

    /// Whether a live-state `session` (with the run's launch time `started` - see
    /// `run_started`) is actually stuck: either it started before the current boot
    /// (whatever process was recording it died with that boot without ever writing
    /// `Done`/`Error`/`Saved`->done), or its recorded wrapper process
    /// (`Session::wrapper`) is provably gone within the current boot. The
    /// `Session`+`started`-only entry point for callers that don't have a scanned
    /// `RunEntry` - the session window watches its *own* live `session.json` directly,
    /// not a `library::scan` result. Same core (`stale_core` + the real boot time and
    /// wrapper verdict) as `RunEntry::is_stale`, so the two can never disagree about
    /// the same run.
    ///
    /// A genuinely live window is never misclassified: the wrapper only spawns the
    /// session window for a run it just started *this* boot, so the pre-boot rule
    /// never trips for it, and it's still alive to keep matching its own
    /// pid/starttime for the probe rule.
    pub fn stale_session(session: &super::Session, started: u64) -> bool {
        stale_session_at(session, started, boot_time())
    }

    /// The kernel's boot time (unix epoch seconds), from `/proc/stat`'s `btime` line.
    /// Linux-only, which is fine — beisl only runs on Linux. `None` on any failure
    /// (missing/unreadable file, no `btime` line, unparseable value): the caller's
    /// fallback is "boot time unknown", never a guess.
    fn boot_time() -> Option<u64> {
        std::fs::read_to_string("/proc/stat")
            .ok()?
            .lines()
            .find_map(|l| l.strip_prefix("btime "))
            .and_then(|v| v.trim().parse().ok())
    }

    /// Parse a run dir's `run-<unix_ts>` name into its launch timestamp — `0` when
    /// unparseable (no `run-` prefix, non-numeric suffix, or no file name at all),
    /// mirroring `scan`'s own fallback for a stray dir name. `scan` uses this so the
    /// parse exists in exactly one place; the session window uses it too, to test its
    /// own run dir against `stale_session` without re-deriving the timestamp.
    pub fn run_started(dir: &Path) -> u64 {
        dir.file_name()
            .and_then(|n| n.to_str())
            .and_then(|n| n.strip_prefix("run-"))
            .and_then(|ts| ts.parse().ok())
            .unwrap_or(0)
    }

    /// `perf.data`'s size in bytes — 0 when missing or empty, so a recorder that died
    /// before writing and one that never started read the same: nothing to analyze.
    pub fn raw_bytes(dir: &Path) -> u64 {
        std::fs::metadata(dir.join("perf.data"))
            .map(|m| m.len())
            .unwrap_or(0)
    }

    /// perf.data's (len, mtime-as-unix-secs) — the fingerprint `mark_invalid`/
    /// `invalid_of` compare, so a rewritten file is detected even at the same size.
    fn perf_data_stamp(dir: &Path) -> (u64, u64) {
        let Ok(meta) = std::fs::metadata(dir.join("perf.data")) else {
            return (0, 0);
        };
        let mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0);
        (meta.len(), mtime)
    }

    #[derive(Serialize, Deserialize)]
    struct InvalidMarker {
        len: u64,
        mtime: u64,
        error: String,
    }

    /// Record that analysis was attempted on `dir`'s `perf.data` and failed —
    /// fingerprinted against its current (len, mtime) so a later rewrite auto-clears
    /// the verdict (see `invalid_of`) instead of lying forever. Best-effort, like
    /// `Session::write`; callers ignore the error (`let _ =`).
    pub fn mark_invalid(dir: &Path, error: &str) -> std::io::Result<()> {
        let (len, mtime) = perf_data_stamp(dir);
        let marker = InvalidMarker {
            len,
            mtime,
            error: error.to_string(),
        };
        let json = serde_json::to_vec_pretty(&marker)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        std::fs::write(dir.join(INVALID_FILE), json)
    }

    /// The recorded error, but ONLY if `perf.data`'s (len, mtime) still match the
    /// marker — a changed file (or an unreadable/missing marker) means the verdict no
    /// longer applies, so this returns `None`: derived honesty, never a stale "failed".
    pub fn invalid_of(dir: &Path) -> Option<String> {
        let data = std::fs::read(dir.join(INVALID_FILE)).ok()?;
        let marker: InvalidMarker = serde_json::from_slice(&data).ok()?;
        let stamp = perf_data_stamp(dir);
        (marker.len, marker.mtime)
            .eq(&stamp)
            .then_some(marker.error)
    }

    /// Drop a stale marker once analysis succeeds — without this, a retry that fixes
    /// the real cause (e.g. `perf` got installed) on UNCHANGED `perf.data` would still
    /// report the old failure alongside the new artifact. Best-effort: a missing
    /// marker is not an error.
    pub fn clear_invalid(dir: &Path) {
        let _ = std::fs::remove_file(dir.join(INVALID_FILE));
    }

    /// `$XDG_DATA_HOME/beisl` (falls back to `~/.local/share/beisl`) — the same
    /// resolution the wrapper uses when it creates run dirs.
    pub fn data_root() -> PathBuf {
        if let Ok(x) = std::env::var("XDG_DATA_HOME") {
            if !x.is_empty() {
                return PathBuf::from(x).join("beisl");
            }
        }
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
        PathBuf::from(home).join(".local/share/beisl")
    }

    /// Where the wrapper writes traces: `<data_root>/traces`.
    pub fn default_root() -> PathBuf {
        data_root().join("traces")
    }

    /// List past runs under `root`, newest first. A missing root is an empty library,
    /// not an error; foreign or unreadable entries are skipped and a corrupt
    /// `session.json` degrades to `session: None` — a damaged run must never take the
    /// listing down with it.
    pub fn scan(root: &Path) -> Vec<RunEntry> {
        let Ok(entries) = std::fs::read_dir(root) else {
            return Vec::new();
        };
        let mut runs: Vec<RunEntry> = entries
            .flatten()
            .filter_map(|e| {
                let id = e.file_name().to_str()?.to_string();
                if !id.starts_with("run-") {
                    return None;
                }
                if !e.file_type().ok()?.is_dir() {
                    return None;
                }
                let dir = e.path();
                Some(RunEntry {
                    started: run_started(&dir),
                    session: Session::read(&dir),
                    has_artifact: dir.join(ARTIFACT_FILE).is_file(),
                    has_report: dir.join(REPORT_FILE).is_file(),
                    raw_bytes: raw_bytes(&dir),
                    invalid: invalid_of(&dir),
                    dir,
                    id,
                })
            })
            .collect();
        runs.sort_by(|a, b| b.started.cmp(&a.started).then(b.id.cmp(&a.id)));
        runs
    }

    /// Delete one run directory — and refuse anything that isn't a `run-*` directory
    /// sitting directly under `root`. The check canonicalizes both sides first, so a
    /// symlink smuggled into the traces dir can't turn "delete this trace" into
    /// "delete whatever it points at". The GUI's confirm step is UX; this is the
    /// actual safety boundary, so it lives with the scan, not in a view.
    pub fn delete_run(root: &Path, run_dir: &Path) -> std::io::Result<()> {
        use std::io::{Error, ErrorKind};

        let denied = |why: &str| Error::new(ErrorKind::PermissionDenied, why.to_string());
        // symlink_metadata: a symlink *named* run-* must be refused before canonicalize
        // would happily resolve it out of the traces root.
        let meta = std::fs::symlink_metadata(run_dir)?;
        if !meta.is_dir() {
            return Err(denied("not a run directory"));
        }
        let root = std::fs::canonicalize(root)?;
        let run = std::fs::canonicalize(run_dir)?;
        let ok = run.parent() == Some(root.as_path())
            && run
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("run-"));
        if !ok {
            return Err(denied("refusing to delete outside the traces root"));
        }
        std::fs::remove_dir_all(&run)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_the_file() {
        let dir = std::env::temp_dir().join(format!("beisl-sess-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let s = Session {
            game: "Blood West.exe".into(),
            state: State::Recording,
            warnings: vec!["perf can record needs setup".into()],
            record_mode: "degraded".into(),
            ..Default::default()
        };
        s.write(&dir).unwrap();
        let back = Session::read(&dir).unwrap();
        assert_eq!(back.game, "Blood West.exe");
        assert_eq!(back.state, State::Recording);
        assert_eq!(back.warnings.len(), 1);
        assert_eq!(back.record_mode, "degraded");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn wrapper_field_absent_from_disk_deserializes_to_none() {
        // The legacy-file guarantee: a session.json written before this field existed
        // (or by any other producer that omits it) must still parse, with `wrapper`
        // defaulting to `None` rather than failing the whole read.
        let dir = std::env::temp_dir().join(format!("beisl-sess-legacy-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(Session::file_in(&dir), br#"{"game":"Old.exe","trace_dir":"","hotkey":"","state":"Recording","elapsed_secs":0,"trace_bytes":0,"warnings":[],"message":"","stop_requested":false}"#).unwrap();
        let back = Session::read(&dir).unwrap();
        assert_eq!(back.game, "Old.exe");
        assert!(back.wrapper.is_none());
        std::fs::remove_dir_all(&dir).ok();
    }

    // -- WrapperProc ----------------------------------------------------------------------

    /// A syntactically valid `/proc/<pid>/stat` line with `starttime` planted at field
    /// 22. After the `comm)`, real `/proc/<pid>/stat` has: state, then 18 more fields
    /// (ppid..itrealvalue) before starttime - 19 tokens total before it, so it lands
    /// at whitespace index 19 (state is index 0), matching `parse_starttime`'s doc.
    fn fake_stat(pid: u32, comm: &str, starttime: u64) -> String {
        // ppid pgrp session tty_nr tpgid flags minflt cminflt majflt cmajflt utime
        // stime cutime cstime priority nice num_threads itrealvalue (18 fields).
        let filler = "0 ".repeat(18);
        format!("{pid} ({comm}) S {filler}{starttime} 0 0")
    }

    #[test]
    fn parse_pid_reads_field_1_from_a_normal_stat_line() {
        let stat = fake_stat(1234, "bash", 567890);
        assert_eq!(parse_pid(&stat), Some(1234));
    }

    #[test]
    fn parse_pid_handles_a_comm_with_spaces_and_parens() {
        // comm can't confuse this one either: pid is purely numeric, so the FIRST `(`
        // in the line is always where comm starts, regardless of what's inside it.
        let stat = fake_stat(999, "my (game) launcher", 424242);
        assert_eq!(parse_pid(&stat), Some(999));
    }

    #[test]
    fn parse_pid_is_none_for_truncated_or_garbage_input() {
        assert_eq!(parse_pid(""), None);
        assert_eq!(parse_pid("no parens here at all"), None);
        assert_eq!(parse_pid("not-a-number (sh) S"), None);
    }

    #[test]
    fn parse_starttime_reads_field_22_from_a_normal_stat_line() {
        let stat = fake_stat(1234, "bash", 567890);
        assert_eq!(parse_starttime(&stat), Some(567890));
    }

    #[test]
    fn parse_starttime_handles_a_comm_with_spaces_and_parens() {
        // comm (field 2) is unquoted and copied verbatim from the kernel - a process
        // named e.g. "my (game) launcher" must not confuse the parser into splitting
        // on the WRONG `)`. The parser must split on the LAST one in the line.
        let stat = fake_stat(999, "my (game) launcher", 424242);
        assert_eq!(parse_starttime(&stat), Some(424242));
    }

    #[test]
    fn parse_starttime_is_none_for_truncated_or_garbage_input() {
        assert_eq!(parse_starttime(""), None);
        assert_eq!(parse_starttime("no parens here at all"), None);
        // Has a `)` but not enough fields after it to reach index 19.
        assert_eq!(parse_starttime("123 (sh) S 1 2 3"), None);
        // Enough fields, but the starttime slot itself isn't numeric.
        let filler = "0 ".repeat(18);
        let bad = format!("1 (sh) S {filler}not-a-number 0 0");
        assert_eq!(parse_starttime(&bad), None);
    }

    #[test]
    fn wrapper_proc_current_reports_the_test_process_itself_alive() {
        // A free real-/proc sanity check: our own identity, probed right back, must
        // read as alive.
        let me = WrapperProc::current().expect("/proc/self/stat must be readable in CI");
        assert_eq!(me.pid, std::process::id());
        assert_eq!(me.verdict(), Some(true));
    }

    #[test]
    fn wrapper_proc_verdict_is_dead_when_starttime_does_not_match() {
        // Same pid + namespace as us, but a starttime that can never be ours - the
        // pid-recycling case.
        let mut me = WrapperProc::current().expect("/proc/self/stat must be readable in CI");
        me.starttime = me.starttime.wrapping_add(12345);
        assert_eq!(me.verdict(), Some(false));
    }

    #[test]
    fn wrapper_proc_verdict_is_inconclusive_for_a_mismatched_pid_namespace() {
        // A recorded namespace that isn't ours (the pressure-vessel-container case):
        // the pid isn't ours to interpret, so the verdict must be None, never a guess.
        let mut me = WrapperProc::current().expect("/proc/self/stat must be readable in CI");
        me.pid_ns = "pid:[1]".to_string();
        assert_eq!(me.verdict(), None);
    }

    /// A `WrapperProc` for our OWN process with its starttime perturbed - a
    /// deterministic "provably dead" probe (`verdict() == Some(false)`) without ever
    /// touching another process, the same trick as
    /// `wrapper_proc_verdict_is_dead_when_starttime_does_not_match`. Used below to
    /// prove `stale_session_at`/`is_stale_at` actually consult `WrapperProc::verdict`
    /// - every other staleness test in this file injects a verdict straight into
    /// `stale_core`, which would still pass even if the real join were deleted.
    fn dead_wrapper() -> WrapperProc {
        let mut w = WrapperProc::current().expect("/proc/self/stat must be readable in CI");
        w.starttime = w.starttime.wrapping_add(12345);
        w
    }

    /// A `WrapperProc` for our own process, unperturbed - a deterministic "provably
    /// alive" probe (`verdict() == Some(true)`): we ARE this pid, right now.
    fn alive_wrapper() -> WrapperProc {
        WrapperProc::current().expect("/proc/self/stat must be readable in CI")
    }

    // -- library::scan ------------------------------------------------------------------

    /// A fixture traces root with one run dir per shape the scanner must survive.
    fn fixture_root(tag: &str) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!("beisl-lib-{tag}-{}", std::process::id()));
        std::fs::remove_dir_all(&root).ok();
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    fn put_run(root: &std::path::Path, id: &str, game: Option<&str>, analyzed: bool) {
        let dir = root.join(id);
        std::fs::create_dir_all(&dir).unwrap();
        if let Some(g) = game {
            Session {
                game: g.into(),
                ..Default::default()
            }
            .write(&dir)
            .unwrap();
        }
        if analyzed {
            std::fs::write(dir.join(library::ARTIFACT_FILE), b"{}").unwrap();
            std::fs::write(dir.join(library::REPORT_FILE), b"report").unwrap();
        }
    }

    #[test]
    fn scan_lists_newest_first_with_analysis_state() {
        let root = fixture_root("order");
        put_run(&root, "run-100", Some("Blood West.exe"), true);
        put_run(&root, "run-300", Some("Outlaws.exe"), false);
        put_run(&root, "run-200", None, false); // session.json never appeared

        let runs = library::scan(&root);
        assert_eq!(
            runs.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
            ["run-300", "run-200", "run-100"]
        );
        assert!(runs[2].has_artifact && runs[2].has_report);
        assert!(!runs[0].has_artifact);
        assert_eq!(runs[0].title(), "Outlaws.exe");
        // No session file: the row still lists, titled by its id.
        assert!(runs[1].session.is_none());
        assert_eq!(runs[1].title(), "run-200");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn scan_survives_junk_without_panicking() {
        let root = fixture_root("junk");
        // A corrupt session.json degrades to session: None, the run stays listed.
        put_run(&root, "run-500", None, false);
        std::fs::write(root.join("run-500").join(FILE), b"{ not json").unwrap();
        // Foreign entries are not runs: a stray file, a dir without the run- prefix,
        // and a run-named *file* (not a dir).
        std::fs::write(root.join("notes.txt"), b"x").unwrap();
        std::fs::create_dir_all(root.join("exports")).unwrap();
        std::fs::write(root.join("run-666"), b"x").unwrap();
        // A run dir with an unparseable timestamp sorts last instead of vanishing.
        put_run(&root, "run-broken-ts", Some("Old.exe"), false);

        let runs = library::scan(&root);
        assert_eq!(
            runs.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
            ["run-500", "run-broken-ts"]
        );
        assert!(runs[0].session.is_none());
        assert_eq!(runs[1].started, 0);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn scan_of_missing_root_is_an_empty_library() {
        let root = std::env::temp_dir().join(format!("beisl-lib-gone-{}", std::process::id()));
        std::fs::remove_dir_all(&root).ok();
        assert!(library::scan(&root).is_empty());
    }

    // -- library::raw_bytes / mark_invalid / invalid_of ----------------------------------

    #[test]
    fn raw_bytes_is_zero_for_missing_or_empty_and_n_for_real_data() {
        let dir = fixture_root("raw-bytes");
        assert_eq!(library::raw_bytes(&dir), 0, "no perf.data at all");
        std::fs::write(dir.join("perf.data"), b"").unwrap();
        assert_eq!(library::raw_bytes(&dir), 0, "empty perf.data");
        std::fs::write(dir.join("perf.data"), b"perf data bytes").unwrap();
        assert_eq!(library::raw_bytes(&dir), 15);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn mark_and_read_invalid_round_trips() {
        let dir = fixture_root("invalid-roundtrip");
        std::fs::write(dir.join("perf.data"), b"").unwrap();
        library::mark_invalid(&dir, "perf script produced no samples").unwrap();
        assert_eq!(
            library::invalid_of(&dir),
            Some("perf script produced no samples".to_string())
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn invalid_marker_is_ignored_once_perf_data_changes() {
        let dir = fixture_root("invalid-stale");
        std::fs::write(dir.join("perf.data"), b"").unwrap();
        library::mark_invalid(&dir, "perf script produced no samples").unwrap();
        // The recorder wrote real data on a later run — the old verdict no longer
        // describes this file, so it must auto-clear.
        std::fs::write(dir.join("perf.data"), b"real data now").unwrap();
        assert_eq!(library::invalid_of(&dir), None);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn clear_invalid_drops_the_marker_even_on_unchanged_perf_data() {
        // The realistic retry: perf.data never changes (only the missing `perf`
        // binary did), so invalid_of's fingerprint match alone can't clear it.
        let dir = fixture_root("invalid-clear");
        std::fs::write(dir.join("perf.data"), b"").unwrap();
        library::mark_invalid(&dir, "perf script produced no samples").unwrap();
        library::clear_invalid(&dir);
        assert_eq!(library::invalid_of(&dir), None);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn invalid_of_is_none_without_a_marker() {
        let dir = fixture_root("invalid-none");
        assert_eq!(library::invalid_of(&dir), None);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Far beyond any real machine's boot time (year ~2100) — a `started` this large
    /// is always post-boot, so a live-state fixture built on it is genuinely live
    /// (not stale) regardless of which machine runs the test.
    const FAR_FUTURE: u64 = 4_102_444_800;

    #[test]
    fn is_live_covers_recorder_and_game_owned_states() {
        let live = [
            State::Starting,
            State::Running,
            State::Recording,
            State::Stopping,
            State::Saved, // recorder is done, but the game is still running
        ];
        for state in live {
            let entry = library::RunEntry {
                dir: PathBuf::new(),
                id: "run-1".into(),
                started: FAR_FUTURE,
                session: Some(Session {
                    state,
                    ..Default::default()
                }),
                has_artifact: false,
                has_report: false,
                raw_bytes: 0,
                invalid: None,
            };
            assert!(entry.is_live(), "{state:?} should be live");
        }

        for state in [State::Done, State::Error] {
            let entry = library::RunEntry {
                dir: PathBuf::new(),
                id: "run-1".into(),
                started: 1,
                session: Some(Session {
                    state,
                    ..Default::default()
                }),
                has_artifact: false,
                has_report: false,
                raw_bytes: 0,
                invalid: None,
            };
            assert!(!entry.is_live(), "{state:?} should not be live");
        }

        // A damaged/missing session.json is a relic, not a live run.
        let entry = library::RunEntry {
            dir: PathBuf::new(),
            id: "run-1".into(),
            started: 1,
            session: None,
            has_artifact: false,
            has_report: false,
            raw_bytes: 0,
            invalid: None,
        };
        assert!(!entry.is_live());
    }

    /// A live-state entry with the given `started`, for the `is_stale_at`/`is_live_at`
    /// core tests below (boot injected, so these never touch the real kernel clock).
    fn live_entry(state: State, started: u64) -> library::RunEntry {
        library::RunEntry {
            dir: PathBuf::new(),
            id: "run-1".into(),
            started,
            session: Some(Session {
                state,
                ..Default::default()
            }),
            has_artifact: false,
            has_report: false,
            raw_bytes: 0,
            invalid: None,
        }
    }

    #[test]
    fn stale_at_flags_a_live_state_started_before_the_injected_boot() {
        let entry = live_entry(State::Recording, 100);
        assert!(
            entry.is_stale_at(Some(200)),
            "pre-boot start should be stale"
        );
        assert!(!entry.is_live_at(Some(200)), "stale excludes live");
    }

    #[test]
    fn stale_at_is_false_for_a_live_state_started_after_the_injected_boot() {
        let entry = live_entry(State::Recording, 300);
        assert!(
            !entry.is_stale_at(Some(200)),
            "post-boot start is not stale"
        );
        assert!(entry.is_live_at(Some(200)), "post-boot start is live");
    }

    #[test]
    fn stale_at_is_false_when_boot_time_is_unknown() {
        // Conservative edge: an unknown boot time must never manufacture a false
        // "stale" that would let someone delete a still-recording run. Even a
        // started value that would be stale under any known boot stays live here.
        let entry = live_entry(State::Recording, 100);
        assert!(!entry.is_stale_at(None));
        assert!(entry.is_live_at(None));
    }

    #[test]
    fn stale_at_is_false_for_an_unparseable_dir_name() {
        // Conservative edge: started == 0 (an unparseable `run-*` timestamp) means an
        // unknown age, not an old one.
        let entry = live_entry(State::Recording, 0);
        assert!(!entry.is_stale_at(Some(200)));
        assert!(entry.is_live_at(Some(200)));
    }

    #[test]
    fn stale_at_never_flags_settled_states() {
        // Settled is settled, not "stale" — even with a started time that would be
        // stale for any live state.
        for state in [State::Done, State::Error] {
            let entry = live_entry(state, 100);
            assert!(
                !entry.is_stale_at(Some(200)),
                "{state:?} must never be stale"
            );
            assert!(
                !entry.is_live_at(Some(200)),
                "{state:?} is settled, not live"
            );
        }
    }

    // -- is_stale_at/is_live_at + a real WrapperProc (the actual join) ------------------
    //
    // `started` is deliberately POST-boot below, so the pre-boot rule can't be what's
    // firing - only the probe can explain the result. This is the entry point the
    // Traces page and beisl-mcp actually call, so it's the one that must exercise the
    // real `WrapperProc::verdict()` call, not a hand-injected verdict.

    #[test]
    fn is_stale_at_flags_a_live_state_with_a_dead_probe_even_post_boot() {
        let mut entry = live_entry(State::Recording, 300);
        entry.session.as_mut().unwrap().wrapper = Some(dead_wrapper());
        assert!(
            entry.is_stale_at(Some(200)),
            "a dead probe must be stale even post-boot"
        );
        assert!(!entry.is_live_at(Some(200)));
    }

    #[test]
    fn is_stale_at_is_not_stale_with_an_alive_probe_post_boot() {
        let mut entry = live_entry(State::Recording, 300);
        entry.session.as_mut().unwrap().wrapper = Some(alive_wrapper());
        assert!(!entry.is_stale_at(Some(200)));
        assert!(entry.is_live_at(Some(200)));
    }

    /// A live-state `Session` with no recorded wrapper (verdict always `None` -
    /// inconclusive), for the `stale_session_at` tests below that only exercise the
    /// pre-boot half of the rule.
    fn live_session(state: State) -> Session {
        Session {
            state,
            ..Default::default()
        }
    }

    // -- library::stale_session_at (the Session+started-only entry point) ---------------
    //
    // Mirrors the `is_stale_at`/`is_live_at` core tests above one-for-one: both now
    // delegate to the same `stale_core`, so a regression here is a regression there
    // too. `wrapper: None` throughout - these only exercise the pre-boot half; the
    // probe half is covered by the `stale_core` tests below.

    #[test]
    fn stale_session_at_flags_a_live_state_started_before_the_injected_boot() {
        assert!(library::stale_session_at(
            &live_session(State::Recording),
            100,
            Some(200)
        ));
    }

    #[test]
    fn stale_session_at_is_false_for_a_live_state_started_after_the_injected_boot() {
        assert!(!library::stale_session_at(
            &live_session(State::Recording),
            300,
            Some(200)
        ));
    }

    #[test]
    fn stale_session_at_is_false_when_boot_time_is_unknown() {
        // Same conservative edge as is_stale_at: an unknown boot time must never
        // manufacture a false "stale".
        assert!(!library::stale_session_at(
            &live_session(State::Recording),
            100,
            None
        ));
    }

    #[test]
    fn stale_session_at_is_false_for_an_unparseable_started() {
        // started == 0 (an unparseable run-* timestamp, via run_started) means an
        // unknown age, not an old one.
        assert!(!library::stale_session_at(
            &live_session(State::Recording),
            0,
            Some(200)
        ));
    }

    #[test]
    fn stale_session_at_never_flags_settled_states() {
        for state in [State::Done, State::Error] {
            assert!(
                !library::stale_session_at(&live_session(state), 100, Some(200)),
                "{state:?} must never be stale"
            );
        }
    }

    // -- library::stale_session_at + a real WrapperProc (the actual join) --------------
    //
    // Every test above passes `wrapper: None`, so it only ever exercises the pre-boot
    // half - deleting the `.and_then(WrapperProc::verdict)` call inside
    // `stale_session_at` and always passing `None` to `stale_core` would still pass
    // all of them. These two use a real probe (see `dead_wrapper`/`alive_wrapper`)
    // with a POST-boot `started`, so only the probe half can explain the result.

    #[test]
    fn stale_session_at_flags_a_live_state_with_a_dead_probe_even_post_boot() {
        let session = Session {
            state: State::Recording,
            wrapper: Some(dead_wrapper()),
            ..Default::default()
        };
        assert!(library::stale_session_at(&session, 300, Some(200)));
    }

    #[test]
    fn stale_session_at_is_not_stale_with_an_alive_probe_post_boot() {
        let session = Session {
            state: State::Recording,
            wrapper: Some(alive_wrapper()),
            ..Default::default()
        };
        assert!(!library::stale_session_at(&session, 300, Some(200)));
    }

    // -- library::stale_core (verdict injected - the wrapper-liveness half) -------------

    #[test]
    fn stale_core_flags_a_live_state_with_a_dead_verdict_even_in_the_current_boot() {
        // The new case this item adds: a wrapper killed within the current boot, so
        // `started` is post-boot (never trips the pre-boot rule) but the probe says
        // the recorded pid/starttime is gone.
        assert!(library::stale_core(
            State::Recording,
            300,
            Some(200),
            Some(false)
        ));
    }

    #[test]
    fn stale_core_is_not_stale_with_an_alive_verdict() {
        assert!(!library::stale_core(
            State::Recording,
            300,
            Some(200),
            Some(true)
        ));
    }

    #[test]
    fn stale_core_with_an_inconclusive_verdict_falls_back_to_the_boot_rule() {
        // None (legacy run with no Session::wrapper, or a probe from a mismatched PID
        // namespace) must behave exactly like the pre-probe rule: pre-boot is stale,
        // post-boot is not.
        assert!(library::stale_core(State::Recording, 100, Some(200), None));
        assert!(!library::stale_core(State::Recording, 300, Some(200), None));
    }

    #[test]
    fn stale_core_pre_boot_rule_stands_alone_even_with_an_alive_verdict() {
        // A same pid+starttime recurring across a reboot must never rescue a run that
        // predates the current boot - the boot rule is unconditional.
        assert!(library::stale_core(
            State::Recording,
            100,
            Some(200),
            Some(true)
        ));
    }

    #[test]
    fn stale_core_never_flags_settled_states_even_with_a_dead_verdict() {
        for state in [State::Done, State::Error] {
            assert!(
                !library::stale_core(state, 100, Some(200), Some(false)),
                "{state:?} must never be stale"
            );
        }
    }

    // -- library::run_started ------------------------------------------------------------

    #[test]
    fn run_started_parses_the_dir_name_timestamp() {
        assert_eq!(
            library::run_started(std::path::Path::new("/traces/run-12345")),
            12345
        );
    }

    #[test]
    fn run_started_is_zero_for_a_non_run_dir_name() {
        assert_eq!(
            library::run_started(std::path::Path::new("/traces/exports")),
            0
        );
    }

    #[test]
    fn run_started_is_zero_for_a_garbage_timestamp() {
        assert_eq!(
            library::run_started(std::path::Path::new("/traces/run-not-a-number")),
            0
        );
    }

    #[test]
    fn delete_run_removes_only_real_runs_under_root() {
        let root = fixture_root("del");
        put_run(&root, "run-100", Some("Game.exe"), true);
        assert!(library::delete_run(&root, &root.join("run-100")).is_ok());
        assert!(!root.join("run-100").exists());
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn delete_run_refuses_escapes_and_foreign_paths() {
        let root = fixture_root("delrefuse");
        // A directory outside the root, and a victim dir a symlink points at.
        let outside = std::env::temp_dir().join(format!("beisl-victim-{}", std::process::id()));
        std::fs::create_dir_all(&outside).unwrap();

        // Not under root.
        assert!(library::delete_run(&root, &outside).is_err());
        // Under root but not a run-* name.
        std::fs::create_dir_all(root.join("exports")).unwrap();
        assert!(library::delete_run(&root, &root.join("exports")).is_err());
        // A run-named SYMLINK to elsewhere: refused, and the victim survives.
        std::os::unix::fs::symlink(&outside, root.join("run-666")).unwrap();
        assert!(library::delete_run(&root, &root.join("run-666")).is_err());
        assert!(outside.exists());
        // Nonexistent: plain error, no panic.
        assert!(library::delete_run(&root, &root.join("run-404")).is_err());

        std::fs::remove_dir_all(&root).ok();
        std::fs::remove_dir_all(&outside).ok();
    }
}
