//! Hand a compat wall to gamebus, which houses findings; beisl only detects them.
//!
//! The contract is gamebus-setup's inbox (its `setup/inbox.rs`, agreed 2026-09-05): one
//! JSON file per drop under `<data>/gamebus-presenced/compat-inbox/`, written to a
//! `.json.tmp` name and renamed onto `.json` in the same directory, named so a lexical
//! sort is arrival order, and never touched again once published. The payload is the
//! flat object `gamebus-setup compat --record` takes; fields it does not know ride
//! along on the observation untouched. gamebus-setup drains the inbox on its next run,
//! so nothing of theirs has to be installed or running when the game exits, and the
//! wrapper never execs their tool on the detection path.
//!
//! Only walls with a Steam appid are dropped: their side keys a finding on it and
//! parks a keyless drop as `.rejected`, which would be litter, not a finding.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use session::Session;

/// Where gamebus-setup looks: `$XDG_DATA_HOME`, else `~/.local/share`, exactly as its
/// stash. The tool offers `compat --inbox` to print this, but asking would put their
/// binary on our exit path; the rule is theirs and small enough to share.
pub fn inbox_dir() -> Option<PathBuf> {
    let data = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))?;
    Some(data.join("gamebus-presenced").join("compat-inbox"))
}

/// One delivery per wall this run hit, in gamebus's flat `--record` shape. Empty when
/// the run has no Steam appid to be keyed on. Facts from the artifact (wine flavor,
/// GPU vendor, layer split, attribution) come from `artifact.json` when the post-run
/// analysis produced one; a walled run often exits clean and is not analyzed, and then
/// `compat_tool` - the launch's compat tool as Steam named it - stands in for the wine
/// flavor, since that is the one fact a ProtonDB report cannot do without.
pub fn deliveries(
    run_dir: &Path,
    sess: &Session,
    walls: &[(&'static str, String)],
    compat_tool: Option<&str>,
) -> Vec<serde_json::Value> {
    if sess.steam_appid.is_empty() || walls.is_empty() {
        return Vec::new();
    }
    let run_id = run_dir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let art =
        artifact::TraceArtifact::read_json(&run_dir.join(session::library::ARTIFACT_FILE)).ok();
    walls
        .iter()
        .map(|(wall, evidence)| {
            let mut d = serde_json::Map::new();
            d.insert("steam_appid".into(), sess.steam_appid.clone().into());
            d.insert("title".into(), sess.game.clone().into());
            d.insert("wall".into(), (*wall).into());
            d.insert("source".into(), "beisl".into());
            d.insert("signature".into(), evidence.clone().into());
            d.insert("log".into(), format!("beisl:{run_id}").into());
            d.insert("trace_dir".into(), run_dir.display().to_string().into());
            d.insert("record_mode".into(), sess.record_mode.clone().into());
            d.insert("beisl_version".into(), env!("CARGO_PKG_VERSION").into());
            match &art {
                Some(a) if a.wine_flavor != "unknown" => {
                    d.insert("wine".into(), a.wine_flavor.clone().into());
                }
                _ => {
                    if let Some(tool) = compat_tool {
                        d.insert("wine".into(), tool.into());
                    }
                }
            }
            if let Some(a) = &art {
                if !a.gpu_vendor.is_empty() {
                    d.insert("gpu_vendor".into(), a.gpu_vendor.clone().into());
                }
                let split: BTreeMap<String, f64> = a
                    .layers
                    .iter()
                    .map(|l| (l.layer.clone(), f64::from(l.pct)))
                    .collect();
                if !split.is_empty() {
                    d.insert(
                        "layer_split".into(),
                        serde_json::to_value(split).unwrap_or_default(),
                    );
                    d.insert("attributed_pct".into(), f64::from(a.attributed_pct).into());
                }
            }
            serde_json::Value::Object(d)
        })
        .collect()
}

/// The compat tool Steam launched with, by its directory name (`SpritzWine-Prater`,
/// `Proton - Experimental`, `GE-Proton11-3`): the first entry of
/// `STEAM_COMPAT_TOOL_PATHS`, which Steam sets for every Proton launch. A name, not a
/// version - the artifact's resolved flavor is better when there is one.
pub fn compat_tool_name(paths: Option<&str>) -> Option<String> {
    let first = paths?.split(':').find(|p| !p.is_empty())?;
    let name = Path::new(first).file_name()?.to_string_lossy().into_owned();
    (!name.is_empty()).then_some(name)
}

/// Publish one drop with every delivery of this run. Returns the published path.
pub fn spool(
    dir: &Path,
    run_id: &str,
    deliveries: &[serde_json::Value],
) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let published = dir.join(format!("{millis}-{run_id}.json"));
    let staged = dir.join(format!("{millis}-{run_id}.json.tmp"));
    let body = serde_json::to_string_pretty(deliveries).unwrap_or_else(|_| "[]".into());
    std::fs::write(&staged, body)?;
    std::fs::rename(&staged, &published)?;
    Ok(published)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("beisl-handoff-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_drop_is_keyed_on_the_appid_and_names_the_wall_in_beisls_words() {
        let run = scratch("run-1788634440");
        let sess = Session {
            game: "WARDOGS Playtest".into(),
            steam_appid: "4809930".into(),
            record_mode: "degraded".into(),
            ..Default::default()
        };
        let walls = vec![(
            "kernel-driver",
            "fixme:ntdll:NtLoadDriver (...elytra...), stub!".to_string(),
        )];
        let d = deliveries(&run, &sess, &walls, Some("SpritzWine-Prater"));
        assert_eq!(d.len(), 1);
        assert_eq!(
            d[0]["wine"], "SpritzWine-Prater",
            "no artifact: the compat tool stands in"
        );
        assert_eq!(d[0]["steam_appid"], "4809930");
        assert_eq!(d[0]["wall"], "kernel-driver");
        assert_eq!(d[0]["title"], "WARDOGS Playtest");
        assert_eq!(d[0]["source"], "beisl");
        assert!(d[0]["log"]
            .as_str()
            .unwrap()
            .starts_with("beisl:beisl-handoff-"));
        // No artifact in this run dir: the analysis-only facts stay absent, not null.
        assert!(d[0].get("layer_split").is_none());
    }

    #[test]
    fn no_appid_means_nothing_to_hand_over() {
        let run = scratch("keyless");
        let sess = Session {
            game: "game.exe".into(),
            ..Default::default()
        };
        assert!(deliveries(&run, &sess, &[("wine-stub", "x".into())], None).is_empty());
    }

    #[test]
    fn the_compat_tool_is_its_directory_name() {
        assert_eq!(
            compat_tool_name(Some("/media/SSD/Projekte/SpritzWine-Prater/build/redist")).as_deref(),
            Some("redist"),
            "a dev checkout names its redist dir; the artifact's flavor is the better source then"
        );
        assert_eq!(
            compat_tool_name(Some("/home/f/.local/share/Steam/compatibilitytools.d/GE-Proton11-3:/x/SteamLinuxRuntime_sniper")).as_deref(),
            Some("GE-Proton11-3")
        );
        assert_eq!(compat_tool_name(None), None);
        assert_eq!(compat_tool_name(Some("")), None);
    }

    #[test]
    fn a_drop_is_published_by_rename_and_sorts_by_arrival() {
        let inbox = scratch("inbox");
        let one = spool(&inbox, "run-1", &[serde_json::json!({"wall": "wine-stub"})]).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(2));
        let two = spool(&inbox, "run-2", &[]).unwrap();
        assert!(one.extension().is_some_and(|e| e == "json"));
        assert!(
            one.file_name().unwrap().to_string_lossy() < two.file_name().unwrap().to_string_lossy()
        );
        let leftovers: Vec<_> = std::fs::read_dir(&inbox)
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "no staged file survives a publish");
        let body: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&one).unwrap()).unwrap();
        assert!(body.is_array());
    }
}
