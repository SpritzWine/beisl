//! MangoHud integration — the frametime *side-channel*.
//!
//! We do NOT vendor MangoHud. We compose with it: point it at our run directory so
//! it writes its frametime CSV for the whole session (auto-started, hotkey
//! pauses/resumes), and (later) watch that CSV to detect low-performance peaks
//! (spikes = bookmarks into the perf trace). MangoHud draws the in-game overlay; we
//! never become a Vulkan layer ourselves.

use std::path::Path;
use std::process::Command;

use preflight::gpu::PhysicalGpu;
use preflight::GpuPin;

/// The MangoHud hotkey that toggles frametime logging — with [`AUTOSTART_LOG_SECS`]
/// this now means *pausing/resuming* an already-running log rather than starting one.
pub const TOGGLE_KEY: &str = "Shift_L+F2";

/// Seconds after game start when MangoHud begins logging on its own. Logging used to be
/// hotkey-only, which left "why did it crash" sessions with a CSV covering only the
/// window the user happened to toggle — or none at all. Always-on costs little (tens of
/// bytes per frame) and MangoHud 0.8.x writes + flushes the CSV *per frame*, so the
/// frametimes right up to a crash survive on disk (only the `_summary.csv`, written at
/// log-stop, is lost). One second, not zero: MangoHud sleeps this long before
/// `start_logging()`, and 0 disables the feature.
pub const AUTOSTART_LOG_SECS: u32 = 1;

/// Configure `cmd`'s environment so MangoHud logs frametimes into `run_dir` for the
/// whole session (auto-started, see [`AUTOSTART_LOG_SECS`]), sampling the GPU at
/// `pci_dev` (see [`sample_pci_dev`]) when one is known.
pub fn configure(cmd: &mut Command, run_dir: &Path, pci_dev: Option<&str>) {
    cmd.env("MANGOHUD", "1");

    // Append to any MANGOHUD_CONFIG the user already set rather than clobbering it
    // (for duplicate keys MangoHud takes the last occurrence, so ours win).
    let mut cfg = std::env::var("MANGOHUD_CONFIG").unwrap_or_default();
    if !cfg.is_empty() {
        cfg.push(',');
    }
    cfg.push_str(&format!(
        "output_folder={},toggle_logging={},autostart_log={}",
        run_dir.display(),
        TOGGLE_KEY,
        AUTOSTART_LOG_SECS
    ));
    // Which GPU the stats sampler reads. Left to itself MangoHud picks a card
    // independently of Vulkan device selection, and on a multi-GPU system that can be an
    // idle iGPU while the game renders on the discrete card — filling the CSV's GPU
    // columns with garbage (0% load, idle clocks).
    if let Some(addr) = pci_dev {
        cfg.push_str(&format!(",pci_dev={addr}"));
    }
    cmd.env("MANGOHUD_CONFIG", cfg);

    // TODO: for bare-wine / non-Steam launchers the Vulkan/GL layer must be enabled
    //   explicitly (Steam's mangohud integration and the `mangohud` prefix handle it
    //   for us; otherwise set VK_ADD_LAYER / the `mangohud` exec prefix).
}

/// The GPU MangoHud should sample, as its `pci_dev=` config value — `None` when we
/// can't tell which card the game will render on.
pub fn sample_pci_dev(pin: Option<&GpuPin>) -> Option<String> {
    sample_pci_dev_from(pin, &preflight::gpu::physical_gpus())
}

/// Testable core of [`sample_pci_dev`]: the pinned GPU's PCI address (resolved from the
/// pin's vendor:device selector), else the sole hardware GPU's when there is exactly one.
/// Multi-GPU with no pin stays `None` — guessing which card the game renders on would
/// recreate the exact wrong-GPU-sampling bug this exists to fix.
///
/// The value is the full sysfs address (`0000:03:00.0`, == `PCI_SLOT_NAME`): MangoHud
/// 0.8.x string-compares `pci_dev` against the address component of the
/// `/sys/class/drm/cardN/device` symlink, and older releases substring-match the whole
/// symlink path — the full form satisfies both. `PhysicalGpu::pci_slot` is already
/// exactly that string, so no reformatting happens here.
fn sample_pci_dev_from(pin: Option<&GpuPin>, gpus: &[PhysicalGpu]) -> Option<String> {
    if let Some(sel) = pin.and_then(|p| p.device_select.as_deref()) {
        if let Some(slot) = preflight::gpu::pci_slot_for_device_select(gpus, sel) {
            return Some(slot);
        }
    }
    // No pin (or one we couldn't resolve to a card): exactly one hardware GPU is the
    // only case unambiguous enough to pick automatically.
    let mut hw = gpus.iter().filter(|g| g.vendor_id.is_some());
    match (hw.next(), hw.next()) {
        (Some(only), None) => only.pci_slot.clone(),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // `configure` reads the process-global MANGOHUD_CONFIG env var; serialize the tests
    // that set/unset it (same pattern as preflight's config tests).
    static ENV_LOCK: Mutex<()> = Mutex::new(());

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

    fn mangohud_config(cmd: &Command) -> String {
        cmd.get_envs()
            .find(|(k, _)| *k == "MANGOHUD_CONFIG")
            .and_then(|(_, v)| v)
            .map(|v| v.to_string_lossy().into_owned())
            .expect("MANGOHUD_CONFIG must be set")
    }

    #[test]
    fn configure_appends_pci_dev_only_when_known() {
        let _guard = ENV_LOCK.lock().unwrap();
        // SAFETY: env mutation is serialized by ENV_LOCK for the test's duration.
        unsafe {
            std::env::remove_var("MANGOHUD_CONFIG");
        }

        let mut cmd = Command::new("true");
        configure(&mut cmd, Path::new("/tmp/run"), Some(ARC_SLOT));
        let cfg = mangohud_config(&cmd);
        assert_eq!(
            cfg,
            format!(
                "output_folder=/tmp/run,toggle_logging={TOGGLE_KEY},\
                 autostart_log={AUTOSTART_LOG_SECS},pci_dev={ARC_SLOT}"
            )
        );

        let mut cmd = Command::new("true");
        configure(&mut cmd, Path::new("/tmp/run"), None);
        assert!(
            !mangohud_config(&cmd).contains("pci_dev"),
            "no GPU known — key must be absent"
        );
    }

    #[test]
    fn configure_appends_to_the_users_existing_config() {
        let _guard = ENV_LOCK.lock().unwrap();
        // SAFETY: env mutation is serialized by ENV_LOCK for the test's duration.
        unsafe {
            std::env::set_var("MANGOHUD_CONFIG", "fps_limit=60");
        }

        let mut cmd = Command::new("true");
        configure(&mut cmd, Path::new("/tmp/run"), Some(ARC_SLOT));
        let cfg = mangohud_config(&cmd);
        assert!(
            cfg.starts_with("fps_limit=60,"),
            "user config must survive: {cfg}"
        );
        assert!(cfg.ends_with(&format!(",pci_dev={ARC_SLOT}")), "got: {cfg}");

        unsafe {
            std::env::remove_var("MANGOHUD_CONFIG");
        }
    }

    fn gpu(vendor: &str, device: &str, slot: &str) -> PhysicalGpu {
        PhysicalGpu {
            card: "cardN".into(),
            vendor_id: Some(vendor.into()),
            device_id: Some(device.into()),
            pci_slot: Some(slot.into()),
        }
    }

    /// The machine that motivated this: AMD iGPU + Intel Arc, game pinned to the Arc.
    /// MangoHud's sampler had picked the idle iGPU; the pin must aim it at the Arc.
    #[test]
    fn pinned_gpu_wins_over_everything() {
        let gpus = vec![
            gpu(IGPU_VENDOR, IGPU_DEVICE, IGPU_SLOT),
            gpu(ARC_VENDOR, ARC_DEVICE, ARC_SLOT),
        ];
        let pin = GpuPin {
            name: "Intel — intel_icd (/usr/share/vulkan/icd.d)".into(),
            icd_paths: vec!["/usr/share/vulkan/icd.d/intel_icd.x86_64.json".into()],
            device_select: Some(ARC_SELECT.into()),
        };
        assert_eq!(
            sample_pci_dev_from(Some(&pin), &gpus).as_deref(),
            Some(ARC_SLOT)
        );
    }

    #[test]
    fn unpinned_defaults_to_the_sole_gpu_and_refuses_to_guess_between_several() {
        let one = vec![gpu(ARC_VENDOR, ARC_DEVICE, ARC_SLOT)];
        assert_eq!(sample_pci_dev_from(None, &one).as_deref(), Some(ARC_SLOT));

        let two = vec![
            gpu(IGPU_VENDOR, IGPU_DEVICE, IGPU_SLOT),
            gpu(ARC_VENDOR, ARC_DEVICE, ARC_SLOT),
        ];
        assert_eq!(
            sample_pci_dev_from(None, &two),
            None,
            "ambiguous — must not guess"
        );
    }

    #[test]
    fn unresolvable_pin_still_falls_back_to_the_sole_gpu() {
        let one = vec![gpu(ARC_VENDOR, ARC_DEVICE, ARC_SLOT)];
        // A stale pin (card since removed/renumbered) whose selector matches nothing.
        let pin = GpuPin {
            name: "NVIDIA — nouveau_icd (/usr/share/vulkan/icd.d)".into(),
            icd_paths: vec!["/usr/share/vulkan/icd.d/nouveau_icd.x86_64.json".into()],
            device_select: Some("10de:2684!".into()),
        };
        assert_eq!(
            sample_pci_dev_from(Some(&pin), &one).as_deref(),
            Some(ARC_SLOT)
        );
        // No selector on the pin at all behaves the same.
        let pin = GpuPin {
            device_select: None,
            ..pin
        };
        assert_eq!(
            sample_pci_dev_from(Some(&pin), &one).as_deref(),
            Some(ARC_SLOT)
        );
    }
}
