//! Where a feature comes from, and what that means for comparing spikes.
//!
//! A labelled spike from another game, or from a Proton game when this one is native, is
//! still evidence, but not on every feature alike. The spike's own shape is the symptom
//! and compares anywhere; the machine's sensors describe the same hardware whatever runs
//! on it, so a hardware cause learned on one game transfers to the next; the game's own
//! threads and rhythm are its habits and count little elsewhere; the Wine and translation
//! layers exist only where they run, so a native game is not compared on them at all.
//!
//! Florian, 2026-10-22: "hardware related issues could transfer across games and will not
//! be submitted (in case the data will be shared sometime), gamedata will not be
//! considered with that much of a weight across games, environment ones could be filtered
//! when comparing native vs proton games." The GPU's readings got their own group the
//! same day, when labelling real traces showed games rendering on different cards of
//! one machine (an Arc A770 and an RX 6700 XT, beside an iGPU).

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FeatureGroup {
    /// The frame and its neighbours: length, bursts, repeats, whether the game ran.
    Spike,
    /// The machine's CPU-side sensors around the spike: CPU clock, load, temperature and
    /// power, RAM and swap.
    Hardware,
    /// The GPU's sensors around the spike. Apart from the rest of the machine because
    /// one machine can render on more than one card: what an Arc's readings mean is
    /// not what a Radeon's do.
    Gpu,
    /// The game's own threads, code and rhythm.
    Game,
    /// The stack between game and hardware: Wine, DXVK/vkd3d, driver, kernel, and the
    /// threads that belong to them.
    Environment,
}

impl FeatureGroup {
    /// Whether a feature of this group may ever leave the machine, should labels be
    /// shared one day. The machine's own readings never do.
    pub fn shareable(self) -> bool {
        !matches!(self, FeatureGroup::Hardware | FeatureGroup::Gpu)
    }
}

/// The group a feature belongs to, by its name as `artifact::features` writes it.
/// `None` for a name no group claims: a feature must be placed before it is used, and
/// the artifact crate tests that every one it writes is.
pub fn group_of(feature: &str) -> Option<FeatureGroup> {
    use FeatureGroup::*;
    const SPIKE: [&str; 9] = [
        "ft_ratio_log",
        "prev_ratio_log",
        "next_ratio_log",
        "burst_1s",
        "since_prev_spike_s",
        "repeat_len_log",
        "game_rate_ratio",
        "sample_rate_ratio",
        "t_log_s",
    ];
    if SPIKE.contains(&feature) {
        // t_log_s is how far into the session: loading at the start is the symptom's
        // context, not a habit of one game.
        return Some(Spike);
    }
    if feature == "game_base_rate_log" || feature == "layer_game_delta" {
        return Some(Game);
    }
    if feature == "compile_share" || feature == "compile_delta" {
        // The driver's compiler is the stack's, whichever thread the game lent it.
        return Some(Environment);
    }
    if let Some(class) = feature
        .strip_prefix("thread_")
        .and_then(|r| r.rsplit_once('_'))
        .map(|(class, _)| class)
    {
        return match class {
            "render" | "worker" | "audio" | "other" => Some(Game),
            "shader_compile" | "cache_io" | "runtime" | "observer" => Some(Environment),
            _ => None,
        };
    }
    if let Some(layer) = feature
        .strip_prefix("layer_")
        .and_then(|r| r.strip_suffix("_delta"))
    {
        return match layer {
            "wine" | "dxvk_vkd3d" | "driver" | "kernel" => Some(Environment),
            _ => None,
        };
    }
    let sensor = feature.strip_suffix("_delta")?;
    const SENSORS: [&str; 12] = [
        "cpu_load",
        "gpu_load",
        "cpu_mhz",
        "gpu_core_clock",
        "gpu_mem_clock",
        "cpu_temp",
        "gpu_temp",
        "cpu_power",
        "gpu_power",
        "ram_used",
        "swap_used",
        "gpu_vram_used",
    ];
    if !SENSORS.contains(&sensor) {
        return None;
    }
    Some(if sensor.starts_with("gpu_") {
        Gpu
    } else {
        Hardware
    })
}

/// What runs between the game and the hardware, as far as it matters for comparing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Env {
    /// The game's own process mapped no Windows code.
    Native,
    /// Wine, Proton, or any of their builds.
    Wine,
    #[default]
    Unknown,
}

/// Which game a spike is from, and through what: what a comparison between two spikes
/// is conditioned on. Labels keep theirs; a spike being judged brings its own.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Context {
    /// The game as the trace names it. `None` when unknown, which counts as "another
    /// game" for every comparison.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub game: Option<String>,
    #[serde(default)]
    pub env: Env,
    /// The card the game ran on, as MangoHud's CSV names it. Kept in local labels only:
    /// like every hardware fact, it never leaves the machine.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gpu: Option<String>,
}

impl Context {
    /// From what an artifact knows: the game's name and whether its process mapped
    /// Windows code (`game_maps_pe`).
    pub fn new(game: &str, maps_pe: Option<bool>) -> Self {
        let game = (!game.is_empty() && game != "unknown").then(|| game.to_string());
        let env = match maps_pe {
            Some(true) => Env::Wine,
            Some(false) => Env::Native,
            None => Env::Unknown,
        };
        Context {
            game,
            env,
            gpu: None,
        }
    }

    pub fn with_gpu(mut self, gpu: Option<String>) -> Self {
        self.gpu = gpu.filter(|g| !g.trim().is_empty());
        self
    }
}

/// Across another game, the game's own features count this much.
pub const OTHER_GAME: f32 = 0.25;
/// When either side's environment, or card, is unknown, its features count this much.
pub const UNSURE_ENV: f32 = 0.5;

/// How much a feature of `group` counts when comparing a spike from `a` with one from
/// `b`. Zero takes the feature out of the comparison.
pub fn weight(group: FeatureGroup, a: &Context, b: &Context) -> f32 {
    match group {
        FeatureGroup::Spike | FeatureGroup::Hardware => 1.0,
        FeatureGroup::Gpu => match (&a.gpu, &b.gpu) {
            (Some(x), Some(y)) if x == y => 1.0,
            (Some(_), Some(_)) => 0.0,
            _ => UNSURE_ENV,
        },
        FeatureGroup::Game => match (&a.game, &b.game) {
            (Some(x), Some(y)) if x == y => 1.0,
            _ => OTHER_GAME,
        },
        FeatureGroup::Environment => match (a.env, b.env) {
            (Env::Unknown, _) | (_, Env::Unknown) => UNSURE_ENV,
            (x, y) if x == y => 1.0,
            _ => 0.0,
        },
    }
}

/// `features` without anything that must stay on this machine: what a shared label
/// would carry. Nothing shares yet; this is the rule any export goes through.
pub fn shareable(features: &crate::Features) -> crate::Features {
    features
        .iter()
        .filter(|(k, _)| group_of(k).is_some_and(FeatureGroup::shareable))
        .map(|(k, v)| (k.clone(), *v))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_kind_of_feature_has_its_group() {
        use FeatureGroup::*;
        for (name, group) in [
            ("burst_1s", Spike),
            ("game_rate_ratio", Spike),
            ("t_log_s", Spike),
            ("cpu_mhz_delta", Hardware),
            ("ram_used_delta", Hardware),
            ("gpu_vram_used_delta", Gpu),
            ("gpu_temp_delta", Gpu),
            ("game_base_rate_log", Game),
            ("layer_game_delta", Game),
            ("thread_render_share", Game),
            ("thread_other_delta", Game),
            ("layer_dxvk_vkd3d_delta", Environment),
            ("layer_kernel_delta", Environment),
            ("thread_shader_compile_share", Environment),
            ("thread_cache_io_delta", Environment),
            ("thread_observer_share", Environment),
            ("compile_delta", Environment),
        ] {
            assert_eq!(group_of(name), Some(group), "{name}");
        }
        assert_eq!(group_of("thread_quantum_delta"), None);
        assert_eq!(group_of("layer_martian_delta"), None);
        assert_eq!(group_of("vibes"), None);
    }

    #[test]
    fn comparisons_weigh_groups_by_what_the_two_spikes_share() {
        let proton_a = Context::new("Icarus", Some(true));
        let proton_b = Context::new("Saints Row", Some(true));
        let native = Context::new("vkcube", Some(false));
        let unknown = Context::new("unknown", None);
        use FeatureGroup::*;
        // The symptom and the machine transfer anywhere.
        for g in [Spike, Hardware] {
            assert_eq!(weight(g, &proton_a, &native), 1.0);
        }
        // A game's habits count fully only within that game.
        assert_eq!(weight(Game, &proton_a, &proton_a), 1.0);
        assert_eq!(weight(Game, &proton_a, &proton_b), OTHER_GAME);
        assert_eq!(weight(Game, &unknown, &unknown), OTHER_GAME);
        // Wine's layers mean nothing to a native game: filtered out, not just lowered.
        assert_eq!(weight(Environment, &proton_a, &proton_b), 1.0);
        assert_eq!(weight(Environment, &proton_a, &native), 0.0);
        assert_eq!(weight(Environment, &native, &unknown), UNSURE_ENV);
        // A GPU's readings compare only on the same card: the CPU's stay machine-wide.
        let arc = proton_a
            .clone()
            .with_gpu(Some("Intel(R) Arc(tm) A770 Graphics (DG2)".into()));
        let radeon = proton_a
            .clone()
            .with_gpu(Some("AMD Radeon RX 6700 XT (RADV NAVI22)".into()));
        assert_eq!(weight(Gpu, &arc, &arc), 1.0);
        assert_eq!(weight(Gpu, &arc, &radeon), 0.0);
        assert_eq!(weight(Gpu, &arc, &proton_b), UNSURE_ENV);
        assert_eq!(weight(Hardware, &arc, &radeon), 1.0);
    }

    #[test]
    fn the_machines_readings_never_go_out() {
        let f: crate::Features = [
            ("burst_1s", 2.0),
            ("cpu_mhz_delta", -0.3),
            ("gpu_temp_delta", 4.0),
            ("layer_wine_delta", 0.1),
            ("thread_render_share", 0.4),
            ("not_a_feature", 1.0),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect();
        let out: Vec<String> = shareable(&f).into_keys().collect();
        assert_eq!(out, ["burst_1s", "layer_wine_delta", "thread_render_share"]);
    }
}
