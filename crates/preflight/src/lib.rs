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

use std::path::{Path, PathBuf};

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
}

/// How to repair a failing check. Turned into a concrete `pkexec` argv by [`fix_command`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Fix {
    /// Install a package; `role` is mapped to the distro's package name at fix time.
    InstallPackage { role: &'static str },
    /// Lower a sysctl (root-only, hence `pkexec`). The blunt fallback.
    Sysctl { key: &'static str, value: &'static str },
    /// Grant `perf` CAP_PERFMON — least-privilege, no global change, output stays
    /// user-owned. Preferred where `perf` is a real ELF binary; on Debian's wrapper
    /// script it doesn't stick, so [`perf_record_fix`] picks `Sysctl` there instead.
    SetcapPerfmon,
}

#[derive(Clone, Debug)]
pub struct Check {
    pub id: CheckId,
    pub label: &'static str,
    /// The teaching / entertainment line bound to this step.
    pub why: &'static str,
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
                let can_record = perf_has_perfmon()
                    || matches!(read_paranoid(), Some(v) if v <= PARANOID_MAX);
                ok_or_fix(can_record)
            }
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
            why: "We sample the CPU with perf to see where wine spends its time.",
            blocking: true,
            status: Status::Pending,
            fix: Some(Fix::InstallPackage { role: "perf" }),
        },
        Check {
            id: CheckId::LlvmSymbolizer,
            label: "llvm-symbolizer",
            why: "Turns raw addresses in the trace into function names.",
            blocking: true,
            status: Status::Pending,
            fix: Some(Fix::InstallPackage { role: "llvm" }),
        },
        Check {
            id: CheckId::PerfParanoid,
            label: "perf can record",
            why: "The kernel restricts profiling by default; we grant perf the CAP_PERFMON \
                  capability so it can record without weakening the whole system.",
            blocking: true,
            status: Status::Pending,
            fix: Some(perf_record_fix()),
        },
        Check {
            id: CheckId::MangoHud,
            label: "MangoHud (optional)",
            why: "Its frametime log flags the low-perf moments to zoom into. Optional.",
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
              headroom and stop the capture if it runs low.",
        blocking: false,
        status,
        fix: None,
    }
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
            &Fix::Sysctl { key: "kernel.perf_event_paranoid", value: "-1" },
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
        assert_eq!(arch, vec!["pkexec", "pacman", "-S", "--noconfirm", "mangohud"]);
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
    fn all_checks_have_why_and_blocking_flags() {
        let checks = all_checks();
        assert_eq!(checks.len(), 4);
        assert!(checks.iter().all(|c| !c.why.is_empty()));
        // MangoHud is the only optional one in the core list.
        assert_eq!(
            checks.iter().filter(|c| !c.blocking).map(|c| c.id).collect::<Vec<_>>(),
            vec![CheckId::MangoHud]
        );
    }
}
