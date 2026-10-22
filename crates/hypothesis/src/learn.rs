//! The learned layers and the gate that decides whether they may speak.
//!
//! Small on purpose: a user labels tens of spikes, not thousands, so a nearest-neighbour
//! vote and a logistic regression trained in milliseconds are the right size, and both
//! can say why they answered.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::groups::{self, FeatureGroup};
use crate::rules;
use crate::{Candidate, Cause, Context, Features, Hypothesis, Label, Source, MIN_CONFIDENCE};

/// Neighbours considered per vote.
const K: usize = 5;
/// Beyond this standardised distance a labelled spike is not a neighbour at all.
const NEAR: f32 = 1.5;
/// The nearest neighbour must be at least this close for a confident answer.
const CLOSE: f32 = 0.75;
/// Share of the weighted vote the winner needs.
const AGREE: f32 = 0.8;
/// Less shared feature weight than this (a feature at full weight counts 1) and two
/// spikes are not comparable.
const MIN_SHARED: usize = 3;
/// The model's answer needs at least this probability.
const MODEL_P: f32 = 0.6;
/// A learned layer must be right this often on the user's labels to speak at all.
const PRECISION_FLOOR: f32 = 0.7;
/// And must have answered at least this many of them, or its record is luck.
const MIN_GRADED: usize = 5;
/// Trusted labels before the model is even graded; below this it overfits noise.
pub const MIN_FOR_MODEL: usize = 15;
const FOLDS: usize = 5;

/// How a layer did on the user's trusted labels.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
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
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
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

/// What training produces, saved so that using the learned layers never means
/// training them: the scale features are compared on, the model, and the gate's
/// verdict. The neighbours still vote over the labels as they are now, so a new label
/// counts at once; whether a layer may speak changes only at the next training.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Snapshot {
    pub version: u32,
    /// Unix seconds.
    pub trained_at: u64,
    pub trained_on: usize,
    /// The newest label it saw, so a reader can count the ones that came after.
    pub newest_label_at: u64,
    pub gate: Gate,
    scaler: Scaler,
    model: Option<Logistic>,
}

/// Bump when [`Snapshot`]'s shape changes: an older one is ignored, and the learned
/// layers stay silent until the next training rather than misreading it.
pub const SNAPSHOT_VERSION: u32 = 3;

impl Learned {
    /// Labels known, nothing trained: the learned layers stay silent.
    pub(crate) fn untrained(labels: Vec<Label>) -> Self {
        let labels = teaching(labels);
        let gate = Gate {
            labels: labels.len(),
            trusted: labels.iter().filter(|l| l.trusted()).count(),
            ..Gate::default()
        };
        Learned {
            labels,
            scaler: Scaler {
                stats: BTreeMap::new(),
            },
            model: None,
            gate,
        }
    }

    pub(crate) fn from_snapshot(snap: Snapshot, labels: Vec<Label>) -> Self {
        Learned {
            labels: teaching(labels),
            scaler: snap.scaler,
            model: snap.model,
            gate: snap.gate,
        }
    }

    pub(crate) fn snapshot(&self, trained_at: u64) -> Snapshot {
        Snapshot {
            version: SNAPSHOT_VERSION,
            trained_at,
            trained_on: self.labels.len(),
            newest_label_at: self.labels.iter().map(|l| l.at).max().unwrap_or(0),
            gate: self.gate.clone(),
            scaler: Scaler {
                stats: self.scaler.stats.clone(),
            },
            model: self.model.clone(),
        }
    }
    pub(crate) fn train(labels: Vec<Label>) -> Self {
        let labels = teaching(labels);
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
            let (f, ctx) = (&labels[i].features, &labels[i].context);
            if let Some((cause, _, _)) = knn(f, ctx, &labels, &scaler, Some(i)) {
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
    pub(crate) fn answer(
        &self,
        f: &Features,
        ctx: &Context,
        candidates: &[Candidate],
    ) -> Option<Hypothesis> {
        let evidence_for = |cause: Cause| -> Vec<String> {
            candidates
                .iter()
                .find(|c| c.cause == cause)
                .map(|c| c.evidence.clone())
                .unwrap_or_default()
        };
        if self.gate.neighbours_on {
            if let Some((cause, confidence, like)) = knn(f, ctx, &self.labels, &self.scaler, None) {
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

/// The labels that teach: a can't-tell answer is kept in the store, out of here.
fn teaching(mut labels: Vec<Label>) -> Vec<Label> {
    labels.retain(|l| l.cause.is_known());
    labels
}

fn rules_answer(f: &Features) -> Option<Cause> {
    rules::candidates(f)
        .first()
        .filter(|c| c.score >= MIN_CONFIDENCE)
        .map(|c| c.cause)
}

/// Per-feature mean and spread over the labels, so a count of spikes and a share of
/// samples weigh alike in a distance.
#[derive(Clone, Debug, Serialize, Deserialize)]
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

    /// Root-mean-square standardised difference over the features both spikes have,
    /// each weighted by what its group means between their two contexts (a game's
    /// habits count little across games, Wine's layers not at all against a native game).
    fn distance(&self, a: &Features, ca: &Context, b: &Features, cb: &Context) -> Option<f32> {
        let (mut sum, mut weight) = (0.0f32, 0.0f32);
        for (k, &va) in a {
            let w = groups::group_of(k).map_or(1.0, |g| groups::weight(g, ca, cb));
            if w == 0.0 {
                continue;
            }
            let (Some(&vb), Some(za)) = (b.get(k), self.z(k, va)) else {
                continue;
            };
            let zb = self.z(k, vb)?;
            sum += w * (za - zb).powi(2);
            weight += w;
        }
        (weight >= MIN_SHARED as f32).then(|| (sum / weight).sqrt())
    }
}

/// The weighted vote of the nearest labelled spikes, if it is confident: close enough,
/// enough of them, and agreeing. `skip` leaves one label out (for grading).
fn knn(
    f: &Features,
    ctx: &Context,
    labels: &[Label],
    scaler: &Scaler,
    skip: Option<usize>,
) -> Option<(Cause, f32, Vec<String>)> {
    let mut near: Vec<(f32, &Label)> = labels
        .iter()
        .enumerate()
        .filter(|&(i, _)| Some(i) != skip)
        .filter_map(|(_, l)| Some((scaler.distance(f, ctx, &l.features, &l.context)?, l)))
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
#[derive(Clone, Debug, Serialize, Deserialize)]
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
        // A game's habits get ten times the shrinkage: the model is shared by every game
        // labelled, and should not hang a cause on one game's thread mix.
        let l2: Vec<f32> = m
            .keys
            .iter()
            .map(|k| match groups::group_of(k) {
                Some(FeatureGroup::Game) => L2 * 10.0,
                _ => L2,
            })
            .collect();
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
                for ((w, g), l2) in m.w[c].iter_mut().zip(&gw[c]).zip(&l2) {
                    *w -= RATE * (g / total + l2 * *w);
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
                    ("cpu_mhz_delta", f[3]),
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
            ("cpu_mhz_delta", 0.9),
        ]);
        let v = judge.judge(&probe, &Context::default());
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
            ("cpu_mhz_delta", 0.95),
        ]);
        let scaler = &judge.learned.scaler;
        let ctx = Context::default();
        assert!(knn(&between, &ctx, &judge.learned.labels, scaler, None).is_none());
        let h = judge.judge(&between, &ctx).hypothesis.unwrap();
        assert_eq!((h.cause, h.source), (Cause::Blocked, Source::Model));
    }

    #[test]
    fn a_saved_training_judges_like_the_one_it_came_from_and_counts_what_came_after() {
        use crate::{LabelStore, ModelStore};
        let dir = std::env::temp_dir().join(format!("beisl-retrain-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let labels = LabelStore::at(dir.join("labels.jsonl"));
        let model = ModelStore::at(dir.join("label-model.json"));
        let mut all = clusters(30, LabelSource::User);
        for l in &all {
            labels.append(l).unwrap();
        }

        // Labels but no training: the learned layers say nothing yet.
        let before = Judge::load(&labels, &model);
        assert!(!before.gate().neighbours_on && !before.gate().model_on);
        assert_eq!((before.trained_at(), before.untrained_labels()), (None, 30));

        let trained = Judge::retrain(&labels, &model).unwrap();
        let loaded = Judge::load(&labels, &model);
        assert_eq!(loaded.gate(), trained.gate());
        assert_eq!(loaded.untrained_labels(), 0);
        let probe = crate::tests::feats(&[
            ("ft_ratio_log", 4.0),
            ("burst_1s", 1.5),
            ("t_log_s", 3.0),
            ("cpu_mhz_delta", 0.95),
        ]);
        let ctx = Context::default();
        assert_eq!(loaded.judge(&probe, &ctx), trained.judge(&probe, &ctx));

        // A label after the training is counted as new, and already votes.
        let mut late = all.pop().unwrap();
        late.t_seconds = 999.0;
        late.at += 60;
        labels.append(&late).unwrap();
        assert_eq!(Judge::load(&labels, &model).untrained_labels(), 1);

        // A training from another format is ignored, not misread.
        let mut stale = model.load().unwrap();
        stale.version += 1;
        model.save(&stale).unwrap();
        assert!(model.load().is_none());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Two labelled spikes, one per cause, identical on the symptom and differing only on
    /// a game feature and an environment feature: which one a probe resembles must depend
    /// on whose game and stack it comes from.
    #[test]
    fn neighbours_weigh_game_and_environment_by_context() {
        let feats = |game_thread: f32, wine: f32| {
            crate::tests::feats(&[
                ("burst_1s", 1.0),
                ("ft_ratio_log", 3.0),
                ("repeat_len_log", 0.5),
                ("thread_render_share", game_thread),
                ("layer_wine_delta", wine),
            ])
        };
        let labelled = |cause, game: &str, maps_pe, game_thread, wine| {
            let mut l = label("run-1", 0.0, cause, LabelSource::User);
            l.features = feats(game_thread, wine);
            l.context = Context::new(game, Some(maps_pe));
            l
        };
        // Spread so every feature has a scale.
        let labels = vec![
            labelled(Cause::CpuWork, "Icarus", true, 0.9, 0.0),
            labelled(Cause::Blocked, "Icarus", true, 0.1, 0.8),
            labelled(Cause::CpuWork, "Icarus", true, 0.8, 0.1),
            labelled(Cause::Blocked, "Icarus", true, 0.2, 0.9),
        ];
        let scaler = Scaler::fit(&labels);
        let probe = feats(0.9, 0.85);
        let d = |ctx: &Context, l: &Label| {
            scaler
                .distance(&probe, ctx, &l.features, &l.context)
                .unwrap()
        };
        let same_game = Context::new("Icarus", Some(true));
        let other_game = Context::new("Saints Row", Some(true));
        let native = Context::new("vkcube", Some(false));
        let (work, blocked) = (&labels[0], &labels[1]);
        // The probe has the CpuWork spike's thread mix and the Blocked spike's Wine time.
        // In its own game the thread mix counts fully: nearest is CpuWork.
        assert!(d(&same_game, work) < d(&same_game, blocked));
        // From another game the thread mix fades to a quarter and the Wine layer decides.
        assert!(d(&other_game, blocked) < d(&other_game, work));
        // A native game is not compared on Wine's layer at all: against the CpuWork
        // spike only identical features remain.
        assert_eq!(d(&native, work), 0.0);
        assert!(d(&native, blocked) > 0.0);
    }

    #[test]
    fn a_cant_tell_answer_never_teaches() {
        let mut labels = clusters(30, LabelSource::User);
        let taught = Judge::new(labels.clone());
        for l in labels.iter_mut().take(10) {
            l.cause = Cause::Unknown;
        }
        let judge = Judge::new(labels);
        // Ten fewer labels to learn and grade on, and no "unknown" class to predict.
        assert_eq!(judge.gate().labels, 20);
        assert_eq!(taught.gate().labels, 30);
        let probe = crate::tests::feats(&[
            ("ft_ratio_log", 6.0),
            ("burst_1s", 3.0),
            ("t_log_s", 1.0),
            ("cpu_mhz_delta", 0.9),
        ]);
        let v = judge.judge(&probe, &Context::default());
        assert_ne!(v.hypothesis.map(|h| h.cause), Some(Cause::Unknown));
    }

    #[test]
    fn too_few_labels_leave_the_rules_alone() {
        let judge = Judge::new(clusters(4, LabelSource::User));
        assert!(!judge.gate().neighbours_on && !judge.gate().model_on);
    }
}
