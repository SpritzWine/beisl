//! Dual-ICD / multi-GPU detection — the "enumeration roulette" trap from
//! DESIGN.md's Saints Row case study: with more than one Vulkan-capable GPU (or more than
//! one driver install serving the *same* GPU, e.g. a system Mesa and a `/usr/local` build),
//! which device a game actually gets is unstable across runs, silently invalidating any
//! trace or A/B comparison.
//!
//! Detection here is deliberately **Vulkan-call-free** — too heavy for a preflight check —
//! so it works from two cheap, static sources instead:
//!
//! * the installed **ICD manifests** (`/usr/share/vulkan/icd.d` and friends), hand-parsed
//!   for `library_path` (the format is a single flat JSON object; not worth a `serde_json`
//!   dependency for one field), classified hardware vs. software (`lvp`/lavapipe, `dzn`,
//!   `virtio` are excluded — they're not "another GPU" to land on);
//! * **`/sys/class/drm/card*/device`** for the physical GPUs the kernel actually sees
//!   (vendor/device PCI ids), which is the ground truth for "how many GPUs are there".
//!
//! [`detect_candidates`] cross-references the two to build human-labelled pin candidates
//! (`GpuPinCandidate`) good enough to hand straight to `preflight::config::GpuPin`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The standard Vulkan ICD manifest search locations (x86_64 desktop Linux).
const ICD_DIRS: &[&str] = &[
    "/usr/share/vulkan/icd.d",
    "/usr/local/share/vulkan/icd.d",
    "/etc/vulkan/icd.d",
    // NixOS has no /usr/share: its loader is built to look here instead.
    "/run/opengl-driver/share/vulkan/icd.d",
];

/// Driver-family filename stems that are SOFTWARE, not another physical GPU: `lvp`
/// (lavapipe, CPU rasterizer), `dzn` (Dozen, D3D12 translation), `virtio` (virtio-gpu, a
/// passthrough of a passthrough). Excluded from every hardware count.
const SOFTWARE_ICD_STEMS: &[&str] = &["lvp", "dzn", "virtio"];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Arch {
    X86_64,
    I686,
}

/// One hardware Vulkan ICD manifest found on disk.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IcdManifest {
    pub json_path: PathBuf,
    pub library_path: String,
    /// The filename stem with the arch suffix stripped, e.g. `intel_icd`.
    pub family: String,
    pub arch: Arch,
}

/// One physical GPU the kernel sees (a DRM "card" node, not a connector).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PhysicalGpu {
    pub card: String,
    /// PCI vendor id, e.g. `"0x8086"` — absent for non-PCI or unreadable nodes.
    pub vendor_id: Option<String>,
    /// PCI device id, e.g. `"0x56a0"`.
    pub device_id: Option<String>,
    /// Full PCI address from the uevent's `PCI_SLOT_NAME`, e.g. `"0000:03:00.0"` — the
    /// exact string MangoHud's `pci_dev=` config key matches a card by.
    pub pci_slot: Option<String>,
}

/// A candidate to pin to, good enough to become a [`crate::config::GpuPin`] as-is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GpuPinCandidate {
    pub name: String,
    pub icd_paths: Vec<String>,
    pub device_select: Option<String>,
}

/// Scan the standard ICD directories for hardware manifests.
pub fn hardware_icd_manifests() -> Vec<IcdManifest> {
    scan_icd_dirs(&ICD_DIRS.iter().map(PathBuf::from).collect::<Vec<_>>())
}

/// Testable core: scan the given directories for hardware ICD manifests (software ones
/// filtered out).
pub fn scan_icd_dirs(dirs: &[PathBuf]) -> Vec<IcdManifest> {
    let mut out = Vec::new();
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        for e in entries.flatten() {
            let path = e.path();
            if path.extension().and_then(|s| s.to_str()) != Some("json") {
                continue;
            }
            let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
            let (family, arch) = split_arch(stem);
            let family = family.to_string();
            if SOFTWARE_ICD_STEMS.iter().any(|s| family.starts_with(s)) {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            let Some(library_path) = json_string_field(&text, "library_path") else {
                continue;
            };
            out.push(IcdManifest {
                json_path: path,
                library_path,
                family,
                arch,
            });
        }
    }
    out
}

/// Split Mesa's `<family>.<arch>.json` naming into the family stem and the arch. Files
/// without a recognised suffix (non-Mesa loaders sometimes omit it) are assumed 64-bit.
fn split_arch(stem: &str) -> (&str, Arch) {
    if let Some(f) = stem.strip_suffix(".x86_64") {
        (f, Arch::X86_64)
    } else if let Some(f) = stem.strip_suffix(".i686") {
        (f, Arch::I686)
    } else {
        (stem, Arch::X86_64)
    }
}

/// Hand-rolled extraction of a top-level string field (`"key": "value"`). The Vulkan ICD
/// manifest format is a single flat object with string/enum leaves, so this is simpler and
/// lighter than pulling in `serde_json` for one field.
fn json_string_field(text: &str, key: &str) -> Option<String> {
    let needle = format!("\"{key}\"");
    let idx = text.find(&needle)?;
    let rest = &text[idx + needle.len()..];
    let colon = rest.find(':')?;
    let rest = rest[colon + 1..].trim_start();
    let rest = rest.strip_prefix('"')?;
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

/// The physical GPUs visible in `/sys/class/drm`.
pub fn physical_gpus() -> Vec<PhysicalGpu> {
    scan_drm(Path::new("/sys/class/drm"))
}

/// Testable core: DRM "card" directories proper (`card0`, `card1`, …) under `base` —
/// connector entries like `card0-DP-5` are skipped, same filter as the GPU-hang watcher.
pub fn scan_drm(base: &Path) -> Vec<PhysicalGpu> {
    let mut cards: Vec<String> = match std::fs::read_dir(base) {
        Ok(entries) => entries
            .flatten()
            .filter_map(|e| {
                let name = e.file_name().to_string_lossy().into_owned();
                let is_card = name
                    .strip_prefix("card")
                    .is_some_and(|r| !r.is_empty() && r.bytes().all(|b| b.is_ascii_digit()));
                is_card.then_some(name)
            })
            .collect(),
        Err(_) => return Vec::new(),
    };
    cards.sort();

    cards
        .into_iter()
        .filter_map(|card| {
            let dev_dir = base.join(&card).join("device");
            if !dev_dir.exists() {
                return None;
            }
            let vendor_id = std::fs::read_to_string(dev_dir.join("vendor"))
                .ok()
                .map(|s| s.trim().to_string());
            let device_id = std::fs::read_to_string(dev_dir.join("device"))
                .ok()
                .map(|s| s.trim().to_string());
            // The uevent's PCI_SLOT_NAME is the card's full PCI address (`0000:03:00.0`),
            // kept verbatim (lowercased) because MangoHud's `pci_dev` key string-compares
            // against exactly this sysfs form.
            let pci_slot = std::fs::read_to_string(dev_dir.join("uevent"))
                .ok()
                .and_then(|t| {
                    t.lines().find_map(|l| {
                        l.strip_prefix("PCI_SLOT_NAME=")
                            .map(|v| v.trim().to_ascii_lowercase())
                    })
                });
            Some(PhysicalGpu {
                card,
                vendor_id,
                device_id,
                pci_slot,
            })
        })
        .collect()
}

/// How many of the DRM cards are real hardware — i.e. have a readable `vendor` file. This
/// is the count [`crate::multi_gpu_check`] gates on.
pub fn hardware_gpu_count(gpus: &[PhysicalGpu]) -> usize {
    gpus.iter().filter(|g| g.vendor_id.is_some()).count()
}

/// Best-effort vendor name from a PCI vendor id.
pub fn vendor_name(vendor_id: &str) -> &'static str {
    match vendor_id {
        "0x8086" => "Intel",
        "0x1002" => "AMD",
        "0x10de" => "NVIDIA",
        _ => "GPU",
    }
}

/// Resolve a `MESA_VK_DEVICE_SELECT` value (`"8086:56a0"`, optional trailing `!`) back to
/// the matching physical GPU's PCI address, e.g. `"0000:03:00.0"`. `None` unless exactly
/// one GPU matches: with twin cards of the same model the selector itself is ambiguous,
/// and pointing a sampler at the wrong twin is the very mistake callers use this to avoid.
pub fn pci_slot_for_device_select(gpus: &[PhysicalGpu], device_select: &str) -> Option<String> {
    let (vendor, device) = device_select.trim_end_matches('!').split_once(':')?;
    let mut matches = gpus.iter().filter(|g| {
        pci_id_eq(g.vendor_id.as_deref(), vendor) && pci_id_eq(g.device_id.as_deref(), device)
    });
    match (matches.next(), matches.next()) {
        (Some(only), None) => only.pci_slot.clone(),
        _ => None,
    }
}

/// Case-insensitive PCI id compare between sysfs's `0x8086` form and the bare `8086` form
/// `MESA_VK_DEVICE_SELECT` values carry.
fn pci_id_eq(sysfs_id: Option<&str>, bare: &str) -> bool {
    sysfs_id.is_some_and(|v| v.trim_start_matches("0x").eq_ignore_ascii_case(bare))
}

/// Which PCI vendor an ICD driver family serves, when we recognise it — used to pair an
/// ICD manifest with a physical GPU's vendor id.
pub(crate) fn family_vendor(family: &str) -> Option<&'static str> {
    match family {
        "intel_icd" | "intel_hasvk_icd" => Some("0x8086"),
        "radeon_icd" | "amdgpu_icd" => Some("0x1002"),
        "nouveau_icd" => Some("0x10de"),
        _ => None,
    }
}

/// The detected pin candidates: every hardware ICD manifest cross-referenced against a
/// physical GPU sharing its vendor.
pub fn detect_candidates() -> Vec<GpuPinCandidate> {
    build_candidates(&hardware_icd_manifests(), &physical_gpus())
}

/// Testable core of [`detect_candidates`].
///
/// Grouped by **(containing directory, family)** rather than by family alone: two installs
/// of the same driver in different directories (the dual-ICD trap itself — a system Mesa
/// and a `/usr/local` build both serving the same GPU) surface as two *separate*,
/// disambiguated candidates instead of being silently merged into one, so the picker lets
/// you choose which install to pin to.
pub fn build_candidates(manifests: &[IcdManifest], gpus: &[PhysicalGpu]) -> Vec<GpuPinCandidate> {
    let mut groups: BTreeMap<(PathBuf, String), Vec<&IcdManifest>> = BTreeMap::new();
    for m in manifests {
        let dir = m
            .json_path
            .parent()
            .unwrap_or_else(|| Path::new(""))
            .to_path_buf();
        groups.entry((dir, m.family.clone())).or_default().push(m);
    }

    let mut out = Vec::new();
    for ((dir, family), ms) in groups {
        // Need at least the 64-bit driver to build a usable `VK_DRIVER_FILES` entry.
        let Some(x64) = ms.iter().find(|m| m.arch == Arch::X86_64) else {
            continue;
        };
        let mut icd_paths = vec![x64.json_path.display().to_string()];
        // Pair the i686 manifest from the SAME directory automatically — wine/Proton needs
        // both arches (the case study's manual fix used both intel jsons).
        if let Some(i686) = ms.iter().find(|m| m.arch == Arch::I686) {
            icd_paths.push(i686.json_path.display().to_string());
        }

        let vendor_id = family_vendor(&family);
        // Only surface candidates for a vendor we can actually see hardware for — keeps the
        // picker to real GPUs on this machine, not every driver family Mesa ships.
        let Some(matched) =
            vendor_id.and_then(|v| gpus.iter().find(|g| g.vendor_id.as_deref() == Some(v)))
        else {
            continue;
        };
        let vendor_word = vendor_id.map(vendor_name).unwrap_or("GPU");
        let name = format!("{vendor_word} \u{2014} {family} ({})", dir.display());
        let device_select = match (&matched.vendor_id, &matched.device_id) {
            (Some(v), Some(d)) => Some(format!(
                "{}:{}!",
                v.trim_start_matches("0x"),
                d.trim_start_matches("0x")
            )),
            _ => None,
        };
        out.push(GpuPinCandidate {
            name,
            icd_paths,
            device_select,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // Case-study machine from DESIGN.md: Intel Arc A770 + AMD iGPU (the dual-GPU + dual-ICD
    // trap where MangoHud's sampler had picked the idle iGPU until the pin aimed it at the Arc).
    const ARC_VENDOR: &str = "0x8086";
    const ARC_DEVICE: &str = "0x56a0";
    const ARC_SLOT: &str = "0000:03:00.0";
    const IGPU_VENDOR: &str = "0x1002";
    const IGPU_DEVICE: &str = "0x164e";
    const IGPU_SLOT: &str = "0000:16:00.0";
    // The Arc as a MESA_VK_DEVICE_SELECT selector: bare ids (no 0x), force-suffix `!`.
    const ARC_SELECT: &str = "8086:56a0!";

    /// Minimal tempdir (std-only, mirrors `beisl::gpu`'s test helper — no dev-deps here).
    struct TempDir(PathBuf);
    impl TempDir {
        fn new(tag: &str) -> Self {
            let d = std::env::temp_dir().join(format!(
                "beisl-preflight-gpu-test-{tag}-{}-{:?}",
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

    fn write_icd(dir: &Path, filename: &str, library_path: &str) {
        std::fs::create_dir_all(dir).unwrap();
        let json = format!(
            "{{\n    \"ICD\": {{\n        \"api_version\": \"1.4.0\",\n        \
             \"library_arch\": \"64\",\n        \"library_path\": \"{library_path}\"\n    \
             }},\n    \"file_format_version\": \"1.0.1\"\n}}"
        );
        std::fs::write(dir.join(filename), json).unwrap();
    }

    #[test]
    fn classifies_hardware_vs_software_icds() {
        let tmp = TempDir::new("classify");
        let dir = tmp.path().join("icd.d");
        write_icd(
            &dir,
            "intel_icd.x86_64.json",
            "/usr/lib64/libvulkan_intel.so",
        );
        write_icd(&dir, "intel_icd.i686.json", "/usr/lib/libvulkan_intel.so");
        write_icd(&dir, "lvp_icd.x86_64.json", "/usr/lib64/libvulkan_lvp.so");
        write_icd(&dir, "dzn_icd.x86_64.json", "/usr/lib64/libvulkan_dzn.so");
        write_icd(
            &dir,
            "virtio_icd.x86_64.json",
            "/usr/lib64/libvulkan_virtio.so",
        );
        write_icd(
            &dir,
            "radeon_icd.x86_64.json",
            "/usr/lib64/libvulkan_radeon.so",
        );

        let manifests = scan_icd_dirs(&[dir]);
        let families: Vec<&str> = manifests.iter().map(|m| m.family.as_str()).collect();
        assert!(families.contains(&"intel_icd"));
        assert!(families.contains(&"radeon_icd"));
        assert!(
            !families.contains(&"lvp_icd"),
            "lavapipe must be excluded: {families:?}"
        );
        assert!(
            !families.contains(&"dzn_icd"),
            "dzn must be excluded: {families:?}"
        );
        assert!(
            !families.contains(&"virtio_icd"),
            "virtio must be excluded: {families:?}"
        );

        let intel_x64 = manifests
            .iter()
            .find(|m| m.family == "intel_icd" && m.arch == Arch::X86_64)
            .unwrap();
        assert_eq!(intel_x64.library_path, "/usr/lib64/libvulkan_intel.so");
        let intel_i686 = manifests
            .iter()
            .find(|m| m.family == "intel_icd" && m.arch == Arch::I686)
            .unwrap();
        assert_eq!(intel_i686.library_path, "/usr/lib/libvulkan_intel.so");
    }

    #[test]
    fn scan_drm_counts_only_cards_with_a_vendor_file() {
        let tmp = TempDir::new("drm");
        let base = tmp.path().join("drm");
        // A real GPU: vendor + device files, plus the uevent with its PCI address.
        std::fs::create_dir_all(base.join("card0/device")).unwrap();
        std::fs::write(base.join("card0/device/vendor"), format!("{ARC_VENDOR}\n")).unwrap();
        std::fs::write(base.join("card0/device/device"), format!("{ARC_DEVICE}\n")).unwrap();
        std::fs::write(
            base.join("card0/device/uevent"),
            format!("DRIVER=i915\nPCI_CLASS=30000\nPCI_SLOT_NAME={ARC_SLOT}\n"),
        )
        .unwrap();
        // A second real GPU — no uevent, so no PCI address.
        std::fs::create_dir_all(base.join("card1/device")).unwrap();
        std::fs::write(base.join("card1/device/vendor"), "0x1002\n").unwrap();
        std::fs::write(base.join("card1/device/device"), "0x73df\n").unwrap();
        // A virtual/no-vendor DRM node (e.g. vkms/vgem) — must not count as hardware.
        std::fs::create_dir_all(base.join("card2/device")).unwrap();
        // Connector entries, must be skipped entirely.
        std::fs::create_dir_all(base.join("card0-DP-1")).unwrap();

        let gpus = scan_drm(&base);
        assert_eq!(gpus.len(), 3, "{gpus:?}");
        assert_eq!(hardware_gpu_count(&gpus), 2);
        let intel = gpus.iter().find(|g| g.card == "card0").unwrap();
        assert_eq!(intel.vendor_id.as_deref(), Some(ARC_VENDOR));
        assert_eq!(intel.device_id.as_deref(), Some(ARC_DEVICE));
        assert_eq!(intel.pci_slot.as_deref(), Some(ARC_SLOT));
        let amd = gpus.iter().find(|g| g.card == "card1").unwrap();
        assert_eq!(amd.pci_slot, None);
    }

    fn gpu(card: &str, vendor: &str, device: &str, slot: &str) -> PhysicalGpu {
        PhysicalGpu {
            card: card.into(),
            vendor_id: Some(vendor.into()),
            device_id: Some(device.into()),
            pci_slot: Some(slot.into()),
        }
    }

    #[test]
    fn pci_slot_for_device_select_resolves_a_unique_match() {
        // Mirrors the machine that motivated this: idle AMD iGPU + the Intel Arc the game
        // actually renders on — the selector must land on the Arc's address.
        let gpus = vec![
            gpu("card0", IGPU_VENDOR, IGPU_DEVICE, IGPU_SLOT),
            gpu("card1", ARC_VENDOR, ARC_DEVICE, ARC_SLOT),
        ];
        assert_eq!(
            pci_slot_for_device_select(&gpus, ARC_SELECT).as_deref(),
            Some(ARC_SLOT)
        );
        // Without the force-suffix and in different hex case too — test case-insensitive matching.
        assert_eq!(
            pci_slot_for_device_select(&gpus, "8086:56A0").as_deref(),
            Some(ARC_SLOT)
        );
    }

    #[test]
    fn pci_slot_for_device_select_refuses_ambiguity_and_misses() {
        let twins = vec![
            gpu("card0", "0x1002", "0x73df", "0000:0b:00.0"),
            gpu("card1", "0x1002", "0x73df", "0000:0c:00.0"),
        ];
        // Twin cards: vendor:device can't say which one, so don't guess.
        assert_eq!(pci_slot_for_device_select(&twins, "1002:73df!"), None);
        // No matching hardware, or a malformed selector: nothing to resolve.
        assert_eq!(pci_slot_for_device_select(&twins, ARC_SELECT), None);
        assert_eq!(pci_slot_for_device_select(&twins, "not-a-selector"), None);
    }

    #[test]
    fn build_candidates_disambiguates_duplicate_installs_of_the_same_driver() {
        // Mirrors this project's own dev machine: intel_icd installed both system-wide and
        // under /usr/local (the exact "dual ICD" trap from the case study).
        let manifests = vec![
            IcdManifest {
                json_path: PathBuf::from("/usr/share/vulkan/icd.d/intel_icd.x86_64.json"),
                library_path: "/usr/lib64/libvulkan_intel.so".into(),
                family: "intel_icd".into(),
                arch: Arch::X86_64,
            },
            IcdManifest {
                json_path: PathBuf::from("/usr/share/vulkan/icd.d/intel_icd.i686.json"),
                library_path: "/usr/lib/libvulkan_intel.so".into(),
                family: "intel_icd".into(),
                arch: Arch::I686,
            },
            IcdManifest {
                json_path: PathBuf::from("/usr/local/share/vulkan/icd.d/intel_icd.x86_64.json"),
                library_path: "/usr/local/lib64/libvulkan_intel.so".into(),
                family: "intel_icd".into(),
                arch: Arch::X86_64,
            },
        ];
        let gpus = vec![PhysicalGpu {
            card: "card1".into(),
            vendor_id: Some(ARC_VENDOR.into()),
            device_id: Some(ARC_DEVICE.into()),
            pci_slot: Some(ARC_SLOT.into()),
        }];

        let candidates = build_candidates(&manifests, &gpus);
        assert_eq!(candidates.len(), 2, "{candidates:?}");

        let system = candidates
            .iter()
            .find(|c| c.name.contains("/usr/share"))
            .expect("system install candidate");
        assert_eq!(system.icd_paths.len(), 2, "system install should pair i686");
        assert_eq!(system.device_select.as_deref(), Some(ARC_SELECT));

        let local = candidates
            .iter()
            .find(|c| c.name.contains("/usr/local"))
            .expect("local install candidate");
        assert_eq!(
            local.icd_paths.len(),
            1,
            "no i686 counterpart was installed locally"
        );
        assert_eq!(local.device_select.as_deref(), Some(ARC_SELECT));
    }

    #[test]
    fn build_candidates_skips_families_with_no_matching_physical_gpu() {
        let manifests = vec![IcdManifest {
            json_path: PathBuf::from("/usr/share/vulkan/icd.d/nouveau_icd.x86_64.json"),
            library_path: "/usr/lib64/libvulkan_nouveau.so".into(),
            family: "nouveau_icd".into(),
            arch: Arch::X86_64,
        }];
        // No NVIDIA GPU present.
        let gpus = vec![PhysicalGpu {
            card: "card0".into(),
            vendor_id: Some(IGPU_VENDOR.into()),
            device_id: Some(IGPU_DEVICE.into()),
            pci_slot: Some(IGPU_SLOT.into()),
        }];
        assert!(build_candidates(&manifests, &gpus).is_empty());
    }
}
