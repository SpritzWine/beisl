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

/// What one ask came back with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Lookup {
    Found(Presence),
    /// The service answered and had nothing for us: no activity, or none we would
    /// stake a trace's name on.
    Nothing,
    /// No `busctl`, or no service on the bus. The caller's response is the same as for
    /// `Nothing` - keep the name it has - but at the end of a run it is worth one log
    /// line saying which it was.
    Unreachable,
}

/// Ask presence what is running.
///
/// With `steam_appid` known (Steam told us at launch), the only acceptable answer is the
/// activity carrying that appid: a second Steam game running alongside must not lend its
/// name to this trace. Without one, a single activity is taken as is; among several, the
/// only one with a Steam appid, else we decline. Presence exposes a pid too, but with a
/// launcher chain the pid beisl knows is the launcher's, so it is no tie-break. A wrong
/// game name on a trace is worse than no upgrade at all.
pub fn lookup(steam_appid: Option<&str>) -> Lookup {
    let Some(paths) = activity_paths() else {
        return Lookup::Unreachable;
    };
    let chosen = match (steam_appid, paths.len()) {
        (_, 0) => None,
        (Some(want), _) => paths
            .iter()
            .filter_map(|p| read_activity(p))
            .find(|a| a.steam_appid.as_deref() == Some(want)),
        (None, 1) => read_activity(&paths[0]),
        (None, _) => {
            let mut with_appid = paths
                .iter()
                .filter_map(|p| read_activity(p))
                .filter(|a| a.steam_appid.is_some());
            let first = with_appid.next();
            if with_appid.next().is_some() {
                None
            } else {
                first
            }
        }
    };
    match chosen {
        Some(p) if !p.name.is_empty() => Lookup::Found(p),
        _ => Lookup::Nothing,
    }
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
