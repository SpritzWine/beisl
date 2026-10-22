//! The user's labels: what a spike really was, as they (or an agent, or the harness)
//! judged it. One JSON object per line, appended, in beisl's data directory and nowhere
//! else.
//!
//! Append-only so nothing a person said is ever rewritten in place: a correction is a
//! newer label for the same spike, and the newest one wins when the file is read.

use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::{Cause, Context, Features};

/// Who said what a spike was.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LabelSource {
    /// The person at the keyboard: the ground truth this exists for.
    User,
    /// An assistant, through beisl-mcp: an informed guess until the user confirms it.
    Agent,
    /// The fault-injection harness: true for what it injected, on a synthetic game.
    Harness,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Label {
    /// The run directory's name (`run-<unix>`).
    pub run: String,
    /// Where the spike sits on the run's timeline, as in the artifact.
    pub t_seconds: f64,
    pub frametime_ms: f32,
    /// The spike's features when it was labelled, so a label outlives its trace.
    pub features: Features,
    /// Its game and environment, which decide how it compares with other spikes.
    /// Defaulted (unknown) on labels from before it existed.
    #[serde(default)]
    pub context: Context,
    pub cause: Cause,
    pub source: LabelSource,
    /// The user stood behind it: always true for their own, set for an agent's or the
    /// harness's label once they confirm it.
    #[serde(default)]
    pub confirmed: bool,
    /// Free text: what made the labeller sure, if they said.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub note: String,
    /// Unix seconds.
    pub at: u64,
}

impl Label {
    /// How much this label counts in training. An agent's unconfirmed guess must not be
    /// able to outvote the user, and synthetic spikes are not the user's games.
    pub fn weight(&self) -> f32 {
        match (self.source, self.confirmed) {
            (LabelSource::User, _) | (_, true) => 1.0,
            (LabelSource::Harness, false) => 0.5,
            (LabelSource::Agent, false) => 0.3,
        }
    }

    /// Whether it counts as ground truth when the learned layers are graded.
    pub fn trusted(&self) -> bool {
        self.weight() >= 1.0
    }

    /// The spike this labels: a run and a timeline position to the millisecond.
    pub fn key(&self) -> (String, i64) {
        (self.run.clone(), (self.t_seconds * 1000.0).round() as i64)
    }
}

/// What reading the store found.
#[derive(Debug, Default)]
pub struct Loaded {
    /// The newest label per spike, oldest spike first.
    pub labels: Vec<Label>,
    /// Lines that did not parse: from a newer beisl, or damaged. Kept in the file.
    pub skipped: usize,
}

pub struct LabelStore {
    path: PathBuf,
}

impl LabelStore {
    pub fn at(path: impl Into<PathBuf>) -> Self {
        LabelStore { path: path.into() }
    }

    /// `labels.jsonl` beside the traces, under beisl's data root.
    pub fn default_path() -> Self {
        LabelStore::at(session::library::data_root().join("labels.jsonl"))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn append(&self, label: &Label) -> io::Result<()> {
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let line = serde_json::to_string(label).map_err(io::Error::other)?;
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        // One write per line, so a reader never sees half a label from this process.
        f.write_all(format!("{line}\n").as_bytes())
    }

    /// Every spike's newest label. A missing file is no labels, not an error.
    pub fn load(&self) -> Loaded {
        let Ok(text) = std::fs::read_to_string(&self.path) else {
            return Loaded::default();
        };
        let mut out = Loaded::default();
        let mut newest: Vec<Label> = Vec::new();
        for line in text.lines().filter(|l| !l.trim().is_empty()) {
            match serde_json::from_str::<Label>(line) {
                Ok(label) => {
                    let key = label.key();
                    newest.retain(|l| l.key() != key);
                    newest.push(label);
                }
                Err(_) => out.skipped += 1,
            }
        }
        out.labels = newest;
        out
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::tests::feats;

    pub(crate) fn label(run: &str, t: f64, cause: Cause, source: LabelSource) -> Label {
        Label {
            run: run.into(),
            t_seconds: t,
            frametime_ms: 100.0,
            features: feats(&[("burst_1s", 0.0)]),
            context: Context::default(),
            cause,
            source,
            confirmed: source == LabelSource::User,
            note: String::new(),
            at: 1_792_600_000,
        }
    }

    #[test]
    fn the_newest_label_for_a_spike_wins_and_damage_is_counted_not_fatal() {
        let dir = std::env::temp_dir().join(format!("beisl-labels-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = LabelStore::at(dir.join("labels.jsonl"));
        assert!(store.load().labels.is_empty(), "no file is no labels");

        let mut agent = label("run-1", 12.3456, Cause::CpuContention, LabelSource::Agent);
        store.append(&agent).unwrap();
        store
            .append(&label("run-1", 40.0, Cause::Blocked, LabelSource::User))
            .unwrap();
        // The user corrects the agent: same spike, to the millisecond.
        agent.cause = Cause::Throttle;
        agent.source = LabelSource::User;
        agent.t_seconds = 12.3457;
        store.append(&agent).unwrap();
        std::fs::OpenOptions::new()
            .append(true)
            .open(store.path())
            .unwrap()
            .write_all(b"{not json\n")
            .unwrap();

        let loaded = store.load();
        assert_eq!(loaded.skipped, 1);
        let causes: Vec<_> = loaded
            .labels
            .iter()
            .map(|l| (l.t_seconds, l.cause))
            .collect();
        assert_eq!(causes, [(40.0, Cause::Blocked), (12.3457, Cause::Throttle)]);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn an_agent_counts_less_until_the_user_stands_behind_it() {
        let mut a = label("run-1", 1.0, Cause::Blocked, LabelSource::Agent);
        assert!(a.weight() < 1.0 && !a.trusted());
        a.confirmed = true;
        assert!(a.trusted());
        assert!(label("run-1", 1.0, Cause::Blocked, LabelSource::User).trusted());
        assert!(!label("run-1", 1.0, Cause::Blocked, LabelSource::Harness).trusted());
    }
}
