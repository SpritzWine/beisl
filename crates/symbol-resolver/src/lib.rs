//! Turns `perf.data` sample addresses into wine symbol names.
//!
//! Wine-agnostic by default, with a fidelity that depends on what the traced wine
//! build carries — this split is what lets Beisl trace *anyone's* wine, not just ours:
//!
//! * [`FidelityTier::Full`]    — SpritzWine/Prater: our build ships frame pointers +
//!   full DWARF, so internal statics resolve, not just exports.
//! * [`FidelityTier::Exports`] — stock Proton/GE (stripped): resolve via
//!   `/proc/<pid>/maps` (module bases) ⋈ each module's on-disk export table. Degraded
//!   but real and *comparable* — this is the tier that makes a SpritzWine-vs-Proton
//!   A/B possible at all.
//! * [`FidelityTier::Distro`]  — system wine with debuginfo packages installed.
//!
//! The generic mechanism needs zero cooperation from wine: `/proc/<pid>/maps` is the
//! kernel's view of the loaded PE/ELF modules, and the export tables come from the
//! DLL files on disk. SpritzWine merely *upgrades* the fidelity on top.
//!
//! Every trace is stamped with its tier (see the `artifact` crate) so an A/B never
//! silently compares full symbols against partial ones.

use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FidelityTier {
    /// Frame pointers + full DWARF (SpritzWine/Prater).
    Full,
    /// Export tables only (stripped stock Proton/GE).
    Exports,
    /// System wine with installed debuginfo.
    Distro,
    Unknown,
}

/// A file-backed module loaded in the traced process (from `/proc/<pid>/maps`).
#[derive(Clone, Debug)]
pub struct LoadedModule {
    pub path: String,
    pub base: u64,
    pub size: u64,
}

/// Read the module map of a live pid — the kernel's view, so it works for any wine.
pub fn module_map(_pid: u32) -> std::io::Result<Vec<LoadedModule>> {
    // TODO: parse /proc/<pid>/maps, coalesce file-backed executable regions per DSO.
    Ok(Vec::new())
}

/// Detect the fidelity tier from the loaded wine modules.
pub fn detect_tier(_modules: &[LoadedModule]) -> FidelityTier {
    // TODO: probe ntdll/kernelbase for DWARF (Full) vs export-table-only (Exports),
    //   or an installed debuginfo path (Distro).
    FidelityTier::Unknown
}

/// A single resolved sample: which symbol the instruction pointer landed in.
#[derive(Clone, Debug)]
pub struct ResolvedSample {
    pub module: String,
    pub symbol: String,
    pub offset: u64,
}

/// Resolve the addresses in `perf.data` against the loaded `modules`.
pub fn resolve(_perf_data: &Path, _modules: &[LoadedModule]) -> Vec<ResolvedSample> {
    // TODO: addr -> (module, symbol) via export tables (+ DWARF on the Full tier).
    Vec::new()
}
