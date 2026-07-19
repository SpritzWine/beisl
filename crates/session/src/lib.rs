//! Live session state, shared between the `beisl` wrapper (the writer) and the
//! `beisl-setup --session` window (the reader), via a small JSON file in the trace
//! directory. The wrapper owns the game + perf lifecycle and publishes state here; the
//! window polls it to show settings, recording state, and "what's wrong" while you play.
//! One reverse channel: the window sets `stop_requested` to ask the wrapper to stop.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub const FILE: &str = "session.json";

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
/// the UI-agnostic "list past runs" answer the trace library (DESIGN.md, v1) builds on.
/// It lives here because `session.json` is already this crate's bridge between the
/// wrapper and the GUI — the library is the same bridge, across time instead of live.
pub mod library {
    use std::path::{Path, PathBuf};

    use super::Session;

    /// Written by the wrapper's post-run analysis next to `session.json`.
    pub const ARTIFACT_FILE: &str = "artifact.json";
    pub const REPORT_FILE: &str = "report.txt";

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
        /// The converse isn't detected: a wrapper that dies within the *current*
        /// boot leaves its run wrongly-live until the next reboot — that would
        /// need a pid liveness probe (wrapper pid in session.json + /proc check),
        /// a separate mechanism for a much rarer window.
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
                .is_some_and(|s| stale_session_at(s.state, self.started, boot))
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

    /// Core of `stale_session`, boot time injectable — same pattern as
    /// `RunEntry::is_stale_at`, and in fact the function it now delegates to. Two
    /// conservative edges, each favoring "protect the data" over "reclaim the row":
    /// boot time unknown -> nothing is stale (a false "live" only keeps a row around a
    /// little longer; a false "stale" could let someone delete/hide an in-progress
    /// recording). An unparseable/zero `started` -> not stale either, for the same
    /// reason — an unknown age is never treated as an old one. Settled states
    /// (`Done`/`Error`) are excluded by `is_live_state` itself: settled is settled,
    /// not "stale".
    pub(crate) fn stale_session_at(state: super::State, started: u64, boot: Option<u64>) -> bool {
        is_live_state(state) && started > 0 && boot.is_some_and(|b| started < b)
    }

    /// Whether a live-state session (`state`, with the run's launch time `started` —
    /// see `run_started`) is actually stuck: it started before the current boot, so
    /// whatever process was recording it died with that boot without ever writing
    /// `Done`/`Error`/`Saved`->done. The `Session`+`started`-only entry point for
    /// callers that don't have a scanned `RunEntry` — the session window watches its
    /// *own* live `session.json` directly, not a `library::scan` result. Same core
    /// (`is_live_state` + the real boot time) as `RunEntry::is_stale`, so the two can
    /// never disagree about the same run.
    ///
    /// A genuinely live window is never misclassified: the wrapper only spawns the
    /// session window for a run it just started *this* boot, so `started >=
    /// boot_time()` always holds for it and this returns `false`. Only a run stuck
    /// from a *previous* boot ever trips it.
    pub fn stale_session(state: super::State, started: u64) -> bool {
        stale_session_at(state, started, boot_time())
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

    // -- library::stale_session_at (the Session+started-only entry point) ---------------
    //
    // Mirrors the `is_stale_at`/`is_live_at` core tests above one-for-one: both now
    // delegate to this same function, so a regression here is a regression there too.

    #[test]
    fn stale_session_at_flags_a_live_state_started_before_the_injected_boot() {
        assert!(library::stale_session_at(State::Recording, 100, Some(200)));
    }

    #[test]
    fn stale_session_at_is_false_for_a_live_state_started_after_the_injected_boot() {
        assert!(!library::stale_session_at(State::Recording, 300, Some(200)));
    }

    #[test]
    fn stale_session_at_is_false_when_boot_time_is_unknown() {
        // Same conservative edge as is_stale_at: an unknown boot time must never
        // manufacture a false "stale".
        assert!(!library::stale_session_at(State::Recording, 100, None));
    }

    #[test]
    fn stale_session_at_is_false_for_an_unparseable_started() {
        // started == 0 (an unparseable run-* timestamp, via run_started) means an
        // unknown age, not an old one.
        assert!(!library::stale_session_at(State::Recording, 0, Some(200)));
    }

    #[test]
    fn stale_session_at_never_flags_settled_states() {
        for state in [State::Done, State::Error] {
            assert!(
                !library::stale_session_at(state, 100, Some(200)),
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
