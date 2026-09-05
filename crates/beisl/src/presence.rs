//! Game identity from gamebus-presenced, over `org.gamebus.Presence.v1`.
//!
//! beisl names a run after whatever it can see: the launcher argv (`upc.exe`), then the
//! MangoHud CSV's process name (`acs`, `Icarus-Win64-Shipping`). Neither is what a human
//! calls the game, and neither is a Steam appid - and a compat finding is worth nothing
//! to ProtonDB or AreWeAntiCheatYet without both. gamebus-presenced already correlates
//! Steam, Discord and GameMode into exactly that, so beisl reads it rather than
//! re-deriving it. The arrow points one way: beisl consumes presence, never the reverse.
//!
//! Read through `busctl` rather than a D-Bus crate on purpose. The main workspace is
//! std-only and crates.io-light (see CLAUDE.md); zbus would drag an async runtime into a
//! wrapper that spawns `perf` and waits. Shelling out is what this crate already does for
//! every other external tool, and it costs one short-lived process per attempt.
//!
//! Everything here is best-effort and silent on failure: gamebus is optional, and a
//! missing service must never colour a trace.

use std::process::Command;

/// How long to let `busctl` block. The caller is inside the wrapper's poll loop, so a
/// wedged bus must not stall the trace: two seconds is far longer than a live service
/// needs and short enough that nobody notices when one isn't there.
const BUS_TIMEOUT: &str = "--timeout=2";

const SERVICE: &str = "org.gamebus.Presence.v1";
const MANAGER_PATH: &str = "/org/gamebus/Presence/v1";
const MANAGER_IFACE: &str = "org.gamebus.Presence.v1.Manager";
const ACTIVITY_IFACE: &str = "org.gamebus.Presence.v1.Activity";

/// What presence knows that beisl doesn't.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Presence {
    /// Human name for the game ("WARDOGS Playtest"), as Steam or Discord reports it.
    pub name: String,
    /// `app_ids["steam"]` - the Steam appid, the id ProtonDB and gamedb both key on.
    /// `None` for a non-Steam launch, which is a normal case, not a failure.
    pub steam_appid: Option<String>,
}

/// Ask presence what is running, or `None` if it can't say.
///
/// `None` covers every uninteresting case identically - no `busctl`, no service on the
/// bus, no activity, or more than one activity we can't choose between - because the
/// caller's response to all of them is the same: keep the name it already had.
pub fn lookup() -> Option<Presence> {
    let paths = activity_paths()?;
    // One activity is the ordinary case and needs no tie-break. With several we would be
    // guessing which one is the game beisl launched - presence exposes a pid, but with a
    // launcher chain the pid beisl knows is the launcher's, not the game's - so we take
    // the only one that carries a Steam appid and otherwise decline. A wrong game name on
    // a trace is worse than no upgrade at all.
    let chosen = match paths.len() {
        0 => return None,
        1 => read_activity(&paths[0])?,
        _ => {
            let mut with_appid = paths
                .iter()
                .filter_map(|p| read_activity(p))
                .filter(|a| a.steam_appid.is_some());
            let first = with_appid.next()?;
            if with_appid.next().is_some() {
                return None;
            }
            first
        }
    };
    if chosen.name.is_empty() {
        return None;
    }
    Some(chosen)
}

/// `Manager.ListActivities()`, as object paths. `busctl` prints an `ao` reply as the
/// type, the count, then each path quoted: `ao 1 "/org/gamebus/..."`.
fn activity_paths() -> Option<Vec<String>> {
    let out = busctl(&[
        "call",
        SERVICE,
        MANAGER_PATH,
        MANAGER_IFACE,
        "ListActivities",
    ])?;
    Some(quoted_fields(&out))
}

/// The two properties beisl wants off one activity object. Read in a single `busctl`
/// call - it accepts several property names and answers one line each, in order - so a
/// property vanishing mid-read can't pair a name with someone else's appid.
fn read_activity(path: &str) -> Option<Presence> {
    let out = busctl(&[
        "get-property",
        SERVICE,
        path,
        ACTIVITY_IFACE,
        "Name",
        "AppIds",
    ])?;
    let mut lines = out.lines();
    let name = quoted_fields(lines.next()?).into_iter().next()?;
    let appid = steam_appid(lines.next().unwrap_or_default());
    Some(Presence {
        name,
        steam_appid: appid,
    })
}

/// `app_ids["steam"]` out of a `busctl` dict line: `a{ss} 2 "steam" "480" "discord" "x"`.
/// The fields alternate key, value after the leading type and count, so the appid is
/// whatever follows the `steam` key.
fn steam_appid(line: &str) -> Option<String> {
    let fields = quoted_fields(line);
    let i = fields.iter().position(|f| f == "steam")?;
    fields.get(i + 1).filter(|v| !v.is_empty()).cloned()
}

/// Every double-quoted field in a `busctl` output line. `busctl` escapes an embedded
/// quote as `\"`, which matters because a game name is arbitrary text.
fn quoted_fields(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        if c != '"' {
            continue;
        }
        let mut field = String::new();
        loop {
            match chars.next() {
                None => return out, // unterminated: a truncated line, take what we have
                Some('\\') => {
                    if let Some(escaped) = chars.next() {
                        field.push(escaped);
                    }
                }
                Some('"') => break,
                Some(c) => field.push(c),
            }
        }
        out.push(field);
    }
    out
}

/// Run `busctl --user` and hand back stdout, or `None` for any failure at all - the
/// binary missing, the service absent, a non-zero exit. The caller cannot act on the
/// difference, and gamebus is an optional dependency.
fn busctl(args: &[&str]) -> Option<String> {
    let out = Command::new("busctl")
        .arg("--user")
        .arg(BUS_TIMEOUT)
        .args(args)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_shapes_busctl_actually_prints() {
        // Captured from a live `busctl --user` against gamebus-presenced 0.3.0
        // (2026-09-05): type tag, count, then the values.
        assert_eq!(
            quoted_fields(r#"ao 1 "/org/gamebus/Presence/v1/Activity/steam-1867240""#),
            ["/org/gamebus/Presence/v1/Activity/steam-1867240"]
        );
        assert_eq!(quoted_fields("ao 0"), Vec::<String>::new());
        assert_eq!(
            quoted_fields(r#"s "WARDOGS Playtest""#),
            ["WARDOGS Playtest"]
        );
    }

    #[test]
    fn a_quoted_game_name_survives_parsing() {
        // Game names are arbitrary text and some carry quotes; busctl escapes them.
        assert_eq!(
            quoted_fields(r#"s "Marc's \"Big\" Adventure""#),
            [r#"Marc's "Big" Adventure"#]
        );
    }

    #[test]
    fn steam_appid_is_read_by_key_not_by_position() {
        // The dict order is not ours to rely on: discord may come first.
        assert_eq!(
            steam_appid(r#"a{ss} 2 "discord" "client-9" "steam" "1867240""#),
            Some("1867240".to_string())
        );
        assert_eq!(
            steam_appid(r#"a{ss} 1 "steam" "480""#),
            Some("480".to_string())
        );
    }

    #[test]
    fn a_non_steam_launch_has_no_appid_and_that_is_fine() {
        assert_eq!(steam_appid(r#"a{ss} 1 "discord" "client-9""#), None);
        assert_eq!(steam_appid("a{ss} 0"), None);
        assert_eq!(steam_appid(r#"a{ss} 1 "steam" """#), None);
    }
}
