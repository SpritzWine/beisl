//! The Beisl trace artifact — the single, exportable file that merges the MangoHud
//! frametime *side-channel* (a spike index) with the *symbolicated* perf samples.
//!
//! It is what the GUI opens, what the MCP server serves to Claude, and what a
//! reviewer exports to hand us as a structured perf report instead of "it feels
//! slow." Being self-describing (it carries its own fidelity tier and wine flavor)
//! is what keeps SpritzWine-vs-Proton A/Bs honest.

/// A low-performance peak detected from the MangoHud frametime CSV — a bookmark into
/// the perf samples ("here's where it hitched; look at the CPU right here").
#[derive(Clone, Debug)]
pub struct Spike {
    pub t_seconds: f64,
    pub frametime_ms: f32,
}

/// Metadata stamped on every trace so it is self-describing.
#[derive(Clone, Debug)]
pub struct TraceMeta {
    /// Game / command traced.
    pub game: String,
    /// e.g. "SpritzWine-Prater", "Proton 9.0", "GE-Proton", "system wine".
    pub wine_flavor: String,
    /// `symbol_resolver::FidelityTier`, stringified — so nobody reads a partial
    /// Proton trace as if it were a full one.
    pub fidelity: String,
    pub duration_s: f64,
}

/// The merged, exportable trace.
#[derive(Clone, Debug, Default)]
pub struct TraceArtifact {
    pub meta: Option<TraceMeta>,
    /// Low-perf peaks from the MangoHud side-channel (the index into `samples`).
    pub spikes: Vec<Spike>,
    // TODO: resolved samples (symbol, self/total, timeline) so the GUI + MCP can
    //   answer `top_wine_self_time` and `symbols_in_window(t0, t1)` cheaply.
}

impl TraceArtifact {
    /// Write the artifact to `path`.
    pub fn write(&self, _path: &std::path::Path) -> std::io::Result<()> {
        // TODO: serialize into a compact, self-contained container (rusqlite or a
        //   versioned binary) that the GUI and MCP can query without perf.data.
        Ok(())
    }
}
