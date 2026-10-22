//! Turns `perf.data` sample addresses into wine symbol names.
//!
//! Wine-agnostic by default, with a fidelity that depends on what the traced wine
//! build carries — this split is what lets Beisl trace *anyone's* wine, not just ours:
//!
//! * [`FidelityTier::Full`]    — SpritzWine/Prater with retained DWARF: internal
//!   statics resolve, not just exports.
//! * [`FidelityTier::Exports`] — stock Proton/GE (stripped): perf attributes ELF DSOs
//!   natively; PE DLLs resolve via their on-disk export tables (`ntdll.dll+0x… →
//!   nearest export`). Degraded but real and *comparable* — the tier that makes a
//!   SpritzWine-vs-Proton A/B possible at all.
//! * [`FidelityTier::Distro`]  — system wine with debuginfo packages installed.
//!
//! We don't parse the binary `perf.data` ourselves: `perf script` is already a required
//! dependency and the codebase composes with tools rather than vendoring them. perf does
//! the ELF symbolization and hands us module + raw offset for the PE side (its symbolizer
//! is ELF-only, verified live — see DESIGN.md "The gate"); the [`pe`] module closes that
//! gap. Every resolution carries a [`Diagnostics`] summary (tier, unresolved share, the
//! anonymous hot modules) so the CLI now, and the GUI/MCP later, surface *issues* from
//! the same struct.

use std::collections::HashMap;
use std::io;
use std::path::Path;
use std::process::{Command, Stdio};

pub mod pe;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FidelityTier {
    /// Retained DWARF (SpritzWine/Prater with debug info kept).
    Full,
    /// Export tables only (stripped stock Proton/GE).
    Exports,
    /// System wine with installed debuginfo.
    Distro,
    Unknown,
}

impl FidelityTier {
    pub fn as_str(self) -> &'static str {
        match self {
            FidelityTier::Full => "Full",
            FidelityTier::Exports => "Exports",
            FidelityTier::Distro => "Distro",
            FidelityTier::Unknown => "Unknown",
        }
    }
}

/// The stack layer a sample belongs to — the "who ate the frame" axis. Every sample lands
/// in exactly one bucket by its DSO, so the shares sum across the *whole* trace (named or
/// not: a kernel-heavy trace shows a big kernel bar even though we can't name kernel
/// functions without its debuginfo). This is the classification the headline layer bar
/// reads instead of re-deriving it from the top symbols.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Layer {
    /// Linux kernel + vDSO: syscalls, scheduler, page faults, GPU submit ioctls.
    Kernel,
    /// GPU userspace driver: Mesa/NVIDIA Vulkan ICDs, DRI, libdrm.
    Driver,
    /// Direct3D→Vulkan translation: DXVK (d3d9/10/11) and vkd3d-proton (d3d12).
    Translation,
    /// wine itself — its builtin PE DLLs (under the wine tree) and ELF loader.
    Wine,
    /// The game's own cost: native ELF, its PE exe/DLLs, JIT (mono/.NET), the C runtime.
    Game,
    /// Samples we can't place — `[unknown]`, unmapped.
    Other,
}

impl Layer {
    pub fn as_str(self) -> &'static str {
        match self {
            Layer::Kernel => "kernel",
            Layer::Driver => "driver",
            Layer::Translation => "dxvk/vkd3d",
            Layer::Wine => "wine",
            Layer::Game => "game",
            Layer::Other => "other",
        }
    }
}

/// What a thread does, read from its name: in degraded mode, with no kernel and no
/// foreign samples, the threads a spike's samples land in are much of what can tell its
/// causes apart. Names as measured across 26 real traces (2026-10-22); the kernel cuts a
/// `comm` at 15 characters, which every prefix here survives.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ThreadClass {
    /// DXVK's pipeline compile workers (`dxvk-shader-n`/`-l`/`-h`). The driver compiles
    /// on the calling thread, so driver time in these threads is shader compilation.
    ShaderCompile,
    /// Getting frames out: DXVK's command stream and submission threads, vkd3d-proton's
    /// queue/fence/swapchain threads, engine render threads.
    Render,
    /// Pipeline- and shader-cache file IO: `dxvk-cache`, `vkd3d-disk$`, and Mesa's
    /// `:disk$N` threads, which compress and write what the driver compiled on a miss.
    CacheIo,
    /// Engine job pools (Unreal, Unity, VRage, .NET).
    Worker,
    Audio,
    /// Wine and the runtime around it: wineserver, Wine's helper threads and services,
    /// pressure-vessel, Steam's launch wrapper.
    Runtime,
    /// The observer itself: MangoHud's sampler thread, inside the game's process.
    Observer,
    /// Anything else, the game's own main thread included (it carries the exe's name).
    Other,
}

impl ThreadClass {
    pub fn as_str(self) -> &'static str {
        match self {
            ThreadClass::ShaderCompile => "shader-compile",
            ThreadClass::Render => "render",
            ThreadClass::CacheIo => "cache-io",
            ThreadClass::Worker => "worker",
            ThreadClass::Audio => "audio",
            ThreadClass::Runtime => "runtime",
            ThreadClass::Observer => "observer",
            ThreadClass::Other => "other",
        }
    }
}

/// Classify a thread by its `comm`. Most specific first: `dxvk-shader-*` before the
/// rest of `dxvk-*`, the cache threads before the render ones.
pub fn thread_class(comm: &str) -> ThreadClass {
    let lower = comm.to_ascii_lowercase();
    let starts = |p: &[&str]| p.iter().any(|x| lower.starts_with(x));
    if starts(&["dxvk-shader"]) {
        ThreadClass::ShaderCompile
    } else if starts(&["dxvk-cache", "vkd3d-disk", ":disk$"]) {
        ThreadClass::CacheIo
    } else if starts(&["dxvk-", "vkd3d", "renderthread", "rhithread"]) {
        ThreadClass::Render
    } else if starts(&[
        "taskgraph",
        "poolthread",
        "job.worker",
        "vrageworker",
        ".net tp",
    ]) {
        ThreadClass::Worker
    } else if starts(&["audio", "fmod", "faudio"]) {
        ThreadClass::Audio
    } else if starts(&["mangohud"]) {
        ThreadClass::Observer
    } else if starts(&[
        "wine",
        "explorer.exe",
        "xalia.exe",
        "tabtip.exe",
        "services.exe",
        "rpcss.exe",
        "plugplay.exe",
        "svchost.exe",
        "pressure-vessel",
        "pv-",
        "steam-launch-wr",
        "reaper",
    ]) {
        ThreadClass::Runtime
    } else {
        ThreadClass::Other
    }
}

/// Whether a named sample sits in a GPU driver's shader compiler: Mesa's NIR passes and
/// SPIR-V front end, RADV's ACO, Intel's backend, or LLVM (radeonsi, llvmpipe). Thread
/// names cannot see this: vkd3d-proton compiles on the game's own threads, and DXVK's
/// workers spend part of their time elsewhere. Found on a real Saints Row trace, where
/// 92-96% of the game's samples in each stalled frame were `nir_*`/`aco::` on unnamed
/// threads (2026-10-22).
pub fn is_shader_compiler(module: &str, symbol: &str) -> bool {
    module.starts_with("libLLVM")
        || symbol.starts_with("nir_")
        || symbol.contains("_nir_")
        || symbol.starts_with("vtn_")
        || symbol.starts_with("spirv_to_nir")
        || symbol.contains("aco::")
        || symbol.starts_with("aco_")
        || symbol.starts_with("brw_")
        || symbol.starts_with("elk_")
}

/// Bucket a sample's DSO into a [`Layer`]. Ordered most-specific first and wine-agnostic:
/// it keys on the *kind* of module, not on a particular build — the translation DLLs and
/// driver libs carry the same names under any Proton/GE/SpritzWine tree, and wine's own
/// builtins are the PEs living under a `/lib/wine/` (or `/wine/`) path, which a game's own
/// PE never is.
pub fn classify_layer(dso: &str) -> Layer {
    let l = dso.to_ascii_lowercase();
    let base = l.rsplit('/').next().unwrap_or(&l);

    // perf pseudo-DSOs first — they have no path to key on. `[JIT] tid N` is the game's
    // JIT'd code; `[unknown]` is genuinely unbacked (perf couldn't map the ip to a module);
    // every *other* bracketed DSO is kernel-space — `[kernel.kallsyms]`, `[vdso]`, and the
    // in-kernel driver modules (`[amdgpu]`, `[nvidia]`, `[i915]`), i.e. GPU submit lands in
    // the kernel bucket, not lost to Other.
    if base.starts_with("[jit]") {
        return Layer::Game;
    }
    if l == "[unknown]" {
        return Layer::Other;
    }
    if l.starts_with('[') {
        return Layer::Kernel;
    }
    if !dso.starts_with('/') {
        return Layer::Other; // defensive: a non-path, non-bracket DSO
    }

    // D3D→Vulkan translation, by the well-known module names (shipped identically under any
    // wine tree, so the name — not the path — is the reliable key). Checked before wine so a
    // DXVK `d3d11.dll` dropped into the wine tree reads as translation, not wine.
    if base.starts_with("dxvk")
        || base.starts_with("vkd3d")
        || base.starts_with("libvkd3d")
        || matches!(
            base,
            "d3d9.dll"
                | "d3d10.dll"
                | "d3d10_1.dll"
                | "d3d10core.dll"
                | "d3d11.dll"
                | "d3d12.dll"
                | "d3d12core.dll"
                | "dxgi.dll"
        )
    {
        return Layer::Translation;
    }

    // wine's own builtins live under the wine tree; its ELF support is libwine*. A PE
    // anywhere else is the game's own DLL/exe. Checked *before* the driver keywords so wine's
    // own `winevulkan.dll` (a builtin PE under `/lib/wine/`) isn't misread as a GPU driver.
    if l.contains("/lib/wine/") || l.contains("/wine/") || base.starts_with("libwine") {
        return Layer::Wine;
    }

    // GPU userspace driver: Vulkan ICDs/loader, Mesa DRI, vendor libs (kept specific to
    // avoid swallowing unrelated `libgl*` like glib).
    if base.contains("vulkan")
        || base.ends_with("_dri.so")
        || base.contains("nvidia")
        || base.contains("radeonsi")
        || base.contains("zink")
        || base.starts_with("libdrm")
        || base.starts_with("libglx")
        || base.starts_with("libegl")
        || base.starts_with("libglapi")
        || base.starts_with("libgbm")
    {
        return Layer::Driver;
    }

    // Everything else with a real path is the game process's own userland.
    Layer::Game
}

/// One executable mapping from a perf `PERF_RECORD_MMAP2` record — the post-mortem stand-in
/// for `/proc/<pid>/maps` (the live pid is long gone by resolve time, but perf recorded the
/// map events into `perf.data`).
#[derive(Clone, Debug)]
pub struct Mapping {
    pub path: String,
    pub start: u64,
    pub size: u64,
    /// File offset of this segment (`@ 0x1000` in the record): RVA = ip − start + pgoff.
    pub pgoff: u64,
    /// The mapping process's pid (MMAP2's `pid/tid:` prefix) — lets a PE mapping be
    /// attributed to the process that mapped it, not just the capture as a whole.
    pub pid: i32,
}

impl Mapping {
    fn contains(&self, ip: u64) -> bool {
        ip >= self.start && ip < self.start.saturating_add(self.size)
    }
    fn rva(&self, ip: u64) -> u32 {
        (ip - self.start + self.pgoff) as u32
    }
}

/// A layer's sample tally: how many samples landed in it, and how many of those we could
/// name a function for. The gap (`samples − named`) is code we placed by *module* but
/// couldn't name inside — a stripped ELF without debuginfo, or a PE past any export — so
/// `named / samples` is the layer's symbol coverage: how much of its hot-function list is
/// real, and thus where installing debuginfo / the Full tier would actually reveal something.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LayerTally {
    pub layer: Layer,
    pub samples: u64,
    pub named: u64,
}

/// One aggregated hot symbol: how many samples landed in it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SymbolCount {
    /// DSO basename, e.g. `ntdll.dll` or `libvulkan_intel.so`.
    pub module: String,
    pub symbol: String,
    pub samples: u64,
}

/// The self-describing "issues" summary — what the UI binds an issues panel to.
#[derive(Clone, Debug)]
pub struct Diagnostics {
    pub tier: FidelityTier,
    pub total_samples: u64,
    /// Samples with no name: kernel/anonymous/`[unknown]` we couldn't attribute.
    pub unresolved_samples: u64,
    /// Real on-disk modules whose hot samples we couldn't name, biggest first — the
    /// actionable "why is this anonymous" list. The remedy depends on the module kind
    /// (the caller decides by extension): an ELF (`libvulkan_intel.so`) wants its debuginfo
    /// package; a wine PE past any export wants the `Full` (retained-DWARF) tier.
    pub anonymous_modules: Vec<(String, u64)>,
}

impl Diagnostics {
    pub fn unresolved_pct(&self) -> f64 {
        if self.total_samples == 0 {
            0.0
        } else {
            100.0 * self.unresolved_samples as f64 / self.total_samples as f64
        }
    }
}

/// The result of resolving a `perf.data`: hot symbols (descending) plus diagnostics.
#[derive(Clone, Debug)]
pub struct Resolution {
    pub symbols: Vec<SymbolCount>,
    pub diagnostics: Diagnostics,
    /// Every sample's [`LayerTally`], biggest first — the "who ate the frame" split the
    /// headline bar reads. Covers the whole trace (over `total_samples`), so it includes
    /// kernel/anonymous samples the named `symbols` list can't; each tally also carries how
    /// many of its samples we could name (the layer's symbol coverage).
    pub layers: Vec<LayerTally>,
    /// Best-effort wine build identifier from a PE module's path (`SpritzWine-Prater`,
    /// `Proton - Experimental`, …) — the artifact's `wine_flavor`. Derived ONLY from PE
    /// paths mapped by the dominant (highest-sample) pid, i.e. the game's own process —
    /// see [`decide_wine_flavor`]. `None` if that pid maps no PE, or maps one whose path
    /// doesn't name a known build.
    pub wine_flavor: Option<String>,
    /// Whether the dominant pid mapped ANY PE module — the honesty signal that lets a
    /// caller distinguish "native, no wine at all" (`false`) from "wine, but the flavor
    /// keyword didn't match" (`true` with `wine_flavor` still `None`). See
    /// [`decide_wine_flavor`].
    pub game_maps_pe: bool,
    /// Best-effort GPU vendor of the card that ran, from the driver-layer DSOs seen
    /// (`intel`/`amd`/`nvidia`/`qualcomm`) — the one with the most samples. Drives
    /// vendor-colored UI (Intel blue, AMD red, Nvidia green, …). `None` if no driver sample
    /// identified a vendor.
    pub gpu_vendor: Option<String>,
    /// One tally per requested [`Window`], in request order.
    pub windows: Vec<WindowTally>,
    /// Times of the first and last sample on perf's clock; `None` when perf.data
    /// carried no timestamps.
    pub sample_span: Option<(f64, f64)>,
}

/// A time range on perf's sample clock, in seconds, whose samples are also tallied on
/// their own. Only meaningful when the clock is one the caller knows (beisl records
/// with `-k CLOCK_MONOTONIC`); the resolver does not interpret it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Window {
    pub start: f64,
    pub end: f64,
}

/// The samples one [`Window`] holds: its own layer split, and how many of them belong to
/// the dominant pid (the game, by the same rule [`decide_wine_flavor`] uses).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct WindowTally {
    pub samples: u64,
    pub dominant_pid_samples: u64,
    /// Biggest first, ties in layer order, like [`Resolution::layers`].
    pub layers: Vec<LayerTally>,
    /// The window's hottest named functions, aggregated and ordered like
    /// [`Resolution::symbols`], at most [`WINDOW_SYMBOLS`].
    pub symbols: Vec<SymbolCount>,
    /// Samples per [`ThreadClass`], biggest first, ties in class order.
    pub threads: Vec<(ThreadClass, u64)>,
    /// Samples in a driver's shader compiler ([`is_shader_compiler`]), whatever thread
    /// ran them.
    pub compile: u64,
}

/// How many hot symbols a [`WindowTally`] keeps. A frame holds a handful of samples;
/// past the first few the counts are ones.
pub const WINDOW_SYMBOLS: usize = 10;

/// Best-effort GPU vendor from a driver DSO basename — the Mesa ICDs/DRI and the vendor
/// blobs all carry the vendor in their name, wherever the tree puts them. Used only for the
/// driver *layer* (see [`classify_layer`]); returns a stable lowercase tag.
pub fn gpu_vendor(dso: &str) -> Option<&'static str> {
    let l = dso.to_ascii_lowercase();
    let base = l.rsplit('/').next().unwrap_or(&l);
    if base.contains("intel") || base.contains("iris") || base.contains("_anv") {
        Some("intel")
    } else if base.contains("radeon") || base.contains("amdgpu") || base.contains("amdvlk") {
        Some("amd") // radeonsi / radv / amdvlk all match
    } else if base.contains("nvidia") || base.contains("nouveau") || base.contains("nvk") {
        Some("nvidia")
    } else if base.contains("freedreno") || base.contains("turnip") || base.contains("adreno") {
        Some("qualcomm")
    } else {
        None
    }
}

/// Resolve every sample in `perf_data` to a wine symbol, via `perf script`.
///
/// perf symbolizes ELF DSOs itself; PE DLLs come back `[unknown]` and are resolved here
/// against their on-disk export tables (each DLL parsed once, cached). Returns the hot
/// symbols aggregated by `(module, symbol)` and a [`Diagnostics`] summary.
///
/// Thin back-compat wrapper: same as [`resolve_perf_data_with_debuginfod`] with no
/// debuginfod URLs, i.e. today's behavior (the child inherits the ambient environment
/// unchanged). New callers that want reliable driver-symbol fetch should call that
/// function directly.
pub fn resolve_perf_data(perf_data: &Path) -> io::Result<Resolution> {
    resolve_perf_data_with_debuginfod(perf_data, None, &[])
}

/// Resolve every sample in `perf_data` to a wine symbol, via `perf script`, optionally
/// pointing `perf` at a debuginfod server for missing ELF debug symbols.
///
/// `perf` fetches missing ELF debuginfo on demand from debuginfod, but only if
/// `DEBUGINFOD_URLS` is set in *its* environment — and a trace resolved inside Steam's
/// game-launch session does not inherit an interactive login shell's exports. Left to
/// ambient inheritance, that means driver-layer symbols (the Mesa/Intel/AMD Vulkan ICDs —
/// the only layer debuginfod can serve; PE/JIT internals need the Full/DWARF tier
/// regardless) silently vanish depending on *how* beisl happened to be launched, not on
/// whether a debuginfod server is actually reachable. Passing `Some(urls)` here makes that
/// an explicit, reproducible choice instead of an accident of the calling shell.
///
/// `debuginfod_urls` is the same space-separated URL list `DEBUGINFOD_URLS` takes.
/// `None` leaves the var unset, so the child inherits the ambient environment exactly as
/// [`resolve_perf_data`] always has.
///
/// `windows` are tallied in the same pass, so a spike's split costs no second
/// `perf script` run.
pub fn resolve_perf_data_with_debuginfod(
    perf_data: &Path,
    debuginfod_urls: Option<&str>,
    windows: &[Window],
) -> io::Result<Resolution> {
    let mut cmd = Command::new("perf");
    cmd.args([
        "script",
        "--show-mmap-events",
        "-F",
        "comm,pid,time,ip,sym,dso",
        "-i",
    ])
    .arg(perf_data)
    .stdout(Stdio::piped())
    .stderr(Stdio::null());
    if let Some(urls) = debuginfod_urls {
        cmd.env("DEBUGINFOD_URLS", urls);
    }
    let mut child = cmd.spawn()?;
    let stdout = child.stdout.take().expect("piped");
    let reader = io::BufReader::new(stdout);

    let mut mappings: Vec<Mapping> = Vec::new();
    let mut export_cache: HashMap<String, Option<pe::ExportTable>> = HashMap::new();
    let mut counts: HashMap<(String, String), u64> = HashMap::new();
    let mut anon: HashMap<String, u64> = HashMap::new();
    let mut total: u64 = 0;
    let mut unresolved: u64 = 0;
    // Per layer: (samples, named) — the named count is the layer's symbol coverage.
    let mut layers: HashMap<Layer, (u64, u64)> = HashMap::new();
    // GPU vendor tally over driver-layer samples, so a mixed system picks the busiest card.
    let mut gpu_vendors: HashMap<&'static str, u64> = HashMap::new();
    let mut pe_paths: Vec<String> = Vec::new();
    // Per-pid signal for `decide_wine_flavor`: sample counts (who dominates the capture)
    // and the PE paths each pid mapped (a full `-a` capture samples every process on the
    // box, not just the game — see that function's doc for the CarX Street case this
    // guards against).
    let mut pid_samples: HashMap<i32, u64> = HashMap::new();
    let mut pid_pe_paths: HashMap<i32, Vec<String>> = HashMap::new();
    let mut win = WindowTallies::new(windows);
    let mut span: Option<(f64, f64)> = None;

    use io::BufRead as _;
    for line in reader.lines() {
        let line = line?;
        if let Some(m) = parse_mmap(&line) {
            if is_pe(&m.path) {
                if !pe_paths.contains(&m.path) {
                    pe_paths.push(m.path.clone());
                }
                let per_pid = pid_pe_paths.entry(m.pid).or_default();
                if !per_pid.contains(&m.path) {
                    per_pid.push(m.path.clone());
                }
            }
            mappings.push(m);
            continue;
        }
        // Other perf metadata records (`PERF_RECORD_MMAP` without the 2, COMM, FORK, …)
        // can carry trailing text `parse_sample` would misread as a `(dso)` and count as a
        // bogus unresolved sample, skewing `unresolved_pct`. They're never samples — skip.
        if line.contains("PERF_RECORD_") {
            continue;
        }
        let Some(sample) = parse_sample(&line) else {
            continue;
        };
        total += 1;
        *pid_samples.entry(sample.pid).or_insert(0) += 1;
        // Every sample lands in exactly one layer, whether or not we can name its function —
        // the split is over the whole trace, not just the resolved symbols.
        let layer = classify_layer(&sample.dso);
        if layer == Layer::Driver {
            if let Some(v) = gpu_vendor(&sample.dso) {
                *gpu_vendors.entry(v).or_insert(0) += 1;
            }
        }

        // A confident function name for this sample, or None. Both paths aggregate by
        // *name* (offset stripped) so a function's samples coalesce into one hot row.
        let named: Option<String> = if sample.sym != "[unknown]" {
            // perf already symbolized it (ELF).
            Some(
                sample
                    .sym
                    .split('+')
                    .next()
                    .unwrap_or(&sample.sym)
                    .to_string(),
            )
        } else if is_pe(&sample.dso) {
            // PE: nearest export — but only trust it when the offset is small. A dense DLL
            // (ntdll: 1476 exports) puts every sample a few bytes past a real export; a
            // sparse one (d3d12core: ~1 export) would otherwise map a megabyte of internal
            // code onto that single name. Beyond NEAR_EXPORT that's noise, not a function —
            // honest to leave unnamed (the Full/DWARF tier is what names it).
            resolve_pe(&sample, &mappings, &mut export_cache)
                .filter(|&(_, off)| off <= NEAR_EXPORT)
                .map(|(name, _)| name)
        } else {
            None
        };

        let tally = layers.entry(layer).or_insert((0, 0));
        tally.0 += 1;
        let module = basename(&sample.dso);
        if let Some(t) = sample.time {
            span = Some(span.map_or((t, t), |(lo, hi)| (lo.min(t), hi.max(t))));
            win.add(
                t,
                sample.pid,
                &sample.comm,
                layer,
                named.as_deref().map(|s| (module, s)),
            );
        }
        match named {
            Some(sym) => {
                tally.1 += 1;
                *counts.entry((module.to_string(), sym)).or_insert(0) += 1;
            }
            None => {
                unresolved += 1;
                // A real on-disk module we couldn't name (ELF w/o debuginfo, or a PE past
                // any export) is an actionable issue; kernel/`[unknown]`/JIT pseudo-DSOs
                // are not.
                if is_real_dso(&sample.dso) {
                    *anon.entry(module.to_string()).or_insert(0) += 1;
                }
            }
        }
    }
    // `perf script` on a corrupt/unsupported perf.data exits nonzero (its stderr is
    // /dev/null); with nothing parsed that's a silent failure, so surface it rather than
    // returning an empty resolution as if the trace were merely quiet.
    let status = child.wait()?;
    if !status.success() && total == 0 {
        return Err(io::Error::other(
            "perf script produced no samples — unreadable or unsupported perf.data?",
        ));
    }

    let symbols = sorted_symbols(counts);

    let mut anonymous_modules: Vec<(String, u64)> = anon.into_iter().collect();
    anonymous_modules.sort_by_key(|&(_, n)| std::cmp::Reverse(n));
    anonymous_modules.truncate(8);

    let layers = sorted_tallies(layers);

    let gpu_vendor = gpu_vendors
        .into_iter()
        .max_by_key(|&(_, n)| n)
        .map(|(v, _)| v.to_string());

    let (flavor, game_maps_pe) = decide_wine_flavor(&pid_samples, &pid_pe_paths);
    // The dominant pid is only known once every sample is in.
    let windows = win.finish(dominant_pid(&pid_samples));

    Ok(Resolution {
        diagnostics: Diagnostics {
            // `detect_tier` deliberately still looks at every PE path in the whole
            // capture, not just the dominant pid's — scoping the symbol-fidelity tier the
            // same way `wine_flavor` is scoped is out of scope here (this item is about
            // the flavor claim, not tier detection).
            tier: detect_tier(&pe_paths),
            total_samples: total,
            unresolved_samples: unresolved,
            anonymous_modules,
        },
        layers,
        wine_flavor: flavor,
        game_maps_pe,
        gpu_vendor,
        symbols,
        windows,
        sample_span: span,
    })
}

/// The requested windows' counts, filled sample by sample in the one `perf script` pass.
/// A sample counts in every window holding its time. Memory grows with the distinct pids
/// and functions each window sees, never with its samples.
struct WindowTallies<'a> {
    windows: &'a [Window],
    accs: Vec<WindowAcc>,
}

#[derive(Default)]
struct WindowAcc {
    /// layer -> (samples, named)
    layers: HashMap<Layer, (u64, u64)>,
    pids: HashMap<i32, u64>,
    symbols: HashMap<(String, String), u64>,
    threads: HashMap<ThreadClass, u64>,
    compile: u64,
}

impl<'a> WindowTallies<'a> {
    fn new(windows: &'a [Window]) -> Self {
        let accs = windows.iter().map(|_| WindowAcc::default()).collect();
        Self { windows, accs }
    }

    /// One sample at perf time `t`; `named` is its `(module basename, function)`.
    fn add(&mut self, t: f64, pid: i32, comm: &str, layer: Layer, named: Option<(&str, &str)>) {
        for (w, acc) in self.windows.iter().zip(&mut self.accs) {
            if t < w.start || t >= w.end {
                continue;
            }
            let l = acc.layers.entry(layer).or_insert((0, 0));
            l.0 += 1;
            *acc.pids.entry(pid).or_insert(0) += 1;
            *acc.threads.entry(thread_class(comm)).or_insert(0) += 1;
            if named.is_some_and(|(module, sym)| is_shader_compiler(module, sym)) {
                acc.compile += 1;
            }
            if let Some((module, sym)) = named {
                l.1 += 1;
                *acc.symbols
                    .entry((module.to_string(), sym.to_string()))
                    .or_insert(0) += 1;
            }
        }
    }

    fn finish(self, dominant: Option<i32>) -> Vec<WindowTally> {
        self.accs
            .into_iter()
            .map(|acc| {
                let mut symbols = sorted_symbols(acc.symbols);
                symbols.truncate(WINDOW_SYMBOLS);
                WindowTally {
                    samples: acc.pids.values().sum(),
                    dominant_pid_samples: dominant
                        .and_then(|p| acc.pids.get(&p).copied())
                        .unwrap_or(0),
                    layers: sorted_tallies(acc.layers),
                    symbols,
                    compile: acc.compile,
                    threads: {
                        let mut t: Vec<(ThreadClass, u64)> = acc.threads.into_iter().collect();
                        t.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
                        t
                    },
                }
            })
            .collect()
    }
}

/// Most samples first; ties by module, then name, so equal counts don't come out in hash
/// order.
fn sorted_symbols(counts: HashMap<(String, String), u64>) -> Vec<SymbolCount> {
    let mut v: Vec<SymbolCount> = counts
        .into_iter()
        .map(|((module, symbol), samples)| SymbolCount {
            module,
            symbol,
            samples,
        })
        .collect();
    v.sort_by(|a, b| {
        b.samples
            .cmp(&a.samples)
            .then_with(|| a.module.cmp(&b.module))
            .then_with(|| a.symbol.cmp(&b.symbol))
    });
    v
}

/// Descending by share; ties broken by the layer's stable order so the bar is deterministic.
fn sorted_tallies(by_layer: HashMap<Layer, (u64, u64)>) -> Vec<LayerTally> {
    let mut v: Vec<LayerTally> = by_layer
        .into_iter()
        .map(|(layer, (samples, named))| LayerTally {
            layer,
            samples,
            named,
        })
        .collect();
    v.sort_by(|a, b| {
        b.samples
            .cmp(&a.samples)
            .then((a.layer as u8).cmp(&(b.layer as u8)))
    });
    v
}

/// Decide the wine flavor — or honest nativeness — from per-pid sample counts and the PE
/// paths each pid mapped. A full (`-a`, system-wide) `perf` capture samples EVERY process
/// on the box, not just the game, so a whole-capture union of PE paths lets a stray
/// background wine process make a native game's trace claim a flavor it never ran (real
/// case: CarX Street, native, misdetected as SpritzWine-Prater from an unrelated wine
/// process elsewhere in the capture). The fix: approximate the GAME as the pid with the
/// most samples — in a gaming trace the game dominates, and it's the only per-pid signal
/// that survives to resolve time (the live pids are long gone) — and derive the flavor
/// ONLY from that pid's own PE mappings.
///
/// Returns `(flavor, game_maps_pe)`. `game_maps_pe` is what lets a caller say "native" (no
/// PE at all) rather than a silent `None` that would otherwise read identically to "wine,
/// but the flavor keyword didn't match anything known".
///
/// Known accepted limitation: a wineserver-dominated capture (a native ELF process that
/// sync-hammers wineserver harder than it runs its own code) would out-sample the actual
/// game and still map no PE — a false "native" verdict this pid-dominance heuristic can't
/// distinguish from the real thing.
fn decide_wine_flavor(
    pid_samples: &HashMap<i32, u64>,
    pid_pe_paths: &HashMap<i32, Vec<String>>,
) -> (Option<String>, bool) {
    let Some(dominant) = dominant_pid(pid_samples) else {
        return (None, false);
    };
    let paths = pid_pe_paths
        .get(&dominant)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    (paths.iter().find_map(|p| wine_flavor(p)), !paths.is_empty())
}

/// The pid with the most samples: the game, in a gaming trace. `max_by_key` over a
/// HashMap would otherwise pick nondeterministically among tied sample counts (iteration
/// order isn't stable), so the same perf.data could flip verdicts between runs; break ties
/// by lowest pid: arbitrary, but fixed and repeatable.
fn dominant_pid(pid_samples: &HashMap<i32, u64>) -> Option<i32> {
    pid_samples
        .iter()
        .max_by_key(|&(&pid, &n)| (n, std::cmp::Reverse(pid)))
        .map(|(&pid, _)| pid)
}

/// Best-effort wine build name from a PE module path. A wine tree is `<build>/lib/wine/
/// <arch>-windows/ntdll.dll`; the identifying segment is the most specific ancestor of
/// `lib/wine` whose name mentions wine/proton/spritz/prater/ge — e.g.
/// `…/SpritzWine-Prater/build/redist/files/lib/wine/…` → `SpritzWine-Prater`. `None` when
/// nothing matches (we'd rather say nothing than guess a flavor for an A/B).
fn wine_flavor(pe_path: &str) -> Option<String> {
    let left = pe_path.split("/lib/wine/").next()?;
    left.split('/')
        .rfind(|seg| {
            let l = seg.to_ascii_lowercase();
            ["proton", "wine", "spritz", "prater", "ge-", "lutris"]
                .iter()
                .any(|k| l.contains(k))
        })
        .map(str::to_string)
}

/// Fidelity tier from the wine PE modules seen: DWARF in the main `ntdll.dll` → `Full`,
/// any wine PE without it → `Exports`, none → `Unknown`. (`Distro` — system wine with
/// debuginfo — is a later refinement; the spike distinguishes the two that gate A/B.)
pub fn detect_tier(pe_paths: &[String]) -> FidelityTier {
    if pe_paths.is_empty() {
        return FidelityTier::Unknown;
    }
    let ntdll = pe_paths
        .iter()
        .find(|p| basename(p).eq_ignore_ascii_case("ntdll.dll"));
    if let Some(p) = ntdll {
        if let Ok(bytes) = std::fs::read(p) {
            if pe::has_dwarf(&bytes) {
                return FidelityTier::Full;
            }
        }
    }
    FidelityTier::Exports
}

struct Sample {
    /// The thread's name, as perf printed it (spaces kept, runs of them collapsed).
    comm: String,
    pid: i32,
    /// perf's sample time in seconds; `None` for a line printed without `time`.
    time: Option<f64>,
    ip: u64,
    sym: String,
    dso: String,
}

/// Parse a `perf script` sample line: `<comm...> <pid> [<time>:] <ip> <sym...> (<dso>)`.
///
/// Both the comm and the symbol can hold spaces (engine thread names, demangled C++ such
/// as `aco::(anonymous namespace)::...`), and the dso can hold parentheses (Wine's
/// `/memfd:wine-mapping (deleted)`). So the dso is the line's final balanced group, and
/// with a time on the line (beisl always asks for one) the rest is anchored from the left
/// on `<pid> <time>: <ip>`. Reading from the right instead dropped 48% of a .NET game's
/// samples and 12% of one whose stalls were the shader compiler (2026-10-22).
fn parse_sample(line: &str) -> Option<Sample> {
    let (head, dso) = split_dso(line.trim_end())?;
    let toks: Vec<&str> = head.split_whitespace().collect();
    if let Some(k) = toks.iter().position(|t| is_time(t)).filter(|&k| k >= 1) {
        let ip = u64::from_str_radix(toks.get(k + 1)?.trim_start_matches("0x"), 16).ok()?;
        let sym = toks.get(k + 2..).filter(|s| !s.is_empty())?.join(" ");
        return Some(Sample {
            comm: toks[..k - 1].join(" "),
            pid: toks[k - 1].parse().ok()?,
            time: toks[k].trim_end_matches(':').parse().ok(),
            ip,
            sym,
            dso,
        });
    }
    // No time column: `<comm...> <pid> <ip> <sym>`, read from the right, the one shape
    // that has no anchor to read from the left.
    let mut rev = toks.iter().rev();
    let sym = rev.next()?.to_string();
    let ip = u64::from_str_radix(rev.next()?.trim_start_matches("0x"), 16).ok()?;
    let pid = rev.next()?.parse().ok()?;
    let mut comm: Vec<&str> = rev.copied().collect();
    comm.reverse();
    Some(Sample {
        comm: comm.join(" "),
        pid,
        time: None,
        ip,
        sym,
        dso,
    })
}

/// The line's final balanced parenthesis group as the dso, and what comes before it.
fn split_dso(line: &str) -> Option<(&str, String)> {
    let body = line.strip_suffix(')')?;
    let mut depth = 1;
    for (i, c) in body.char_indices().rev() {
        match c {
            ')' => depth += 1,
            '(' => {
                depth -= 1;
                if depth == 0 {
                    let dso = &body[i + 1..];
                    return (!dso.is_empty()).then(|| (&body[..i], dso.to_string()));
                }
            }
            _ => {}
        }
    }
    None
}

/// perf's sample time: `12756.875611:`.
fn is_time(tok: &str) -> bool {
    tok.strip_suffix(':')
        .is_some_and(|t| t.contains('.') && t.parse::<f64>().is_ok())
}

/// Parse a `PERF_RECORD_MMAP2` line into a [`Mapping`]:
/// `… PERF_RECORD_MMAP2 pid/tid: [0xSTART(0xSIZE) @ 0xPGOFF …]: prot /path`.
fn parse_mmap(line: &str) -> Option<Mapping> {
    let after = line.split_once("PERF_RECORD_MMAP2")?.1;
    let after_trimmed = after.trim_start();
    let slash = after_trimmed.find('/')?;
    let pid: i32 = after_trimmed[..slash].parse().ok()?;
    let lb = after.find('[')?;
    let paren = after[lb..].find('(')? + lb;
    let rparen = after[paren..].find(')')? + paren;
    let at = after[rparen..].find("@ ")? + rparen + 2;
    let at_end = after[at..].find([' ', ']'])? + at;
    let start =
        u64::from_str_radix(after[lb + 1..paren].trim().trim_start_matches("0x"), 16).ok()?;
    let size = u64::from_str_radix(after[paren + 1..rparen].trim_start_matches("0x"), 16).ok()?;
    let pgoff = u64::from_str_radix(after[at..at_end].trim_start_matches("0x"), 16).ok()?;
    // `]: prot /path` — path is everything after the prot token.
    let tail = after.split_once("]:")?.1.trim_start();
    let path = tail.split_once(' ')?.1.trim().to_string();
    if path.is_empty() {
        return None;
    }
    Some(Mapping {
        path,
        start,
        size,
        pgoff,
        pid,
    })
}

/// Offset past the nearest export beyond which the name is noise, not a function (see the
/// classify site). Chosen to accept a densely-exported DLL's neighborhood (ntdll averages
/// ~340 bytes between exports) and reject a sparsely-exported one's megabyte-deep internals.
const NEAR_EXPORT: u32 = 0x8000;

/// The nearest export to this PE sample as `(name, offset_past_it)`, or `None` if the ip
/// isn't in a mapping we have, the DLL isn't on disk, or it carries no export table.
fn resolve_pe(
    sample: &Sample,
    mappings: &[Mapping],
    cache: &mut HashMap<String, Option<pe::ExportTable>>,
) -> Option<(String, u32)> {
    let m = mappings
        .iter()
        .rev()
        .find(|m| m.path == sample.dso && m.contains(sample.ip))?;
    let rva = m.rva(sample.ip);
    let table = cache.entry(sample.dso.clone()).or_insert_with(|| {
        std::fs::read(&sample.dso)
            .ok()
            .and_then(|b| pe::ExportTable::parse(&b))
    });
    let (name, off) = table.as_ref()?.resolve(rva)?;
    Some((name.to_string(), off))
}

fn basename(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

fn is_pe(path: &str) -> bool {
    let l = path.to_ascii_lowercase();
    l.ends_with(".dll") || l.ends_with(".exe")
}

/// A real on-disk DSO (has a path), not a perf pseudo-DSO (`[kernel]`, `[unknown]`,
/// `[JIT] …`).
fn is_real_dso(dso: &str) -> bool {
    dso.starts_with('/')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_sample_line_with_spaced_comm() {
        let s = parse_sample(
            "  wine_rpcrt4_io 80406  6ffffff8f8df [unknown] (/lib/wine/x86_64-windows/ntdll.dll)",
        )
        .unwrap();
        assert_eq!(s.pid, 80406);
        assert_eq!(s.comm, "wine_rpcrt4_io");
        assert_eq!(s.ip, 0x6ffffff8f8df);
        assert_eq!(s.sym, "[unknown]");
        assert_eq!(s.dso, "/lib/wine/x86_64-windows/ntdll.dll");
        // A name with a space survives: engines number their threads that way.
        let t = parse_sample("RenderThread 1 4242 1.5: 7f00 foo (/x/game.so)").unwrap();
        assert_eq!(
            (t.comm.as_str(), t.pid, t.time),
            ("RenderThread 1", 4242, Some(1.5))
        );
    }

    #[test]
    fn the_drivers_compiler_is_found_by_symbol() {
        for (module, sym) in [
            ("libvulkan_radeon.so", "nir_opt_algebraic"),
            (
                "libvulkan_radeon.so",
                "aco::(anonymous namespace)::schedule_block",
            ),
            ("libvulkan_radeon.so", "ac_nir_lower_ngg"),
            ("libvulkan_intel.so", "brw_compile_fs"),
            ("libvulkan_intel.so", "vtn_handle_body_instruction"),
            ("libLLVM.so.19", "llvm::SelectionDAG::Combine"),
        ] {
            assert!(is_shader_compiler(module, sym), "{module} {sym}");
        }
        for (module, sym) in [
            ("libvulkan_radeon.so", "radv_CmdDrawIndexed"),
            ("libc.so.6", "__memcpy_avx_unaligned_erms"),
            ("game.exe", "UpdateNirvana"),
        ] {
            assert!(!is_shader_compiler(module, sym), "{module} {sym}");
        }
    }

    /// Thread names as they appear in real traces (2026-10-22), 15-character kernel cut
    /// included.
    #[test]
    fn thread_names_sort_into_what_the_thread_does() {
        use ThreadClass::*;
        for (comm, class) in [
            ("dxvk-shader-n", ShaderCompile),
            ("dxvk-shader-h", ShaderCompile),
            ("dxvk-cache", CacheIo),
            ("vkd3d-disk$", CacheIo),
            (":disk$0", CacheIo),
            ("dxvk-cs", Render),
            ("dxvk-submit", Render),
            ("vkd3d_queue", Render),
            ("vkd3d-swapchain", Render),
            ("RenderThread 1", Render),
            ("RHIThread", Render),
            ("TaskGraphThread", Worker),
            ("PoolThread 12", Worker),
            ("Job.Worker 3", Worker),
            ("VRageWorker 9", Worker),
            (".NET TP Worker", Worker),
            ("audio_client_ma", Audio),
            ("FMOD stream thr", Audio),
            ("FAudio_AudioCli", Audio),
            ("mangohud-amdgpu", Observer),
            ("wineserver", Runtime),
            ("wine_rpcrt4_io", Runtime),
            ("explorer.exe", Runtime),
            ("xalia.exe", Runtime),
            ("pressure-vessel", Runtime),
            ("AssettoCorsa.ex", Other),
            ("Icarus-Win64-Sh", Other),
        ] {
            assert_eq!(thread_class(comm), class, "{comm}");
        }
    }

    #[test]
    fn parses_an_elf_resolved_sample() {
        let s = parse_sample("game 123 7f0011 __memcpy_avx+0x2a (/usr/lib/libc.so.6)").unwrap();
        assert_eq!(s.pid, 123);
        assert_eq!(s.sym, "__memcpy_avx+0x2a");
        assert_eq!(basename(&s.dso), "libc.so.6");
    }

    #[test]
    fn parses_a_timed_sample_line() {
        // Real `-F comm,pid,time,ip,sym,dso` output (vkcube, 2026-10-22).
        let s = parse_sample(
            "          vkcube   93767  2389.770379:      7f6eaa51547a util_set_thread_affinity \
             (/usr/lib64/libvulkan_intel.so)",
        )
        .unwrap();
        assert_eq!(s.pid, 93767);
        assert_eq!(s.time, Some(2389.770379));
        assert_eq!(s.ip, 0x7f6eaa51547a);
        assert_eq!(s.sym, "util_set_thread_affinity");
        // Without the field nothing else moves.
        let untimed = parse_sample("game 123 7f0011 foo (/usr/lib/libc.so.6)").unwrap();
        assert_eq!((untimed.pid, untimed.time), (123, None));
    }

    #[test]
    fn a_window_tallies_its_own_hot_symbols() {
        let windows = [
            Window {
                start: 1.0,
                end: 2.0,
            },
            Window {
                start: 1.5,
                end: 3.0,
            },
        ];
        let mut w = WindowTallies::new(&windows);
        let blorp = Some(("libvulkan_intel.so", "blorp_exec"));
        w.add(1.2, 7, "dxvk-shader-n", Layer::Driver, blorp);
        w.add(1.2, 7, "dxvk-shader-n", Layer::Driver, blorp);
        // Where the windows overlap a sample counts in both.
        w.add(1.6, 7, "dxvk-shader-n", Layer::Driver, blorp);
        let names: Vec<String> = (0..11).map(|i| format!("f{i:02}")).collect();
        for n in &names {
            w.add(1.3, 9, "Game.exe", Layer::Game, Some(("libc.so.6", n)));
        }
        // Unnamed: in the counts and the split, never in the list.
        w.add(1.4, 7, "dxvk-cs", Layer::Other, None);
        // Half-open: the first window's end belongs to the second only.
        w.add(2.0, 7, "vkcube", Layer::Game, Some(("vkcube", "main")));
        w.add(0.5, 7, "vkcube", Layer::Game, Some(("vkcube", "early")));

        let t = w.finish(Some(7));
        assert_eq!((t[0].samples, t[0].dominant_pid_samples), (15, 4));
        assert_eq!(t[0].layers[0].layer, Layer::Game);
        assert_eq!((t[0].layers[0].samples, t[0].layers[0].named), (11, 11));
        assert_eq!(
            t[0].layers
                .iter()
                .find(|l| l.layer == Layer::Other)
                .unwrap()
                .named,
            0
        );
        // Capped at the ten hottest, ties by module then name.
        assert_eq!(t[0].symbols.len(), WINDOW_SYMBOLS);
        assert_eq!(
            (t[0].symbols[0].symbol.as_str(), t[0].symbols[0].samples),
            ("blorp_exec", 3)
        );
        let rest: Vec<&str> = t[0].symbols[1..]
            .iter()
            .map(|s| s.symbol.as_str())
            .collect();
        assert_eq!(
            rest,
            &names.iter().map(String::as_str).collect::<Vec<_>>()[..9]
        );

        assert_eq!((t[1].samples, t[1].dominant_pid_samples), (2, 2));
        let second: Vec<(&str, u64)> = t[1]
            .symbols
            .iter()
            .map(|s| (s.module.as_str(), s.samples))
            .collect();
        assert_eq!(second, [("libvulkan_intel.so", 1), ("vkcube", 1)]);

        // Thread classes beside the layers: 11 game-thread samples, 3 compile, 1 render.
        assert_eq!(
            t[0].threads,
            [
                (ThreadClass::Other, 11),
                (ThreadClass::ShaderCompile, 3),
                (ThreadClass::Render, 1)
            ]
        );
    }

    /// Real lines from 2026-10-22 traces that the right-anchored parse dropped.
    #[test]
    fn parses_parenthesised_dsos_and_spaced_symbols() {
        let memfd = parse_sample(
            "     Main Thread  173967 12756.875611:      6fff9ce5019a [unknown] \
             (/memfd:wine-mapping (deleted))",
        )
        .unwrap();
        assert_eq!(memfd.dso, "/memfd:wine-mapping (deleted)");
        assert_eq!((memfd.comm.as_str(), memfd.pid), ("Main Thread", 173967));
        assert_eq!(memfd.sym, "[unknown]");
        assert_eq!(memfd.ip, 0x6fff9ce5019a);

        let aco = parse_sample(
            "  SRLauncher.exe  415938 20972.320303:      7ff47928f5c7 \
             aco::(anonymous namespace)::add_coupling_code(aco::(anonymous namespace)::\
             exec_ctx&, aco::Block*) (/run/host/usr/local/lib64/libvulkan_radeon.so)",
        )
        .unwrap();
        assert_eq!(aco.dso, "/run/host/usr/local/lib64/libvulkan_radeon.so");
        assert_eq!(aco.time, Some(20972.320303));
        assert!(aco
            .sym
            .starts_with("aco::(anonymous namespace)::add_coupling_code("));
        assert!(aco.sym.ends_with("aco::Block*)"));

        // A thread name that is all digits after a space must not be read as the pid.
        let pool = parse_sample("PoolThread 12 4242 1.5: 7f00 foo bar (/x/game.so)").unwrap();
        assert_eq!(
            (pool.comm.as_str(), pool.pid, pool.sym.as_str()),
            ("PoolThread 12", 4242, "foo bar")
        );
        assert!(parse_sample("x 1 2.5: 7f00 sym (unbalanced").is_none());
    }

    #[test]
    fn parses_a_real_mmap2_record() {
        let m = parse_mmap(
            "    wineboot.exe   80406 [012]  4970.137089: PERF_RECORD_MMAP2 80406/80406: \
             [0x6ffffff31000(0x7b000) @ 0x1000 103:01 43010604 2607827909]: r-xp \
             /opt/wine/x86_64-windows/ntdll.dll",
        )
        .unwrap();
        assert_eq!(m.pid, 80406);
        assert_eq!(m.start, 0x6ffffff31000);
        assert_eq!(m.size, 0x7b000);
        assert_eq!(m.pgoff, 0x1000);
        assert_eq!(m.path, "/opt/wine/x86_64-windows/ntdll.dll");
        // Image base = start − pgoff; the live sample RVA the Python prototype validated.
        assert_eq!(m.rva(0x6ffffff8f8df), 0x5f8df);
    }

    #[test]
    fn non_mmap_and_junk_lines_are_skipped() {
        assert!(parse_mmap("game 1 2.3: PERF_RECORD_COMM: foo").is_none());
        assert!(parse_sample("# comment").is_none());
        assert!(parse_sample("no parens here").is_none());
    }

    #[test]
    fn tier_is_unknown_without_pe_modules() {
        assert_eq!(detect_tier(&[]), FidelityTier::Unknown);
    }

    #[test]
    fn wine_flavor_picks_the_identifying_build_segment() {
        // Synthetic build names, one per recognised keyword — no real product/version.
        assert_eq!(
            wine_flavor("/opt/example-wine-build/files/lib/wine/x86_64-windows/ntdll.dll")
                .as_deref(),
            Some("example-wine-build")
        );
        assert_eq!(
            wine_flavor("/games/some-proton-x/files/lib/wine/x86_64-windows/ntdll.dll").as_deref(),
            Some("some-proton-x")
        );
        // The most specific matching ancestor wins over an earlier one.
        assert_eq!(
            wine_flavor("/lutris-root/runners/custom-wine/lib/wine/x86_64-windows/ntdll.dll")
                .as_deref(),
            Some("custom-wine")
        );
        // A path with nothing wine-identifying yields None rather than a bad guess.
        assert_eq!(
            wine_flavor("/opt/game/bin/x/lib/wine/x86_64-windows/ntdll.dll"),
            None
        );
    }

    /// The scoping fix's test matrix: a full (`-a`) capture's per-pid samples/PE-paths in,
    /// flavor/nativeness out. Covers the real CarX Street misdetection (native game, a
    /// stray unrelated wine process elsewhere in the same system-wide capture) plus the
    /// other three shapes the decision must get right.
    #[test]
    fn decide_wine_flavor_scopes_to_the_dominant_pid() {
        // Native game dominates the capture; a stray, unrelated wine process (few samples)
        // is also in the (`-a`) capture and maps a recognisable wine PE. Must NOT leak into
        // the game's verdict — real CarX Street case.
        let mut samples = HashMap::new();
        samples.insert(100, 10_000); // the game: native, no PE
        samples.insert(200, 5); // a stray background wine proc
        let mut pe_paths = HashMap::new();
        pe_paths.insert(
            200,
            vec!["/opt/some-proton-build/files/lib/wine/x86_64-windows/ntdll.dll".to_string()],
        );
        assert_eq!(
            decide_wine_flavor(&samples, &pe_paths),
            (None, false),
            "a stray wine pid must not make a native game claim a flavor"
        );

        // The wine game's own process dominates and maps a recognisable build: flavor found.
        let mut samples = HashMap::new();
        samples.insert(100, 10_000);
        let mut pe_paths = HashMap::new();
        pe_paths.insert(
            100,
            vec!["/opt/some-proton-build/files/lib/wine/x86_64-windows/ntdll.dll".to_string()],
        );
        assert_eq!(
            decide_wine_flavor(&samples, &pe_paths),
            (Some("some-proton-build".to_string()), true)
        );

        // The dominant pid maps a PE, but its path names no known build keyword: PE
        // mapped (honestly wine) but flavor stays unmatched, not silently "native".
        let mut samples = HashMap::new();
        samples.insert(100, 10_000);
        let mut pe_paths = HashMap::new();
        pe_paths.insert(
            100,
            vec!["/opt/nowhere-marker/files/lib/wine/x86_64-windows/ntdll.dll".to_string()],
        );
        assert_eq!(decide_wine_flavor(&samples, &pe_paths), (None, true));

        // An empty capture claims nothing at all — the most honest answer available.
        assert_eq!(
            decide_wine_flavor(&HashMap::new(), &HashMap::new()),
            (None, false)
        );
    }

    /// Equal sample counts must not leave the verdict up to `HashMap` iteration order — the
    /// same perf.data has to resolve to the same answer every time. pid 100 and pid 300 tie
    /// at 10,000 samples; the lower pid (100, native) must win regardless of insertion order.
    #[test]
    fn decide_wine_flavor_breaks_sample_ties_deterministically() {
        let mut samples = HashMap::new();
        samples.insert(300, 10_000); // tied wine pid, inserted first, higher pid
        samples.insert(100, 10_000); // tied native pid, lower pid — must win the tie
        let mut pe_paths = HashMap::new();
        pe_paths.insert(
            300,
            vec!["/opt/some-proton-build/files/lib/wine/x86_64-windows/ntdll.dll".to_string()],
        );
        assert_eq!(
            decide_wine_flavor(&samples, &pe_paths),
            (None, false),
            "the lower pid must win a sample-count tie, deterministically"
        );
    }

    /// Opt-in full-pipeline proof: set `BEISL_TEST_PERF` to a `perf.data` from a real wine
    /// trace (Proton, GE, SpritzWine/Prater, …) to resolve it end to end. Skipped otherwise
    /// — no baked-in path, so the parser unit tests carry correctness on any machine.
    #[test]
    fn resolves_a_real_perf_data_when_provided() {
        let Some(path) = std::env::var_os("BEISL_TEST_PERF") else {
            return;
        };
        let res = resolve_perf_data(Path::new(&path)).expect("resolves");
        assert!(res.diagnostics.total_samples > 0, "no samples in the trace");
        // Every sample is bucketed, so the layer shares cover the whole trace.
        assert_eq!(
            res.layers.iter().map(|t| t.samples).sum::<u64>(),
            res.diagnostics.total_samples,
            "layer tallies must sum to every sample"
        );
        // A wine trace stamps a real tier and names at least some PE functions.
        assert_ne!(res.diagnostics.tier, FidelityTier::Unknown);
        assert!(
            res.symbols
                .iter()
                .any(|s| s.module.to_ascii_lowercase().ends_with(".dll")),
            "no PE symbols resolved"
        );
    }

    #[test]
    fn classify_layer_buckets_by_module_kind() {
        use Layer::*;
        // kernel + pseudo-DSOs
        assert_eq!(classify_layer("[kernel.kallsyms]"), Kernel);
        assert_eq!(classify_layer("[vdso]"), Kernel);
        // an in-kernel GPU driver module is kernel-space, not Other
        assert_eq!(classify_layer("[amdgpu]"), Kernel);
        assert_eq!(classify_layer("[nvidia]"), Kernel);
        assert_eq!(classify_layer("[JIT] tid 4242"), Game);
        assert_eq!(classify_layer("[unknown]"), Other);
        // translation, by name, wherever the tree puts it
        assert_eq!(
            classify_layer("/opt/w/x86_64-windows/dxgi.dll"),
            Translation
        );
        assert_eq!(
            classify_layer("/opt/w/x86_64-windows/d3d11.dll"),
            Translation
        );
        assert_eq!(
            classify_layer("/prefix/system32/vkd3d-proton.dll"),
            Translation
        );
        // GPU driver
        assert_eq!(classify_layer("/usr/lib/libvulkan_intel.so"), Driver);
        assert_eq!(classify_layer("/usr/lib/dri/radeonsi_dri.so"), Driver);
        assert_eq!(classify_layer("/usr/lib/libnvidia-glcore.so.560"), Driver);
        // a lookalike that must NOT be read as a driver
        assert_eq!(classify_layer("/usr/lib/libglib-2.0.so.0"), Game);
        // wine builtins live under the wine tree; a same-named PE elsewhere is the game's
        assert_eq!(
            classify_layer("/opt/w/lib/wine/x86_64-windows/ntdll.dll"),
            Wine
        );
        // wine's own winevulkan.dll is a builtin, not a GPU driver (wine check wins)
        assert_eq!(
            classify_layer("/opt/w/lib/wine/x86_64-windows/winevulkan.dll"),
            Wine
        );
        assert_eq!(classify_layer("/games/Foo/bin/Foo.exe"), Game);
        assert_eq!(classify_layer("/usr/lib/libc.so.6"), Game);
    }

    #[test]
    fn gpu_vendor_reads_the_driver_basename() {
        assert_eq!(gpu_vendor("/usr/lib/libvulkan_intel.so"), Some("intel"));
        assert_eq!(gpu_vendor("/usr/lib/dri/iris_dri.so"), Some("intel"));
        assert_eq!(gpu_vendor("/usr/lib/dri/radeonsi_dri.so"), Some("amd"));
        assert_eq!(gpu_vendor("/usr/lib/libvulkan_radeon.so"), Some("amd"));
        assert_eq!(gpu_vendor("/usr/lib/libGLX_nvidia.so.0"), Some("nvidia"));
        assert_eq!(gpu_vendor("/usr/lib/dri/nouveau_dri.so"), Some("nvidia"));
        assert_eq!(
            gpu_vendor("/usr/lib/libvulkan_freedreno.so"),
            Some("qualcomm")
        );
        assert_eq!(gpu_vendor("/usr/lib/libc.so.6"), None);
    }

    #[test]
    fn unresolved_pct_handles_empty() {
        let d = Diagnostics {
            tier: FidelityTier::Unknown,
            total_samples: 0,
            unresolved_samples: 0,
            anonymous_modules: vec![],
        };
        assert_eq!(d.unresolved_pct(), 0.0);
    }
}
