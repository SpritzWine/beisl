//! The Beisl trace artifact — the single, self-contained file that merges the MangoHud
//! frametime *side-channel* (a spike index) with the *symbolicated* perf samples.
//!
//! It is what the GUI opens, what the MCP server serves to Claude, and what a reviewer
//! exports to hand us a structured perf report instead of "it feels slow." Being
//! self-describing (it carries its own fidelity tier and wine flavor) keeps
//! SpritzWine-vs-Proton A/Bs honest.
//!
//! v0 serializes to JSON: the aggregated *top* symbols and the worst spikes, not the raw
//! sample stream, so the file is small and every consumer (GUI, MCP, a human `cat`) reads
//! it without re-running `perf`. [`TraceArtifact::build`] does the merge — resolve the
//! `perf.data` (via `symbol-resolver`) and read the frametime CSV — so the wrapper's
//! post-run step and `beisl symbols` share one code path.

use std::io;
use std::path::Path;

use serde::{Deserialize, Serialize};
use symbol_resolver::{FidelityTier, Layer, Resolution};

/// Bump when the on-disk shape changes incompatibly; consumers check it before trusting.
pub const SCHEMA_VERSION: u32 = 1;

/// A low-performance peak from the MangoHud frametime CSV — a bookmark ("here's where it
/// hitched"). `t_seconds` is measured from the start of the logged window (cumulative
/// frametimes, so it's independent of the CSV's absolute-clock column).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Spike {
    pub t_seconds: f64,
    pub frametime_ms: f32,
}

/// Session-wide frametime health, from the CSV.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FrameStats {
    pub frames: u64,
    pub avg_fps: f32,
    pub median_frametime_ms: f32,
    /// 99th-percentile frametime — the "1% low" felt as stutter.
    pub p99_frametime_ms: f32,
    pub worst_frametime_ms: f32,
    /// Seconds the logged window spans (sum of frametimes).
    pub logged_seconds: f64,
}

/// One hot symbol with its share of samples precomputed (so consumers don't need the total).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HotSymbol {
    pub module: String,
    pub symbol: String,
    pub pct: f32,
}

/// One layer's slice of the "who ate the frame" split, biggest first. `layer` is one of
/// `kernel/driver/dxvk/vkd3d/wine/game`; `pct` is its share of the *attributed* samples
/// (see [`TraceArtifact::attributed_pct`]). `symbol_pct` is the layer's own symbol coverage
/// — how many of *its* samples we could name a function for — so a big layer with a low
/// `symbol_pct` flags where debuginfo / the Full tier would reveal what's inside.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LayerShare {
    pub layer: String,
    pub pct: f32,
    #[serde(default)]
    pub symbol_pct: f32,
}

/// The self-describing "issues" summary — what a UI issues panel binds to.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Diagnostics {
    pub fidelity: String,
    pub total_samples: u64,
    pub unresolved_pct: f32,
    /// `(module, pct)` we couldn't name — ELF wants debuginfo, a PE wants the Full tier.
    pub anonymous_modules: Vec<(String, f32)>,
}

/// The merged, exportable trace.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TraceArtifact {
    pub schema_version: u32,
    pub game: String,
    /// e.g. `SpritzWine-Prater`, `Proton - Experimental`, or `unknown`.
    pub wine_flavor: String,
    /// GPU vendor of the card that ran (`intel`/`amd`/`nvidia`/`qualcomm`), for vendor-colored
    /// UI. Empty string when undetected. Defaulted so pre-vendor artifacts still load.
    #[serde(default)]
    pub gpu_vendor: String,
    pub duration_s: f64,
    /// Present when a MangoHud CSV was logged; absent otherwise.
    pub frame_stats: Option<FrameStats>,
    /// Worst frametime spikes, worst first (bookmarks into the timeline).
    pub spikes: Vec<Spike>,
    /// A downsampled frametime timeline for the chart — `(t_seconds, frametime_ms)`, capped
    /// at a few hundred points (max-per-bucket, so hitches survive the downsample) to keep
    /// the artifact small. Empty when no MangoHud CSV was logged. Defaulted so pre-timeline
    /// artifacts still load.
    #[serde(default)]
    pub timeline: Vec<(f64, f32)>,
    /// The layer split — where CPU went across the stack (kernel/driver/dxvk/wine/game),
    /// biggest first. This is what the headline "who ate the frame" bar renders. The shares
    /// are over the *attributed* samples only (see `attributed_pct`), not the whole trace:
    /// the unmapped `[unknown]` samples are a measurement gap, not a stack layer, so folding
    /// them in as a peer bucket would both shrink the real layers and present the gap as if
    /// it were an answer. They're disclosed via `attributed_pct` instead — never redistributed
    /// into a named layer (that could be graphics/driver code we simply couldn't place).
    /// Defaulted so an artifact written before this field still deserializes (schema stays v1:
    /// the addition is read-compatible).
    #[serde(default)]
    pub layers: Vec<LayerShare>,
    /// Share of all samples that landed in *some* layer (100 − the unmapped `[unknown]`
    /// fraction). The `layers` shares are normalized over this, so the coverage caveat is
    /// carried explicitly rather than baked into the bar.
    #[serde(default)]
    pub attributed_pct: f32,
    /// Hottest resolved wine/game functions by self time, biggest first.
    pub hot_symbols: Vec<HotSymbol>,
    pub diagnostics: Diagnostics,
}

/// How many top symbols / worst spikes to retain (keeps the file small and the report
/// readable; the raw perf.data stays in the run dir for a deeper look).
const TOP_SYMBOLS: usize = 25;
const TOP_SPIKES: usize = 10;

impl TraceArtifact {
    /// Build the artifact for a trace run directory: resolve its `perf.data` and fold in
    /// the frametime CSV if one is present. `game` and `duration_s` come from the wrapper
    /// (the session); the wine flavor is inferred during resolution.
    pub fn build(run_dir: &Path, game: &str, duration_s: f64) -> io::Result<Self> {
        let res = symbol_resolver::resolve_perf_data(&run_dir.join("perf.data"))?;
        let (frame_stats, spikes, timeline) = find_csv(run_dir)
            .and_then(|csv| frames_from_csv(&csv))
            .map(|(stats, spikes, timeline)| (Some(stats), spikes, timeline))
            .unwrap_or((None, Vec::new(), Vec::new()));
        Ok(Self::from_parts(
            game,
            duration_s,
            res,
            frame_stats,
            spikes,
            timeline,
        ))
    }

    fn from_parts(
        game: &str,
        duration_s: f64,
        res: Resolution,
        frame_stats: Option<FrameStats>,
        spikes: Vec<Spike>,
        timeline: Vec<(f64, f32)>,
    ) -> Self {
        let total = res.diagnostics.total_samples.max(1) as f32;
        // ALL resolved functions, not just wine `.dll`s: a native game's ELF code, the
        // game's own `.exe`, or a mesa/DXVK `.so` (when its debuginfo is present) are
        // equally "who ate the frame". Filtering to `.dll` would bias the artifact toward
        // a wine-is-the-culprit story — the opposite of DESIGN's "it's the game/driver, not
        // wine, is a first-class result." `res.symbols` is already sorted by self time.
        let hot_symbols = res
            .symbols
            .iter()
            .take(TOP_SYMBOLS)
            .map(|s| HotSymbol {
                module: s.module.clone(),
                symbol: s.symbol.clone(),
                pct: 100.0 * s.samples as f32 / total,
            })
            .collect();
        let anonymous_modules = res
            .diagnostics
            .anonymous_modules
            .iter()
            .map(|(m, n)| (m.clone(), 100.0 * *n as f32 / total))
            .collect();
        // The resolver bucketed every sample, including the unmapped `[unknown]` ones it
        // parks in `Layer::Other`. That's a measurement gap, not a stack layer: normalize the
        // real layers over the *attributed* samples (total − gap) so driver-vs-wine-vs-game is
        // a fair comparison, and carry the gap separately as `attributed_pct`. Resolver order
        // (biggest first) is monotonic under this scaling, so it's preserved.
        let gap: u64 = res
            .layers
            .iter()
            .find(|t| t.layer == Layer::Other)
            .map_or(0, |t| t.samples);
        let attributed = res.diagnostics.total_samples.saturating_sub(gap);
        let layers = if attributed == 0 {
            Vec::new()
        } else {
            res.layers
                .iter()
                .filter(|t| t.layer != Layer::Other)
                .map(|t| LayerShare {
                    layer: t.layer.as_str().to_string(),
                    pct: 100.0 * t.samples as f32 / attributed as f32,
                    // The layer's own symbol coverage: named / placed-in-layer.
                    symbol_pct: if t.samples == 0 {
                        0.0
                    } else {
                        100.0 * t.named as f32 / t.samples as f32
                    },
                })
                .collect()
        };
        let attributed_pct = if res.diagnostics.total_samples == 0 {
            0.0
        } else {
            100.0 * attributed as f32 / res.diagnostics.total_samples as f32
        };
        TraceArtifact {
            schema_version: SCHEMA_VERSION,
            game: game.to_string(),
            wine_flavor: res.wine_flavor.unwrap_or_else(|| "unknown".to_string()),
            gpu_vendor: res.gpu_vendor.unwrap_or_default(),
            duration_s,
            frame_stats,
            spikes,
            timeline,
            layers,
            attributed_pct,
            hot_symbols,
            diagnostics: Diagnostics {
                fidelity: res.diagnostics.tier.as_str().to_string(),
                total_samples: res.diagnostics.total_samples,
                unresolved_pct: res.diagnostics.unresolved_pct() as f32,
                anonymous_modules,
            },
        }
    }

    /// Serialize to `path` as pretty JSON.
    pub fn write_json(&self, path: &Path) -> io::Result<()> {
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        std::fs::write(path, json)
    }

    /// Read a previously written `artifact.json`. Keeps deserialization inside this crate so
    /// consumers (the GUI, later the trace library) bind to the type without pulling serde
    /// themselves — the same reason `write_json` lives here.
    pub fn read_json(path: &Path) -> io::Result<Self> {
        let data = std::fs::read(path)?;
        serde_json::from_slice(&data).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
    }

    /// A one-line headline for the session window / log: the hottest wine function and tier.
    pub fn headline(&self) -> String {
        match self.hot_symbols.first() {
            Some(h) => {
                format!(
                    "Top: {} {} ({} tier)",
                    h.module, h.symbol, self.diagnostics.fidelity
                )
            }
            None => format!(
                "No wine symbols resolved ({} tier)",
                self.diagnostics.fidelity
            ),
        }
    }

    /// A human-readable report (what `beisl symbols` prints and the run dir keeps as
    /// `report.txt`).
    pub fn render_report(&self) -> String {
        use std::fmt::Write as _;
        let mut o = String::new();
        let _ = writeln!(o, "Beisl trace report — {}", self.game);
        let _ = writeln!(
            o,
            "  wine: {}   duration: {:.0}s",
            self.wine_flavor, self.duration_s
        );
        let d = &self.diagnostics;
        let _ = writeln!(
            o,
            "  fidelity: {}   samples: {}   unresolved: {:.0}%",
            d.fidelity, d.total_samples, d.unresolved_pct
        );
        if d.fidelity == FidelityTier::Exports.as_str() {
            let _ = writeln!(
                o,
                "  note: Exports tier — PE symbols are nearest-export; A/B-comparable, not full."
            );
        }

        if let Some(f) = &self.frame_stats {
            let _ = writeln!(
                o,
                "\nFrametimes ({} frames, {:.0}s logged):\n  \
                 avg {:.0} fps   median {:.1} ms   1% low {:.1} ms   worst {:.1} ms",
                f.frames,
                f.logged_seconds,
                f.avg_fps,
                f.median_frametime_ms,
                f.p99_frametime_ms,
                f.worst_frametime_ms
            );
            if !self.spikes.is_empty() {
                let _ = writeln!(o, "  worst spikes:");
                for s in &self.spikes {
                    let _ = writeln!(o, "    {:6.1} ms at {:.1}s", s.frametime_ms, s.t_seconds);
                }
            }
        } else {
            let _ = writeln!(o, "\nFrametimes: no MangoHud CSV logged for this run.");
        }

        if !self.layers.is_empty() {
            let _ = writeln!(
                o,
                "\nWho ate the frame (CPU by layer, of {:.0}% attributed):",
                self.attributed_pct
            );
            for l in &self.layers {
                // A 24-cell proportional bar; `#` filled to the layer's share. The trailing
                // "N% named" is the layer's symbol coverage — how much of it we could name.
                let filled = ((l.pct / 100.0) * 24.0).round() as usize;
                let bar: String = "#".repeat(filled) + &".".repeat(24 - filled.min(24));
                let _ = writeln!(
                    o,
                    "  {:<11} {bar} {:4.1}%   {:3.0}% named",
                    l.layer, l.pct, l.symbol_pct
                );
            }
            // The unmapped remainder isn't a layer — it's coverage we didn't get. Disclose it
            // rather than fold it into a bucket: it could be any layer, graphics/driver too.
            let gap = 100.0 - self.attributed_pct;
            if gap >= 0.5 {
                let _ = writeln!(
                    o,
                    "  ({gap:.0}% of samples were unmapped — could belong to any layer, \
                     graphics/driver included)"
                );
            }
        }

        let _ = writeln!(o, "\nHottest functions (self time, all layers):");
        if self.hot_symbols.is_empty() {
            let _ = writeln!(o, "  (none resolved — see diagnostics above)");
        }
        for h in self.hot_symbols.iter().take(15) {
            let _ = writeln!(o, "  {:5.1}%  {:<26} {}", h.pct, h.module, h.symbol);
        }

        if !d.anonymous_modules.is_empty() {
            let _ = writeln!(o, "\nAnonymous hot modules (samples we couldn't name):");
            for (module, pct) in &d.anonymous_modules {
                let hint = if module.to_ascii_lowercase().ends_with(".dll") {
                    "PE — needs Full-tier build (retained DWARF)"
                } else {
                    "install debuginfo"
                };
                let _ = writeln!(o, "  {pct:5.1}%  {module:<26} ({hint})");
            }
        }
        o
    }
}

/// The MangoHud CSV in a run dir (named after the game), if any.
fn find_csv(run_dir: &Path) -> Option<std::path::PathBuf> {
    std::fs::read_dir(run_dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .find(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("csv")))
}

/// What [`frames_from_csv`] yields: session stats, the worst spikes, and the downsampled
/// timeline for the chart.
type CsvFrames = (FrameStats, Vec<Spike>, Vec<(f64, f32)>);

/// Parse a MangoHud frametime CSV into stats + worst spikes.
///
/// Layout (verified): two system-info header lines, then a data-header line whose columns
/// name `frametime` (ms) among others, then data rows. We build the timeline from the
/// running sum of frametimes rather than the absolute-clock `elapsed` column, so it's
/// robust to that column's units.
fn frames_from_csv(path: &Path) -> Option<CsvFrames> {
    let text = std::fs::read_to_string(path).ok()?;
    let mut lines = text.lines();
    // Skip the two system-info lines; the third line is the data header.
    let header = lines.by_ref().nth(2)?;
    let ft_col = header.split(',').position(|c| c.trim() == "frametime")?;

    let mut fts: Vec<f32> = Vec::new();
    let mut timeline: Vec<(f64, f32)> = Vec::new();
    let mut t = 0.0f64;
    for row in lines {
        let Some(field) = row.split(',').nth(ft_col) else {
            continue;
        };
        let Ok(ms) = field.trim().parse::<f32>() else {
            continue;
        };
        if ms <= 0.0 || !ms.is_finite() {
            continue;
        }
        timeline.push((t, ms));
        t += ms as f64 / 1000.0;
        fts.push(ms);
    }
    if fts.is_empty() {
        return None;
    }

    let mut sorted = fts.clone();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let median = sorted[sorted.len() / 2];
    let p99 = sorted[(sorted.len() * 99 / 100).min(sorted.len() - 1)];
    let worst = *sorted.last().unwrap();
    let logged_seconds = t;
    let avg_fps = if logged_seconds > 0.0 {
        fts.len() as f32 / logged_seconds as f32
    } else {
        0.0
    };

    // A spike is a frame markedly worse than typical: twice the median, and at least a
    // visible-stutter 33 ms (below ~30 fps). Report the worst few, worst first.
    let threshold = (median * 2.0).max(33.0);
    let mut spikes: Vec<Spike> = timeline
        .iter()
        .filter(|&&(_, ms)| ms >= threshold)
        .map(|&(t_seconds, frametime_ms)| Spike {
            t_seconds,
            frametime_ms,
        })
        .collect();
    spikes.sort_by(|a, b| b.frametime_ms.partial_cmp(&a.frametime_ms).unwrap());
    spikes.truncate(TOP_SPIKES);

    Some((
        FrameStats {
            frames: fts.len() as u64,
            avg_fps,
            median_frametime_ms: median,
            p99_frametime_ms: p99,
            worst_frametime_ms: worst,
            logged_seconds,
        },
        spikes,
        downsample(&timeline),
    ))
}

/// Cap the timeline at [`TIMELINE_POINTS`] for the chart. Buckets take the *max* frametime
/// (not the mean) so a hitch inside a bucket still shows as a peak instead of averaging away.
const TIMELINE_POINTS: usize = 240;
fn downsample(timeline: &[(f64, f32)]) -> Vec<(f64, f32)> {
    if timeline.len() <= TIMELINE_POINTS {
        return timeline.to_vec();
    }
    let bucket = timeline.len().div_ceil(TIMELINE_POINTS);
    timeline
        .chunks(bucket)
        .map(|c| {
            let t = c[0].0;
            let ms = c.iter().map(|&(_, m)| m).fold(0.0_f32, f32::max);
            (t, ms)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    // Unique per call site so parallel tests don't clobber a shared temp path.
    fn write_csv(tag: &str, body: &str) -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!("beisl-csv-{}-{tag}.csv", std::process::id()));
        std::fs::write(&p, body).unwrap();
        p
    }

    #[test]
    fn parses_frametimes_and_flags_the_spike() {
        // Two info lines, a data header, then frames: three ~16 ms then one 50 ms hitch.
        let csv = "os,cpu\nSteam,Ryzen\nfps,frametime,elapsed\n\
                   60,16.0,1\n60,16.0,2\n60,16.0,3\n20,50.0,4\n";
        let (stats, spikes, timeline) = frames_from_csv(&write_csv("spike", csv)).unwrap();
        assert_eq!(timeline.len(), 4); // short run: not downsampled
        assert_eq!(stats.frames, 4);
        assert_eq!(stats.worst_frametime_ms, 50.0);
        assert_eq!(spikes.len(), 1);
        assert_eq!(spikes[0].frametime_ms, 50.0);
        // The spike sits after three 16 ms frames = 0.048 s in.
        assert!(
            (spikes[0].t_seconds - 0.048).abs() < 1e-3,
            "t={}",
            spikes[0].t_seconds
        );
    }

    #[test]
    fn a_smooth_run_has_no_spikes() {
        let csv = "os,cpu\nSteam,Ryzen\nfps,frametime,elapsed\n60,16.0,1\n60,16.5,2\n60,16.2,3\n";
        let (stats, spikes, _timeline) = frames_from_csv(&write_csv("smooth", csv)).unwrap();
        assert!(spikes.is_empty());
        assert!(stats.avg_fps > 55.0 && stats.avg_fps < 65.0);
    }

    #[test]
    fn artifact_round_trips_through_json() {
        let a = TraceArtifact {
            schema_version: SCHEMA_VERSION,
            game: "Test".into(),
            wine_flavor: "example-wine-build".into(),
            gpu_vendor: "amd".into(),
            duration_s: 42.0,
            frame_stats: None,
            spikes: vec![],
            timeline: vec![],
            layers: vec![
                LayerShare {
                    layer: "wine".into(),
                    pct: 60.0,
                    symbol_pct: 20.0,
                },
                LayerShare {
                    layer: "driver".into(),
                    pct: 40.0,
                    symbol_pct: 5.0,
                },
            ],
            attributed_pct: 75.0,
            hot_symbols: vec![HotSymbol {
                module: "ntdll.dll".into(),
                symbol: "RtlAllocateHeap".into(),
                pct: 1.5,
            }],
            diagnostics: Diagnostics {
                fidelity: "Exports".into(),
                total_samples: 100,
                unresolved_pct: 80.0,
                anonymous_modules: vec![("libvulkan_intel.so".into(), 16.6)],
            },
        };
        let json = serde_json::to_string(&a).unwrap();
        let back: TraceArtifact = serde_json::from_str(&json).unwrap();
        assert_eq!(back.game, "Test");
        assert_eq!(back.hot_symbols[0].symbol, "RtlAllocateHeap");
        assert_eq!(back.layers[0].layer, "wine");
        assert_eq!(back.layers[0].pct, 60.0);
        assert!(a.render_report().contains("Who ate the frame"));
        assert!(a.headline().contains("RtlAllocateHeap"));
    }

    #[test]
    fn layer_shares_normalize_over_attributed_samples_not_the_gap() {
        use symbol_resolver::{Diagnostics, FidelityTier, Layer, LayerTally, Resolution};
        let tally = |layer, samples, named| LayerTally {
            layer,
            samples,
            named,
        };
        // 100 samples: 40 unmapped (the gap); 60 attributed as game 45 (36 named) /
        // driver 15 (3 named).
        let res = Resolution {
            symbols: vec![],
            diagnostics: Diagnostics {
                tier: FidelityTier::Exports,
                total_samples: 100,
                unresolved_samples: 40,
                anonymous_modules: vec![],
            },
            layers: vec![
                tally(Layer::Game, 45, 36),
                tally(Layer::Driver, 15, 3),
                tally(Layer::Other, 40, 0),
            ],
            wine_flavor: None,
            gpu_vendor: None,
        };
        let a = TraceArtifact::from_parts("G", 1.0, res, None, vec![], vec![]);
        // The gap is disclosed as coverage, never as a peer layer.
        assert_eq!(a.attributed_pct, 60.0);
        assert!(a.layers.iter().all(|l| l.layer != "other"));
        // Shares are over the 60 attributed, not the 100 total: game = 45/60 = 75%.
        let game = a.layers.iter().find(|l| l.layer == "game").unwrap();
        assert!((game.pct - 75.0).abs() < 0.01, "game {}", game.pct);
        // Symbol coverage is per layer: game 36/45 = 80%, driver 3/15 = 20%.
        assert!(
            (game.symbol_pct - 80.0).abs() < 0.01,
            "game {}",
            game.symbol_pct
        );
        let driver = a.layers.iter().find(|l| l.layer == "driver").unwrap();
        assert!((driver.pct - 25.0).abs() < 0.01, "driver {}", driver.pct);
        assert!(
            (driver.symbol_pct - 20.0).abs() < 0.01,
            "driver {}",
            driver.symbol_pct
        );
        // The report states coverage and names the caveat.
        assert!(a.render_report().contains("of 60% attributed"));
        assert!(a.render_report().contains("named"));
        assert!(a.render_report().contains("unmapped"));
    }

    #[test]
    fn write_then_read_json_round_trips() {
        let a = TraceArtifact {
            schema_version: SCHEMA_VERSION,
            game: "RoundTrip".into(),
            wine_flavor: "example-wine-build".into(),
            gpu_vendor: "amd".into(),
            duration_s: 12.0,
            frame_stats: None,
            spikes: vec![],
            timeline: vec![],
            layers: vec![LayerShare {
                layer: "game".into(),
                pct: 100.0,
                symbol_pct: 50.0,
            }],
            attributed_pct: 88.0,
            hot_symbols: vec![],
            diagnostics: Diagnostics {
                fidelity: "Exports".into(),
                total_samples: 10,
                unresolved_pct: 0.0,
                anonymous_modules: vec![],
            },
        };
        let p = std::env::temp_dir().join(format!("beisl-art-{}.json", std::process::id()));
        a.write_json(&p).unwrap();
        let back = TraceArtifact::read_json(&p).unwrap();
        assert_eq!(back.game, "RoundTrip");
        assert_eq!(back.attributed_pct, 88.0);
        assert_eq!(back.layers[0].symbol_pct, 50.0);
        std::fs::remove_file(&p).ok();
    }

    /// An artifact written before the `layers` field must still deserialize (schema stayed
    /// v1 because the addition is read-compatible) — `layers` just defaults to empty.
    #[test]
    fn an_artifact_without_layers_still_loads() {
        let json = r#"{
            "schema_version": 1, "game": "Old", "wine_flavor": "unknown",
            "duration_s": 1.0, "frame_stats": null, "spikes": [], "hot_symbols": [],
            "diagnostics": {"fidelity": "Exports", "total_samples": 0,
                            "unresolved_pct": 0.0, "anonymous_modules": []}
        }"#;
        let back: TraceArtifact = serde_json::from_str(json).unwrap();
        assert!(back.layers.is_empty());
    }
}
