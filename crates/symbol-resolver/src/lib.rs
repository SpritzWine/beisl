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
    /// `Proton - Experimental`, …) — the artifact's `wine_flavor`. `None` if no wine PE.
    pub wine_flavor: Option<String>,
    /// Best-effort GPU vendor of the card that ran, from the driver-layer DSOs seen
    /// (`intel`/`amd`/`nvidia`/`qualcomm`) — the one with the most samples. Drives
    /// vendor-colored UI (Intel blue, AMD red, Nvidia green, …). `None` if no driver sample
    /// identified a vendor.
    pub gpu_vendor: Option<String>,
}

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
    resolve_perf_data_with_debuginfod(perf_data, None)
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
pub fn resolve_perf_data_with_debuginfod(
    perf_data: &Path,
    debuginfod_urls: Option<&str>,
) -> io::Result<Resolution> {
    let mut cmd = Command::new("perf");
    cmd.args([
        "script",
        "--show-mmap-events",
        "-F",
        "comm,pid,ip,sym,dso",
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

    use io::BufRead as _;
    for line in reader.lines() {
        let line = line?;
        if let Some(m) = parse_mmap(&line) {
            if is_pe(&m.path) && !pe_paths.contains(&m.path) {
                pe_paths.push(m.path.clone());
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
        match named {
            Some(sym) => {
                tally.1 += 1;
                *counts.entry((basename(&sample.dso), sym)).or_insert(0) += 1;
            }
            None => {
                unresolved += 1;
                // A real on-disk module we couldn't name (ELF w/o debuginfo, or a PE past
                // any export) is an actionable issue; kernel/`[unknown]`/JIT pseudo-DSOs
                // are not.
                if is_real_dso(&sample.dso) {
                    *anon.entry(basename(&sample.dso)).or_insert(0) += 1;
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

    let mut symbols: Vec<SymbolCount> = counts
        .into_iter()
        .map(|((module, symbol), samples)| SymbolCount {
            module,
            symbol,
            samples,
        })
        .collect();
    symbols.sort_by(|a, b| b.samples.cmp(&a.samples).then(a.module.cmp(&b.module)));

    let mut anonymous_modules: Vec<(String, u64)> = anon.into_iter().collect();
    anonymous_modules.sort_by_key(|&(_, n)| std::cmp::Reverse(n));
    anonymous_modules.truncate(8);

    // Descending by share; ties broken by the layer's stable order so the bar is deterministic.
    let mut layers: Vec<LayerTally> = layers
        .into_iter()
        .map(|(layer, (samples, named))| LayerTally {
            layer,
            samples,
            named,
        })
        .collect();
    layers.sort_by(|a, b| {
        b.samples
            .cmp(&a.samples)
            .then((a.layer as u8).cmp(&(b.layer as u8)))
    });

    let gpu_vendor = gpu_vendors
        .into_iter()
        .max_by_key(|&(_, n)| n)
        .map(|(v, _)| v.to_string());

    Ok(Resolution {
        diagnostics: Diagnostics {
            tier: detect_tier(&pe_paths),
            total_samples: total,
            unresolved_samples: unresolved,
            anonymous_modules,
        },
        layers,
        wine_flavor: pe_paths.iter().find_map(|p| wine_flavor(p)),
        gpu_vendor,
        symbols,
    })
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
    ip: u64,
    sym: String,
    dso: String,
}

/// Parse a `perf script` sample line: `<comm…> <pid> <ip> <sym> (<dso>)`. The comm can
/// carry spaces, so we anchor on the trailing `(<dso>)` and take the last two tokens
/// before it as `sym` then `ip` (both single tokens).
fn parse_sample(line: &str) -> Option<Sample> {
    let open = line.rfind('(')?;
    let close = line[open..].find(')')? + open;
    let dso = line[open + 1..close].to_string();
    if dso.is_empty() {
        return None;
    }
    let mut toks = line[..open].split_whitespace().rev();
    let sym = toks.next()?.to_string();
    let ip = u64::from_str_radix(toks.next()?.trim_start_matches("0x"), 16).ok()?;
    Some(Sample { ip, sym, dso })
}

/// Parse a `PERF_RECORD_MMAP2` line into a [`Mapping`]:
/// `… PERF_RECORD_MMAP2 pid/tid: [0xSTART(0xSIZE) @ 0xPGOFF …]: prot /path`.
fn parse_mmap(line: &str) -> Option<Mapping> {
    let after = line.split_once("PERF_RECORD_MMAP2")?.1;
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

fn basename(path: &str) -> String {
    path.rsplit('/').next().unwrap_or(path).to_string()
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
            "  wine_rpcrt4_io      6ffffff8f8df [unknown] (/lib/wine/x86_64-windows/ntdll.dll)",
        )
        .unwrap();
        assert_eq!(s.ip, 0x6ffffff8f8df);
        assert_eq!(s.sym, "[unknown]");
        assert_eq!(s.dso, "/lib/wine/x86_64-windows/ntdll.dll");
    }

    #[test]
    fn parses_an_elf_resolved_sample() {
        let s = parse_sample("game 123 7f0011 __memcpy_avx+0x2a (/usr/lib/libc.so.6)").unwrap();
        assert_eq!(s.sym, "__memcpy_avx+0x2a");
        assert_eq!(basename(&s.dso), "libc.so.6");
    }

    #[test]
    fn parses_a_real_mmap2_record() {
        let m = parse_mmap(
            "    wineboot.exe   80406 [012]  4970.137089: PERF_RECORD_MMAP2 80406/80406: \
             [0x6ffffff31000(0x7b000) @ 0x1000 103:01 43010604 2607827909]: r-xp \
             /opt/wine/x86_64-windows/ntdll.dll",
        )
        .unwrap();
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
