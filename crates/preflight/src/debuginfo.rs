//! Detection (never installation) of debug symbols for Vulkan driver libraries — the
//! "driver hotspots are anonymous addresses" shortcoming noted from the Star Wars Outlaws
//! trace analysis: `libvulkan_intel.so` etc. show up as bare addresses in a perf report
//! because Fedora/Debian ship driver debuginfo as a separate, opt-in package. This module
//! only figures out whether that gap exists; the caller wires the result into a `Check`
//! (mirroring how `gpu.rs` scans ICD manifests without ever touching Vulkan itself) and any
//! fix stays a plain "install this package" hint, same as the rest of `preflight`.
//!
//! The standard Linux toolchain identifies a binary's debug companion by its GNU build-id
//! (an ELF note baked in at link time) rather than by path, so that's what we key on:
//! `build_id` reads it straight out of the driver `.so`, `debug_file_installed` checks the
//! distro's standard `.build-id/xx/yyyy….debug` layout under `/usr/lib/debug`, and
//! `debuginfod_configured` accounts for the alternative where no on-disk file is needed at
//! all because `perf`/`gdb` can fetch symbols on demand from a debuginfod server.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use crate::gpu;

/// NT_GNU_BUILD_ID note type (see `elf(5)` / `binutils/include/elf/common.h`).
const NT_GNU_BUILD_ID: u32 = 3;

/// The GNU build-id of an ELF file as lowercase hex, or `None` if it isn't a little-endian
/// ELF64 file (the only shape the driver libraries we probe come in) or carries no
/// `PT_NOTE` build-id note. Reads only the ELF header, program headers, and `PT_NOTE`
/// segments — not the whole file — since Vulkan drivers can be tens of megabytes.
pub fn build_id(path: &Path) -> Option<String> {
    let mut f = File::open(path).ok()?;

    // Elf64_Ehdr is exactly 64 bytes: e_ident[16], then a run of fixed-width fields.
    let mut header = [0u8; 64];
    f.read_exact(&mut header).ok()?;
    if &header[0..4] != b"\x7fELF" {
        return None;
    }
    // EI_CLASS (offset 4): 2 == ELFCLASS64. EI_DATA (offset 5): 1 == ELFDATA2LSB.
    if header[4] != 2 || header[5] != 1 {
        return None;
    }
    let e_phoff = u64::from_le_bytes(header[32..40].try_into().unwrap());
    let e_phentsize = u16::from_le_bytes(header[54..56].try_into().unwrap());
    let e_phnum = u16::from_le_bytes(header[56..58].try_into().unwrap());

    // Elf64_Phdr is 56 bytes; a smaller value means a format we don't understand.
    if e_phentsize < 56 {
        return None;
    }

    for i in 0..e_phnum {
        let off = e_phoff + u64::from(i) * u64::from(e_phentsize);
        f.seek(SeekFrom::Start(off)).ok()?;
        let mut phdr = [0u8; 56];
        f.read_exact(&mut phdr).ok()?;
        let p_type = u32::from_le_bytes(phdr[0..4].try_into().unwrap());
        const PT_NOTE: u32 = 4;
        if p_type != PT_NOTE {
            continue;
        }
        let p_offset = u64::from_le_bytes(phdr[8..16].try_into().unwrap());
        let p_filesz = u64::from_le_bytes(phdr[32..40].try_into().unwrap());
        if let Some(id) = read_note_segment(&mut f, p_offset, p_filesz) {
            return Some(id);
        }
    }
    None
}

/// Read and parse one `PT_NOTE` segment for a GNU build-id note. Segment sizes are capped
/// at 1 MiB — real note segments are a few hundred bytes at most, so anything past that is
/// either not a note segment we understand or a corrupt/hostile file, not worth allocating
/// for.
fn read_note_segment(f: &mut File, offset: u64, size: u64) -> Option<String> {
    if size == 0 || size > 1024 * 1024 {
        return None;
    }
    f.seek(SeekFrom::Start(offset)).ok()?;
    let mut buf = vec![0u8; size as usize];
    f.read_exact(&mut buf).ok()?;
    parse_notes(&buf)
}

/// Walk a note segment's `Elf64_Nhdr` records (namesz/descsz/type, each 4-byte-aligned)
/// looking for the GNU build-id note.
fn parse_notes(buf: &[u8]) -> Option<String> {
    let mut pos = 0usize;
    while pos + 12 <= buf.len() {
        let namesz = u32::from_le_bytes(buf[pos..pos + 4].try_into().unwrap()) as usize;
        let descsz = u32::from_le_bytes(buf[pos + 4..pos + 8].try_into().unwrap()) as usize;
        let ntype = u32::from_le_bytes(buf[pos + 8..pos + 12].try_into().unwrap());
        pos += 12;

        let name_end = pos.checked_add(namesz)?;
        if name_end > buf.len() {
            return None;
        }
        let name = &buf[pos..name_end];
        pos = align4(name_end);

        let desc_end = pos.checked_add(descsz)?;
        if desc_end > buf.len() {
            return None;
        }
        let desc = &buf[pos..desc_end];
        pos = align4(desc_end);

        if ntype == NT_GNU_BUILD_ID && name == b"GNU\0" {
            return Some(hex_encode(desc));
        }
    }
    None
}

fn align4(n: usize) -> usize {
    (n + 3) & !3
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// Is a debug file installed for this build-id under the standard Fedora/Debian
/// `.build-id` layout (`<debug_root>/.build-id/<first 2 hex chars>/<rest>.debug`)?
pub fn debug_file_installed(build_id: &str, debug_root: &Path) -> bool {
    if build_id.len() < 3 {
        return false;
    }
    let (prefix, rest) = build_id.split_at(2);
    debug_root
        .join(".build-id")
        .join(prefix)
        .join(format!("{rest}.debug"))
        .exists()
}

/// Is `debuginfod` configured to fetch symbols on demand? Uses the real environment and
/// `/etc/debuginfod` — see [`debuginfod_configured_from`] for the testable core and the
/// rationale.
pub fn debuginfod_configured() -> bool {
    debuginfod_configured_from(
        std::env::var("DEBUGINFOD_URLS").ok().as_deref(),
        Path::new("/etc/debuginfod"),
    )
}

/// Testable core of [`debuginfod_configured`]. With debuginfod configured, `perf`/`gdb`
/// fetch matching debug files from a server the moment they need them, so a missing
/// on-disk `.debug` file under `/usr/lib/debug` is no longer evidence of a symbolization
/// gap — [`unsymbolized_families`] should stay quiet in that case rather than nag about a
/// problem that's already solved a different way.
///
/// True if `DEBUGINFOD_URLS` is set to a non-blank value, or any `*.urls` file under
/// `config_dir` (the package-manager-dropped config directory, `/etc/debuginfod` for real)
/// has at least one non-blank, non-comment (`#`) line.
pub fn debuginfod_configured_from(env_urls: Option<&str>, config_dir: &Path) -> bool {
    if env_urls.is_some_and(|s| !s.trim().is_empty()) {
        return true;
    }
    let Ok(entries) = std::fs::read_dir(config_dir) else {
        return false;
    };
    for e in entries.flatten() {
        let path = e.path();
        if path.extension().and_then(|s| s.to_str()) != Some("urls") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        if text
            .lines()
            .any(|l| !l.trim().is_empty() && !l.trim().starts_with('#'))
        {
            return true;
        }
    }
    false
}

/// The driver families (deduped, sorted) that are missing debug symbols for their x86_64
/// hardware ICD, scanning the real ICD manifests against `/usr/lib/debug`. See
/// [`unsymbolized_families_from`] for the testable core.
pub fn unsymbolized_families() -> Vec<String> {
    // With debuginfod configured, perf fetches symbols on demand — a missing on-disk
    // debug file is not a symbolization gap, so report none.
    if debuginfod_configured() {
        return Vec::new();
    }
    // Only families serving a GPU that's physically present: distros install the whole
    // Mesa ICD set (asahi, broadcom, … on an x86 desktop), but a trace can only ever
    // contain samples from the driver of a card that exists. Families we can't map to a
    // vendor stay out — an advisory check that goes quiet on exotic hardware beats one
    // that cries wolf about nine drivers on every stock install.
    let gpus = gpu::physical_gpus();
    let manifests: Vec<_> = gpu::hardware_icd_manifests()
        .into_iter()
        .filter(|m| {
            gpu::family_vendor(&m.family).is_some_and(|v| {
                gpus.iter().any(|g| {
                    g.vendor_id
                        .as_deref()
                        .is_some_and(|gv| gv.eq_ignore_ascii_case(v))
                })
            })
        })
        .collect();
    unsymbolized_families_from(&manifests, Path::new("/usr/lib/debug"))
}

/// Testable core of [`unsymbolized_families`].
///
/// A manifest is SKIPPED (not counted as missing symbols) when its library path is
/// relative, doesn't exist, or yields no build-id: none of those let us actually judge
/// whether symbols are missing, and treating "can't tell" as "missing" would misreport a
/// driver install we simply couldn't inspect as a symbolization gap. Only i686 or software
/// (lavapipe etc.) manifests never reach here — [`gpu::hardware_icd_manifests`] already
/// filters those — so this only looks at x86_64 hardware ICDs.
pub fn unsymbolized_families_from(
    manifests: &[gpu::IcdManifest],
    debug_root: &Path,
) -> Vec<String> {
    let mut out: Vec<String> = manifests
        .iter()
        .filter(|m| m.arch == gpu::Arch::X86_64)
        .filter_map(|m| {
            let path = Path::new(&m.library_path);
            if !path.is_absolute() || !path.exists() {
                return None;
            }
            let id = build_id(path)?;
            if debug_file_installed(&id, debug_root) {
                None
            } else {
                Some(m.family.clone())
            }
        })
        .collect();
    out.sort();
    out.dedup();
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// Minimal tempdir (std-only, mirrors `gpu.rs`'s test helper — no dev-deps here).
    struct TempDir(PathBuf);
    impl TempDir {
        fn new(tag: &str) -> Self {
            let d = std::env::temp_dir().join(format!(
                "beisl-preflight-debuginfo-test-{tag}-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            std::fs::create_dir_all(&d).unwrap();
            TempDir(d)
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// Build a minimal synthetic ELF64-LE blob: header + one PT_NOTE program header + (if
    /// `with_note`) a GNU build-id note in the segment it points at.
    fn synthetic_elf(with_note: bool) -> Vec<u8> {
        let note_body: Vec<u8> = if with_note {
            let name = b"GNU\0";
            let desc = [0xde, 0xad, 0xbe, 0xef, 0x01, 0x02, 0x03, 0x04];
            let mut n = Vec::new();
            n.extend_from_slice(&(name.len() as u32).to_le_bytes());
            n.extend_from_slice(&(desc.len() as u32).to_le_bytes());
            n.extend_from_slice(&NT_GNU_BUILD_ID.to_le_bytes());
            n.extend_from_slice(name); // already 4-byte aligned (4 bytes)
            n.extend_from_slice(&desc); // already 4-byte aligned (8 bytes)
            n
        } else {
            Vec::new()
        };

        let ehdr_size = 64u64;
        let phdr_size = 56u64;
        let phoff = ehdr_size;
        let note_offset = phoff + phdr_size;

        let mut buf = vec![0u8; (note_offset + note_body.len() as u64) as usize];

        // e_ident
        buf[0..4].copy_from_slice(b"\x7fELF");
        buf[4] = 2; // ELFCLASS64
        buf[5] = 1; // ELFDATA2LSB
        buf[6] = 1; // EI_VERSION
                    // e_type/e_machine/e_version: left zeroed, irrelevant to build_id.
        buf[32..40].copy_from_slice(&phoff.to_le_bytes()); // e_phoff
        buf[54..56].copy_from_slice(&(phdr_size as u16).to_le_bytes()); // e_phentsize
        buf[56..58].copy_from_slice(&1u16.to_le_bytes()); // e_phnum == 1

        // Phdr[0]: PT_NOTE pointing at the note segment.
        let phdr_off = phoff as usize;
        buf[phdr_off..phdr_off + 4].copy_from_slice(&4u32.to_le_bytes()); // p_type = PT_NOTE
        buf[phdr_off + 8..phdr_off + 16].copy_from_slice(&note_offset.to_le_bytes()); // p_offset
        buf[phdr_off + 32..phdr_off + 40].copy_from_slice(&(note_body.len() as u64).to_le_bytes()); // p_filesz

        buf[note_offset as usize..].copy_from_slice(&note_body);
        buf
    }

    fn write_temp(tmp: &TempDir, name: &str, bytes: &[u8]) -> PathBuf {
        let p = tmp.path().join(name);
        std::fs::write(&p, bytes).unwrap();
        p
    }

    #[test]
    fn build_id_reads_a_synthetic_note() {
        let tmp = TempDir::new("build-id-synthetic");
        let path = write_temp(&tmp, "with-note.so", &synthetic_elf(true));
        let id = build_id(&path).expect("build-id note should parse");
        assert_eq!(id, "deadbeef01020304");
    }

    #[test]
    fn build_id_none_for_non_elf() {
        let tmp = TempDir::new("build-id-non-elf");
        let path = write_temp(
            &tmp,
            "not-elf.txt",
            b"just some text, not an ELF file at all",
        );
        assert!(build_id(&path).is_none());
    }

    #[test]
    fn build_id_none_for_note_less_elf() {
        let tmp = TempDir::new("build-id-no-note");
        // A PT_NOTE segment with zero size: no note to find.
        let path = write_temp(&tmp, "no-note.so", &synthetic_elf(false));
        assert!(build_id(&path).is_none());
    }

    #[test]
    fn build_id_real_world_sanity() {
        // If the dynamic linker is where we expect, it's a real ELF64-LE binary that must
        // carry a build-id on any modern distro toolchain.
        for candidate in [
            "/usr/lib64/ld-linux-x86-64.so.2",
            "/lib64/ld-linux-x86-64.so.2",
        ] {
            let p = Path::new(candidate);
            if p.exists() {
                assert!(build_id(p).is_some(), "{candidate} should have a build-id");
                return;
            }
        }
    }

    #[test]
    fn debug_file_installed_positive_and_negative() {
        let tmp = TempDir::new("debug-file");
        let root = tmp.path();
        let id = "abcdef0123456789";
        std::fs::create_dir_all(root.join(".build-id/ab")).unwrap();
        std::fs::write(root.join(".build-id/ab/cdef0123456789.debug"), b"").unwrap();

        assert!(debug_file_installed(id, root));
        assert!(!debug_file_installed("fedcba9876543210", root));
        assert!(!debug_file_installed("", root));
    }

    #[test]
    fn debuginfod_configured_from_env_var() {
        let tmp = TempDir::new("debuginfod-env");
        // Non-empty env value wins regardless of the config dir (which doesn't even exist).
        assert!(debuginfod_configured_from(
            Some("https://debuginfod.fedoraproject.org/"),
            &tmp.path().join("nonexistent")
        ));
        // Blank/whitespace env value is treated as unset.
        assert!(!debuginfod_configured_from(
            Some("   "),
            &tmp.path().join("nonexistent")
        ));
        assert!(!debuginfod_configured_from(
            None,
            &tmp.path().join("nonexistent")
        ));
    }

    #[test]
    fn debuginfod_configured_from_config_dir() {
        let tmp = TempDir::new("debuginfod-dir");
        let dir = tmp.path();
        std::fs::write(
            dir.join("fedora.urls"),
            "# comment\n\nhttps://debuginfod.fedoraproject.org/\n",
        )
        .unwrap();
        assert!(debuginfod_configured_from(None, dir));

        let tmp2 = TempDir::new("debuginfod-dir-comments-only");
        let dir2 = tmp2.path();
        std::fs::write(dir2.join("empty.urls"), "# just a comment\n\n").unwrap();
        assert!(!debuginfod_configured_from(None, dir2));

        // Config dir doesn't exist at all.
        assert!(!debuginfod_configured_from(None, &dir2.join("nope")));
    }

    fn manifest(library_path: &str, family: &str, arch: gpu::Arch) -> gpu::IcdManifest {
        gpu::IcdManifest {
            json_path: PathBuf::from(format!("/usr/share/vulkan/icd.d/{family}.json")),
            library_path: library_path.to_string(),
            family: family.to_string(),
            arch,
        }
    }

    #[test]
    fn unsymbolized_families_from_reports_missing_debug_and_skips_unjudgeable() {
        let tmp = TempDir::new("unsymbolized");
        // A real ELF we can get a build-id from — copy a small system binary so we don't
        // have to hand-roll a second synthetic ELF.
        let lib_path = tmp.path().join("libvulkan_intel.so");
        std::fs::copy("/usr/bin/true", &lib_path).expect("/usr/bin/true must exist to copy");
        let id = build_id(&lib_path).expect("/usr/bin/true should carry a build-id");

        let debug_root = tmp.path().join("debug-root");

        let manifests = vec![
            manifest(lib_path.to_str().unwrap(), "intel_icd", gpu::Arch::X86_64),
            // Missing library path entirely: can't judge, must be skipped.
            manifest(
                "/nonexistent/libvulkan_radeon.so",
                "radeon_icd",
                gpu::Arch::X86_64,
            ),
            // Relative path: can't judge, must be skipped.
            manifest("libvulkan_nouveau.so", "nouveau_icd", gpu::Arch::X86_64),
            // i686 counterpart of the intel driver: filtered by arch, not by debug state.
            manifest(lib_path.to_str().unwrap(), "intel_icd", gpu::Arch::I686),
        ];

        // No debug file installed anywhere yet: only the real, existing x86_64 library
        // should be reported.
        let missing = unsymbolized_families_from(&manifests, &debug_root);
        assert_eq!(missing, vec!["intel_icd".to_string()]);

        // Install the matching debug file: now nothing is reported.
        let (prefix, rest) = id.split_at(2);
        std::fs::create_dir_all(debug_root.join(".build-id").join(prefix)).unwrap();
        std::fs::write(
            debug_root
                .join(".build-id")
                .join(prefix)
                .join(format!("{rest}.debug")),
            b"",
        )
        .unwrap();
        let missing = unsymbolized_families_from(&manifests, &debug_root);
        assert!(missing.is_empty(), "{missing:?}");
    }

    #[test]
    fn unsymbolized_families_from_dedupes_and_sorts() {
        let tmp = TempDir::new("unsymbolized-dedup");
        let lib_path = tmp.path().join("libvulkan_radeon.so");
        std::fs::copy("/usr/bin/true", &lib_path).expect("/usr/bin/true must exist to copy");
        let debug_root = tmp.path().join("debug-root"); // never populated

        let manifests = vec![
            manifest(lib_path.to_str().unwrap(), "radeon_icd", gpu::Arch::X86_64),
            manifest(lib_path.to_str().unwrap(), "radeon_icd", gpu::Arch::X86_64),
            manifest(lib_path.to_str().unwrap(), "amdgpu_icd", gpu::Arch::X86_64),
        ];
        assert_eq!(
            unsymbolized_families_from(&manifests, &debug_root),
            vec!["amdgpu_icd".to_string(), "radeon_icd".to_string()]
        );
    }
}
