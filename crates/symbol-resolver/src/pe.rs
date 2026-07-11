//! Minimal PE export-table reader — the `Exports` tier's engine.
//!
//! perf's symbolizer is ELF-only, so a wine PE DLL (`ntdll.dll`, `d3d12core.dll`, …)
//! comes back as `module + raw offset` with the symbol `[unknown]` (verified live, see
//! DESIGN.md "The gate"). This turns that offset into the nearest exported name by
//! reading the DLL's export directory straight off disk: build a sorted `(rva → name)`
//! table once per module, then binary-search each sample's RVA to the export at or
//! below it. That misattributes an internal static to its nearest *export* (giveaway
//! `+0x9NN`-style offsets) — which is exactly the ceiling the `Full` (retained-DWARF)
//! tier lifts, and why every trace is stamped with its tier.
//!
//! Hand-rolled and bounds-checked rather than pulling a PE crate: the main workspace is
//! std-only and this needs perhaps 120 lines. Malformed input yields `None`, never a
//! panic — the file on disk may not be the PE we think it is.

/// The exported symbols of one PE image, sorted by RVA for nearest-at-or-below lookup.
pub struct ExportTable {
    /// `(function_rva, name)` ascending by rva.
    exports: Vec<(u32, String)>,
}

impl ExportTable {
    /// Parse the export directory of a PE64 (PE32+) image. `None` if `data` isn't a
    /// PE we can read or carries no exports.
    pub fn parse(data: &[u8]) -> Option<Self> {
        let e_lfanew = read_u32(data, 0x3c)? as usize;
        if read_bytes(data, e_lfanew, 4)? != b"PE\0\0" {
            return None;
        }
        let coff = e_lfanew + 4;
        let n_sections = read_u16(data, coff + 2)? as usize;
        let opt_size = read_u16(data, coff + 16)? as usize;
        let opt = coff + 20;
        let magic = read_u16(data, opt)?;
        // 0x20b = PE32+ (64-bit). We only trace 64-bit wine PE here; 0x10b (PE32) has the
        // data directory at a different offset and is left for when 32-bit matters.
        let dd_off = match magic {
            0x20b => opt + 112,
            _ => return None,
        };
        let export_rva = read_u32(data, dd_off)?; // data directory[0] = export
        if export_rva == 0 {
            return None;
        }

        // RVA → file offset via the section table (each section maps a [va, va+vsize)
        // window to raw file bytes at PointerToRawData).
        let sec_base = opt + opt_size;
        let mut sections = Vec::with_capacity(n_sections);
        for i in 0..n_sections {
            let s = sec_base + i * 40;
            let vsize = read_u32(data, s + 8)?;
            let vaddr = read_u32(data, s + 12)?;
            let praw = read_u32(data, s + 20)?;
            sections.push((vaddr, vsize.max(0x1000), praw));
        }
        // Every arithmetic step here is on values read from an untrusted file, so keep it
        // checked: `find` already bounds `rva`, and `checked_add` turns a garbage
        // `PointerToRawData` into `None` rather than a wrapping/overflowing offset.
        let r2o = |rva: u32| -> Option<usize> {
            sections
                .iter()
                .find(|&&(va, vs, _)| rva >= va && rva < va.wrapping_add(vs))
                .and_then(|&(va, _, praw)| Some((praw.checked_add(rva - va)?) as usize))
        };

        // IMAGE_EXPORT_DIRECTORY.
        let eo = r2o(export_rva)?;
        let n_names = read_u32(data, eo + 0x18)? as usize;
        let addr_funcs = r2o(read_u32(data, eo + 0x1c)?)?;
        let addr_names = r2o(read_u32(data, eo + 0x20)?)?;
        let addr_ords = r2o(read_u32(data, eo + 0x24)?)?;

        // NOT `with_capacity(n_names)`: `n_names` is an untrusted `u32` from the file, and a
        // corrupt/mislabeled DLL (`is_pe` is only an extension check) with a garbage count
        // near u32::MAX would ask the allocator for ~100 GB and `handle_alloc_error` aborts
        // the *process* — uncatchable. Grow as we actually read; the bounds-checked `read_*`
        // below yields `None` the moment an index runs off the end.
        let mut exports = Vec::new();
        for i in 0..n_names {
            let name_rva = read_u32(data, addr_names + i * 4)?;
            let ord = read_u16(data, addr_ords + i * 2)? as usize;
            let func_rva = read_u32(data, addr_funcs + ord * 4)?;
            if let Some(name) = read_cstr(data, r2o(name_rva)?) {
                exports.push((func_rva, name));
            }
        }
        if exports.is_empty() {
            return None;
        }
        exports.sort_by_key(|&(rva, _)| rva);
        Some(Self { exports })
    }

    /// The export at or below `rva`, as `(name, offset_past_it)`. `None` if `rva` sits
    /// below the first export.
    pub fn resolve(&self, rva: u32) -> Option<(&str, u32)> {
        let i = self.exports.partition_point(|&(r, _)| r <= rva);
        if i == 0 {
            return None;
        }
        let (r, ref name) = self.exports[i - 1];
        Some((name, rva - r))
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.exports.len()
    }
}

/// Does this PE carry retained DWARF debug info (mingw wine keeps it in `.debug_*`
/// sections)? That's the `Full`-tier signal: with DWARF, internal statics resolve, not
/// just exports. `false` for a stripped image (the `Exports` tier). Best-effort and
/// bounds-checked — a file we can't parse simply isn't `Full`.
pub fn has_dwarf(data: &[u8]) -> bool {
    let Some(e_lfanew) = read_u32(data, 0x3c).map(|v| v as usize) else {
        return false;
    };
    if read_bytes(data, e_lfanew, 4) != Some(b"PE\0\0") {
        return false;
    }
    let coff = e_lfanew + 4;
    let (Some(n_sections), Some(opt_size)) = (
        read_u16(data, coff + 2).map(|v| v as usize),
        read_u16(data, coff + 16).map(|v| v as usize),
    ) else {
        return false;
    };
    let sec_base = coff + 20 + opt_size;
    (0..n_sections).any(|i| {
        // Section name is 8 bytes at the start of each 40-byte header.
        read_bytes(data, sec_base + i * 40, 8).is_some_and(|n| n.starts_with(b".debug"))
    })
}

fn read_bytes(data: &[u8], off: usize, n: usize) -> Option<&[u8]> {
    data.get(off..off.checked_add(n)?)
}
fn read_u16(data: &[u8], off: usize) -> Option<u16> {
    Some(u16::from_le_bytes(
        read_bytes(data, off, 2)?.try_into().unwrap(),
    ))
}
fn read_u32(data: &[u8], off: usize) -> Option<u32> {
    Some(u32::from_le_bytes(
        read_bytes(data, off, 4)?.try_into().unwrap(),
    ))
}
fn read_cstr(data: &[u8], off: usize) -> Option<String> {
    let rest = data.get(off..)?;
    let end = rest.iter().position(|&b| b == 0).unwrap_or(rest.len());
    Some(String::from_utf8_lossy(&rest[..end]).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a byte-minimal but structurally real PE32+ with a two-symbol export table,
    /// so the parser is exercised without a fixture file. One section, everything at
    /// tidy offsets; RVA == file offset here (VirtualAddress == PointerToRawData) to keep
    /// the r2o mapping trivial.
    fn synthetic_pe() -> Vec<u8> {
        let mut d = vec![0u8; 0x600];
        d[0..2].copy_from_slice(b"MZ");
        let e_lfanew = 0x80usize;
        d[0x3c..0x40].copy_from_slice(&(e_lfanew as u32).to_le_bytes());
        d[e_lfanew..e_lfanew + 4].copy_from_slice(b"PE\0\0");
        let coff = e_lfanew + 4;
        d[coff + 2..coff + 4].copy_from_slice(&1u16.to_le_bytes()); // 1 section
        let opt_size = 240usize;
        d[coff + 16..coff + 18].copy_from_slice(&(opt_size as u16).to_le_bytes());
        let opt = coff + 20;
        d[opt..opt + 2].copy_from_slice(&0x20bu16.to_le_bytes()); // PE32+
        let export_rva = 0x200u32;
        d[opt + 112..opt + 116].copy_from_slice(&export_rva.to_le_bytes());
        d[opt + 116..opt + 120].copy_from_slice(&0x100u32.to_le_bytes()); // export size

        // One section covering the whole image, RVA == file offset.
        let sec = opt + opt_size;
        d[sec + 8..sec + 12].copy_from_slice(&0x1000u32.to_le_bytes()); // vsize
        d[sec + 12..sec + 16].copy_from_slice(&0u32.to_le_bytes()); // vaddr
        d[sec + 20..sec + 24].copy_from_slice(&0u32.to_le_bytes()); // praw

        // Export directory at RVA 0x200.
        let eo = 0x200usize;
        let (af, an, ao) = (0x260u32, 0x280u32, 0x290u32);
        d[eo + 0x14..eo + 0x18].copy_from_slice(&2u32.to_le_bytes()); // NumberOfFunctions
        d[eo + 0x18..eo + 0x1c].copy_from_slice(&2u32.to_le_bytes()); // NumberOfNames
        d[eo + 0x1c..eo + 0x20].copy_from_slice(&af.to_le_bytes());
        d[eo + 0x20..eo + 0x24].copy_from_slice(&an.to_le_bytes());
        d[eo + 0x24..eo + 0x28].copy_from_slice(&ao.to_le_bytes());
        // AddressOfFunctions: two RVAs.
        d[af as usize..af as usize + 4].copy_from_slice(&0x1000u32.to_le_bytes()); // Alpha
        d[af as usize + 4..af as usize + 8].copy_from_slice(&0x2000u32.to_le_bytes()); // Beta
                                                                                       // AddressOfNames: two name RVAs.
        d[an as usize..an as usize + 4].copy_from_slice(&0x2a0u32.to_le_bytes());
        d[an as usize + 4..an as usize + 8].copy_from_slice(&0x2a6u32.to_le_bytes());
        // AddressOfNameOrdinals: name[0]->fn0, name[1]->fn1.
        d[ao as usize..ao as usize + 2].copy_from_slice(&0u16.to_le_bytes());
        d[ao as usize + 2..ao as usize + 4].copy_from_slice(&1u16.to_le_bytes());
        d[0x2a0..0x2a5].copy_from_slice(b"Alpha");
        d[0x2a6..0x2aa].copy_from_slice(b"Beta");
        d
    }

    #[test]
    fn parses_and_resolves_nearest_export() {
        let t = ExportTable::parse(&synthetic_pe()).expect("valid PE");
        assert_eq!(t.len(), 2);
        // Exact hit.
        assert_eq!(t.resolve(0x1000), Some(("Alpha", 0)));
        // Inside Alpha (below Beta).
        assert_eq!(t.resolve(0x1500), Some(("Alpha", 0x500)));
        // Inside Beta.
        assert_eq!(t.resolve(0x2004), Some(("Beta", 4)));
        // Below the first export.
        assert_eq!(t.resolve(0x10), None);
    }

    #[test]
    fn rejects_non_pe() {
        assert!(ExportTable::parse(b"not a pe file at all").is_none());
        assert!(ExportTable::parse(&[]).is_none());
        // MZ but garbage e_lfanew must not panic.
        let mut d = vec![0u8; 64];
        d[0..2].copy_from_slice(b"MZ");
        d[0x3c..0x40].copy_from_slice(&0xdead_beefu32.to_le_bytes());
        assert!(ExportTable::parse(&d).is_none());
    }

    #[test]
    fn a_garbage_export_count_yields_none_not_an_abort() {
        // A structurally-valid PE whose export directory claims a `NumberOfNames` near
        // u32::MAX. Must return None (the bounds-checked reads run off the file), never ask
        // the allocator for ~100 GB. Regression guard for the `with_capacity` abort.
        let mut d = synthetic_pe();
        let eo = 0x200usize; // export dir, matching synthetic_pe()
        d[eo + 0x18..eo + 0x1c].copy_from_slice(&0xffff_fffeu32.to_le_bytes());
        assert!(ExportTable::parse(&d).is_none());
    }

    /// The real proof: against the on-disk wine ntdll.dll when present (the same file the
    /// captured trace mapped), the parser must find a large export set and resolve a known
    /// sample RVA to a real ntdll symbol. Guarded so CI without wine still passes.
    #[test]
    fn resolves_real_wine_ntdll_when_present() {
        // Opt-in real-file proof: point `BEISL_TEST_NTDLL` at any wine `ntdll.dll` to run
        // it. Skipped otherwise (no baked-in path) so the synthetic tests carry correctness
        // on any machine.
        let Some(path) = std::env::var_os("BEISL_TEST_NTDLL") else {
            return;
        };
        let Ok(bytes) = std::fs::read(&path) else {
            return;
        };
        let t = ExportTable::parse(&bytes).expect("real ntdll parses");
        assert!(t.len() > 1000, "ntdll exports {} < 1000", t.len());
        // RVA 0x5f8df was a live sample in run-1783746830; must land on a real export.
        let (name, off) = t.resolve(0x5f8df).expect("resolves");
        assert!(
            name.starts_with(|c: char| c.is_ascii_alphabetic()),
            "sane name: {name}"
        );
        assert!(off < 0x1000, "near an export, got +{off:#x}");
    }
}
