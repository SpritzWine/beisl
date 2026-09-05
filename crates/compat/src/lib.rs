//! What a log line says about a game's ability to run at all.
//!
//! One judgment, two readers: the wrapper tails the logs while the game runs and warns in
//! the session window; beisl-mcp reads the same files after the fact and reports a
//! `compat-wall` event. Both must agree on what a wall is, so the rule lives here and
//! nowhere else. std only, like everything the wrapper links.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::OnceLock;

/// The `compat-wall` signatures, WORST FIRST - worst meaning "least liftable from here":
/// a vendor kernel driver is a wall nobody on this side can remove, while a missing export
/// or a path bug are Wine bugs somebody can actually fix. The first one a run matches
/// becomes the event's `signature`; the rest ride along in `signatures`, so one event per
/// run keeps `list_events`' dedupe-by-`(run_id, kind)` contract intact.
pub const WALL_PRIORITY: &[&str] = &["kernel-driver", "wine-stub", "volume-guid"];

/// The kernel drivers Wine actually SHIPS on this machine, keyed by STEM (`winebus`, not
/// `winebus.sys`) - discovered once, on first
/// need, and cached for the life of the process. Wine loading one of these is Wine doing
/// its job; a driver from OUTSIDE this set is a vendor kernel module, which no Wine can
/// honour. That difference is the whole gate of the `kernel-driver` wall signature -
/// without it, every ordinary run that brings up winebus and mountmgr would fire.
///
/// Discovered rather than listed on purpose. Wine's driver roster changes release to
/// release, and two trees on one machine need not agree (a distro wine and a Proton build
/// ship different sets), so a roster baked into this binary would start rotting the day it
/// was written and would be wrong for anyone whose Wine is newer than their beisl. The
/// scan is the same "builtins live under the wine tree" rule `symbol-resolver`'s
/// `classify_layer` already uses to tell a wine PE from a game's own.
///
/// Keyed by stem because Wine names a driver two ways and only the stem is common to
/// both: the file is `winebus.sys`, but `NtLoadDriver` logs the registry service name,
/// `...\\Services\\MountMgr`. Verified against a real log: all seven services Wine loaded
/// there (MountMgr, NDIS, nsiproxy, winebth, winebus, winehid, wineusb) map onto shipped
/// files by stem, and the vendor one did not.
///
/// An EMPTY result is meaningful: no Wine tree was found, so builtin and vendor cannot be
/// told apart - and [`wall_signature`] then declines to fire rather than guessing. Silent
/// beats wrong here, per this tool's whole stance.
pub fn wine_builtin_drivers() -> &'static BTreeSet<String> {
    static CACHE: OnceLock<BTreeSet<String>> = OnceLock::new();
    CACHE.get_or_init(|| {
        let mut found = BTreeSet::new();
        for root in wine_tree_roots() {
            // Builtin PEs live at <root>/lib{,64}/wine/<arch>-windows/. Both libdirs:
            // Fedora puts the tree in lib64, Debian and every Proton build in lib.
            for libdir in ["lib", "lib64"] {
                let Ok(arches) = std::fs::read_dir(root.join(libdir).join("wine")) else {
                    continue;
                };
                for arch in arches.flatten() {
                    let Ok(files) = std::fs::read_dir(arch.path()) else {
                        continue;
                    };
                    for file in files.flatten() {
                        let name = file.file_name().to_string_lossy().to_ascii_lowercase();
                        if let Some(stem) = name.strip_suffix(".sys") {
                            found.insert(stem.to_string());
                        }
                    }
                }
            }
        }
        found
    })
}

/// Where a Wine tree plausibly lives on this machine, for [`wine_builtin_drivers`].
/// Cheap by construction - a handful of `read_dir`s, no walk: every Steam
/// `compatibilitytools.d` entry IS a wine build, and official Proton is picked out of
/// `steamapps/common` BY NAME rather than by descending into every installed game.
/// Proton and GE trees nest the wine tree one level down (`dist/` or `files/`), so each
/// root is offered in all three shapes.
fn wine_tree_roots() -> Vec<PathBuf> {
    let mut roots = vec![PathBuf::from("/usr"), PathBuf::from("/usr/local")];
    let home = PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
    // The same two Steam roots preflight's own `steam_client_scrubs_path` checks.
    for steam in [".local/share/Steam", ".steam/steam"] {
        let steam = home.join(steam);
        if let Ok(tools) = std::fs::read_dir(steam.join("compatibilitytools.d")) {
            roots.extend(tools.flatten().map(|e| e.path()));
        }
        if let Ok(games) = std::fs::read_dir(steam.join("steamapps/common")) {
            roots.extend(
                games
                    .flatten()
                    .filter(|e| e.file_name().to_string_lossy().starts_with("Proton"))
                    .map(|e| e.path()),
            );
        }
    }
    roots
        .into_iter()
        .flat_map(|r| [r.join("dist"), r.join("files"), r])
        .collect()
}

/// Driver identities named on one already-lowercased log line, normalised to a stem so
/// they can be compared against [`wine_builtin_drivers`].
///
/// Two shapes, because Wine names a driver two ways and the interesting one is NOT the
/// filename. `NtLoadDriver` logs the registry service path - `...\Services\MountMgr` - and
/// a first version of this looked only for `.sys` filenames, found nothing on those lines,
/// and so never fired at all. A `.sys` path is still matched because other Wine paths do
/// carry one.
pub fn driver_stems(lower_line: &str) -> Vec<String> {
    let mut stems = Vec::new();
    // Registry service path: everything after the last `services\` up to the closing
    // quote or separator. Wine's debugstr doubles the backslashes, hence the trim.
    if let Some(rest) = lower_line.rsplit("services\\").next() {
        if rest != lower_line {
            let name: String = rest
                .trim_start_matches('\\')
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '-' || *c == '.')
                .collect();
            let name = name.strip_suffix(".sys").unwrap_or(&name);
            if !name.is_empty() {
                stems.push(name.to_string());
            }
        }
    }
    // A `.sys` filename anywhere on the line.
    let mut from = 0;
    while let Some(hit) = lower_line[from..].find(".sys") {
        let end = from + hit;
        let start = lower_line[..end]
            .rfind(['\\', '/', '"', '\'', ' ', '\t', '(', ',', '=', ':'])
            .map_or(0, |i| i + 1);
        if start < end {
            stems.push(lower_line[start..end].to_string());
        }
        from = end + ".sys".len();
    }
    stems
}

/// Which compat wall, if any, one log line is evidence of. Both arms are EXACT matches on
/// text a healthy run cannot produce - the same stance as the `gpu` kind, and the reason
/// this kind needs no crash to gate it (a wall often ends in a *clean* exit: the game
/// refuses, quits politely, and every other signal stays silent).
///
/// * `wine-stub` - the game called an export this Wine doesn't have. Wine has TWO
///   emitters that word it differently, so the match is on the words they share rather
///   than on either one's full sentence. `dlls/ntdll/exception.c` says "wine: Call from
///   %p to unimplemented function %s.%s, aborting" from the game process via `MESSAGE`;
///   `programs/winedbg/info.c` says "unimplemented function %s.%s called" from winedbg,
///   the debugger Wine spawns on an unhandled exception. The winedbg form is what an
///   actual stub CRASH produces - the common case - and requiring the ntdll wording
///   missed exactly that. Neither is channel-gated (`MESSAGE` resolves to
///   `wine_dbg_printf` with no `TRACE_ON` test in the path, and winedbg's `dbg_printf`
///   is a plain write from another process), so `WINEDEBUG` cannot suppress either and
///   this signature costs the user nothing. The other two arms are channel output
///   (`ntdll`/`file`) and only appear for a user who enabled those themselves.
/// * `volume-guid` - `c00000cb` (STATUS_BAD_DEVICE_TYPE) on a volume-GUID path. The
///   status alone is far too common to match on: in a real trace, 48 of its 48
///   occurrences were `\??\pipe\...` named-pipe failures, entirely unrelated. It is the
///   CO-OCCURRENCE with a `Volume{` path on the same line that makes it exact. Unlike
///   `wine-stub` this is `TRACE()` output on the `file` channel, so it only appears for
///   someone who has turned that channel on themselves; it can never fire by default.
///
/// * `kernel-driver` - a driver load naming a driver Wine does not ship: a vendor
///   kernel-mode module, the wall no Wine can honour. The evidence is the load ATTEMPT,
///   which Wine records even though it cannot satisfy it - the real WARDOGS line is
///   `fixme:ntdll:NtLoadDriver (L"...\Services\elytra_ZkqZiVbyre8nFULw"), stub!`. What
///   the line carries is a registry SERVICE name, not a filename, which is why matching
///   only `.sys` paths made this arm inert; see [`driver_stems`]. `builtins` empty means
///   no Wine tree was found to compare against, and the arm then stays silent rather than
///   calling winebus an anti-cheat.
pub fn wall_signature(lower_line: &str, builtins: &BTreeSet<String>) -> Option<&'static str> {
    let driver_load = lower_line.contains("zwloaddriver") || lower_line.contains("ntloaddriver");
    if driver_load
        && !builtins.is_empty()
        && driver_stems(lower_line)
            .iter()
            .any(|stem| !builtins.contains(stem))
    {
        return Some("kernel-driver");
    }
    if lower_line.contains("unimplemented function ") {
        return Some("wine-stub");
    }
    if lower_line.contains("c00000cb") && lower_line.contains("volume{") {
        return Some("volume-guid");
    }
    None
}

/// What to tell a person watching the run, per signature: what happened, and whether
/// anyone on this side can do anything about it.
pub fn explain(signature: &str) -> &'static str {
    match signature {
        "kernel-driver" => {
            "The game asked Wine to load a kernel-mode driver it does not ship - an \
             anti-cheat, most likely. Wine cannot honour that, so expect the game to refuse \
             to start. Nothing on this machine can lift it."
        }
        "wine-stub" => {
            "The game called a function this Wine does not implement. That is a gap in \
             Wine, fixable upstream; the crash it causes is not this machine's fault."
        }
        "volume-guid" => {
            "Wine failed to resolve a volume-GUID path (status c00000cb). A Wine path bug, \
             fixable upstream."
        }
        _ => "The game log names a reason this game structurally cannot run here.",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn builtins(names: &[&str]) -> BTreeSet<String> {
        names.iter().map(|n| n.to_string()).collect()
    }

    #[test]
    fn a_vendor_driver_service_is_a_wall_but_wines_own_are_not() {
        // Verbatim from a real Proton log of the WARDOGS wall (2026-09-05). The vendor line
        // names a registry SERVICE, not a .sys file; Wine's own load on every run.
        let wine = builtins(&[
            "mountmgr", "ndis", "nsiproxy", "winebth", "winebus", "winehid", "wineusb",
        ]);
        let vendor = r#"32171.287:015c:0190:fixme:ntdll:NtLoadDriver (L"\\Registry\\Machine\\System\\CurrentControlSet\\Services\\elytra_r7tC-LhUH4-egxUm"), stub!"#;
        assert_eq!(
            wall_signature(&vendor.to_lowercase(), &wine),
            Some("kernel-driver")
        );
        for own in [
            r#"8113.430:003c:004c:trace:ntoskrnl:ZwLoadDriver (L"\\Registry\\Machine\\System\\CurrentControlSet\\Services\\MountMgr")"#,
            r#"8113.779:003c:004c:trace:ntoskrnl:ZwLoadDriver (L"\\Registry\\Machine\\System\\CurrentControlSet\\Services\\NDIS")"#,
        ] {
            assert_eq!(wall_signature(&own.to_lowercase(), &wine), None, "{own}");
        }
        // No Wine tree found: builtin and vendor cannot be told apart, so decline.
        assert_eq!(wall_signature(&vendor.to_lowercase(), &builtins(&[])), None);
    }

    #[test]
    fn stems_come_from_service_paths_and_sys_files_alike() {
        assert_eq!(
            driver_stems(
                r#"(l"\\registry\\machine\\system\\currentcontrolset\\services\\mountmgr")"#
            ),
            ["mountmgr"]
        );
        assert_eq!(
            driver_stems(r#"(l"\\??\\c:\\windows\\system32\\drivers\\lighthouse_driver.sys")"#),
            ["lighthouse_driver"]
        );
    }

    #[test]
    fn the_other_two_arms_are_exact() {
        let none = builtins(&[]);
        assert_eq!(
            wall_signature(
                "wine: call from 0x7b00 to unimplemented function ntdll.dll.x, aborting",
                &none
            ),
            Some("wine-stub")
        );
        assert_eq!(
            wall_signature(
                r#"err:ntdll:get_dos_device l"\\??\\volume{9c8e}\\" failed with c00000cb"#,
                &none
            ),
            Some("volume-guid")
        );
        assert_eq!(
            wall_signature("err:ntdll:socket failed, status c00000cb", &none),
            None
        );
    }

    #[test]
    fn every_signature_has_words_for_a_human() {
        for sig in WALL_PRIORITY {
            assert!(explain(sig).len() > 40, "{sig}");
        }
    }
}
