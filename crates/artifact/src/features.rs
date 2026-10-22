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

use crate::{Frame, KeptFrames, SensorRow, SpikeWindow, BASELINE_S, MIN_SPLIT_SAMPLES, SENSORS};

pub type Features = BTreeMap<String, f32>;

/// How long after a frame the sensors still count as "during" it: MangoHud refreshes
/// them about every 0.57 s (measured 2026-10-22), so a reading taken within that of the
/// frame's end is the first that can reflect it.
const SENSOR_LAG_S: f64 = 0.6;

/// How far before and after a frame its level is read. Two seconds, not one: one long
/// neighbour then fills half the window, and two back-to-back hitches read as a
/// loading screen (SE2, 2026-10-22: 551 and 420 ms frames, then smooth play).
const LEVEL_S: f64 = 2.0;

/// Caps that keep a long quiet stretch from dominating a distance.
const SINCE_CAP_S: f32 = 60.0;
const LOG_TIME_CAP_S: f32 = 600.0;

/// Shape, phase and sensor features of frame `i`, from the CSV alone.
pub(crate) fn frame_features(kept: &KeptFrames, i: usize) -> Features {
    let (frames, threshold, spike_lengths) = (&kept.frames, kept.threshold, &kept.spike_lengths);
    let mut out = Features::new();
    let f = &frames[i];
    let base_from = first_at_or_after(frames, f.t_s - BASELINE_S);
    let base = median(frames[base_from..i].iter().map(|x| x.ms));
    let local = base.unwrap_or(f.ms).max(0.001);
    // How the seconds before ran against the whole log: a loading screen runs at a few
    // frames a second, and a 1.6 s frame there is a load step, not a hitch (Assetto
    // Corsa, 2026-10-22: 40-350 times the run median). Time-weighted on both sides, as
    // the log's is: CarX loads between bursts of sub-millisecond menu frames. Absent
    // with no second before.
    if let Some(b) = level(&frames[..i], f.t_s - LEVEL_S, f.t_s).filter(|_| kept.median_ms > 0.0) {
        out.insert(
            "base_level_log".into(),
            (b.max(0.001) / kept.median_ms).ln(),
        );
    }
    // How close to the end: a game's last frames are its shutdown.
    let end = f.t_s + f.ms as f64 / 1000.0;
    // And the seconds after: a load that starts from smooth play shows only there (Space
    // Engineers 2, 2026-10-22: a 3.3 s frame, a 1.5 s one, then a run at 67 ms).
    let after = level(&frames[i + 1..], end, end + LEVEL_S);
    if let Some(a) = after.filter(|_| kept.median_ms > 0.0) {
        out.insert(
            "next_level_log".into(),
            (a.max(0.001) / kept.median_ms).ln(),
        );
    }
    out.insert(
        "to_end_s".into(),
        ((kept.logged_seconds - end).max(0.0) as f32).min(SINCE_CAP_S),
    );
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

    sensor_features(frames, i, base_from, &kept.sensors, &kept.live, &mut out);
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
        // The first reading taken after the spike: the row closing a long frame still
        // carries the one from before it (CarX Street, 2026-10-22).
        let during = sensors
            .iter()
            .find(|r| r.frame > i && r.frame < during_to.max(i + 2))
            .and_then(|r| r.values[c]);
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
    if let Some((now, was)) = w.compile_pct.zip(w.baseline_compile_pct) {
        out.insert("compile_share".into(), now / 100.0);
        out.insert("compile_delta".into(), (now - was) / 100.0);
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

/// The frametime that held for most of `from..to`, each frame weighted by how much of
/// that stretch it covered: a long frame starting just before the end counts for the
/// moment it overlaps, not its whole length (Icarus, 2026-10-22: a 2.3 s freeze 20 ms
/// before the end of the window made a separate freeze before it read as loading).
fn level(frames: &[Frame], from: f64, to: f64) -> Option<f32> {
    let mut v: Vec<(f32, f64)> = frames
        .iter()
        .map(|x| {
            let end = x.t_s + x.ms as f64 / 1000.0;
            (x.ms, end.min(to) - x.t_s.max(from))
        })
        .filter(|&(_, w)| w > 0.0)
        .collect();
    v.sort_by(|a, b| a.0.total_cmp(&b.0));
    let half = v.iter().map(|&(_, w)| w).sum::<f64>() / 2.0;
    let mut acc = 0.0;
    v.into_iter().find_map(|(ms, w)| {
        acc += w;
        (acc >= half).then_some(ms)
    })
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
        let kept = KeptFrames::new(frames(&ms), 0, vec![]);
        let f = frame_features(&kept, 100);
        assert!((f["ft_ratio_log"] - 10.0f32.ln()).abs() < 1e-4);
        assert!(f["prev_ratio_log"].abs() < 1e-4);
        assert!((f["next_ratio_log"] - 2.5f32.ln()).abs() < 1e-4);
        // The 40 ms frame right after is a spike too, in the same second.
        assert_eq!(f["burst_1s"], 1.0);
        assert_eq!(f["since_prev_spike_s"], SINCE_CAP_S);
        // No other spike of its length.
        assert_eq!(f["repeat_len_log"], 0.0);
        // The seconds around it ran at the log's pace.
        assert!(f["base_level_log"].abs() < 1e-4 && f["next_level_log"].abs() < 1e-4);
    }

    #[test]
    fn a_load_from_smooth_play_shows_in_the_second_after() {
        // The SE2 shape: smooth play, a 3.3 s frame, 1.5 s, then 67 ms frames.
        let mut ms = vec![16.7; 2000];
        ms.extend([3300.0, 1500.0]);
        ms.extend(vec![67.0; 30]);
        let kept = KeptFrames::new(frames(&ms), 0, vec![]);
        let into = frame_features(&kept, 2000);
        assert!(into["base_level_log"].abs() < 1e-4);
        assert!((into["next_level_log"] - (1500.0f32 / 16.7).ln()).abs() < 1e-3);
        let during = frame_features(&kept, 2001);
        assert!((during["next_level_log"] - (67.0f32 / 16.7).ln()).abs() < 1e-3);
        // The last frame has no second after.
        let last = frame_features(&kept, ms.len() - 1);
        assert!(!last.contains_key("next_level_log"));
    }

    #[test]
    fn a_level_is_where_the_time_went_not_the_frame_count() {
        // CarX's shape: a minute of 20 ms play, then a load of 450 ms frames between
        // bursts of 0.4 ms menu frames. By count the load runs at 0.4 ms; by time, at 450.
        let mut ms = vec![20.0; 3000];
        for _ in 0..6 {
            ms.extend(vec![0.4; 200]);
            ms.push(450.0);
        }
        let spike = ms.len();
        ms.push(600.0);
        ms.extend(vec![20.0; 200]);
        let kept = KeptFrames::new(frames(&ms), 0, vec![]);
        assert_eq!(kept.median_ms, 20.0);
        let f = frame_features(&kept, spike);
        assert!((f["base_level_log"] - 22.5f32.ln()).abs() < 1e-3, "{f:?}");
    }

    #[test]
    fn a_frame_counts_for_the_time_it_overlaps() {
        // Icarus's shape: two freezes 2 s apart. The second starts just inside the
        // first's after-window and must not make that window read as frozen.
        let mut ms = vec![20.0; 3000];
        let first = ms.len();
        ms.push(2500.0);
        ms.extend(vec![20.0; 99]);
        ms.push(2300.0);
        ms.extend(vec![20.0; 100]);
        let kept = KeptFrames::new(frames(&ms), 0, vec![]);
        let f = frame_features(&kept, first);
        assert!(f["next_level_log"].abs() < 1e-3, "{f:?}");
    }

    #[test]
    fn a_repeated_length_is_what_throttling_looks_like() {
        let mut kept = KeptFrames::new(frames(&[16.0, 97.5]), 0, vec![]);
        kept.spike_lengths = vec![97.5, 97.6, 196.7, 196.9, 207.0, 664.8];
        let f = frame_features(&kept, 1);
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
        // MangoHud's placeholder row first (zeros before its first read), then real ones.
        let rows = vec![
            SensorRow {
                frame: 0,
                values: [Some(0.0); SENSORS.len()],
            },
            SensorRow {
                frame: 1,
                values: v,
            },
            SensorRow {
                frame: 101,
                values: w,
            },
        ];
        let kept = KeptFrames::new(fr, 0, rows);
        let f = frame_features(&kept, 100);
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
        let rows = vec![
            SensorRow {
                frame: 0,
                values: [Some(0.0); SENSORS.len()],
            },
            SensorRow {
                frame: 1,
                values: v,
            },
            SensorRow {
                frame: 151,
                values: w,
            },
        ];
        let kept = KeptFrames::new(fr, 0, rows);
        let mut f = frame_features(&kept, 150);
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
            compile_pct: Some(30.0),
            baseline_compile_pct: Some(2.0),
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
            compile_pct: None,
            baseline_compile_pct: None,
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
