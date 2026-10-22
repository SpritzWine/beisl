//! The learned layers and the gate that decides whether they may speak.
//!
//! Small on purpose: a user labels tens of spikes, not thousands, so a nearest-neighbour
//! vote and a logistic regression trained in milliseconds are the right size, and both
//! can say why they answered.

use std::collections::BTreeMap;

use serde::Serialize;

use crate::rules;
use crate::{Candidate, Cause, Features, Hypothesis, Label, Source, MIN_CONFIDENCE};

/// Neighbours considered per vote.
const K: usize = 5;
/// Beyond this standardised distance a labelled spike is not a neighbour at all.
const NEAR: f32 = 1.5;
/// The nearest neighbour must be at least this close for a confident answer.
const CLOSE: f32 = 0.75;
/// Share of the weighted vote the winner needs.
const AGREE: f32 = 0.8;
/// Fewer shared features than this and two spikes are not comparable.
const MIN_SHARED: usize = 3;
/// The model's answer needs at least this probability.
const MODEL_P: f32 = 0.6;
/// A learned layer must be right this often on the user's labels to speak at all.
const PRECISION_FLOOR: f32 = 0.7;
/// And must have answered at least this many of them, or its record is luck.
const MIN_GRADED: usize = 5;
/// Trusted labels before the model is even graded; below this it overfits noise.
const MIN_FOR_MODEL: usize = 15;
const FOLDS: usize = 5;

/// How a layer did on the user's trusted labels.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize)]
pub struct Score {
    pub answered: usize,
    pub right: usize,
    /// How often the rules were right on the very same spikes, the bar to clear.
    pub rules_right: usize,
}

impl Score {
    pub fn precision(&self) -> f32 {
        self.right as f32 / self.answered.max(1) as f32
    }

    fn earned(&self) -> bool {
        self.answered >= MIN_GRADED
            && self.precision() >= PRECISION_FLOOR
            && self.right >= self.rules_right
    }
}

/// Which learned layers may speak, and on what record.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Gate {
    pub labels: usize,
    /// Labels the user stands behind: the only ones a layer is graded on.
    pub trusted: usize,
    pub rules: Score,
    pub neighbours: Score,
    pub model: Score,
    pub neighbours_on: bool,
    pub model_on: bool,
}

pub(crate) struct Learned {
    labels: Vec<Label>,
    scaler: Scaler,
    model: Option<Logistic>,
    pub(crate) gate: Gate,
}

impl Learned {
    pub(crate) fn train(labels: Vec<Label>) -> Self {
        let scaler = Scaler::fit(&labels);
        let trusted: Vec<usize> = (0..labels.len()).filter(|&i| labels[i].trusted()).collect();
        let mut gate = Gate {
            labels: labels.len(),
            trusted: trusted.len(),
            ..Gate::default()
        };

        for &i in &trusted {
            let truth = labels[i].cause;
            let ruled = rules_answer(&labels[i].features);
            if ruled.is_some() {
                gate.rules.answered += 1;
                gate.rules.right += (ruled == Some(truth)) as usize;
            }
            if let Some((cause, _, _)) = knn(&labels[i].features, &labels, &scaler, Some(i)) {
                gate.neighbours.answered += 1;
                gate.neighbours.right += (cause == truth) as usize;
                gate.neighbours.rules_right += (ruled == Some(truth)) as usize;
            }
        }
        gate.neighbours_on = gate.neighbours.earned();

        if trusted.len() >= MIN_FOR_MODEL {
            for fold in 0..FOLDS {
                let held: Vec<usize> = trusted
                    .iter()
                    .copied()
                    .enumerate()
                    .filter(|(n, _)| n % FOLDS == fold)
                    .map(|(_, i)| i)
                    .collect();
                let train: Vec<&Label> = (0..labels.len())
                    .filter(|i| !held.contains(i))
                    .map(|i| &labels[i])
                    .collect();
                let Some(m) = Logistic::train(&train, &scaler) else {
                    continue;
                };
                for &i in &held {
                    let truth = labels[i].cause;
                    if let Some((cause, _)) = m.predict(&labels[i].features, &scaler) {
                        gate.model.answered += 1;
                        gate.model.right += (cause == truth) as usize;
                        gate.model.rules_right +=
                            (rules_answer(&labels[i].features) == Some(truth)) as usize;
                    }
                }
            }
            gate.model_on = gate.model.earned();
        }

        let model = if gate.model_on {
            Logistic::train(&labels.iter().collect::<Vec<_>>(), &scaler)
        } else {
            None
        };
        Learned {
            labels,
            scaler,
            model,
            gate,
        }
    }

    /// A learned answer, when a layer has earned the right and is sure; the rules'
    /// candidates lend their evidence for the same cause.
    pub(crate) fn answer(&self, f: &Features, candidates: &[Candidate]) -> Option<Hypothesis> {
        let evidence_for = |cause: Cause| -> Vec<String> {
            candidates
                .iter()
                .find(|c| c.cause == cause)
                .map(|c| c.evidence.clone())
                .unwrap_or_default()
        };
        if self.gate.neighbours_on {
            if let Some((cause, confidence, like)) = knn(f, &self.labels, &self.scaler, None) {
                let mut evidence = vec![format!(
                    "close to {} spike(s) you labelled {}",
                    like.len(),
                    cause.as_str()
                )];
                evidence.extend(evidence_for(cause));
                return Some(Hypothesis {
                    cause,
                    confidence,
                    source: Source::Neighbours { like },
                    evidence,
                });
            }
        }
        if let Some(m) = &self.model {
            if let Some((cause, p)) = m.predict(f, &self.scaler) {
                let mut evidence = vec![format!(
                    "a model trained on your {} labels",
                    self.labels.len()
                )];
                evidence.extend(evidence_for(cause));
                return Some(Hypothesis {
                    cause,
                    confidence: p,
                    source: Source::Model,
                    evidence,
                });
            }
        }
        None
    }
}

fn rules_answer(f: &Features) -> Option<Cause> {
    rules::candidates(f)
        .first()
        .filter(|c| c.score >= MIN_CONFIDENCE)
        .map(|c| c.cause)
}

/// Per-feature mean and spread over the labels, so a count of spikes and a share of
/// samples weigh alike in a distance.
struct Scaler {
    stats: BTreeMap<String, (f32, f32)>,
}

impl Scaler {
    fn fit(labels: &[Label]) -> Self {
        let mut acc: BTreeMap<&str, Vec<f32>> = BTreeMap::new();
        for l in labels {
            for (k, &v) in &l.features {
                if v.is_finite() {
                    acc.entry(k.as_str()).or_default().push(v);
                }
            }
        }
        let stats = acc
            .into_iter()
            .filter(|(_, v)| v.len() >= 2)
            .map(|(k, v)| {
                let n = v.len() as f32;
                let mean = v.iter().sum::<f32>() / n;
                let var = v.iter().map(|x| (x - mean).powi(2)).sum::<f32>() / (n - 1.0);
                (k.to_string(), (mean, var.sqrt().max(1e-2)))
            })
            .collect();
        Scaler { stats }
    }

    fn z(&self, key: &str, v: f32) -> Option<f32> {
        let &(mean, sd) = self.stats.get(key)?;
        Some(((v - mean) / sd).clamp(-5.0, 5.0))
    }

    /// Root-mean-square standardised difference over the features both spikes have.
    fn distance(&self, a: &Features, b: &Features) -> Option<f32> {
        let (mut sum, mut n) = (0.0f32, 0usize);
        for (k, &va) in a {
            let (Some(&vb), Some(za)) = (b.get(k), self.z(k, va)) else {
                continue;
            };
            let zb = self.z(k, vb)?;
            sum += (za - zb).powi(2);
            n += 1;
        }
        (n >= MIN_SHARED).then(|| (sum / n as f32).sqrt())
    }
}

/// The weighted vote of the nearest labelled spikes, if it is confident: close enough,
/// enough of them, and agreeing. `skip` leaves one label out (for grading).
fn knn(
    f: &Features,
    labels: &[Label],
    scaler: &Scaler,
    skip: Option<usize>,
) -> Option<(Cause, f32, Vec<String>)> {
    let mut near: Vec<(f32, &Label)> = labels
        .iter()
        .enumerate()
        .filter(|&(i, _)| Some(i) != skip)
        .filter_map(|(_, l)| Some((scaler.distance(f, &l.features)?, l)))
        .filter(|&(d, _)| d <= NEAR)
        .collect();
    near.sort_by(|a, b| a.0.total_cmp(&b.0));
    near.truncate(K);
    if near.len() < 3 || near[0].0 > CLOSE {
        return None;
    }
    let mut votes: BTreeMap<Cause, f32> = BTreeMap::new();
    for &(d, l) in &near {
        *votes.entry(l.cause).or_default() += l.weight() / (d + 0.1);
    }
    let total: f32 = votes.values().sum();
    let (&cause, &won) = votes.iter().max_by(|a, b| a.1.total_cmp(b.1))?;
    let share = won / total;
    if share < AGREE {
        return None;
    }
    let like = near
        .iter()
        .filter(|(_, l)| l.cause == cause)
        .take(3)
        .map(|(_, l)| format!("{} at {:.1}s", l.run, l.t_seconds))
        .collect();
    Some((cause, (share * 0.95).min(0.95), like))
}

/// Multinomial logistic regression over the standardised features, a missing feature
/// read as average (0 after scaling).
struct Logistic {
    keys: Vec<String>,
    classes: Vec<Cause>,
    w: Vec<Vec<f32>>,
    b: Vec<f32>,
}

impl Logistic {
    fn x(&self, f: &Features, scaler: &Scaler) -> Vec<f32> {
        self.keys
            .iter()
            .map(|k| f.get(k).and_then(|&v| scaler.z(k, v)).unwrap_or(0.0))
            .collect()
    }

    fn train(labels: &[&Label], scaler: &Scaler) -> Option<Logistic> {
        let mut classes: Vec<Cause> = labels.iter().map(|l| l.cause).collect();
        classes.sort();
        classes.dedup();
        if classes.len() < 2 {
            return None;
        }
        let keys: Vec<String> = scaler.stats.keys().cloned().collect();
        let mut m = Logistic {
            w: vec![vec![0.0; keys.len()]; classes.len()],
            b: vec![0.0; classes.len()],
            keys,
            classes,
        };
        let data: Vec<(Vec<f32>, usize, f32)> = labels
            .iter()
            .map(|l| {
                let y = m
                    .classes
                    .iter()
                    .position(|&c| c == l.cause)
                    .expect("listed");
                (m.x(&l.features, scaler), y, l.weight())
            })
            .collect();
        let total: f32 = data.iter().map(|d| d.2).sum();
        const EPOCHS: usize = 300;
        const RATE: f32 = 0.5;
        const L2: f32 = 1e-2;
        for _ in 0..EPOCHS {
            let mut gw = vec![vec![0.0f32; m.keys.len()]; m.classes.len()];
            let mut gb = vec![0.0f32; m.classes.len()];
            for (x, y, wt) in &data {
                let p = m.probs(x);
                for c in 0..m.classes.len() {
                    let err = (p[c] - (c == *y) as u8 as f32) * wt;
                    gb[c] += err;
                    for (g, xi) in gw[c].iter_mut().zip(x) {
                        *g += err * xi;
                    }
                }
            }
            for c in 0..m.classes.len() {
                m.b[c] -= RATE * gb[c] / total;
                for (w, g) in m.w[c].iter_mut().zip(&gw[c]) {
                    *w -= RATE * (g / total + L2 * *w);
                }
            }
        }
        Some(m)
    }

    fn probs(&self, x: &[f32]) -> Vec<f32> {
        let logits: Vec<f32> = self
            .w
            .iter()
            .zip(&self.b)
            .map(|(w, b)| b + w.iter().zip(x).map(|(a, c)| a * c).sum::<f32>())
            .collect();
        let max = logits.iter().copied().fold(f32::MIN, f32::max);
        let exp: Vec<f32> = logits.iter().map(|l| (l - max).exp()).collect();
        let sum: f32 = exp.iter().sum();
        exp.iter().map(|e| e / sum).collect()
    }

    fn predict(&self, f: &Features, scaler: &Scaler) -> Option<(Cause, f32)> {
        let p = self.probs(&self.x(f, scaler));
        let (i, &best) = p.iter().enumerate().max_by(|a, b| a.1.total_cmp(b.1))?;
        (best >= MODEL_P).then(|| (self.classes[i], best))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::labels::tests::label;
    use crate::{Judge, LabelSource};

    /// Spikes of two causes the rules cannot tell apart (no game rate at all), in two
    /// tight clusters: only labels can separate them.
    fn clusters(n: usize, source: LabelSource) -> Vec<Label> {
        (0..n)
            .map(|i| {
                let j = (i % 7) as f32 * 0.01;
                let (cause, f) = if i % 2 == 0 {
                    (Cause::ShaderCompile, [2.0 + j, 0.0 + j, 5.0 - j, 0.1])
                } else {
                    (Cause::Blocked, [6.0 - j, 3.0 + j, 1.0 + j, 0.9])
                };
                let mut l = label("run-9", i as f64, cause, source);
                l.features = crate::tests::feats(&[
                    ("ft_ratio_log", f[0]),
                    ("burst_1s", f[1]),
                    ("t_log_s", f[2]),
                    ("thread_render_share", f[3]),
                ]);
                l
            })
            .collect()
    }

    #[test]
    fn neighbours_earn_their_say_on_the_users_labels_and_explain_themselves() {
        let judge = Judge::new(clusters(20, LabelSource::User));
        let gate = judge.gate();
        assert!(gate.neighbours_on, "{gate:?}");
        assert_eq!(gate.neighbours.right, gate.neighbours.answered);
        // The rules said nothing on these, so the bar was only the floor.
        assert_eq!(gate.rules.answered, 0);

        let probe = crate::tests::feats(&[
            ("ft_ratio_log", 6.0),
            ("burst_1s", 3.0),
            ("t_log_s", 1.0),
            ("thread_render_share", 0.9),
        ]);
        let v = judge.judge(&probe);
        let h = v.hypothesis.unwrap();
        assert_eq!(h.cause, Cause::Blocked);
        let Source::Neighbours { like } = &h.source else {
            panic!("{:?}", h.source)
        };
        assert!(like[0].starts_with("run-9 at "));
    }

    #[test]
    fn an_agents_guesses_alone_never_switch_a_layer_on() {
        // Agent labels are training data, but not ground truth: nothing to grade on.
        let judge = Judge::new(clusters(40, LabelSource::Agent));
        let gate = judge.gate();
        assert_eq!((gate.labels, gate.trusted), (40, 0));
        assert!(!gate.neighbours_on && !gate.model_on);
    }

    #[test]
    fn the_model_is_graded_by_folds_and_speaks_between_the_clusters() {
        let judge = Judge::new(clusters(30, LabelSource::User));
        let gate = judge.gate();
        assert!(gate.model_on, "{gate:?}");
        assert!(gate.model.precision() >= PRECISION_FLOOR);
        // Far from both clusters on most features: not a confident neighbour, but the
        // model still separates by the one feature that splits them.
        let between = crate::tests::feats(&[
            ("ft_ratio_log", 4.0),
            ("burst_1s", 1.5),
            ("t_log_s", 3.0),
            ("thread_render_share", 0.95),
        ]);
        let scaler = &judge.learned.scaler;
        assert!(knn(&between, &judge.learned.labels, scaler, None).is_none());
        let h = judge.judge(&between).hypothesis.unwrap();
        assert_eq!((h.cause, h.source), (Cause::Blocked, Source::Model));
    }

    #[test]
    fn too_few_labels_leave_the_rules_alone() {
        let judge = Judge::new(clusters(4, LabelSource::User));
        assert!(!judge.gate().neighbours_on && !judge.gate().model_on);
    }
}
