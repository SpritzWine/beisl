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
    Done,
    Error,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct Session {
    /// Human name for the game (best-effort from the launch command).
    pub game: String,
    /// Where the trace is written.
    pub trace_dir: String,
    /// The MangoHud logging hotkey that toggles a capture.
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
}
