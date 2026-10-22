//! A likely cause for a frametime spike, beside the measurement and never instead of it.
//!
//! Three layers answer in turn, and each must earn its place:
//!
//! 1. **Neighbours**: when spikes the user labelled sit close to this one and agree, that
//!    is the most direct evidence there is, and it can say which spikes it resembles.
//! 2. **The model**: a small logistic regression over every label, for the cases between
//!    clusters that the neighbours are not sure of.
//! 3. **The rules**: readable thresholds over the features, the answer before any label
//!    exists and the fallback after.
//!
//! Below a threshold everything abstains: a wrong cause stated confidently sends the user
//! after the wrong fix. The learned layers speak only once leave-one-out on the user's own
//! labels shows them beating the rules (see [`learn::Gate`]).
//!
//! Every label is local. Where it came from (the user, an agent, the fault-injection
//! harness) is kept, and an agent's label weighs less until the user confirms it.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub mod groups;
pub mod labels;
pub mod learn;
mod rules;

pub use groups::{Context, Env, FeatureGroup};
pub use labels::{Label, LabelSource, LabelStore};
pub use learn::{Gate, Snapshot};

/// A spike as named measurements (built by the artifact crate's `features` module).
pub type Features = BTreeMap<String, f32>;

/// What a spike can be put down to. Kept small on purpose: each has to be tellable apart
/// from the others with what beisl records, or labels for it are guesses.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Cause {
    /// Pipeline/shader compilation on the frame's critical path.
    ShaderCompile,
    /// The game lost its CPU to other work.
    CpuContention,
    /// The CPU was capped: clock dropped, a power or thermal limit, a quota.
    Throttle,
    /// The game was off the CPU for one long moment: waiting on IO or a lock, or stopped.
    Blocked,
    /// The CPU waited on the GPU.
    GpuBound,
    /// Paging or swapping.
    MemoryPressure,
    /// The game kept running and simply had more of its own work in that frame.
    CpuWork,
    /// Can't tell from what was measured. A real answer: stored so the spike stops
    /// being asked, but never trained on and never grading a layer, because a guess
    /// teaches worse than nothing. Not in [`Cause::ALL`], which lists what a spike can
    /// be put down to.
    Unknown,
}

impl Cause {
    pub const ALL: [Cause; 7] = [
        Cause::ShaderCompile,
        Cause::CpuContention,
        Cause::Throttle,
        Cause::Blocked,
        Cause::GpuBound,
        Cause::MemoryPressure,
        Cause::CpuWork,
    ];

    /// The stable name, as stored in labels and accepted on the command line.
    pub fn as_str(self) -> &'static str {
        match self {
            Cause::ShaderCompile => "shader-compile",
            Cause::CpuContention => "cpu-contention",
            Cause::Throttle => "throttle",
            Cause::Blocked => "blocked",
            Cause::GpuBound => "gpu-bound",
            Cause::MemoryPressure => "memory-pressure",
            Cause::CpuWork => "cpu-work",
            Cause::Unknown => "unknown",
        }
    }

    /// One of [`Cause::ALL`], or `unknown`.
    pub fn parse(s: &str) -> Option<Cause> {
        Cause::ALL
            .into_iter()
            .chain([Cause::Unknown])
            .find(|c| c.as_str() == s)
    }

    /// Whether a label with this cause teaches anything: everything but `unknown`.
    pub fn is_known(self) -> bool {
        self != Cause::Unknown
    }

    /// What it means, for a person.
    pub fn describe(self) -> &'static str {
        match self {
            Cause::ShaderCompile => "shader compilation",
            Cause::CpuContention => "other work took the game's CPU",
            Cause::Throttle => "the CPU was held back (clock, power, or a quota)",
            Cause::Blocked => "the game waited off the CPU (IO, a lock, or stopped)",
            Cause::GpuBound => "the CPU waited on the GPU",
            Cause::MemoryPressure => "paging or swapping",
            Cause::CpuWork => "the game's own extra CPU work",
            Cause::Unknown => "can't tell from what was measured",
        }
    }
}

/// Which layer answered.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", tag = "by")]
pub enum Source {
    Rules,
    /// With the labelled spikes it resembles most, `run at t` each.
    Neighbours {
        like: Vec<String>,
    },
    Model,
}

/// A likely cause, how sure, which layer said so, and the measurements behind it. The
/// evidence is the features themselves, so it stays true even when the cause is wrong.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Hypothesis {
    pub cause: Cause,
    pub confidence: f32,
    pub source: Source,
    pub evidence: Vec<String>,
}

/// How a person should be asked about a verdict: a sure one needs only a yes, an unsure
/// one deserves the candidates laid out.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Review {
    Confirm,
    Checklist,
}

/// Everything known about one spike: the hypothesis if any layer is sure enough, every
/// candidate the rules see with its evidence (the checklist), and how to ask.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Verdict {
    pub hypothesis: Option<Hypothesis>,
    pub candidates: Vec<Candidate>,
    pub review: Review,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Candidate {
    pub cause: Cause,
    pub score: f32,
    pub evidence: Vec<String>,
}

/// Below this nobody states a cause.
pub const MIN_CONFIDENCE: f32 = 0.5;
/// From this on a verdict asks only for a confirmation.
pub const CONFIRM_CONFIDENCE: f32 = 0.75;

/// The trained state on disk, beside the labels it was trained from.
pub struct ModelStore {
    path: PathBuf,
}

impl ModelStore {
    pub fn at(path: impl Into<PathBuf>) -> Self {
        ModelStore { path: path.into() }
    }

    pub fn default_path() -> Self {
        ModelStore::at(session::library::data_root().join("label-model.json"))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The last training, if there is one this build can read.
    pub fn load(&self) -> Option<Snapshot> {
        let snap: Snapshot = serde_json::from_slice(&std::fs::read(&self.path).ok()?).ok()?;
        (snap.version == learn::SNAPSHOT_VERSION).then_some(snap)
    }

    /// Staged and renamed, so a judge loading meanwhile sees the old training or the
    /// new one, never half of either.
    pub fn save(&self, snap: &Snapshot) -> io::Result<()> {
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let mut tmp = self.path.as_os_str().to_owned();
        tmp.push(format!(".tmp.{}", std::process::id()));
        let tmp = PathBuf::from(tmp);
        let json = serde_json::to_vec(snap).map_err(io::Error::other)?;
        std::fs::write(&tmp, json)?;
        std::fs::rename(&tmp, &self.path).inspect_err(|_| {
            let _ = std::fs::remove_file(&tmp);
        })
    }
}

/// The cascade: rules, plus whatever the last training taught it.
pub struct Judge {
    learned: learn::Learned,
    trained_at: Option<u64>,
    /// Labels newer than the last training: they already vote as neighbours, but
    /// change the gate and the model only once trained on.
    untrained: usize,
}

impl Judge {
    /// Rules only: what answers before anyone has labelled anything.
    pub fn rules_only() -> Self {
        Judge::new(Vec::new())
    }

    /// Trained here and now on `labels`, kept in memory only.
    pub fn new(labels: Vec<Label>) -> Self {
        Judge {
            learned: learn::Learned::train(labels),
            trained_at: None,
            untrained: 0,
        }
    }

    /// The labels as they are and the last training, without training: what every
    /// report, listing and window uses, so judging stays fast however many labels
    /// pile up. With no training yet, the learned layers are silent.
    pub fn load(labels: &LabelStore, model: &ModelStore) -> Self {
        let loaded = labels.load();
        let labels = loaded.labels;
        match model.load() {
            Some(snap) => Judge {
                // A retraction after the training changed what it should have seen too.
                untrained: labels
                    .iter()
                    .filter(|l| l.at > snap.newest_label_at)
                    .count()
                    + loaded
                        .retracted_at
                        .iter()
                        .filter(|&&at| at > snap.trained_at)
                        .count(),
                trained_at: Some(snap.trained_at),
                learned: learn::Learned::from_snapshot(snap, labels),
            },
            None => Judge {
                untrained: labels.len(),
                trained_at: None,
                learned: learn::Learned::untrained(labels),
            },
        }
    }

    /// The user's labels and their last training.
    pub fn load_default() -> Self {
        Judge::load(&LabelStore::default_path(), &ModelStore::default_path())
    }

    /// Train on every label in `labels` and save the result to `model`: the slow part,
    /// run only when someone asks for it.
    pub fn retrain(labels: &LabelStore, model: &ModelStore) -> io::Result<Self> {
        let learned = learn::Learned::train(labels.load().labels);
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        model.save(&learned.snapshot(now))?;
        Ok(Judge {
            learned,
            trained_at: Some(now),
            untrained: 0,
        })
    }

    /// When the training in use was made (unix seconds); `None` if never, or if this
    /// judge was trained in memory.
    pub fn trained_at(&self) -> Option<u64> {
        self.trained_at
    }

    /// Labels, and retractions, that came after the last training.
    pub fn untrained_labels(&self) -> usize {
        self.untrained
    }

    /// Which learned layers the gate let speak, and on what evidence.
    pub fn gate(&self) -> &Gate {
        &self.learned.gate
    }

    /// `ctx` is the judged spike's game and environment: labelled spikes from another
    /// game or another kind of stack weigh differently (see [`groups`]).
    pub fn judge(&self, f: &Features, ctx: &Context) -> Verdict {
        let candidates = rules::candidates(f);
        let ruled = candidates
            .first()
            .filter(|c| c.score >= MIN_CONFIDENCE)
            .map(|c| Hypothesis {
                cause: c.cause,
                confidence: c.score,
                source: Source::Rules,
                evidence: c.evidence.clone(),
            });
        let hypothesis = self.learned.answer(f, ctx, &candidates).or(ruled);
        let review = match &hypothesis {
            Some(h) if h.confidence >= CONFIRM_CONFIDENCE => Review::Confirm,
            _ => Review::Checklist,
        };
        Verdict {
            hypothesis,
            candidates,
            review,
        }
    }
}

impl Verdict {
    /// One line for a report: the cause, how sure, and who said so; or that nobody is.
    pub fn line(&self) -> String {
        match &self.hypothesis {
            None => "no likely cause yet (unsure)".to_string(),
            Some(h) => {
                let by = match &h.source {
                    Source::Rules => "rules".to_string(),
                    Source::Model => "learned from your labels".to_string(),
                    Source::Neighbours { like } => {
                        format!("like {} you labelled", like.first().map_or("spikes", |s| s))
                    }
                };
                format!(
                    "likely {} ({}, {:.0}%, {by})",
                    h.cause.as_str(),
                    h.cause.describe(),
                    h.confidence * 100.0
                )
            }
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn feats(pairs: &[(&str, f32)]) -> Features {
        pairs.iter().map(|&(k, v)| (k.to_string(), v)).collect()
    }

    #[test]
    fn causes_round_trip_through_their_names() {
        for c in Cause::ALL.into_iter().chain([Cause::Unknown]) {
            assert_eq!(Cause::parse(c.as_str()), Some(c));
            let json = serde_json::to_string(&c).unwrap();
            assert_eq!(json, format!("\"{}\"", c.as_str()));
        }
        assert_eq!(Cause::parse("cosmic-rays"), None);
    }

    #[test]
    fn with_no_labels_the_rules_answer_and_unsure_asks_for_a_checklist() {
        let judge = Judge::rules_only();
        let v = judge.judge(
            &feats(&[
                ("game_rate_ratio", 0.0),
                ("burst_1s", 0.0),
                ("repeat_len_log", 0.0),
            ]),
            &Context::default(),
        );
        let h = v.hypothesis.as_ref().unwrap();
        assert_eq!((h.cause, h.source.clone()), (Cause::Blocked, Source::Rules));
        assert!(h.confidence < CONFIRM_CONFIDENCE);
        assert_eq!(v.review, Review::Checklist);
        // Nothing measured, nothing said.
        let v = judge.judge(&Features::new(), &Context::default());
        assert_eq!(v.hypothesis, None);
        assert!(v.line().contains("unsure"));
    }
}
