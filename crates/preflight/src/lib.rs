//! Preflight — the setup checklist's brain, with no UI dependency.
//!
//! Each [`Check`] knows how to [`Check::detect`] its own state and, when something is
//! missing, which privileged [`Fix`] would repair it. This crate never elevates or runs
//! anything itself: [`fix_command`] returns the exact `pkexec …` argv for the caller
//! (the GUI, wrapped in a progress bar; or a headless runner) to execute. That keeps the
//! logic testable and lets the libcosmic GUI, `beisl --check`, and the installer's
//! control panel all render the *same* state.
//!
//! Every check also carries a `why` string — the "we're doing X because Y" line the
//! setup page shows to make the wait feel like onboarding rather than dead time.

use std::borrow::Cow;
use std::path::{Path, PathBuf};

/// The one Beisl version, inherited from `[workspace.package]` in the root Cargo.toml.
/// beisl-gui is a *separate* cargo workspace (to keep libcosmic's git tree out of the
/// main lockfile) and so can't inherit the manifest field — it takes the version through
/// this const instead, via its existing `preflight` path dependency.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

pub mod config;
pub mod debuginfo;
pub mod gpu;
pub use config::{CleanRunAnalysis, Config, DumpAccessPolicy, GpuPin};

/// The lifecycle of a single check, as the setup page renders it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    Pending,
    Checking,
    Ok,
    /// Detected as missing/mis-set; `Check::fix` says how to repair it.
    NeedsFix,
    Fixing,
    Failed(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CheckId {
    Perf,
    LlvmSymbolizer,
    PerfParanoid,
    MangoHud,
    DiskSpace,
    /// Optional, opt-in: read access to GPU crash dumps for auto-capture. NEVER part of
    /// the blocking "set everything up" sweep — its fix is a standing privilege change and
    /// must be its own explicit click (see [`gpu_dump_check`]).
    GpuDumpAccess,
    /// Optional, informational: the installed MangoHud version crashes the game when
    /// the frametime log is stopped while the HUD is visible (upstream #2053) — the
    /// remedy is a workaround (hide the HUD first) or a MangoHud update, never a
    /// privileged fix (see [`mangohud_hotkey_check`]).
    MangoHudHotkey,
    /// Optional, informational: more than one GPU (or more than one driver install for the
    /// same GPU) is visible to Vulkan — the "enumeration roulette" trap from DESIGN.md's
    /// Saints Row case study. No privileged fix; the remedy is picking a pin in the GUI's
    /// Advanced section (see [`multi_gpu_check`]).
    MultiGpu,
    /// Optional, informational: the kernel hides kallsyms addresses from us, so
    /// kernel-side samples in a trace stay anonymous (the Outlaws crash trace lost its
    /// crash-adjacent kernel activity to exactly this). Detection reads `/proc/kallsyms`
    /// — reality, not the knobs: visibility needs `kptr_restrict=0` AND
    /// `perf_event_paranoid <= 1` (the kernel's `kallsyms_for_perf()` gate), or
    /// CAP_SYSLOG, which our setcap fix deliberately doesn't grant (CAP_PERFMON lets
    /// perf *record* kernel samples, not *name* them). No auto-fix: lowering those knobs
    /// is a system-wide hardening rollback that must not ride along in the "set
    /// everything up" sweep — the same consent line that keeps [`gpu_dump_check`] out of
    /// it. The `why` carries the manual command instead.
    KernelSymbols,
    /// Optional, informational: the Vulkan driver's debug symbols aren't installed, so
    /// time spent inside the driver (`libvulkan_intel.so` etc. — often the hottest
    /// non-game code in a trace) shows as anonymous addresses. Detection probes each
    /// hardware ICD's library for its GNU build-id and the matching
    /// `/usr/lib/debug/.build-id/…` file (debuginfod counts as covered — perf fetches
    /// on demand). No fix: `debuginfo-install`/dbgsym setups are distro-specific
    /// multi-step affairs, so the `why` carries the hint (see
    /// [`driver_debuginfo_check`]).
    DriverDebuginfo,
    /// Optional, informational: the one debuginfod gap Beisl can't paper over. Beisl sets
    /// `DEBUGINFOD_URLS` on its own `perf script` child (so a Steam-session resolve still
    /// fetches driver symbols even where the shell didn't export it), but that only works
    /// if the local `perf` was *built* with debuginfod support. When it wasn't, on-demand
    /// symbol fetch is impossible regardless of config, so we say so rather than let the
    /// driver layer silently go anonymous (see [`perf_debuginfod_check`]).
    PerfDebuginfod,
    /// Steam's containerized client (the steamrt rollout, 2026) executes launch options on
    /// the host but with the container's scrubbed `PATH=/usr/bin:/bin` — so a bare
    /// `beisl %command%` fails with "command not found" even though `~/.local/bin` is on
    /// the user's PATH everywhere else (mangohud survives only because distros package it
    /// straight into `/usr/bin`). The fix installs a tiny root-owned shim at
    /// `/usr/bin/beisl` that forwards to the *invoking* user's `~/.local/bin/beisl` —
    /// per-user at run time, so multi-user systems stay correct and updates flow through
    /// without touching the shim (see [`steam_visible_beisl_check`]).
    SteamVisibleBeisl,
}

/// How to repair a failing check. Turned into a concrete `pkexec` argv by [`fix_command`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Fix {
    /// Install a package; `role` is mapped to the distro's package name at fix time.
    InstallPackage { role: &'static str },
    /// Lower a sysctl (root-only, hence `pkexec`). The blunt fallback.
    Sysctl {
        key: &'static str,
        value: &'static str,
    },
    /// Grant `perf` CAP_PERFMON — least-privilege, no global change, output stays
    /// user-owned. Preferred where `perf` is a real ELF binary; on Debian's wrapper
    /// script it doesn't stick, so [`perf_record_fix`] picks `Sysctl` there instead.
    SetcapPerfmon,
    /// Install the udev rule + tmpfiles entry that grant the `render` group read access
    /// to GPU crash dumps (devcoredump `data` + i915 `error`). One `pkexec` shell writes
    /// both files and reloads udev/tmpfiles — a single polkit prompt. Opt-in only.
    InstallDumpAccessRule,
    /// Remove the same two files and reload udev.
    RemoveDumpAccessRule,
    /// Install the root-owned `/usr/bin/beisl` forwarder shim so Steam's scrubbed launch
    /// PATH can resolve a bare `beisl %command%` (see [`CheckId::SteamVisibleBeisl`]).
    InstallSteamShim,
}

#[derive(Clone, Debug)]
pub struct Check {
    pub id: CheckId,
    pub label: &'static str,
    /// The teaching / entertainment line bound to this step. Usually a `&'static str`
    /// literal; [`multi_gpu_check`] builds one dynamically (it names the actual GPU count),
    /// hence `Cow` rather than a bare `&'static str`.
    pub why: Cow<'static, str>,
    /// A blocking check must pass before a trace can run; non-blocking is advisory.
    pub blocking: bool,
    pub status: Status,
    pub fix: Option<Fix>,
}

/// Absent CAP_PERFMON, system-wide `-a` sampling of CPU **and** kernel needs
/// `perf_event_paranoid <= 0` (at `1` CPU-wide events are already disallowed for
/// unprivileged users; `2` also blocks kernel profiling).
pub const PARANOID_MAX: i32 = 0;

/// Keep this much free where traces are written. perf call-graph captures can reach
/// gigabytes in seconds, so we warn below it (and the recorder monitors during a trace).
pub const MIN_TRACE_FREE_BYTES: u64 = 5 * 1024 * 1024 * 1024; // 5 GiB

impl Check {
    /// Run this check's detection, updating `status` in place. (`DiskSpace` is detected
    /// at construction by [`disk_check`], since it needs the trace path; this is a no-op
    /// for it.)
    pub fn detect(&mut self) {
        self.status = match self.id {
            CheckId::Perf => ok_or_fix(which("perf").is_some()),
            CheckId::LlvmSymbolizer => ok_or_fix(which("llvm-symbolizer").is_some()),
            CheckId::MangoHud => ok_or_fix(which("mangohud").is_some()),
            CheckId::PerfParanoid => {
                let can_record =
                    perf_has_perfmon() || matches!(read_paranoid(), Some(v) if v <= PARANOID_MAX);
                ok_or_fix(can_record)
            }
            CheckId::GpuDumpAccess => dump_access_status(),
            CheckId::MultiGpu => multi_gpu_status(&gpu::physical_gpus()),
            CheckId::KernelSymbols => ok_or_fix(kernel_symbols_visible()),
            CheckId::DriverDebuginfo => driver_debuginfo_status(),
            CheckId::MangoHudHotkey => match mangohud_version().as_deref() {
                Some(v) if mangohud_hotkey_affected(v) => Status::NeedsFix,
                _ => Status::Ok,
            },
            CheckId::DiskSpace => return, // computed in `disk_check`
            CheckId::PerfDebuginfod => return, // computed in `perf_debuginfod_check`
            CheckId::SteamVisibleBeisl => return, // computed in `steam_visible_beisl_check`
        };
    }
}

/// The ordered blocking/optional checklist (call [`Check::detect`] on each). The disk
/// check is separate — the caller adds [`disk_check`] with its chosen trace directory.
pub fn all_checks() -> Vec<Check> {
    vec![
        Check {
            id: CheckId::Perf,
            label: "perf",
            why: "We sample the CPU with perf to see where wine spends its time.".into(),
            blocking: true,
            status: Status::Pending,
            fix: Some(Fix::InstallPackage { role: "perf" }),
        },
        Check {
            id: CheckId::LlvmSymbolizer,
            label: "llvm-symbolizer",
            why: "Turns raw addresses in the trace into function names.".into(),
            blocking: true,
            status: Status::Pending,
            fix: Some(Fix::InstallPackage { role: "llvm" }),
        },
        Check {
            id: CheckId::PerfParanoid,
            label: "perf can record",
            why: "Granting perf the CAP_PERFMON capability gets a full system-wide trace \
                  (kernel included) without weakening the whole system. Without it, on a \
                  kernel that still lets you profile your own processes \
                  (`perf_event_paranoid` <= 2), beisl falls back to a userspace-only trace \
                  — game, DXVK, wine, and GPU-driver time, just no kernel layer."
                .into(),
            // Not blocking: without CAP_PERFMON, beisl still records a degraded
            // (userspace-only) trace at paranoid <= 2 — declining is a fidelity choice,
            // not "can't trace". `beisl --check` renders this as a record-tier ladder;
            // only a genuine `RecordMode::None` (perf missing / paranoid too high) blocks.
            blocking: false,
            status: Status::Pending,
            fix: Some(perf_record_fix()),
        },
        Check {
            id: CheckId::KernelSymbols,
            label: "kernel symbols (optional)",
            why: "Time spent inside the kernel (drivers, syscalls) shows up as nameless \
                  addresses while the kernel hides its symbol table. Game and wine symbols \
                  get names either way; to also name kernel time, a system administrator \
                  can run `sysctl kernel.kptr_restrict=0 kernel.perf_event_paranoid=1` \
                  (in effect until reboot). That weakens a system-wide hardening default, \
                  so Beisl points it out rather than flipping it for you."
                .into(),
            blocking: false,
            status: Status::Pending,
            fix: None,
        },
        Check {
            id: CheckId::MangoHud,
            label: "MangoHud (optional)",
            why: "Its frametime log flags the low-perf moments to zoom into. Optional.".into(),
            blocking: false,
            status: Status::Pending,
            fix: Some(Fix::InstallPackage { role: "mangohud" }),
        },
    ]
}

/// The disk-headroom check for `trace_dir` (detected eagerly, since it needs the path).
/// Advisory (non-blocking): the remedy is "free space or pick a roomier folder", not a
/// `pkexec` command.
pub fn disk_check(trace_dir: &Path) -> Check {
    let status = match disk_free_bytes(trace_dir) {
        Some(free) if free >= MIN_TRACE_FREE_BYTES => Status::Ok,
        Some(_) => Status::NeedsFix,
        None => Status::Failed("cannot read free space".into()),
    };
    Check {
        id: CheckId::DiskSpace,
        label: "disk space for traces",
        why: "perf traces with call stacks can grow to gigabytes in seconds; we keep \
              headroom and stop the capture if it runs low."
            .into(),
        blocking: false,
        status,
        fix: None,
    }
}

/// Where the installed rule + tmpfiles files live once applied.
pub const DUMP_RULE_PATH: &str = "/etc/udev/rules.d/99-beisl-gpu-dump.rules";
pub const DUMP_TMPFILES_PATH: &str = "/etc/tmpfiles.d/beisl-gpu-dump.conf";

/// The shipped rule + tmpfiles bodies, embedded at build time via the assets crate
/// tree so the binary is self-contained (no runtime asset-path discovery) and the files
/// in the repo stay the single source of truth. Written verbatim by [`fix_command`].
const DUMP_RULE_BODY: &str = assets::gpu_dump::UDEV_RULES;
const DUMP_TMPFILES_BODY: &str = assets::gpu_dump::TMPFILES_CONF;

/// The `/usr/bin/beisl` launcher shim body (see [`CheckId::SteamVisibleBeisl`]),
/// embedded like the dump-access rule so the repo file stays the single source of truth.
const STEAM_SHIM_BODY: &str = assets::steam_shim::SHIM;

/// The optional, opt-in "GPU crash-dump capture" check. Kept OUT of [`all_checks`] so the
/// blocking "set everything up" sweep never installs a standing privilege; the caller adds
/// it separately (like [`disk_check`]) and drives its fix from its own labelled button.
///
/// The `why` follows Beisl's layered wording: plain diagnosis first, the offer to help
/// second, the technical detail last.
pub fn gpu_dump_check() -> Check {
    Check {
        id: CheckId::GpuDumpAccess,
        label: "GPU crash-dump capture (optional)",
        why: "When your GPU crashes a game, the proof that the GPU — not the game or Wine — \
               was at fault is locked to the system administrator by default. Turn this on and \
               Beisl can save that evidence itself, the moment it happens. The saved dumps only \
               ever describe your own crashed games' GPU state."
            .into(),
        blocking: false,
        status: dump_access_status(),
        fix: Some(Fix::InstallDumpAccessRule),
    }
}

/// The optional, informational "more than one GPU visible to Vulkan" check.
/// Kept OUT of [`all_checks`], mirroring [`gpu_dump_check`]: it's advisory, never blocking,
/// and its remedy — picking a pin — is a plain config write in the GUI's Advanced section,
/// not a privileged fix, so `fix` is always `None`. The caller adds it like [`disk_check`].
///
/// `why` follows Beisl's layered wording: plain diagnosis first, technical detail last —
/// and unlike the other checks it's genuinely dynamic (it names the actual GPU count and
/// the detected vendor:device ids), which is why [`Check::why`] is a `Cow` rather than a
/// bare `&'static str`.
pub fn multi_gpu_check() -> Check {
    let gpus = gpu::physical_gpus();
    let count = gpu::hardware_gpu_count(&gpus);
    Check {
        id: CheckId::MultiGpu,
        label: "GPU selection (optional)",
        why: multi_gpu_why(count, &gpus).into(),
        blocking: false,
        status: multi_gpu_status(&gpus),
        fix: None,
    }
}

fn multi_gpu_status(gpus: &[gpu::PhysicalGpu]) -> Status {
    if gpu::hardware_gpu_count(gpus) <= 1 {
        Status::Ok
    } else {
        Status::NeedsFix
    }
}

/// The optional, informational "driver debug symbols" check. Kept OUT of [`all_checks`],
/// mirroring [`multi_gpu_check`]: advisory, never blocking, dynamic `why` (it names the
/// actual driver families and the distro's install hint), and its remedy is a manual,
/// distro-specific step — `fix` stays `None` so it can never join the one-click sweep.
/// The caller adds it like [`disk_check`].
///
/// Driver time is often the hottest non-game code in a wine trace (the case studies'
/// `libvulkan_intel.so` hotspots), so anonymous driver samples gut the headline view —
/// but only the *driver's* names are affected, hence advisory.
pub fn driver_debuginfo_check() -> Check {
    let families = debuginfo::unsymbolized_families();
    Check {
        id: CheckId::DriverDebuginfo,
        label: "driver debug symbols (optional)",
        why: driver_debuginfo_why(&families, detect_distro()).into(),
        blocking: false,
        status: if families.is_empty() {
            Status::Ok
        } else {
            Status::NeedsFix
        },
        fix: None,
    }
}

/// The optional, informational `perf`-built-with-debuginfod advisory. Added like
/// [`disk_check`] (constructed already-detected, so [`Check::detect`] just returns for it).
///
/// This is the complement to [`driver_debuginfo_check`]: that one asks "are the driver's
/// debug files reachable?"; this asks "can `perf` fetch them at all?". Beisl injects
/// `DEBUGINFOD_URLS` itself (see `debuginfo::distro_debuginfod_url`), so a missing *config*
/// is no longer a gap — but a `perf` compiled without `HAVE_DEBUGINFOD_SUPPORT` can't fetch
/// no matter what we set, and that's the one case worth surfacing. `fix` stays `None`: the
/// remedy is a distro-specific `perf`/elfutils package, so the hint rides in the `why`.
pub fn perf_debuginfod_check() -> Check {
    let supported = debuginfo::perf_supports_debuginfod();
    Check {
        id: CheckId::PerfDebuginfod,
        label: "perf debuginfod (optional)",
        why: perf_debuginfod_why(supported, detect_distro()).into(),
        blocking: false,
        status: if supported {
            Status::Ok
        } else {
            Status::NeedsFix
        },
        fix: None,
    }
}

fn perf_debuginfod_why(supported: bool, distro: Distro) -> String {
    if supported {
        // Beisl sets DEBUGINFOD_URLS on its own resolve, so this being on means driver
        // symbols get fetched on demand even for a trace resolved inside Steam's session.
        "Your `perf` was built with debuginfod support, so Beisl can fetch GPU-driver \
         debug symbols on demand during analysis — driver time gets real function names \
         without installing anything."
            .to_string()
    } else {
        let hint = match distro {
            Distro::Fedora => "your `perf` package is unusually old — `sudo dnf upgrade perf`",
            Distro::Debian => {
                "install a newer `linux-perf` (the debuginfod client landed in perf 5.8+)"
            }
            Distro::Ubuntu => {
                "install a newer `linux-tools-generic` (the debuginfod client landed in perf 5.8+)"
            }
            Distro::Arch => "update `linux-tools`/`perf` (Arch builds it with debuginfod)",
            Distro::Suse | Distro::Unknown => {
                "install a `perf` built with debuginfod support (elfutils ≥ 0.179)"
            }
        };
        format!(
            "Your `perf` was built without debuginfod support, so Beisl can't fetch \
             GPU-driver debug symbols on demand — driver time will show as anonymous \
             addresses unless the debug files are installed locally. To enable on-demand \
             fetch: {hint}. Optional — game and wine symbols are unaffected."
        )
    }
}

/// The optional, informational MangoHud hotkey advisory. Kept OUT of [`all_checks`]
/// like [`multi_gpu_check`]: advisory, never blocking, `fix` stays `None` — the remedy
/// is a workaround or an upstream update, and beisl must not pretend it can fix
/// someone else's crash.
///
/// The bug (MangoHud #2053, still open as of 2026-07-12): stopping the frametime log
/// while the HUD is visible fails an ImGui assert *inside the game process* — the
/// layer aborts and takes the game with it. Beisl's pause/resume hotkey is exactly
/// that stop, so users deserve the warning before they learn it mid-session (a real
/// The Settlers run died to this during v1 review).
pub fn mangohud_hotkey_check() -> Check {
    let version = mangohud_version();
    Check {
        id: CheckId::MangoHudHotkey,
        label: "MangoHud hotkey safety (optional)",
        why: mangohud_hotkey_why(version.as_deref()).into(),
        blocking: false,
        status: match version.as_deref() {
            Some(v) if mangohud_hotkey_affected(v) => Status::NeedsFix,
            // Unknown version or no CLI: don't cry wolf — the plain MangoHud check
            // already covers absence.
            _ => Status::Ok,
        },
        fix: None,
    }
}

fn mangohud_version() -> Option<String> {
    let out = std::process::Command::new("mangohud")
        .arg("--version")
        .output()
        .ok()?;
    let v = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!v.is_empty()).then_some(v)
}

/// Affected while upstream #2053 is unfixed — which, as of 2026-07-12, is EVERY
/// release (the issue reproduces on latest git). So: any parseable version warns.
/// When a fixed MangoHud ships, put the real bound here (e.g. `< (0, 9)`) — presuming
/// an unreleased version fixes an open bug would make the check go silently green on
/// exactly the crash it exists to warn about.
fn mangohud_hotkey_affected(version: &str) -> bool {
    let v = version.trim_start_matches('v');
    let mut parts = v.split(['.', '-']).filter_map(|p| p.parse::<u32>().ok());
    // Unparseable (git builds, noise): don't alarm — the why still names the bug.
    matches!((parts.next(), parts.next()), (Some(_), Some(_)))
}

fn mangohud_hotkey_why(version: Option<&str>) -> String {
    match version {
        Some(v) if mangohud_hotkey_affected(v) => format!(
            "Your MangoHud ({v}) can crash the game when the frametime log is stopped \
             while the HUD is on screen (MangoHud bug #2053) — and the pause/resume \
             hotkey does exactly that. Until a fixed MangoHud ships: hide the HUD \
             before using the hotkey, or leave the log running (it costs almost \
             nothing)."
        ),
        _ => "Couldn't determine your MangoHud version. A known, still-open MangoHud \
              crash (bug #2053) kills the game when the frametime log is stopped while \
              the HUD is visible — if unsure, hide the HUD before using the pause \
              hotkey."
            .to_string(),
    }
}

/// The "Steam can find beisl" check. Kept OUT of [`all_checks`] like [`gpu_dump_check`];
/// the caller adds it (constructed already-detected, so [`Check::detect`] just returns).
///
/// Steam's containerized client executes launch options on the host but with the
/// container's scrubbed `PATH=/usr/bin:/bin` (verified 2026-07-12: `/bin/sh: line 1:
/// beisl: command not found` in Steam's console log, while the very same trace works
/// with a full path — and mangohud keeps working only because distros put it in
/// `/usr/bin`). Upstream this is ValveSoftware/steam-for-linux#13389, acknowledged as
/// the containerized client mishandling in- vs out-of-container environments — a bug,
/// so it may get fixed; the shim stays harmless after (a restored user PATH puts
/// `~/.local/bin` first again). Non-blocking: a full-path launch option always works.
/// Like [`gpu_dump_check`], NEVER part of the one-click sweep — writing to `/usr/bin`
/// is a system-level change that deserves its own explicit opt-in click. The fix is a
/// one-time root-owned shim at `/usr/bin/beisl` that forwards to the invoking user's
/// `~/.local/bin/beisl` — the stable location every install channel (tarball, bundle, a
/// future AppImage's `beisl install`) refreshes, so it never goes stale. A shim, not a
/// symlink into one user's home: per-user resolution keeps multi-user systems correct
/// and no user-writable file sits behind a system path. On immutable systems `/usr` is
/// read-only; the `why` carries the full-path fallback instead of a fix.
pub fn steam_visible_beisl_check() -> Check {
    let system = detect_system_kind();
    let installed = user_beisl_path().is_file();
    let status = steam_visible_status(
        steam_client_scrubs_path(),
        installed,
        Path::new("/usr/bin/beisl").exists(),
    );
    // Only a mutable /usr can take the shim; elsewhere the why explains the fallback.
    let fix = (status == Status::NeedsFix && system == SystemKind::Mutable)
        .then_some(Fix::InstallSteamShim);
    Check {
        id: CheckId::SteamVisibleBeisl,
        label: "Steam can find beisl",
        why: steam_visible_why(&status, system).into(),
        blocking: false,
        status,
        fix,
    }
}

/// Pure decision: warn only when all three line up — a PATH-scrubbing Steam client, a
/// per-user beisl install for the shim to forward to, and no `/usr/bin/beisl` yet
/// (`exists()` follows symlinks, so a dangling leftover from an older link-style
/// install counts as "not there" and the fix replaces it).
fn steam_visible_status(scrubbing_client: bool, beisl_installed: bool, in_usr_bin: bool) -> Status {
    if scrubbing_client && beisl_installed && !in_usr_bin {
        Status::NeedsFix
    } else {
        Status::Ok
    }
}

fn steam_visible_why(status: &Status, system: SystemKind) -> String {
    match status {
        Status::NeedsFix if system == SystemKind::Mutable => {
            "A recent Steam update changed how games launch: Steam now only finds tools \
             installed system-wide, so `beisl %command%` in a game's launch options \
             stops working. Enable this to add a small system launcher for beisl (one \
             admin prompt) — Steam finds beisl again, updates keep working, and every \
             user on this machine gets their own beisl. (Valve tracks the underlying \
             bug as steam-for-linux #13389; the launcher stays harmless once it's fixed.)"
                .to_string()
        }
        Status::NeedsFix => format!(
            "A recent Steam update changed how games launch: Steam now only finds tools \
             installed system-wide — and this system's /usr is read-only, so no launcher \
             can be added there. Use the full path in the launch options instead: \
             {} %command%",
            user_beisl_path().display()
        ),
        _ => "Steam's launch options resolve `beisl %command%` here — either beisl is \
              reachable from Steam's minimal PATH (/usr/bin) or this Steam still passes \
              your full PATH through."
            .to_string(),
    }
}

/// The stable per-user install location (`beisl install`'s destination) — the one path
/// every distribution channel refreshes, which is what the `/usr/bin` launcher runs.
fn user_beisl_path() -> PathBuf {
    PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/bin/beisl")
}

/// Does the installed Steam launch games through the PATH-scrubbing containerized client?
/// The `steamrt64` client tree is that rollout's on-disk marker; the classic client
/// (no such dir) passes the user's full PATH to launch options, where `~/.local/bin`
/// already works. No Steam at all also returns false — nothing to integrate with.
/// (Flatpak Steam is out of scope: its sandbox can't see /usr/bin either way.)
fn steam_client_scrubs_path() -> bool {
    let home = PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
    [".local/share/Steam", ".steam/steam"]
        .iter()
        .any(|root| home.join(root).join("steamrt64").is_dir())
}

fn driver_debuginfo_status() -> Status {
    if debuginfo::unsymbolized_families().is_empty() {
        Status::Ok
    } else {
        Status::NeedsFix
    }
}

fn driver_debuginfo_why(families: &[String], distro: Distro) -> String {
    if families.is_empty() {
        // Hedged on purpose: this branch also covers setups where nothing was judgeable
        // (NVIDIA-proprietary boxes — family_vendor can't map nvidia_icd to Mesa debug
        // packages — or exotic families we can't tie to hardware). Claiming symbols "are
        // available" there would assert knowledge the probe doesn't have.
        "No missing debug symbols detected for your GPU driver — where we could check, \
         driver time in a trace gets real function names (installed debug files, or \
         fetched on demand via debuginfod)."
            .to_string()
    } else {
        // Only Mesa families can reach this list (the vendor pre-filter drops anything
        // family_vendor can't map, including the proprietary NVIDIA ICD), so a Mesa
        // debug-package hint is always applicable here.
        let hint = match distro {
            Distro::Fedora => "sudo dnf debuginfo-install mesa-vulkan-drivers",
            Distro::Debian | Distro::Ubuntu => {
                "install mesa-vulkan-drivers-dbgsym (needs the ddebs.ubuntu.com / \
                 debian-debug repository)"
            }
            Distro::Arch => {
                "export DEBUGINFOD_URLS=https://debuginfod.archlinux.org and perf \
                 fetches them on demand"
            }
            Distro::Suse | Distro::Unknown => "install your distro's Mesa debug-symbol packages",
        };
        format!(
            "Time inside your GPU driver ({}) will show as anonymous addresses — its \
             debug symbols aren't installed, and driver code is often the hottest \
             non-game code in a trace. To name it: {hint}. Optional — game and wine \
             symbols are unaffected.",
            families.join(", ")
        )
    }
}

fn multi_gpu_why(count: usize, gpus: &[gpu::PhysicalGpu]) -> String {
    if count <= 1 {
        "Only one GPU is visible to Vulkan here, so there's nothing to pin — a trace always \
         lands on it."
            .to_string()
    } else {
        let detail = gpus
            .iter()
            .filter_map(|g| {
                let v = g.vendor_id.as_deref()?;
                let d = g.device_id.as_deref().unwrap_or("????");
                Some(format!(
                    "{} {}:{}",
                    gpu::vendor_name(v),
                    v.trim_start_matches("0x"),
                    d.trim_start_matches("0x")
                ))
            })
            .collect::<Vec<_>>()
            .join(", ");
        format!(
            "{count} GPUs are visible — traces and A/B comparisons can silently land on the \
             wrong one; pin the GPU you mean to test for reliable comparisons. (Detected: \
             {detail}. Pin sets VK_DRIVER_FILES and, where we can resolve it, \
             MESA_VK_DEVICE_SELECT — in the Advanced section.)"
        )
    }
}

/// `Ok` once the rule file is installed AND any GPU error node we can actually see is
/// readable; otherwise `NeedsFix` (rendered as "optional"). Detection is filesystem truth,
/// independent of the persisted [`DumpAccessPolicy`] — the policy is the user's *intent*,
/// this is the *reality*.
fn dump_access_status() -> Status {
    if dump_rule_installed() && dump_nodes_readable() {
        Status::Ok
    } else {
        Status::NeedsFix
    }
}

/// Is the udev rule file present? (Cheap proxy for "the rule was installed".)
pub fn dump_rule_installed() -> bool {
    Path::new(DUMP_RULE_PATH).exists()
}

/// Best-effort: every GPU error node that currently *exists* is readable by us. The i915
/// `error` node is a persistent sysfs attr we can probe directly; devcoredump `data` only
/// exists during an actual hang, so its absence is not a failure. Returns true when there
/// is nothing to check (no hang, non-i915 GPU) — the rule still covers the dynamic case.
fn dump_nodes_readable() -> bool {
    let drm = Path::new("/sys/class/drm");
    let entries = match std::fs::read_dir(drm) {
        Ok(e) => e,
        Err(_) => return true, // no DRM sysfs (unusual) — nothing to gate on
    };
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        // Card dirs proper (`card0`, `card1`), not connectors (`card0-DP-1`).
        if !name
            .strip_prefix("card")
            .is_some_and(|r| !r.is_empty() && r.bytes().all(|b| b.is_ascii_digit()))
        {
            continue;
        }
        let node = drm.join(&name).join("error");
        if node.exists() && !readable(&node) {
            return false;
        }
    }
    true
}

/// Can we open this path for reading right now?
fn readable(path: &Path) -> bool {
    std::fs::File::open(path).is_ok()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Distro {
    Fedora,
    Debian,
    /// Distinct from [`Distro::Debian`]: Ubuntu's perf ships in `linux-tools-*` (Debian's
    /// is `linux-perf`) and it runs its own debuginfod server.
    Ubuntu,
    Arch,
    Suse,
    Unknown,
}

/// Best-effort distro family from `/etc/os-release` (`ID` then `ID_LIKE`).
pub fn detect_distro() -> Distro {
    let text = std::fs::read_to_string("/etc/os-release").unwrap_or_default();
    let field = |key: &str| -> String {
        for line in text.lines() {
            if let Some(v) = line.strip_prefix(key) {
                return v.trim_matches(['"', '\'']).to_ascii_lowercase();
            }
        }
        String::new()
    };
    let hay = format!("{} {}", field("ID="), field("ID_LIKE="));
    if hay.contains("fedora") || hay.contains("rhel") || hay.contains("centos") {
        Distro::Fedora
    } else if hay.contains("ubuntu") {
        // Before Debian: Ubuntu/Mint/Pop carry `ID_LIKE=debian` but package perf their way.
        Distro::Ubuntu
    } else if hay.contains("debian") {
        Distro::Debian
    } else if hay.contains("arch") {
        Distro::Arch
    } else if hay.contains("suse") {
        Distro::Suse
    } else {
        Distro::Unknown
    }
}

/// How (or whether) beisl can obtain `perf` here — orthogonal to [`Distro`], which is only
/// the package *name* axis. Immutable systems change the install transaction or rule it out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SystemKind {
    /// Normal read-write system: `pkexec <pm> install` applies immediately.
    Mutable,
    /// Fedora Atomic / ostree (Bazzite, Bluefin, Aurora, Silverblue, Kinoite): packages are
    /// *layered* with `rpm-ostree install` and take effect only after a reboot.
    OstreeAtomic,
    /// Locked-down immutable (SteamOS): a read-only, wiped-on-update root we can't durably
    /// install `perf` into, so beisl can't run — surface a redirect, not a broken fix.
    Locked,
}

/// Detect the [`SystemKind`]. SteamOS is checked first (a Deck is a locked A/B system, not
/// merely immutable); `/run/ostree-booted` is the one marker every Fedora Atomic derivative
/// sets, so a single check covers Bazzite/Silverblue/Bluefin/… without listing them.
pub fn detect_system_kind() -> SystemKind {
    let os = std::fs::read_to_string("/etc/os-release")
        .unwrap_or_default()
        .to_ascii_lowercase();
    if os.contains("steamos") {
        SystemKind::Locked
    } else if Path::new("/run/ostree-booted").exists() {
        SystemKind::OstreeAtomic
    } else {
        SystemKind::Mutable
    }
}

/// The plain-language redirect for a [`SystemKind::Locked`] system (SteamOS): beisl needs a
/// host `perf` it can't install or keep there, so point at a box where it works.
pub fn locked_system_message() -> &'static str {
    "beisl needs `perf`, which SteamOS's read-only system won't let it install or keep. \
     The Deck is great to play on — but do your tracing on a regular desktop Linux, or an \
     immutable gaming distro like Bazzite (Fedora Atomic). Launch the same game there with \
     `beisl %command%`."
}

/// One-line note for a [`SystemKind::OstreeAtomic`] system: layered installs need a reboot.
pub fn ostree_install_note() -> &'static str {
    "Immutable (ostree) system: install fixes are layered with rpm-ostree and take effect \
     after a reboot."
}

/// Distro package name(s) for a role, or empty if we don't know it for this distro.
pub fn package_for(role: &str, distro: Distro) -> Vec<&'static str> {
    match (role, distro) {
        // Ubuntu has no `linux-perf`; perf rides in linux-tools (`-generic` pulls the
        // running kernel's versioned tools, `-common` gives the `perf` wrapper).
        ("perf", Distro::Ubuntu) => vec!["linux-tools-generic", "linux-tools-common"],
        ("perf", Distro::Debian) => vec!["linux-perf"],
        ("perf", _) => vec!["perf"],
        ("llvm", _) => vec!["llvm"], // provides llvm-symbolizer on the major distros
        ("mangohud", _) => vec!["mangohud"],
        _ => vec![],
    }
}

/// The exact privileged command the caller runs (via `pkexec`) to apply a fix.
/// Empty means "we can't do it automatically here — tell the user".
pub fn fix_command(fix: &Fix, distro: Distro, system: SystemKind) -> Vec<String> {
    match fix {
        Fix::Sysctl { key, value } => vec![
            "pkexec".into(),
            "sysctl".into(),
            "-w".into(),
            format!("{key}={value}"),
        ],
        Fix::SetcapPerfmon => match perf_path() {
            Some(p) => vec![
                "pkexec".into(),
                "setcap".into(),
                "cap_perfmon+ep".into(),
                p.to_string_lossy().into_owned(),
            ],
            None => vec![],
        },
        Fix::InstallDumpAccessRule => vec![
            "pkexec".into(),
            "sh".into(),
            "-c".into(),
            dump_install_script(),
        ],
        Fix::RemoveDumpAccessRule => vec![
            "pkexec".into(),
            "sh".into(),
            "-c".into(),
            dump_remove_script(),
        ],
        // Only a mutable /usr can take the shim; immutable systems get the full-path
        // fallback in the check's why instead of a command that would fail.
        Fix::InstallSteamShim => match system {
            SystemKind::Mutable => vec![
                "pkexec".into(),
                "sh".into(),
                "-c".into(),
                steam_shim_install_script(),
            ],
            _ => vec![],
        },
        Fix::InstallPackage { role } => {
            let pkgs = package_for(role, distro);
            if pkgs.is_empty() {
                return vec![];
            }
            // The install *verb* is the system's, the package *names* are the distro's: an
            // ostree box layers with rpm-ostree (applies after reboot); a locked box can't
            // install at all (the caller redirects instead of running anything).
            let mut cmd: Vec<String> = match system {
                SystemKind::Locked => return vec![],
                SystemKind::OstreeAtomic => {
                    vec!["pkexec".into(), "rpm-ostree".into(), "install".into()]
                }
                SystemKind::Mutable => {
                    let mut c = vec!["pkexec".to_string()];
                    match distro {
                        Distro::Fedora => c.extend(["dnf", "install", "-y"].map(String::from)),
                        Distro::Debian | Distro::Ubuntu => {
                            c.extend(["apt-get", "install", "-y"].map(String::from))
                        }
                        Distro::Arch => c.extend(["pacman", "-S", "--noconfirm"].map(String::from)),
                        Distro::Suse => c.extend(["zypper", "install", "-y"].map(String::from)),
                        Distro::Unknown => return vec![],
                    }
                    c
                }
            };
            cmd.extend(pkgs.into_iter().map(String::from));
            cmd
        }
    }
}

/// The body of the single elevated shell that installs the dump-access rule: write both
/// embedded files, then reload udev and apply tmpfiles so it takes effect immediately
/// (an already-pending devcoredump survives on xe until read, so this can rescue the
/// current dump too). `-e` aborts on the first failure so a half-install can't linger.
fn dump_install_script() -> String {
    format!(
        "set -e\n\
         cat > '{rule}' <<'BEISL_RULE_EOF'\n{rule_body}BEISL_RULE_EOF\n\
         cat > '{tmpf}' <<'BEISL_TMPF_EOF'\n{tmpf_body}BEISL_TMPF_EOF\n\
         chmod 0644 '{rule}' '{tmpf}'\n\
         udevadm control --reload-rules\n\
         udevadm trigger --subsystem-match=drm --subsystem-match=devcoredump\n\
         systemd-tmpfiles --create '{tmpf}'\n",
        rule = DUMP_RULE_PATH,
        tmpf = DUMP_TMPFILES_PATH,
        rule_body = DUMP_RULE_BODY,
        tmpf_body = DUMP_TMPFILES_BODY,
    )
}

/// The body of the elevated shell that removes the rule + tmpfiles entry and reloads udev.
/// (The sysfs perms it set persist until the next boot/rebind; we don't undo them live.)
/// The body of the elevated shell that installs the `/usr/bin/beisl` launcher shim.
/// The `rm -f` first is load-bearing: `cat >` writes *through* an existing symlink, so
/// a leftover link-style install would get its target — the user's real binary —
/// clobbered with shell text instead of being replaced. `-e` aborts on the first
/// failure so a half-install can't linger.
fn steam_shim_install_script() -> String {
    format!(
        "set -e\n\
         rm -f /usr/bin/beisl\n\
         cat > /usr/bin/beisl <<'BEISL_SHIM_EOF'\n{STEAM_SHIM_BODY}BEISL_SHIM_EOF\n\
         chmod 0755 /usr/bin/beisl\n"
    )
}

fn dump_remove_script() -> String {
    format!(
        "rm -f '{rule}' '{tmpf}'\nudevadm control --reload-rules\n",
        rule = DUMP_RULE_PATH,
        tmpf = DUMP_TMPFILES_PATH,
    )
}

/// Prefer CAP_PERFMON (surgical) where `perf` is a real ELF binary; on a wrapper script
/// (Debian) or a missing perf, fall back to lowering the sysctl.
pub fn perf_record_fix() -> Fix {
    match perf_path() {
        Some(p) if is_elf(&p) => Fix::SetcapPerfmon,
        _ => Fix::Sysctl {
            key: "kernel.perf_event_paranoid",
            value: "-1",
        },
    }
}

fn ok_or_fix(present: bool) -> Status {
    if present {
        Status::Ok
    } else {
        Status::NeedsFix
    }
}

/// Locate an executable on `PATH` (no external deps).
fn which(bin: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let cand = dir.join(bin);
        if cand.is_file() {
            return Some(cand);
        }
    }
    None
}

fn perf_path() -> Option<PathBuf> {
    which("perf")
}

/// True if `path` starts with the ELF magic — i.e. a real binary, not a shell wrapper.
fn is_elf(path: &Path) -> bool {
    use std::io::Read;
    let mut magic = [0u8; 4];
    std::fs::File::open(path)
        .and_then(|mut f| f.read_exact(&mut magic))
        .is_ok()
        && magic == [0x7f, b'E', b'L', b'F']
}

/// Does the `perf` binary already carry CAP_PERFMON? (via `getcap`; false if unavailable)
fn perf_has_perfmon() -> bool {
    let Some(p) = perf_path() else {
        return false;
    };
    match std::process::Command::new("getcap").arg(&p).output() {
        Ok(o) => String::from_utf8_lossy(&o.stdout).contains("cap_perfmon"),
        Err(_) => false,
    }
}

fn read_paranoid() -> Option<i32> {
    std::fs::read_to_string("/proc/sys/kernel/perf_event_paranoid")
        .ok()?
        .trim()
        .parse()
        .ok()
}

/// How beisl can record CPU samples for a session, given the kernel's `perf_event_paranoid`
/// and whether `perf` carries CAP_PERFMON. The wrapper reads this at launch to pick the
/// `perf record` invocation; the setup UI reads it to tell the user what *declining* the
/// privilege fix actually costs (a userspace-only trace, not no trace).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecordMode {
    /// System-wide (`perf record -a`): kernel + every process. Needs `perf_event_paranoid
    /// <= 0` (see [`PARANOID_MAX`]) or `perf` carrying CAP_PERFMON — the full-fidelity path.
    Full,
    /// Per-command, userspace only (`perf record -e cycles:u -- <game>`): the game's own
    /// process subtree, no privilege. Unprivileged users may profile the userspace of
    /// processes they launch at `perf_event_paranoid <= 2` (the common default, and
    /// SteamOS); kernel samples are dropped. Keeps the game/dxvk/wine/**userspace-driver**
    /// layers — the driver's hot Vulkan code runs in-process — losing only the kernel bar.
    Degraded,
    /// No recording: `perf` is missing, or paranoia is high enough (a Debian-style
    /// `perf_event_paranoid=3`) to deny even own-process profiling without CAP_PERFMON.
    /// The game runs un-traced (the caller warns and lets it launch).
    None,
}

impl RecordMode {
    pub fn as_str(self) -> &'static str {
        match self {
            RecordMode::Full => "full",
            RecordMode::Degraded => "degraded",
            RecordMode::None => "none",
        }
    }

    /// A short human note for the setup UI / MCP advisor — what the mode actually costs,
    /// so declining the CAP_PERFMON fix reads as a tradeoff rather than a dead end.
    pub fn describe(self) -> &'static str {
        match self {
            RecordMode::Full => "system-wide — kernel + userspace",
            RecordMode::Degraded => "userspace-only — no kernel layer (grant CAP_PERFMON for full)",
            RecordMode::None => "unavailable — the game runs un-traced",
        }
    }
}

/// The [`RecordMode`] for a trace on this machine right now.
pub fn detect_record_mode() -> RecordMode {
    record_mode_from(which("perf").is_some(), perf_has_perfmon(), read_paranoid())
}

/// Testable core of [`detect_record_mode`]. Full needs CPU-wide (`-a`) access — CAP_PERFMON
/// or `paranoid <= 0`. Degraded needs only own-process userspace profiling, allowed at
/// `paranoid <= 2`; an unreadable sysctl is treated as permissive (absent on stripped
/// kernels where the restriction usually isn't enforced), degrading rather than giving up.
fn record_mode_from(perf_present: bool, has_perfmon: bool, paranoid: Option<i32>) -> RecordMode {
    if !perf_present {
        RecordMode::None
    } else if has_perfmon || matches!(paranoid, Some(v) if v <= PARANOID_MAX) {
        RecordMode::Full
    } else if paranoid.is_none() || matches!(paranoid, Some(v) if v <= 2) {
        RecordMode::Degraded
    } else {
        RecordMode::None
    }
}

/// Reality probe for kernel-symbol visibility: does `/proc/kallsyms` show us real
/// addresses? The gate is two knobs deep — `kptr_restrict` must be 0 AND
/// `perf_event_paranoid <= 1` (the kernel's `kallsyms_for_perf()` gate) — and checking
/// the sysctls would re-derive kernel policy and get it wrong (kptr=0 + paranoid=2, the
/// state our preferred setcap fix leaves in place, still zeroes every address). Reading
/// the file measures the thing perf actually needs. Missing kallsyms (unusual) counts as
/// visible — nothing to gate on, same stance as `dump_nodes_readable`.
fn kernel_symbols_visible() -> bool {
    match std::fs::File::open("/proc/kallsyms") {
        Ok(f) => kallsyms_shows_addresses(std::io::BufReader::new(f)),
        Err(_) => true,
    }
}

/// True if any line carries a non-zero address. Addresses come sorted, but the low end
/// is legitimately near-zero (per-cpu symbols), so scan for the first non-zero anywhere:
/// a visible table hits one within a few lines, a hidden one is all zeros and costs a
/// single full scan of the (in-memory procfs) file. Testable core of
/// [`kernel_symbols_visible`].
fn kallsyms_shows_addresses<R: std::io::BufRead>(r: R) -> bool {
    r.lines().map_while(Result::ok).any(|l| {
        l.split_whitespace()
            .next()
            .is_some_and(|a| a.bytes().any(|b| b != b'0'))
    })
}

/// Free bytes on the filesystem holding `path` (or its nearest existing ancestor).
pub fn disk_free_bytes(path: &Path) -> Option<u64> {
    let dir = existing_ancestor(path)?;
    let out = std::process::Command::new("df")
        .args(["-B1", "--output=avail"])
        .arg(&dir)
        .output()
        .ok()?;
    // Line 1 is the "Avail" header; line 2 is the number (locale-independent with -B1).
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .nth(1)?
        .trim()
        .parse()
        .ok()
}

fn existing_ancestor(path: &Path) -> Option<PathBuf> {
    let mut p = path;
    loop {
        if p.exists() {
            return Some(p.to_path_buf());
        }
        p = p.parent()?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn which_finds_a_ubiquitous_binary() {
        assert!(which("sh").is_some());
        assert!(which("definitely-not-a-real-binary-xyzzy").is_none());
    }

    #[test]
    fn sysctl_fix_is_a_pkexec_argv() {
        let cmd = fix_command(
            &Fix::Sysctl {
                key: "kernel.perf_event_paranoid",
                value: "-1",
            },
            Distro::Fedora,
            SystemKind::Mutable,
        );
        assert_eq!(cmd[0], "pkexec");
        assert!(cmd.contains(&"kernel.perf_event_paranoid=-1".to_string()));
    }

    #[test]
    fn install_maps_per_distro() {
        assert_eq!(package_for("perf", Distro::Debian), vec!["linux-perf"]);
        // Ubuntu diverges from Debian: no `linux-perf`, perf lives in linux-tools.
        assert_eq!(
            package_for("perf", Distro::Ubuntu),
            vec!["linux-tools-generic", "linux-tools-common"]
        );
        assert_eq!(package_for("perf", Distro::Fedora), vec!["perf"]);
        let arch = fix_command(
            &Fix::InstallPackage { role: "mangohud" },
            Distro::Arch,
            SystemKind::Mutable,
        );
        assert_eq!(
            arch,
            vec!["pkexec", "pacman", "-S", "--noconfirm", "mangohud"]
        );
        // Unknown distro: no automatic command.
        assert!(fix_command(
            &Fix::InstallPackage { role: "perf" },
            Distro::Unknown,
            SystemKind::Mutable
        )
        .is_empty());
    }

    #[test]
    fn install_verb_follows_system_kind() {
        let perf = || Fix::InstallPackage { role: "perf" };
        // ostree keeps the Fedora package name but layers it with rpm-ostree.
        assert_eq!(
            fix_command(&perf(), Distro::Fedora, SystemKind::OstreeAtomic),
            vec!["pkexec", "rpm-ostree", "install", "perf"]
        );
        // A locked system (SteamOS) has no install command at all — the caller redirects.
        assert!(fix_command(&perf(), Distro::Arch, SystemKind::Locked).is_empty());
    }

    #[test]
    fn setcap_fix_targets_the_perf_binary_when_present() {
        // Only assert shape when perf exists on the test host.
        if let Some(p) = perf_path() {
            let cmd = fix_command(&Fix::SetcapPerfmon, Distro::Fedora, SystemKind::Mutable);
            assert_eq!(cmd[0], "pkexec");
            assert_eq!(cmd[1], "setcap");
            assert!(cmd.last().unwrap().contains(&*p.to_string_lossy()));
        }
    }

    #[test]
    fn disk_free_reads_a_real_number() {
        // The temp dir always exists; free space should parse to some value.
        assert!(disk_free_bytes(&std::env::temp_dir()).is_some());
    }

    #[test]
    fn dump_access_fix_is_one_pkexec_shell() {
        let cmd = fix_command(
            &Fix::InstallDumpAccessRule,
            Distro::Fedora,
            SystemKind::Mutable,
        );
        assert_eq!(cmd[0], "pkexec");
        assert_eq!(cmd[1], "sh");
        assert_eq!(cmd[2], "-c");
        let script = &cmd[3];
        // Writes both files, reloads udev, applies tmpfiles — all in one elevation.
        assert!(script.contains(DUMP_RULE_PATH));
        assert!(script.contains(DUMP_TMPFILES_PATH));
        assert!(script.contains("udevadm control --reload-rules"));
        assert!(script.contains("systemd-tmpfiles --create"));
        // The embedded rule body rode along verbatim.
        assert!(script.contains("SUBSYSTEM==\"devcoredump\""));

        let rm = fix_command(
            &Fix::RemoveDumpAccessRule,
            Distro::Fedora,
            SystemKind::Mutable,
        );
        assert_eq!(rm[0], "pkexec");
        assert!(rm[3].contains("rm -f"));
        assert!(rm[3].contains(DUMP_RULE_PATH));
    }

    #[test]
    fn gpu_dump_check_is_optional_and_out_of_the_blocking_sweep() {
        let c = gpu_dump_check();
        assert_eq!(c.id, CheckId::GpuDumpAccess);
        assert!(!c.blocking);
        assert!(matches!(c.fix, Some(Fix::InstallDumpAccessRule)));
        // Crucially NOT in all_checks() — the "set everything up" sweep must never install
        // a standing privilege on its own.
        assert!(all_checks().iter().all(|c| c.id != CheckId::GpuDumpAccess));
    }

    #[test]
    fn steam_visible_status_warns_only_when_all_three_line_up() {
        use Status::*;
        // (scrubbing client, beisl installed, /usr/bin/beisl present) -> status
        let matrix = [
            ((true, true, false), NeedsFix), // the real case: containerized Steam, no link
            ((true, true, true), Ok),        // already linked
            ((true, false, false), Ok),      // nothing installed to link to yet
            ((false, true, false), Ok),      // classic client passes the full PATH through
            ((false, false, false), Ok),     // no Steam at all
        ];
        for ((scrub, inst, linked), want) in matrix {
            assert_eq!(
                steam_visible_status(scrub, inst, linked),
                want,
                "scrub={scrub} installed={inst} linked={linked}"
            );
        }
    }

    #[test]
    fn steam_visible_check_is_advisory_and_out_of_all_checks() {
        assert!(all_checks()
            .iter()
            .all(|c| c.id != CheckId::SteamVisibleBeisl));
        let c = steam_visible_beisl_check();
        assert!(!c.blocking, "a full-path launch option always works");
        assert!(!c.why.is_empty());
    }

    #[test]
    fn steam_shim_fix_writes_the_shim_on_mutable_and_nothing_elsewhere() {
        let cmd = fix_command(&Fix::InstallSteamShim, Distro::Fedora, SystemKind::Mutable);
        assert_eq!(&cmd[..3], &["pkexec", "sh", "-c"]);
        let script = &cmd[3];
        assert!(script.contains("#!/bin/sh"), "embeds the shim body");
        // rm -f before cat >: a leftover symlink-style install must be replaced, not
        // written through (which would clobber the user's real binary).
        assert!(script.contains("rm -f /usr/bin/beisl"));
        assert!(script.contains("chmod 0755 /usr/bin/beisl"));
        // The multi-user guarantee: nothing user-specific baked into the shim.
        assert!(!script.contains("/home/"), "script: {script}");
        // Immutable /usr can't take the shim — the why carries the fallback instead.
        for system in [SystemKind::OstreeAtomic, SystemKind::Locked] {
            assert!(fix_command(&Fix::InstallSteamShim, Distro::Fedora, system).is_empty());
        }
    }

    #[test]
    fn all_checks_have_why_and_blocking_flags() {
        let checks = all_checks();
        assert_eq!(checks.len(), 5);
        assert!(checks.iter().all(|c| !c.why.is_empty()));
        // Advisory (non-blocking): PerfParanoid (declining CAP_PERFMON still gives a
        // degraded userspace-only trace, so it's a fidelity choice, not a hard gate),
        // KernelSymbols, and MangoHud. Everything else gates a trace.
        assert_eq!(
            checks
                .iter()
                .filter(|c| !c.blocking)
                .map(|c| c.id)
                .collect::<Vec<_>>(),
            vec![
                CheckId::PerfParanoid,
                CheckId::KernelSymbols,
                CheckId::MangoHud
            ]
        );
    }

    #[test]
    fn kernel_symbols_check_is_advisory_and_never_joins_the_sweep() {
        let c = all_checks()
            .into_iter()
            .find(|c| c.id == CheckId::KernelSymbols)
            .unwrap();
        assert!(
            !c.blocking,
            "hidden kernel symbols must never block a trace"
        );
        // No auto-fix: revealing kernel addresses is a system-wide hardening rollback
        // that must stay a deliberate admin action — a fix here would ride along in the
        // GUI's one-click "set everything up" sweep (same consent line as gpu_dump).
        assert!(c.fix.is_none());
        // The why must hand the admin the actual knobs instead.
        assert!(c.why.contains("kptr_restrict"));
        assert!(c.why.contains("perf_event_paranoid"));
    }

    #[test]
    fn kallsyms_probe_reads_reality_not_the_knobs() {
        use std::io::Cursor;
        // Hidden: every address zeroed (kptr_restrict, or kptr=0 with paranoid >= 2 —
        // the false-Ok state a sysctl-value check would miss).
        let hidden = "0000000000000000 A fixed_percpu_data\n0000000000000000 T _stext\n";
        assert!(!kallsyms_shows_addresses(Cursor::new(hidden)));
        // Visible: the low end may legitimately be zero; any later non-zero counts.
        let visible = "0000000000000000 A fixed_percpu_data\nffffffff81000000 T _stext\n";
        assert!(kallsyms_shows_addresses(Cursor::new(visible)));
        assert!(!kallsyms_shows_addresses(Cursor::new("")));
    }

    #[test]
    fn driver_debuginfo_check_is_advisory_and_out_of_the_sweep() {
        // Out of all_checks(), like the other dynamic informational rows — and with no
        // fix, so the GUI's one-click sweep structurally can't touch it.
        assert!(all_checks()
            .iter()
            .all(|c| c.id != CheckId::DriverDebuginfo));
        let c = driver_debuginfo_check();
        assert!(
            !c.blocking,
            "missing driver symbols must never block a trace"
        );
        assert!(c.fix.is_none(), "remedy is manual and distro-specific");
        assert!(!c.why.is_empty());
    }

    #[test]
    fn driver_debuginfo_why_names_families_and_the_distro_hint() {
        let fams = vec!["intel_icd".to_string(), "radeon_icd".to_string()];
        let why = driver_debuginfo_why(&fams, Distro::Fedora);
        assert!(
            why.contains("intel_icd, radeon_icd"),
            "must name the drivers: {why}"
        );
        assert!(why.contains("debuginfo-install"), "Fedora hint: {why}");
        // The all-covered branch reassures instead of hinting.
        let ok = driver_debuginfo_why(&[], Distro::Fedora);
        assert!(ok.contains("No missing debug symbols detected"));
        assert!(!ok.contains("debuginfo-install"));
    }

    #[test]
    fn record_mode_matrix() {
        use RecordMode::*;
        // No perf at all → nothing to record with, regardless of the knobs.
        assert_eq!(record_mode_from(false, true, Some(0)), None);
        // CAP_PERFMON → full even under a locked-down paranoid.
        assert_eq!(record_mode_from(true, true, Some(3)), Full);
        // paranoid <= 0 (no cap) → full.
        assert_eq!(record_mode_from(true, false, Some(0)), Full);
        assert_eq!(record_mode_from(true, false, Some(-1)), Full);
        // paranoid 1..=2 (no cap) → degraded (userspace-only). 2 is the common default/SteamOS.
        assert_eq!(record_mode_from(true, false, Some(1)), Degraded);
        assert_eq!(record_mode_from(true, false, Some(2)), Degraded);
        // Unreadable sysctl → treat as permissive, degrade rather than give up.
        assert_eq!(record_mode_from(true, false, Option::None), Degraded);
        // Debian-style paranoid=3 (no cap) → even own-process profiling is denied.
        assert_eq!(record_mode_from(true, false, Some(3)), None);
    }

    #[test]
    fn record_mode_describe_names_the_tradeoff() {
        assert_eq!(
            RecordMode::Full.describe(),
            "system-wide — kernel + userspace"
        );
        assert!(RecordMode::Degraded.describe().contains("userspace-only"));
        assert!(RecordMode::Degraded.describe().contains("CAP_PERFMON"));
        assert!(RecordMode::None.describe().contains("un-traced"));
    }

    #[test]
    fn perf_paranoid_why_mentions_the_degraded_fallback() {
        // The check used to imply "grant this or you can't record" — no longer true now
        // that a degraded userspace-only trace exists, so the why must say so.
        let why = &all_checks()
            .into_iter()
            .find(|c| c.id == CheckId::PerfParanoid)
            .unwrap()
            .why;
        assert!(why.contains("userspace"), "{why}");
        assert!(why.contains("CAP_PERFMON"), "{why}");
    }

    #[test]
    fn perf_debuginfod_why_reflects_support_and_names_a_distro_hint() {
        // Supported: reassure, no install hint.
        let ok = perf_debuginfod_why(true, Distro::Fedora);
        assert!(ok.contains("built with debuginfod support"), "{ok}");
        // Unsupported: name the gap and carry a distro-specific hint.
        let bad = perf_debuginfod_why(false, Distro::Arch);
        assert!(bad.contains("without debuginfod support"), "{bad}");
        assert!(bad.contains("linux-tools"), "Arch hint: {bad}");
    }

    #[test]
    fn mangohud_hotkey_version_gate() {
        // Upstream #2053 is unfixed: every parseable version warns — including ones
        // newer than today's, because presuming an unreleased fix is the worse error.
        assert!(mangohud_hotkey_affected("v0.8.2"));
        assert!(mangohud_hotkey_affected("0.8.0"));
        assert!(mangohud_hotkey_affected("v0.7.1"));
        assert!(mangohud_hotkey_affected("v0.9.0"));
        assert!(mangohud_hotkey_affected("1.0.0"));
        assert!(mangohud_hotkey_affected("v0.8.3-rc1"));
        // Noise must not alarm.
        assert!(!mangohud_hotkey_affected("git-33c2c7dd"));
        assert!(!mangohud_hotkey_affected(""));
    }

    #[test]
    fn mangohud_hotkey_why_names_the_bug_when_affected() {
        assert!(mangohud_hotkey_why(Some("v0.8.2")).contains("#2053"));
        assert!(mangohud_hotkey_why(None).contains("#2053"));
    }
}
