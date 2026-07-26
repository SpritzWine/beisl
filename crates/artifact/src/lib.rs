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
use std::path::{Path, PathBuf};

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
    /// The frametime `t` such that half of the *logged time* (not frame count) was
    /// spent in frames <= `t` — the median of what the player experienced, weighted by
    /// each frame's own duration. Frame-count medians are skewed by menu/launcher
    /// floods: a real CarX Street trace had per-frame median 0.31 ms (thousands of
    /// sub-millisecond menu frames outvoting gameplay) but per-time median 17.0 ms; a
    /// clean trace barely moves the two apart (16.9 -> 18.4 ms). `default` so artifacts
    /// written before this field deserialize as 0.0.
    #[serde(default)]
    pub time_weighted_median_ms: f32,
    /// 99th-percentile frametime — the "1% low" felt as stutter.
    pub p99_frametime_ms: f32,
    pub worst_frametime_ms: f32,
    /// Seconds the logged window spans (sum of frametimes).
    pub logged_seconds: f64,
    /// Rows discarded as physically implausible (frametime above
    /// [`MAX_PLAUSIBLE_FRAMETIME_MS`]) — a MangoHud logging glitch, not gameplay.
    /// `default` so artifacts written before this field deserialize as 0.
    #[serde(default)]
    pub dropped_rows: u64,
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
    /// Whether the game's own process (the resolver's dominant-pid approximation) mapped
    /// ANY PE module — what lets [`wine_display`](Self::wine_display) say "native" honestly
    /// instead of a bare "unknown" that would read the same as a wine game whose flavor
    /// just didn't match a known keyword. `Option` (not `bool`) so a pre-field artifact is
    /// distinguishable from a resolved "no PE" verdict: `None` renders the old plain
    /// "unknown" (we truly don't know), `Some(false)` renders "native" (we checked, and
    /// the game mapped nothing). Collapsing both to `false` would have old wine traces
    /// whose flavor simply didn't match a keyword start rendering an affirmative "native"
    /// claim the old code never made. `default` (`None`) so pre-field artifacts still load.
    #[serde(default)]
    pub game_maps_pe: Option<bool>,
    /// GPU vendor of the card that ran (`intel`/`amd`/`nvidia`/`qualcomm`), for vendor-colored
    /// UI. Empty string when undetected. Defaulted so pre-vendor artifacts still load.
    #[serde(default)]
    pub gpu_vendor: String,
    /// Which perf record mode produced this trace (`"full"`/`"degraded"`/`"none"`, empty on
    /// legacy runs). "degraded" is userspace-only (no `-a`, no kernel/CAP_PERFMON access), so
    /// its `layers` split has NO kernel bar by construction — that's a measurement limit, not
    /// a quiet kernel. Defaulted so pre-record_mode artifacts still load.
    #[serde(default)]
    pub record_mode: String,
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
    /// (the session); the wine flavor is inferred during resolution. No record mode is
    /// known here — callers that have a `Session` should call `build_with_debuginfod`
    /// directly so the artifact can carry it.
    pub fn build(run_dir: &Path, game: &str, duration_s: f64) -> io::Result<Self> {
        Self::build_with_debuginfod(run_dir, game, duration_s, "", None)
    }

    /// As [`build`](Self::build), but with an explicit `record_mode` (see the field doc on
    /// [`TraceArtifact::record_mode`]) and `DEBUGINFOD_URLS` for the perf-side resolve. The
    /// wrapper's post-run analysis runs inside Steam's game-launch session, which does NOT
    /// carry an interactive login shell's `DEBUGINFOD_URLS`, so the driver layer (Mesa/Intel/
    /// AMD Vulkan — the only layer debuginfod can serve) would silently resolve to anonymous
    /// addresses. Passing the URL through here makes the trace-time resolve match a later
    /// `beisl symbols` run instead of degrading by environment.
    pub fn build_with_debuginfod(
        run_dir: &Path,
        game: &str,
        duration_s: f64,
        record_mode: &str,
        debuginfod_urls: Option<&str>,
    ) -> io::Result<Self> {
        let res = symbol_resolver::resolve_perf_data_with_debuginfod(
            &run_dir.join("perf.data"),
            debuginfod_urls,
        )?;
        let (frame_stats, spikes, timeline) = find_csv(run_dir)
            .and_then(|csv| frames_from_csv(&csv))
            .map(|(stats, spikes, timeline)| (Some(stats), spikes, timeline))
            .unwrap_or((None, Vec::new(), Vec::new()));
        Ok(Self::from_parts(
            game,
            duration_s,
            record_mode,
            res,
            frame_stats,
            spikes,
            timeline,
        ))
    }

    fn from_parts(
        game: &str,
        duration_s: f64,
        record_mode: &str,
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
            // A fresh resolve always knows the answer — only a pre-field artifact read
            // back from disk carries `None`.
            game_maps_pe: Some(res.game_maps_pe),
            gpu_vendor: res.gpu_vendor.unwrap_or_default(),
            record_mode: record_mode.to_string(),
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

    /// Write `artifact.json` and `report.txt` into `run_dir` - the one door every
    /// analysis caller (the wrapper, `beisl symbols`, the GUI's Analyze) goes through, so
    /// they can't drift apart the way they did before (one door printed the report and
    /// saved nothing; another saved the artifact but not the report). Filenames come from
    /// `session::library::{ARTIFACT_FILE, REPORT_FILE}`. Both are staged first and
    /// renamed back to back (see [`write_staged`]), so a failure partway through - e.g.
    /// disk full after the artifact but before the report - can't leave a fresh file
    /// beside a stale sibling that a reader would treat as a matching, current pair.
    pub fn persist(&self, run_dir: &Path) -> io::Result<()> {
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        let report = self.render_report();
        let artifact_path = run_dir.join(session::library::ARTIFACT_FILE);
        let report_path = run_dir.join(session::library::REPORT_FILE);
        write_staged(&[
            (artifact_path.as_path(), json.as_bytes()),
            (report_path.as_path(), report.as_bytes()),
        ])
    }

    /// Read a previously written `artifact.json`. Keeps deserialization inside this crate so
    /// consumers (the GUI, later the trace library) bind to the type without pulling serde
    /// themselves - the same reason [`persist`](Self::persist) lives here.
    pub fn read_json(path: &Path) -> io::Result<Self> {
        let data = std::fs::read(path)?;
        serde_json::from_slice(&data).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
    }

    /// The `wine:` line's value: a matched flavor always wins; otherwise `game_maps_pe`
    /// distinguishes three states the old bare `wine_flavor` string collapsed into one
    /// silent "unknown": `None` (pre-field artifact, we never checked) stays "unknown";
    /// `Some(true)` (a PE mapped, but its path named no known build) is "unknown flavor";
    /// `Some(false)` (the game's own process mapped no PE at all) is the honest "native".
    pub fn wine_display(&self) -> &str {
        if self.wine_flavor != "unknown" {
            return &self.wine_flavor;
        }
        match self.game_maps_pe {
            None => "unknown",
            Some(true) => "unknown flavor",
            Some(false) => "none — native game",
        }
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
            self.wine_display(),
            self.duration_s
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
        if self.record_mode == "degraded" {
            let _ = writeln!(
                o,
                "  note: degraded recording (userspace-only) — the layer split below has no \
                 kernel bar because kernel profiling wasn't available, not because the kernel \
                 was quiet."
            );
        }

        if let Some(f) = &self.frame_stats {
            // The headline median is time-weighted ("experienced") rather than the
            // per-frame value: see `FrameStats::time_weighted_median_ms` for why
            // (menu-flood frame-count bias). The per-frame value stays alongside it for
            // continuity with earlier reports. "1% low" (p99) deliberately stays
            // per-frame — a time-weighted p99 was considered and rejected: worst frames
            // are long *by definition*, so weighting the tail by duration lets a
            // handful of real hitches dominate it, exploding it to ~400 ms on genuinely
            // hitchy traces instead of conveying "1% low" the way gamers read it.
            let _ = writeln!(
                o,
                "\nFrametimes ({} frames, {:.0}s logged):\n  \
                 avg {:.0} fps   median {:.1} ms experienced ({:.1} per-frame)   \
                 1% low {:.1} ms   worst {:.1} ms",
                f.frames,
                f.logged_seconds,
                f.avg_fps,
                f.time_weighted_median_ms,
                f.median_frametime_ms,
                f.p99_frametime_ms,
                f.worst_frametime_ms
            );
            if f.dropped_rows > 0 {
                let _ = writeln!(
                    o,
                    "  note: {} implausible frametime row(s) dropped (>10 s, or matching \
                     time-since-boot — MangoHud logs the machine's uptime as the first \
                     frametime of every new swapchain). A logging glitch, not gameplay.",
                    f.dropped_rows
                );
            }
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

/// Write each `(path, contents)` to a pid-unique sibling temp file, then rename all of
/// them into place back to back, only once every write succeeded. A single truncating
/// write that dies midway would destroy whatever was at `path`; staging first and
/// renaming together additionally means a `persist` pair can't fail between the two
/// files and leave a fresh one beside a stale sibling (rename needs no disk space, so
/// it won't realistically fail once staging did). The pid in the temp name keeps two
/// concurrent writers on the same run dir (`beisl symbols` racing a GUI re-analyze) from
/// sharing - and corrupting - the same temp file. On any failure, every temp created so
/// far is removed and no destination is touched.
fn write_staged(files: &[(&Path, &[u8])]) -> io::Result<()> {
    let pid = std::process::id();
    let tmps: Vec<PathBuf> = files
        .iter()
        .map(|(path, _)| {
            let mut tmp = path.as_os_str().to_owned();
            tmp.push(format!(".tmp.{pid}"));
            PathBuf::from(tmp)
        })
        .collect();
    let result = files
        .iter()
        .zip(&tmps)
        .try_for_each(|((_, contents), tmp)| std::fs::write(tmp, contents))
        .and_then(|()| {
            files
                .iter()
                .zip(&tmps)
                .try_for_each(|((path, _), tmp)| std::fs::rename(tmp, path))
        });
    if result.is_err() {
        for tmp in &tmps {
            let _ = std::fs::remove_file(tmp);
        }
    }
    result
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

/// The plausibility ceiling for one frame. MangoHud occasionally logs physically
/// impossible frametimes (a real CarX Street run had five rows of ~23,000,000 ms —
/// 6.4 *hours* each — in an otherwise healthy 4-minute log; they dragged avg_fps to
/// 0.34 and logged_seconds to 32 hours). 10 s is deliberately generous: multi-second
/// shader-compile stalls are real and must survive; hours are not a frame.
const MAX_PLAUSIBLE_FRAMETIME_MS: f32 = 10_000.0;

/// The glitch's precise signature: the bogus value IS the machine's uptime at the
/// moment the row was logged (MangoHud computes `now - last_present_time` against the
/// zero-initialized timestamp of every new swapchain's first frame, so raw
/// CLOCK_MONOTONIC leaks into the log — measured at 99.7-99.8 % of uptime across two
/// real traces). Matching the signature catches what the fixed ceiling can't: on a
/// fast-boot setup (boot -> Steam -> game in seconds) the leak is small enough to
/// pass 10 s. Leaks mid-session carry the uptime of *their* moment, so the window
/// spans the whole logged duration, padded by [`LEAK_SLACK_S`] + 2 % for clock skew.
fn leak_window(uptime_end_s: f64, logged_dur_s: f64) -> (f64, f64) {
    let slack = LEAK_SLACK_S + uptime_end_s * 0.02;
    (uptime_end_s - logged_dur_s - slack, uptime_end_s + slack)
}
const LEAK_SLACK_S: f64 = 5.0;

/// Is this frametime the monotonic leak? Only answerable with a window (same machine,
/// same boot — see [`uptime_at_csv_end`]). The lower bound is floored at
/// [`LEAK_FLOOR_S`]: no Steam box goes boot -> first presented frame in under ~20 s,
/// and the floor guarantees even a pathological real 10-20 s stall can never be
/// misclassified — below it, the ceiling is deliberately the only gate.
fn is_uptime_leak(ms: f32, window: Option<(f64, f64)>) -> bool {
    let Some((lo, hi)) = window else {
        return false;
    };
    let s = ms as f64 / 1000.0;
    s >= lo.max(LEAK_FLOOR_S) && s <= hi
}
/// The window may never reach below this, whatever the uptime says.
const LEAK_FLOOR_S: f64 = 20.0;

/// Uptime (seconds since boot) when the CSV's last row was written: its mtime
/// (MangoHud flushes per frame, so mtime ≈ end of logging) minus the boot epoch from
/// `/proc/stat`'s `btime`. `None` off-Linux or when the numbers say "different boot /
/// different machine" (non-positive uptime) — an exported trace analyzed elsewhere
/// gets the ceiling only, never a bogus window.
fn uptime_at_csv_end(csv: &Path) -> Option<f64> {
    let stat = std::fs::read_to_string("/proc/stat").ok()?;
    let btime: f64 = stat
        .lines()
        .find_map(|l| l.strip_prefix("btime "))?
        .trim()
        .parse()
        .ok()?;
    let mtime = std::fs::metadata(csv)
        .ok()?
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs_f64();
    let uptime = mtime - btime;
    (uptime > 0.0).then_some(uptime)
}

/// The time-weighted median (see [`FrameStats::time_weighted_median_ms`] for why):
/// walk `sorted_asc` (already plausibility-gated, ascending) accumulating the
/// frametimes themselves as weight, and return the first one at which the cumulative
/// sum reaches half the total — equivalent to the weighted median with weight = value.
/// Empty input yields 0.0, matching the rest of the stats' empty-set convention.
fn time_weighted_median_ms(sorted_asc: &[f32]) -> f32 {
    if sorted_asc.is_empty() {
        return 0.0;
    }
    let total: f64 = sorted_asc.iter().map(|&ms| ms as f64).sum();
    let half = total / 2.0;
    let mut cum = 0.0f64;
    for &ms in sorted_asc {
        cum += ms as f64;
        if cum >= half {
            return ms;
        }
    }
    // Unreachable in practice (cum reaches `total` >= `half` by the last element),
    // but a defined fallback keeps this total rather than partial.
    *sorted_asc.last().unwrap()
}

/// Parse a MangoHud frametime CSV into stats + worst spikes.
///
/// Layout (verified): two system-info header lines, then a data-header line whose columns
/// name `frametime` (ms) among others, then data rows. We build the timeline from the
/// running sum of frametimes rather than the absolute-clock `elapsed` column, so it's
/// robust to that column's units.
fn frames_from_csv(path: &Path) -> Option<CsvFrames> {
    frames_from_csv_gated(path, uptime_at_csv_end(path))
}

/// [`frames_from_csv`] with the uptime injectable, so tests exercise the leak window
/// without depending on the test machine's real boot time.
fn frames_from_csv_gated(path: &Path, uptime_end_s: Option<f64>) -> Option<CsvFrames> {
    let text = std::fs::read_to_string(path).ok()?;
    let mut lines = text.lines();
    // Skip the two system-info lines; the third line is the data header.
    let header = lines.by_ref().nth(2)?;
    let ft_col = header.split(',').position(|c| c.trim() == "frametime")?;

    let raw: Vec<f32> = lines
        .filter_map(|row| row.split(',').nth(ft_col))
        .filter_map(|field| field.trim().parse::<f32>().ok())
        .filter(|ms| *ms > 0.0 && ms.is_finite())
        .collect();

    // The leak window needs the session's logged duration, which needs the rows —
    // so: provisional duration over ceiling-passing rows first (leaks are rare and
    // huge, so their exclusion here is what makes the estimate usable), then the
    // real classification pass.
    let provisional_dur_s: f64 = raw
        .iter()
        .filter(|ms| **ms <= MAX_PLAUSIBLE_FRAMETIME_MS)
        .map(|ms| *ms as f64 / 1000.0)
        .sum();
    let window = uptime_end_s.map(|ue| leak_window(ue, provisional_dur_s));

    let mut fts: Vec<f32> = Vec::new();
    let mut timeline: Vec<(f64, f32)> = Vec::new();
    let mut dropped: u64 = 0;
    let mut t = 0.0f64;
    for ms in raw {
        // Implausible rows are dropped from *everything* — stats, spikes, and the
        // timeline clock: one hours-long value would otherwise shift every later
        // bookmark by hours and flatten the chart to a spike at nothing.
        if ms > MAX_PLAUSIBLE_FRAMETIME_MS || is_uptime_leak(ms, window) {
            dropped += 1;
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
    let time_weighted_median = time_weighted_median_ms(&sorted);
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
            time_weighted_median_ms: time_weighted_median,
            p99_frametime_ms: p99,
            worst_frametime_ms: worst,
            logged_seconds,
            dropped_rows: dropped,
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
    fn implausible_frametimes_are_dropped_not_averaged() {
        // Real-world shape (CarX Street, 2026-07-12): healthy frames with hours-long
        // garbage rows in between. They must vanish from stats, spikes, and the
        // timeline clock — and be counted. The genuine 2.5 s shader-stall survives.
        let csv = "os,cpu\nSteam,Ryzen\nfps,frametime,elapsed\n\
                   60,16.0,1\n60,16.0,2\n0,23000000.0,3\n60,16.0,4\n\
                   0,23024200.0,5\n0.4,2500.0,6\n";
        let (stats, spikes, timeline) = frames_from_csv(&write_csv("garbage", csv)).unwrap();
        assert_eq!(stats.dropped_rows, 2);
        assert_eq!(stats.frames, 4);
        assert_eq!(stats.worst_frametime_ms, 2500.0);
        assert!(
            stats.logged_seconds < 3.0,
            "the clock must not include garbage: {}",
            stats.logged_seconds
        );
        assert!(spikes
            .iter()
            .chain(std::iter::empty())
            .all(|s| s.frametime_ms <= MAX_PLAUSIBLE_FRAMETIME_MS));
        assert!(timeline
            .iter()
            .all(|&(_, ms)| ms <= MAX_PLAUSIBLE_FRAMETIME_MS));
        // The garbage rows (~6.4 h each) must not leak into the time-weighted median
        // either, or it would report hours instead of milliseconds: surviving frames
        // sorted are [16, 16, 16, 2500] ms; total = 2548 ms, half = 1274 ms; the
        // cumulative sum only clears 1274 ms once the 2500 ms frame is folded in
        // (16+16+16 = 48, then +2500 = 2548 >= 1274) -> 2500 ms.
        assert_eq!(stats.time_weighted_median_ms, 2500.0);
        assert!(stats.time_weighted_median_ms <= MAX_PLAUSIBLE_FRAMETIME_MS);
    }

    #[test]
    fn menu_flood_time_weighted_median_reflects_experienced_time_not_frame_count() {
        // Menu/launcher flood (2026-07-12 CarX Street observation, decided with
        // Florian): thousands of sub-millisecond menu frames outvote gameplay in frame
        // COUNT but barely register in logged TIME. Here: 1000 menu frames @ 1 ms +
        // 100 gameplay frames @ 20 ms.
        //   per-frame median: 1100 frames sorted ascending; index 1100/2 = 550 falls
        //   inside the first 1000 (all 1 ms) entries -> 1 ms.
        //   time-weighted median: total time = 1000*1 + 100*20 = 3000 ms; half = 1500
        //   ms. Cumulative sum over the 1000 1-ms frames reaches 1000 ms; each
        //   following 20-ms frame adds 20, so it takes 25 more (1000 + 25*20 = 1500)
        //   to reach exactly half -> the 25th 20-ms frame, i.e. 20 ms.
        let mut csv = String::from("os,cpu\nSteam,Ryzen\nfps,frametime,elapsed\n");
        for i in 0..1000 {
            csv.push_str(&format!("1000,1.0,{i}\n"));
        }
        for i in 0..100 {
            csv.push_str(&format!("50,20.0,{i}\n"));
        }
        let (stats, _, _) = frames_from_csv(&write_csv("menu-flood", &csv)).unwrap();
        assert_eq!(stats.frames, 1100);
        assert_eq!(stats.median_frametime_ms, 1.0);
        assert_eq!(stats.time_weighted_median_ms, 20.0);
    }

    #[test]
    fn uniform_frametimes_make_both_medians_equal() {
        // No flood, nothing to weight away from: the two medians agree.
        let csv = "os,cpu\nSteam,Ryzen\nfps,frametime,elapsed\n\
                   60,16.0,1\n60,16.0,2\n60,16.0,3\n60,16.0,4\n";
        let (stats, _, _) = frames_from_csv(&write_csv("uniform", csv)).unwrap();
        assert_eq!(stats.median_frametime_ms, 16.0);
        assert_eq!(stats.time_weighted_median_ms, 16.0);
    }

    #[test]
    fn uptime_leak_window_matches_the_signature_and_nothing_else() {
        // A 6.4 h session-end uptime, 4-minute log (the real CarX shape): the leak
        // values (uptime at their moment) fall inside; real frametimes never do.
        let w = Some(leak_window(23_070.0, 235.0));
        assert!(is_uptime_leak(23_024_200.0, w)); // 23_024 s — the real leak value (ms)
        assert!(is_uptime_leak(22_995_200.0, w)); // earliest leak in that session
        assert!(!is_uptime_leak(16.0, w)); // a frame
        assert!(!is_uptime_leak(2_500.0, w)); // a real shader stall
        assert!(!is_uptime_leak(9_500.0, w)); // even a huge real stall
                                              // No window (other machine / other boot / non-Linux): never a leak verdict.
        assert!(!is_uptime_leak(23_024_200.0, None));
    }

    #[test]
    fn leak_floor_protects_real_stalls_on_fresh_boots() {
        // Machine up only 13 s at log end: the naive window would reach down into
        // real-stall territory; the 20 s floor keeps anything below it out of reach.
        let w = Some(leak_window(13.0, 5.0));
        assert!(!is_uptime_leak(8_000.0, w)); // 8 s stall on a fresh boot: kept
                                              // And above the floor the window still works where it can exist at all.
        let w = Some(leak_window(60.0, 10.0));
        assert!(is_uptime_leak(55_000.0, w)); // 55 s ≈ uptime: the leak
        assert!(!is_uptime_leak(15_000.0, w)); // below the floor: ceiling's business
    }

    #[test]
    fn injected_uptime_drops_a_leak_row_from_the_csv() {
        // A leak row whose value sits inside the injected uptime window but is also
        // > ceiling — the common real case; it must be dropped and counted exactly once.
        let csv = "os,cpu\nSteam,Ryzen\nfps,frametime,elapsed\n\
                   60,16.0,1\n60,16.0,2\n0,59000.0,3\n60,16.0,4\n";
        let (stats, _, timeline) =
            frames_from_csv_gated(&write_csv("leak", csv), Some(60.0)).unwrap();
        assert_eq!(stats.dropped_rows, 1);
        assert_eq!(stats.frames, 3);
        assert!(stats.logged_seconds < 1.0);
        assert!(timeline.iter().all(|&(_, ms)| ms < 100.0));
    }

    #[test]
    fn frame_stats_json_without_dropped_rows_defaults_to_zero() {
        // Artifacts written before the plausibility gate carry no dropped_rows field —
        // and, likewise, none written before this item carry time_weighted_median_ms.
        // Both must default rather than fail deserialization.
        let j = r#"{"frames":1,"avg_fps":60.0,"median_frametime_ms":16.0,
                    "p99_frametime_ms":16.0,"worst_frametime_ms":16.0,"logged_seconds":1.0}"#;
        let f: FrameStats = serde_json::from_str(j).unwrap();
        assert_eq!(f.dropped_rows, 0);
        assert_eq!(f.time_weighted_median_ms, 0.0);
    }

    #[test]
    fn render_report_headlines_the_experienced_median_and_keeps_per_frame() {
        // The report's summary line must lead with the time-weighted ("experienced")
        // median and keep the per-frame value alongside it — using the real CarX
        // Street numbers this item was decided from (0.31 ms per-frame vs 17.0 ms
        // time-weighted).
        let a = TraceArtifact {
            schema_version: SCHEMA_VERSION,
            game: "Test".into(),
            wine_flavor: "unknown".into(),
            game_maps_pe: Some(false),
            gpu_vendor: String::new(),
            record_mode: String::new(),
            duration_s: 10.0,
            frame_stats: Some(FrameStats {
                frames: 1100,
                avg_fps: 60.0,
                median_frametime_ms: 0.31,
                time_weighted_median_ms: 17.0,
                p99_frametime_ms: 40.0,
                worst_frametime_ms: 50.0,
                logged_seconds: 10.0,
                dropped_rows: 0,
            }),
            spikes: vec![],
            timeline: vec![],
            layers: vec![],
            attributed_pct: 0.0,
            hot_symbols: vec![],
            diagnostics: Diagnostics {
                fidelity: "Exports".into(),
                total_samples: 0,
                unresolved_pct: 0.0,
                anonymous_modules: vec![],
            },
        };
        let report = a.render_report();
        assert!(
            report.contains("median 17.0 ms experienced (0.3 per-frame)"),
            "report was:\n{report}"
        );
        // "1% low" (p99) stays exactly per-frame — unchanged.
        assert!(report.contains("1% low 40.0 ms"));
    }

    #[test]
    fn artifact_round_trips_through_json() {
        let a = TraceArtifact {
            schema_version: SCHEMA_VERSION,
            game: "Test".into(),
            wine_flavor: "example-wine-build".into(),
            game_maps_pe: Some(true),
            gpu_vendor: "amd".into(),
            record_mode: "degraded".into(),
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
        assert_eq!(back.record_mode, "degraded"); // round-trips through JSON
        assert!(a.render_report().contains("Who ate the frame"));
        assert!(a.headline().contains("RtlAllocateHeap"));
    }

    /// The degraded-mode note must appear so a reader can't mistake the missing kernel bar
    /// for a genuinely quiet kernel — and must NOT appear for full/other modes (no
    /// behavior change there).
    #[test]
    fn render_report_notes_degraded_mode_but_not_full() {
        let mut a = TraceArtifact {
            schema_version: SCHEMA_VERSION,
            game: "Test".into(),
            wine_flavor: "example-wine-build".into(),
            game_maps_pe: Some(true),
            gpu_vendor: "amd".into(),
            record_mode: "degraded".into(),
            duration_s: 1.0,
            frame_stats: None,
            spikes: vec![],
            timeline: vec![],
            layers: vec![],
            attributed_pct: 0.0,
            hot_symbols: vec![],
            diagnostics: Diagnostics {
                fidelity: "Exports".into(),
                total_samples: 0,
                unresolved_pct: 0.0,
                anonymous_modules: vec![],
            },
        };
        assert!(a.render_report().contains("degraded recording"));
        assert!(a.render_report().contains("no kernel bar"));

        a.record_mode = "full".into();
        assert!(!a.render_report().contains("degraded recording"));
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
            game_maps_pe: false,
            gpu_vendor: None,
        };
        let a = TraceArtifact::from_parts("G", 1.0, "full", res, None, vec![], vec![]);
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

    /// A write that fails after staging (rename onto a directory) must clean its temp and
    /// leave the obstruction untouched - the point of the staged write is that a failed
    /// commit can't destroy what's at the destination.
    #[test]
    fn a_failed_write_cleans_its_staging_file() {
        let dir = std::env::temp_dir().join(format!("beisl-art-dir-{}", std::process::id()));
        let dst = dir.join("dst-is-a-dir");
        std::fs::create_dir_all(&dst).unwrap();
        assert!(write_staged(&[(dst.as_path(), b"data")]).is_err());
        assert!(
            !dir.join(format!("dst-is-a-dir.tmp.{}", std::process::id()))
                .exists(),
            "staging file survived a failed rename"
        );
        assert!(dst.is_dir(), "obstruction was disturbed");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The pairing guarantee `persist` relies on: a stage-write failure on the SECOND
    /// file (the realistic ENOSPC-mid-persist case) must not leave the first file's
    /// fresh rename sitting next to a stale sibling - neither may land, so an old pair
    /// stays exactly as it was.
    #[test]
    fn write_staged_leaves_no_partial_pair_when_the_second_stage_fails() {
        let dir = std::env::temp_dir().join(format!("beisl-staged-pair-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let good = dir.join("good.txt");
        // A parent that doesn't exist makes the second file's stage write itself fail,
        // before either rename is attempted.
        let bad = dir.join("missing-subdir").join("bad.txt");
        assert!(write_staged(&[(good.as_path(), b"ok"), (bad.as_path(), b"nope")]).is_err());
        assert!(
            !good.exists(),
            "the first file must not land if its sibling's stage failed"
        );
        assert!(
            !dir.join(format!("good.txt.tmp.{}", std::process::id()))
                .exists(),
            "good's staging temp left behind"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A minimal, otherwise-uninteresting artifact for `persist` tests that only care
    /// about identity (which pair of files landed, whether a re-persist changed `game`) -
    /// so a new non-defaulted field breaks one fixture, not every hand-written literal.
    fn sample_artifact(game: &str) -> TraceArtifact {
        TraceArtifact {
            schema_version: SCHEMA_VERSION,
            game: game.into(),
            wine_flavor: "unknown".into(),
            game_maps_pe: Some(false),
            gpu_vendor: String::new(),
            record_mode: "full".into(),
            duration_s: 1.0,
            frame_stats: None,
            spikes: vec![],
            timeline: vec![],
            layers: vec![],
            attributed_pct: 0.0,
            hot_symbols: vec![],
            diagnostics: Diagnostics {
                fidelity: "Exports".into(),
                total_samples: 0,
                unresolved_pct: 0.0,
                anonymous_modules: vec![],
            },
        }
    }

    /// The regression `persist` exists to close: `beisl symbols` used to print the report
    /// and write nothing. Both files must land, `artifact.json` must parse back as the
    /// same artifact, and `report.txt` must actually be the rendered report (not just
    /// non-empty by accident).
    #[test]
    fn persist_writes_both_files_and_they_read_back() {
        let a = sample_artifact("Persisted");
        let dir = std::env::temp_dir().join(format!("beisl-persist-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        a.persist(&dir).unwrap();

        let artifact_path = dir.join(session::library::ARTIFACT_FILE);
        let report_path = dir.join(session::library::REPORT_FILE);
        assert!(artifact_path.is_file(), "artifact.json missing");
        assert!(report_path.is_file(), "report.txt missing");

        let back = TraceArtifact::read_json(&artifact_path).unwrap();
        assert_eq!(back.game, "Persisted");

        let report = std::fs::read_to_string(&report_path).unwrap();
        assert!(!report.is_empty());
        // Match the header's two halves, not the punctuation between them, so this
        // pins "report.txt is the rendered report" without pinning its typography.
        assert!(report.contains("Beisl trace report"));
        assert!(report.contains("Persisted"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Persisting twice over an existing pair (the re-analyze path) must leave both files
    /// readable and, since this exercises the staged-rename write rather than a
    /// truncate-in-place, no pid-tagged temp file behind for either.
    #[test]
    fn persisting_twice_leaves_both_files_readable() {
        let mut a = sample_artifact("First");
        let dir = std::env::temp_dir().join(format!("beisl-persist-twice-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        a.persist(&dir).unwrap();

        a.game = "Second".into();
        a.persist(&dir).unwrap();

        let artifact_path = dir.join(session::library::ARTIFACT_FILE);
        let report_path = dir.join(session::library::REPORT_FILE);
        let back = TraceArtifact::read_json(&artifact_path).unwrap();
        assert_eq!(back.game, "Second");
        let report = std::fs::read_to_string(&report_path).unwrap();
        assert!(report.contains("Second"));

        let pid = std::process::id();
        for path in [&artifact_path, &report_path] {
            let mut tmp = path.as_os_str().to_owned();
            tmp.push(format!(".tmp.{pid}"));
            assert!(
                !Path::new(&tmp).exists(),
                "staging file left behind: {tmp:?}"
            );
        }

        let _ = std::fs::remove_dir_all(&dir);
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
        // Pre-`game_maps_pe` artifacts default to `None` (we never checked) and must render
        // the old plain "unknown" — NOT the affirmative "native" claim, which the old code
        // never made and would be a false positive for a wine trace whose flavor simply
        // didn't match a known keyword.
        assert!(back.game_maps_pe.is_none());
        assert_eq!(back.wine_display(), "unknown");
    }

    /// `wine_display` is the honesty fix's visible surface: a native game (no PE mapped at
    /// all) reads distinctly from a wine game whose flavor keyword just didn't match, which
    /// in turn reads distinctly from a matched flavor. The report and the GUI header both
    /// read this one method so the three states can't drift apart.
    #[test]
    fn wine_display_distinguishes_native_from_unmatched_flavor() {
        let mut a = TraceArtifact {
            schema_version: SCHEMA_VERSION,
            game: "Test".into(),
            wine_flavor: "unknown".into(),
            game_maps_pe: Some(false),
            gpu_vendor: String::new(),
            record_mode: String::new(),
            duration_s: 1.0,
            frame_stats: None,
            spikes: vec![],
            timeline: vec![],
            layers: vec![],
            attributed_pct: 0.0,
            hot_symbols: vec![],
            diagnostics: Diagnostics {
                fidelity: "Exports".into(),
                total_samples: 0,
                unresolved_pct: 0.0,
                anonymous_modules: vec![],
            },
        };
        assert_eq!(a.wine_display(), "none — native game");
        assert!(a.render_report().contains("wine: none — native game"));

        a.game_maps_pe = Some(true); // a PE mapped, but its path named no known build
        assert_eq!(a.wine_display(), "unknown flavor");

        a.wine_flavor = "SpritzWine-Prater".into(); // a real match wins over both
        assert_eq!(a.wine_display(), "SpritzWine-Prater");

        // A pre-field artifact (`None`) must NOT read as the affirmative "native" claim —
        // that would be a false claim the old code never made. It stays the old plain
        // "unknown", same as a resolved-but-unmatched flavor used to render.
        a.wine_flavor = "unknown".into();
        a.game_maps_pe = None;
        assert_eq!(a.wine_display(), "unknown");
    }
}
