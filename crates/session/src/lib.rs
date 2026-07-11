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
                let ts = id.strip_prefix("run-")?.to_string();
                if !e.file_type().ok()?.is_dir() {
                    return None;
                }
                let dir = e.path();
                Some(RunEntry {
                    started: ts.parse().unwrap_or(0),
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
            ..Default::default()
        };
        s.write(&dir).unwrap();
        let back = Session::read(&dir).unwrap();
        assert_eq!(back.game, "Blood West.exe");
        assert_eq!(back.state, State::Recording);
        assert_eq!(back.warnings.len(), 1);
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
