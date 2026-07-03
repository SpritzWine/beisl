//! MangoHud integration — the frametime *side-channel*.
//!
//! We do NOT vendor MangoHud. We compose with it: point it at our run directory so
//! it writes its frametime CSV, and (later) watch that CSV both to detect
//! low-performance peaks (spikes = bookmarks into the perf trace) and to follow the
//! user's logging-toggle hotkey. MangoHud draws the in-game overlay; we never become
//! a Vulkan layer ourselves.

use std::path::Path;
use std::process::Command;

/// Configure `cmd`'s environment so MangoHud logs frametimes into `run_dir`.
pub fn configure(cmd: &mut Command, run_dir: &Path) {
    cmd.env("MANGOHUD", "1");

    // Append to any MANGOHUD_CONFIG the user already set rather than clobbering it.
    let mut cfg = std::env::var("MANGOHUD_CONFIG").unwrap_or_default();
    if !cfg.is_empty() {
        cfg.push(',');
    }
    cfg.push_str(&format!(
        "output_folder={},toggle_logging=Shift_L+F2",
        run_dir.display()
    ));
    cmd.env("MANGOHUD_CONFIG", cfg);

    // TODO: for bare-wine / non-Steam launchers the Vulkan/GL layer must be enabled
    //   explicitly (Steam's mangohud integration and the `mangohud` prefix handle it
    //   for us; otherwise set VK_ADD_LAYER / the `mangohud` exec prefix).
}
