//! A spike as numbers: the measurements a cause can be read from, one named value each.
//!
//! Each value compares the spike frame with the second before it, so a game that always
//! leans on the driver doesn't read as driver stutter. A feature that cannot be measured
//! for this spike (no perf window, too few samples, a CSV column that never moves) is left
//! out of the map, never filled with a zero that would read as a measurement.
//!
//! Names are part of the on-disk format: labels a user gave months ago are stored with
//! these keys, so rename one only together with a migration.

use std::collections::BTreeMap;

use crate::{Frame, SensorRow, SpikeWindow, BASELINE_S, MIN_SPLIT_SAMPLES, SENSORS};

pub type Features = BTreeMap<String, f32>;

/// How long after a frame the sensors still count as "during" it: MangoHud refreshes
/// them about every 0.57 s (measured 2026-10-22), so a reading taken within that of the
/// frame's end is the first that can reflect it.
const SENSOR_LAG_S: f64 = 0.6;

/// Caps that keep a long quiet stretch from dominating a distance.
const SINCE_CAP_S: f32 = 60.0;
const LOG_TIME_CAP_S: f32 = 600.0;

/// Shape and sensor features of `frames[i]`, from the CSV alone. `spike_lengths` holds
/// every frame of the run at or over the spike threshold, sorted, for the repetition
/// count.
pub(crate) fn frame_features(
    frames: &[Frame],
    i: usize,
    threshold: f32,
    spike_lengths: &[f32],
    sensors: &[SensorRow],
    live: &[bool],
) -> Features {
    let mut out = Features::new();
    let f = &frames[i];
    let base_from = first_at_or_after(frames, f.t_s - BASELINE_S);
    let local = median(frames[base_from..i].iter().map(|x| x.ms)).unwrap_or(f.ms);
    let local = local.max(0.001);
    // Logs, here and for the neighbours: a fast game's sub-millisecond frames put a
    // spike thousands of times over its baseline, a 60 fps one a few times.
    out.insert("ft_ratio_log".into(), (f.ms / local).ln());
    if let Some(p) = i.checked_sub(1).and_then(|j| frames.get(j)) {
        out.insert("prev_ratio_log".into(), (p.ms / local).ln());
    }
    if let Some(n) = frames.get(i + 1) {
        out.insert("next_ratio_log".into(), (n.ms / local).ln());
    }
    let around_to = first_at_or_after(frames, f.t_s + 1.0);
    let burst = frames[base_from..around_to]
        .iter()
        .enumerate()
        .filter(|&(k, x)| base_from + k != i && x.ms >= threshold)
        .count();
    out.insert("burst_1s".into(), burst as f32);
    let since = frames[..i]
        .iter()
        .rev()
        .find(|x| x.ms >= threshold)
        .map_or(SINCE_CAP_S, |p| ((f.t_s - p.t_s) as f32).min(SINCE_CAP_S));
    out.insert("since_prev_spike_s".into(), since);
    out.insert("t_log_s".into(), (f.t_s as f32).min(LOG_TIME_CAP_S));
    // Throttling by a cgroup quota repeats the same frame length; contention does not
    // (measured 2026-10-22: 97.5/196.7 ms over and over against 207..665 ms scattered).
    // A count, not a share: a run with many spikes would dilute a share of repeats.
    let tol = (f.ms * 0.01).max(1.0);
    let same = count_within(spike_lengths, f.ms - tol, f.ms + tol).saturating_sub(1);
    out.insert("repeat_len_log".into(), (same as f32).ln_1p());

    sensor_features(frames, i, base_from, sensors, live, &mut out);
    out
}

/// Each live sensor's change from the second before the frame to the readings just
/// after it. Loads and temperatures as plain differences, everything else relative.
fn sensor_features(
    frames: &[Frame],
    i: usize,
    base_from: usize,
    sensors: &[SensorRow],
    live: &[bool],
    out: &mut Features,
) {
    let f = &frames[i];
    let during_to = first_at_or_after(frames, f.t_s + f.ms as f64 / 1000.0 + SENSOR_LAG_S);
    for (c, name) in SENSORS.iter().enumerate() {
        if !live.get(c).copied().unwrap_or(false) {
            continue;
        }
        let mean_over = |from: usize, to: usize| {
            mean(
                sensors
                    .iter()
                    .filter(|r| r.frame >= from && r.frame < to)
                    .filter_map(|r| r.values[c]),
            )
        };
        // The reading in force at the window's start counts for it too: sensors change
        // far less often than frames, so a second may hold none of its own.
        let at = |frame: usize| {
            sensors
                .iter()
                .rev()
                .find(|r| r.frame <= frame)
                .and_then(|r| r.values[c])
        };
        let base = mean_over(base_from, i).or_else(|| at(base_from));
        let during = mean_over(i, during_to.max(i + 1)).or_else(|| at(i));
        let (Some(base), Some(during)) = (base, during) else {
            continue;
        };
        let absolute = name.ends_with("_load") || name.ends_with("_temp");
        let delta = if absolute {
            during - base
        } else if base > 0.0 {
            (during - base) / base
        } else {
            continue;
        };
        out.insert(format!("{name}_delta"), delta);
    }
}

/// Features from the perf window: the game's sample rate, and how the layers and thread
/// classes moved. Each behind the same sample gates the report uses.
pub(crate) fn window_features(w: &SpikeWindow, frame_s: f64, out: &mut Features) {
    out.insert(
        "game_base_rate_log".into(),
        ((w.baseline_game_samples as f64 / BASELINE_S).ln_1p()) as f32,
    );
    if let Some((now, was)) = w.game_rates(frame_s) {
        out.insert("game_rate_ratio".into(), (now / was.max(1.0)) as f32);
    }
    let was = w.baseline_samples as f64 / BASELINE_S;
    if (was * frame_s).max(w.samples as f64) >= MIN_SPLIT_SAMPLES as f64 && frame_s > 0.0 {
        let now = w.samples as f64 / frame_s;
        out.insert("sample_rate_ratio".into(), (now / was.max(1.0)) as f32);
    }
    for l in &w.layers {
        out.insert(
            format!("layer_{}_delta", key(&l.layer)),
            (l.pct - l.baseline_pct) / 100.0,
        );
    }
    for t in &w.threads {
        let k = key(&t.class);
        out.insert(format!("thread_{k}_share"), t.pct / 100.0);
        out.insert(
            format!("thread_{k}_delta"),
            (t.pct - t.baseline_pct) / 100.0,
        );
    }
    // Present with the thread split, behind the same gate.
    if !w.threads.is_empty() {
        out.insert("compile_share".into(), w.compile_pct / 100.0);
        out.insert(
            "compile_delta".into(),
            (w.compile_pct - w.baseline_compile_pct) / 100.0,
        );
    }
}

/// A layer or class name as a feature key: `dxvk/vkd3d` -> `dxvk_vkd3d`.
fn key(name: &str) -> String {
    name.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect()
}

fn first_at_or_after(frames: &[Frame], t: f64) -> usize {
    frames.partition_point(|f| f.t_s < t)
}

fn count_within(sorted: &[f32], lo: f32, hi: f32) -> usize {
    sorted.partition_point(|&x| x <= hi) - sorted.partition_point(|&x| x < lo)
}

fn median(values: impl Iterator<Item = f32>) -> Option<f32> {
    let mut v: Vec<f32> = values.collect();
    if v.is_empty() {
        return None;
    }
    v.sort_by(|a, b| a.total_cmp(b));
    Some(v[v.len() / 2])
}

fn mean(values: impl Iterator<Item = f32>) -> Option<f32> {
    let (sum, n) = values.fold((0.0f64, 0u32), |(s, n), x| (s + x as f64, n + 1));
    (n > 0).then(|| (sum / n as f64) as f32)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ThreadShare, WindowShare};

    fn frames(ms: &[f32]) -> Vec<Frame> {
        let mut t = 0.0;
        ms.iter()
            .map(|&ms| {
                let f = Frame {
                    t_s: t,
                    ms,
                    before_csv_end_s: None,
                };
                t += ms as f64 / 1000.0;
                f
            })
            .collect()
    }

    #[test]
    fn shape_compares_the_frame_with_the_second_before_it() {
        // 100 frames of 16 ms, a 160 ms spike, a 40 ms frame, then 16 ms again.
        let mut ms = vec![16.0; 100];
        ms.extend([160.0, 40.0]);
        ms.extend(vec![16.0; 20]);
        let fr = frames(&ms);
        let lengths = [40.0, 160.0];
        let f = frame_features(&fr, 100, 33.0, &lengths, &[], &[]);
        assert!((f["ft_ratio_log"] - 10.0f32.ln()).abs() < 1e-4);
        assert!(f["prev_ratio_log"].abs() < 1e-4);
        assert!((f["next_ratio_log"] - 2.5f32.ln()).abs() < 1e-4);
        // The 40 ms frame right after is a spike too, in the same second.
        assert_eq!(f["burst_1s"], 1.0);
        assert_eq!(f["since_prev_spike_s"], SINCE_CAP_S);
        // No other spike of its length.
        assert_eq!(f["repeat_len_log"], 0.0);
    }

    #[test]
    fn a_repeated_length_is_what_throttling_looks_like() {
        let lengths = [97.5, 97.6, 196.7, 196.9, 207.0, 664.8];
        let fr = frames(&[16.0, 97.5]);
        let f = frame_features(&fr, 1, 33.0, &lengths, &[], &[]);
        // One of the five other spikes has its length.
        assert!((f["repeat_len_log"] - 1.0f32.ln_1p()).abs() < 1e-6);
    }

    #[test]
    fn sensors_count_only_when_live_and_compare_like_with_like() {
        let fr = frames(&vec![16.0; 200]);
        // cpu_mhz drops from 5000 to 4000 at the spike (frame 100); gpu_load rises.
        let idx = |n: &str| SENSORS.iter().position(|s| *s == n).unwrap();
        let mut v = [None; SENSORS.len()];
        v[idx("cpu_mhz")] = Some(5000.0);
        v[idx("gpu_load")] = Some(40.0);
        let mut w = v;
        w[idx("cpu_mhz")] = Some(4000.0);
        w[idx("gpu_load")] = Some(90.0);
        let rows = [
            SensorRow {
                frame: 0,
                values: v,
            },
            SensorRow {
                frame: 101,
                values: w,
            },
        ];
        let mut live = [false; SENSORS.len()];
        live[idx("cpu_mhz")] = true;
        live[idx("gpu_load")] = true;
        let f = frame_features(&fr, 100, 33.0, &[], &rows, &live);
        assert!(
            (f["cpu_mhz_delta"] + 0.2).abs() < 1e-6,
            "relative for clocks"
        );
        assert!(
            (f["gpu_load_delta"] - 50.0).abs() < 1e-6,
            "points for loads"
        );
        // A column that never moves in the run says nothing, so it is absent.
        assert!(!f.contains_key("gpu_temp_delta"));
    }

    /// The hypothesis crate weighs each feature by its group; one it cannot place would
    /// be compared at full weight everywhere and could leave the machine if shared.
    #[test]
    fn every_feature_written_has_a_group() {
        let fr = frames(&vec![16.0; 300]);
        let mut v = [None; SENSORS.len()];
        let mut w = [None; SENSORS.len()];
        for c in 0..SENSORS.len() {
            v[c] = Some(10.0);
            w[c] = Some(20.0);
        }
        let rows = [
            SensorRow {
                frame: 0,
                values: v,
            },
            SensorRow {
                frame: 151,
                values: w,
            },
        ];
        let mut f = frame_features(&fr, 150, 33.0, &[], &rows, &[true; SENSORS.len()]);
        let shares = |names: &[&str]| {
            names
                .iter()
                .map(|n| (n.to_string(), 10.0, 5.0))
                .collect::<Vec<_>>()
        };
        let window = SpikeWindow {
            samples: 500,
            baseline_samples: 500,
            game_samples: 400,
            baseline_game_samples: 400,
            layers: shares(&["kernel", "driver", "dxvk/vkd3d", "wine", "game"])
                .into_iter()
                .map(|(layer, pct, baseline_pct)| WindowShare {
                    layer,
                    pct,
                    baseline_pct,
                })
                .collect(),
            threads: shares(&[
                "shader-compile",
                "render",
                "cache-io",
                "worker",
                "audio",
                "runtime",
                "observer",
                "other",
            ])
            .into_iter()
            .map(|(class, pct, baseline_pct)| ThreadShare {
                class,
                pct,
                baseline_pct,
            })
            .collect(),
            compile_pct: 30.0,
            baseline_compile_pct: 2.0,
        };
        window_features(&window, 0.1, &mut f);
        assert!(f.len() > 40, "only {} features written", f.len());
        for k in f.keys() {
            assert!(
                hypothesis::groups::group_of(k).is_some(),
                "{k} has no group"
            );
        }
    }

    #[test]
    fn window_features_follow_the_report_gates() {
        let w = SpikeWindow {
            samples: 30,
            baseline_samples: 600,
            game_samples: 1,
            baseline_game_samples: 480,
            layers: vec![WindowShare {
                layer: "dxvk/vkd3d".into(),
                pct: 61.0,
                baseline_pct: 22.0,
            }],
            threads: vec![ThreadShare {
                class: "shader-compile".into(),
                pct: 70.0,
                baseline_pct: 5.0,
            }],
            compile_pct: 0.0,
            baseline_compile_pct: 0.0,
        };
        let mut f = Features::new();
        window_features(&w, 0.05, &mut f);
        assert!((f["game_rate_ratio"] - 20.0 / 480.0).abs() < 1e-4);
        assert!((f["layer_dxvk_vkd3d_delta"] - 0.39).abs() < 1e-6);
        assert!((f["thread_shader_compile_delta"] - 0.65).abs() < 1e-6);
        // 2 game samples expected in 5 ms at 480/s: too few to compare, so absent.
        let mut g = Features::new();
        window_features(&w, 0.005, &mut g);
        assert!(!g.contains_key("game_rate_ratio"));
        assert!(g.contains_key("game_base_rate_log"));
    }
}
