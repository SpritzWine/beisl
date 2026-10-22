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

use serde::{Deserialize, Serialize};

pub mod labels;
pub mod learn;
mod rules;

pub use labels::{Label, LabelSource, LabelStore};
pub use learn::Gate;

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
        }
    }

    pub fn parse(s: &str) -> Option<Cause> {
        Cause::ALL.into_iter().find(|c| c.as_str() == s)
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

/// The cascade, trained once from a set of labels.
pub struct Judge {
    learned: learn::Learned,
}

impl Judge {
    /// Rules only: what answers before anyone has labelled anything.
    pub fn rules_only() -> Self {
        Judge::new(Vec::new())
    }

    pub fn new(labels: Vec<Label>) -> Self {
        Judge {
            learned: learn::Learned::train(labels),
        }
    }

    /// From the user's label store; rules only when there is none.
    pub fn load_default() -> Self {
        Judge::new(LabelStore::default_path().load().labels)
    }

    /// Which learned layers the gate let speak, and on what evidence.
    pub fn gate(&self) -> &Gate {
        &self.learned.gate
    }

    pub fn judge(&self, f: &Features) -> Verdict {
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
        let hypothesis = self.learned.answer(f, &candidates).or(ruled);
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
        for c in Cause::ALL {
            assert_eq!(Cause::parse(c.as_str()), Some(c));
            let json = serde_json::to_string(&c).unwrap();
            assert_eq!(json, format!("\"{}\"", c.as_str()));
        }
        assert_eq!(Cause::parse("cosmic-rays"), None);
    }

    #[test]
    fn with_no_labels_the_rules_answer_and_unsure_asks_for_a_checklist() {
        let judge = Judge::rules_only();
        let v = judge.judge(&feats(&[
            ("game_rate_ratio", 0.0),
            ("burst_1s", 0.0),
            ("repeat_len_log", 0.0),
        ]));
        let h = v.hypothesis.as_ref().unwrap();
        assert_eq!((h.cause, h.source.clone()), (Cause::Blocked, Source::Rules));
        assert!(h.confidence < CONFIRM_CONFIDENCE);
        assert_eq!(v.review, Review::Checklist);
        // Nothing measured, nothing said.
        let v = judge.judge(&Features::new());
        assert_eq!(v.hypothesis, None);
        assert!(v.line().contains("unsure"));
    }
}
