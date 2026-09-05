//! What Steam itself says about the game it just launched.
//!
//! Every Steam launch runs `%command%` with `SteamAppId` in its environment, with an
//! `AppId=N` token in the launch-wrapper argv, and with the game installed under a
//! library whose `appmanifest_N.acf` names it. First-party evidence, in hand at spawn
//! time, before a single frame is drawn.
//!
//! gamebus presence (presence.rs) outranks this when it answers: it correlates Discord
//! and GameMode too. But it only sees a game once its `/proc` sweep catches the process,
//! every 15 s, and a run walled by anti-cheat is usually gone by then. Those are exactly
//! the runs a compat finding is about, so their appid must come from something that has
//! no such window.

use std::path::PathBuf;

/// The appid, and the product name the library's manifest gives it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SteamIdentity {
    pub appid: String,
    /// `None` when no library we can see has a manifest for the appid - a non-Steam
    /// shortcut, or a Steam install in a place we didn't think to look.
    pub name: Option<String>,
}

/// The identity Steam attached to this launch, or `None` when it wasn't a Steam one.
pub fn from_launch(cmd: &[String]) -> Option<SteamIdentity> {
    let appid = ["SteamAppId", "SteamGameId"]
        .iter()
        .find_map(|var| std::env::var(var).ok().and_then(|v| numeric_appid(&v)))
        .or_else(|| appid_from_argv(cmd))?;
    let name = library_roots(cmd).into_iter().find_map(|root| {
        let acf = root
            .join("steamapps")
            .join(format!("appmanifest_{appid}.acf"));
        std::fs::read_to_string(acf)
            .ok()
            .and_then(|text| vdf_values(&text, "name").next())
    });
    Some(SteamIdentity { appid, name })
}

/// Steam sets `SteamAppId=0` for non-Steam shortcuts and `default` for its own client
/// processes; neither names a game.
fn numeric_appid(v: &str) -> Option<String> {
    v.trim()
        .parse::<u64>()
        .ok()
        .filter(|&n| n > 0)
        .map(|n| n.to_string())
}

/// `steam-launch-wrapper -- reaper SteamLaunch AppId=4809930 -- ...`: reaper's token,
/// present on every Linux Steam launch whether or not the environment survived.
fn appid_from_argv(cmd: &[String]) -> Option<String> {
    cmd.iter()
        .find_map(|a| a.strip_prefix("AppId="))
        .and_then(numeric_appid)
}

/// Libraries worth opening, most specific first: a game path in argv names its own
/// library outright, then whatever `libraryfolders.vdf` lists for a launch that carries
/// no path we can see.
fn library_roots(cmd: &[String]) -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = Vec::new();
    let mut push = |root: PathBuf| {
        if !roots.contains(&root) {
            roots.push(root);
        }
    };
    for a in cmd {
        if let Some((lib, _)) = a.split_once("/steamapps/common/") {
            push(PathBuf::from(lib));
        }
    }
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        // Native and Flatpak client locations; the first readable list wins.
        let lists = [
            ".local/share/Steam",
            ".steam/root",
            ".var/app/com.valvesoftware.Steam/.local/share/Steam",
        ];
        if let Some(text) = lists.iter().find_map(|d| {
            std::fs::read_to_string(home.join(d).join("steamapps/libraryfolders.vdf")).ok()
        }) {
            for path in vdf_values(&text, "path") {
                push(PathBuf::from(path));
            }
        }
    }
    roots
}

/// Values of every `"key" "value"` line in Valve's KeyValues text, in order. No
/// nesting: the two files read here are flat where it matters, and a full parser
/// would outweigh the rest of this module.
fn vdf_values<'a>(text: &'a str, key: &'a str) -> impl Iterator<Item = String> + 'a {
    text.lines().filter_map(move |line| {
        let rest = line.trim_start().strip_prefix('"')?;
        let (k, rest) = rest.split_once('"')?;
        if k != key {
            return None;
        }
        let rest = rest.trim_start().strip_prefix('"')?;
        // Game names carry quotes and backslashes; VDF escapes both.
        let mut value = String::new();
        let mut chars = rest.chars();
        while let Some(c) = chars.next() {
            match c {
                '"' => return Some(value),
                '\\' => value.push(chars.next()?),
                c => value.push(c),
            }
        }
        None
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steam_client_placeholders_are_not_appids() {
        assert_eq!(numeric_appid("4809930"), Some("4809930".to_string()));
        assert_eq!(numeric_appid(" 480 "), Some("480".to_string()));
        assert_eq!(numeric_appid("0"), None);
        assert_eq!(numeric_appid("default"), None);
        assert_eq!(numeric_appid(""), None);
    }

    #[test]
    fn appid_comes_from_reapers_token() {
        // The launch argv as beisl.log records it (2026-09-05, WARDOGS Playtest).
        let cmd: Vec<String> = [
            "/home/f/.local/share/Steam/steamrt64/steam-launch-wrapper",
            "--",
            "/home/f/.local/share/Steam/steamrt64/reaper",
            "SteamLaunch",
            "AppId=4809930",
            "--",
            "/media/NVME/SteamLibrary/steamapps/common/WARDOGS Playtest/WardogsLauncher-Shipping.exe",
        ]
        .map(String::from)
        .to_vec();
        assert_eq!(appid_from_argv(&cmd), Some("4809930".to_string()));
        assert_eq!(
            library_roots(&cmd)[0],
            PathBuf::from("/media/NVME/SteamLibrary"),
            "the game's own library comes first, before any libraryfolders.vdf entry"
        );
        assert_eq!(
            appid_from_argv(&["umu-run".to_string(), "game.exe".to_string()]),
            None
        );
    }

    #[test]
    fn vdf_lines_parse_with_tabs_and_escapes() {
        let acf = "\"AppState\"\n{\n\t\"appid\"\t\t\"4809930\"\n\t\"name\"\t\t\"WARDOGS Playtest\"\n\t\"installdir\"\t\t\"WARDOGS Playtest\"\n}\n";
        assert_eq!(
            vdf_values(acf, "name").next().as_deref(),
            Some("WARDOGS Playtest")
        );
        assert_eq!(vdf_values(acf, "appid").next().as_deref(), Some("4809930"));
        assert_eq!(vdf_values(acf, "missing").next(), None);
        let quoted = r#"	"name"		"Marc's \"Big\" Adventure""#;
        assert_eq!(
            vdf_values(quoted, "name").next().as_deref(),
            Some(r#"Marc's "Big" Adventure"#)
        );
        let folders = "\"libraryfolders\"\n{\n\t\"0\"\n\t{\n\t\t\"path\"\t\t\"/home/f/.local/share/Steam\"\n\t}\n\t\"1\"\n\t{\n\t\t\"path\"\t\t\"/media/NVME/SteamLibrary\"\n\t}\n}\n";
        let paths: Vec<String> = vdf_values(folders, "path").collect();
        assert_eq!(
            paths,
            ["/home/f/.local/share/Steam", "/media/NVME/SteamLibrary"]
        );
    }
}
