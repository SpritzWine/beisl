//! The hand-written layer: what beisl can say before anyone has labelled anything.
//!
//! Every rule reads features that compare the spike frame with the second before it, and
//! every score is a prior, not a measured probability: the learned layers exist to beat
//! these numbers on the user's own spikes. A candidate always carries the measurements
//! it rests on, which is what the checklist shows.

use crate::{Candidate, Cause, Features};

/// The game's sample rate fell to at most this share of the second before: it was not
/// running (degraded mode sees only its own process, so this is the clearest signal).
const STARVED: f32 = 0.25;
/// At least this share: it kept running.
const RUNNING: f32 = 0.75;

pub(crate) fn candidates(f: &Features) -> Vec<Candidate> {
    let get = |k: &str| f.get(k).copied();
    let mut out: Vec<Candidate> = Vec::new();
    let mut add = |cause, score, evidence: Vec<String>| {
        if let Some(c) = out.iter_mut().find(|c| c.cause == cause) {
            if score > c.score {
                c.score = score;
            }
            c.evidence.extend(evidence);
        } else {
            out.push(Candidate {
                cause,
                score,
                evidence,
            });
        }
    };

    let rate = get("game_rate_ratio");
    let starved = rate.is_some_and(|r| r <= STARVED);
    let running = rate.is_some_and(|r| r >= RUNNING);
    let rate_line = rate.map(|r| {
        format!(
            "the game ran at {:.0}% of its sample rate in the second before",
            r * 100.0
        )
    });
    let burst = get("burst_1s");
    let repeats = get("repeat_len_log").map(|l| l.exp_m1().round());

    // By symbol first: it sees compilation on any thread, the game's own included.
    if let Some(d) = get("compile_delta").filter(|&d| d >= 0.2) {
        let share = get("compile_share").unwrap_or(d);
        add(
            Cause::ShaderCompile,
            0.8,
            vec![format!(
                "the driver's shader compiler took {:.0}% of the frame's samples, {:.0} \
                 points more than before",
                share * 100.0,
                d * 100.0
            )],
        );
    }
    if let Some(d) = get("thread_shader_compile_delta").filter(|&d| d >= 0.2) {
        let share = get("thread_shader_compile_share").unwrap_or(d);
        add(
            Cause::ShaderCompile,
            0.8,
            vec![format!(
                "shader-compile threads took {:.0}% of the frame's samples, {:.0} points more \
                 than before",
                share * 100.0,
                d * 100.0
            )],
        );
    } else if let Some(s) = get("thread_shader_compile_share").filter(|&s| s >= 0.3) {
        add(
            Cause::ShaderCompile,
            0.6,
            vec![format!(
                "shader-compile threads took {:.0}% of the frame's samples",
                s * 100.0
            )],
        );
    }

    if let Some(d) = get("cpu_mhz_delta").filter(|&d| d <= -0.15) {
        add(
            Cause::Throttle,
            0.7,
            vec![format!("the CPU clock fell {:.0}%", -d * 100.0)],
        );
    }

    if starved {
        let mut why: Vec<String> = rate_line.iter().cloned().collect();
        match (burst, repeats) {
            (Some(b), Some(r)) if b >= 2.0 && r >= 2.0 => {
                why.push(format!(
                    "{b:.0} more spikes within a second, {r:.0} others of the same length: \
                     a repeating cap, not random interference"
                ));
                add(Cause::Throttle, 0.6, why);
            }
            (Some(b), _) if b >= 2.0 => {
                why.push(format!(
                    "{b:.0} more spikes within a second, of varying length"
                ));
                add(Cause::CpuContention, 0.55, why);
            }
            (Some(b), _) if b < 0.5 => {
                why.push("no other spike within a second: one isolated stall".to_string());
                add(Cause::Blocked, 0.55, why);
            }
            _ => {}
        }
        if let Some(g) = get("gpu_load_delta").filter(|&g| g >= 15.0) {
            add(
                Cause::GpuBound,
                0.5,
                vec![format!(
                    "GPU load rose {g:.0} points while the game's CPU side waited"
                )],
            );
        }
    }

    let paging = ["swap_used_delta", "ram_used_delta"]
        .into_iter()
        .filter_map(|k| get(k).filter(|&d| d >= 0.05).map(|d| (k, d)))
        .map(|(k, d)| {
            format!(
                "{} grew {:.0}%",
                k.trim_end_matches("_delta").replace('_', " "),
                d * 100.0
            )
        })
        .collect::<Vec<_>>();
    if !paging.is_empty() {
        add(Cause::MemoryPressure, 0.5, paging);
    }

    // The game's own code has to be what grew: extra samples alone can be the driver's
    // compiler working on the game's threads (a real Saints Row trace, 2026-10-22).
    let compiling = get("compile_delta").is_some_and(|d| d >= 0.2);
    if running && !compiling {
        if let Some(d) = get("layer_game_delta").filter(|&d| d >= 0.1) {
            let mut why: Vec<String> = rate_line.iter().cloned().collect();
            why.push(format!(
                "the game's own code took {:.0} points more",
                d * 100.0
            ));
            add(Cause::CpuWork, 0.5, why);
        }
    }

    out.sort_by(|a, b| b.score.total_cmp(&a.score).then(a.cause.cmp(&b.cause)));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::feats;

    fn top(f: &Features) -> Option<Cause> {
        candidates(f).first().map(|c| c.cause)
    }

    /// The three synthetic runs of 2026-10-22, by their median features.
    #[test]
    fn the_harness_runs_read_as_what_was_done_to_them() {
        let hogs = feats(&[
            ("game_rate_ratio", 0.01),
            ("burst_1s", 5.0),
            ("repeat_len_log", 0.0),
        ]);
        let quota = feats(&[
            ("game_rate_ratio", 0.0),
            ("burst_1s", 13.0),
            ("repeat_len_log", 1.1),
        ]);
        let freeze = feats(&[
            ("game_rate_ratio", 0.0),
            ("burst_1s", 0.0),
            ("repeat_len_log", 1.1),
        ]);
        assert_eq!(top(&hogs), Some(Cause::CpuContention));
        assert_eq!(top(&quota), Some(Cause::Throttle));
        assert_eq!(top(&freeze), Some(Cause::Blocked));
    }

    #[test]
    fn shader_compile_threads_outrank_the_starved_reading() {
        let f = feats(&[
            ("thread_shader_compile_delta", 0.65),
            ("thread_shader_compile_share", 0.7),
            ("game_rate_ratio", 0.1),
            ("burst_1s", 0.0),
        ]);
        let c = candidates(&f);
        assert_eq!(c[0].cause, Cause::ShaderCompile);
        assert!(c[0].evidence[0].contains("70%"));
        // The other reading stays on the checklist.
        assert!(c.iter().any(|x| x.cause == Cause::Blocked));
    }

    /// Saints Row, 2026-10-22: the game's threads stayed busy, 92-96% of it in RADV's
    /// compiler. Busy is not the game's own work.
    #[test]
    fn compiling_on_the_games_threads_is_compile_not_cpu_work() {
        let f = feats(&[
            ("game_rate_ratio", 1.2),
            ("sample_rate_ratio", 1.4),
            ("compile_share", 0.94),
            ("compile_delta", 0.9),
            ("layer_game_delta", -0.3),
            ("burst_1s", 4.0),
        ]);
        let c = candidates(&f);
        assert_eq!(c[0].cause, Cause::ShaderCompile);
        assert!(c[0].evidence[0].contains("94%"));
        assert!(!c.iter().any(|x| x.cause == Cause::CpuWork));
    }

    #[test]
    fn a_game_that_kept_running_is_not_called_starved() {
        let f = feats(&[
            ("game_rate_ratio", 1.1),
            ("layer_game_delta", 0.2),
            ("burst_1s", 0.0),
        ]);
        let c = candidates(&f);
        assert_eq!(c[0].cause, Cause::CpuWork);
        assert!(!c.iter().any(|x| x.cause == Cause::Blocked));
    }
}
