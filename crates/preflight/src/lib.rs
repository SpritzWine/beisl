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

pub mod config;
pub mod gpu;
pub use config::{Config, DumpAccessPolicy, GpuPin};

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
    KptrRestrict,
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
            CheckId::KptrRestrict => ok_or_fix(kernel_symbols_visible()),
            CheckId::DiskSpace => return, // computed in `disk_check`
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
            why: "The kernel restricts profiling by default; we grant perf the CAP_PERFMON \
                  capability so it can record without weakening the whole system."
                .into(),
            blocking: true,
            status: Status::Pending,
            fix: Some(perf_record_fix()),
        },
        Check {
            id: CheckId::KptrRestrict,
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

/// The shipped rule + tmpfiles bodies, embedded at build time from the repo's `data/`
/// tree so the binary is self-contained (no runtime asset-path discovery) and the files
/// in the repo stay the single source of truth. Written verbatim by [`fix_command`].
const DUMP_RULE_BODY: &str = include_str!("../../../data/udev/99-beisl-gpu-dump.rules");
const DUMP_TMPFILES_BODY: &str = include_str!("../../../data/tmpfiles/beisl-gpu-dump.conf");

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
    } else if hay.contains("debian") || hay.contains("ubuntu") {
        Distro::Debian
    } else if hay.contains("arch") {
        Distro::Arch
    } else if hay.contains("suse") {
        Distro::Suse
    } else {
        Distro::Unknown
    }
}

/// Distro package name(s) for a role, or empty if we don't know it for this distro.
pub fn package_for(role: &str, distro: Distro) -> Vec<&'static str> {
    match (role, distro) {
        ("perf", Distro::Debian) => vec!["linux-perf"],
        ("perf", _) => vec!["perf"],
        ("llvm", _) => vec!["llvm"], // provides llvm-symbolizer on the major distros
        ("mangohud", _) => vec!["mangohud"],
        _ => vec![],
    }
}

/// The exact privileged command the caller runs (via `pkexec`) to apply a fix.
/// Empty means "we can't do it automatically here — tell the user".
pub fn fix_command(fix: &Fix, distro: Distro) -> Vec<String> {
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
        Fix::InstallPackage { role } => {
            let pkgs = package_for(role, distro);
            if pkgs.is_empty() {
                return vec![];
            }
            let mut cmd: Vec<String> = vec!["pkexec".into()];
            match distro {
                Distro::Fedora => cmd.extend(["dnf", "install", "-y"].map(String::from)),
                Distro::Debian => cmd.extend(["apt-get", "install", "-y"].map(String::from)),
                Distro::Arch => cmd.extend(["pacman", "-S", "--noconfirm"].map(String::from)),
                Distro::Suse => cmd.extend(["zypper", "install", "-y"].map(String::from)),
                Distro::Unknown => return vec![],
            }
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
fn dump_remove_script() -> String {
    format!(
        "rm -f '{rule}' '{tmpf}'\nudevadm control --reload-rules\n",
        rule = DUMP_RULE_PATH,
        tmpf = DUMP_TMPFILES_PATH,
    )
}

/// Prefer CAP_PERFMON (surgical) where `perf` is a real ELF binary; on a wrapper script
/// (Debian) or a missing perf, fall back to lowering the sysctl.
fn perf_record_fix() -> Fix {
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
        );
        assert_eq!(cmd[0], "pkexec");
        assert!(cmd.contains(&"kernel.perf_event_paranoid=-1".to_string()));
    }

    #[test]
    fn install_maps_per_distro() {
        assert_eq!(package_for("perf", Distro::Debian), vec!["linux-perf"]);
        assert_eq!(package_for("perf", Distro::Fedora), vec!["perf"]);
        let arch = fix_command(&Fix::InstallPackage { role: "mangohud" }, Distro::Arch);
        assert_eq!(
            arch,
            vec!["pkexec", "pacman", "-S", "--noconfirm", "mangohud"]
        );
        // Unknown distro: no automatic command.
        assert!(fix_command(&Fix::InstallPackage { role: "perf" }, Distro::Unknown).is_empty());
    }

    #[test]
    fn setcap_fix_targets_the_perf_binary_when_present() {
        // Only assert shape when perf exists on the test host.
        if let Some(p) = perf_path() {
            let cmd = fix_command(&Fix::SetcapPerfmon, Distro::Fedora);
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
        let cmd = fix_command(&Fix::InstallDumpAccessRule, Distro::Fedora);
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

        let rm = fix_command(&Fix::RemoveDumpAccessRule, Distro::Fedora);
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
    fn all_checks_have_why_and_blocking_flags() {
        let checks = all_checks();
        assert_eq!(checks.len(), 5);
        assert!(checks.iter().all(|c| !c.why.is_empty()));
        // Kernel symbols and MangoHud are advisory; everything else gates a trace.
        assert_eq!(
            checks
                .iter()
                .filter(|c| !c.blocking)
                .map(|c| c.id)
                .collect::<Vec<_>>(),
            vec![CheckId::KptrRestrict, CheckId::MangoHud]
        );
    }

    #[test]
    fn kptr_check_is_advisory_and_never_joins_the_sweep() {
        let c = all_checks()
            .into_iter()
            .find(|c| c.id == CheckId::KptrRestrict)
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
}
