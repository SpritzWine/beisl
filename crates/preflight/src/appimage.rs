//! Running from an AppImage: knowing it, and the shims that point at the file.
//!
//! An AppImage is the application, as one file the user keeps wherever they like.
//! So nothing is copied out of it. Setting up writes small scripts into
//! `~/.local/bin` that hand over to the file at its actual path, and setup rechecks
//! that path every time it runs (DESIGN.md, "AppImage").
//!
//! Detection only, like the rest of this crate: what is here inspects and renders;
//! `beisl install` does the writing.

use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

/// What the AppImage runtime exports into its process. They describe beisl's own
/// packaging and are none of the game's business, so the wrapper drops them before
/// it starts anything.
pub const RUNTIME_VARS: [&str; 4] = ["APPIMAGE", "APPDIR", "ARGV0", "OWD"];

/// The tools a shim is written for. Each is a binary in the image's `usr/bin`.
pub const TOOLS: [&str; 4] = ["beisl", "beisl-setup", "beisl-compat", "beisl-mcp"];

/// The image this process was started from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Running {
    /// The `.AppImage` file itself, as the user has it.
    pub appimage: PathBuf,
    /// Where the runtime mounted it.
    pub appdir: PathBuf,
}

/// `Some` only when it can be shown, not merely claimed. `$APPIMAGE` is inherited:
/// a natively installed beisl-setup started from a terminal that is itself an
/// AppImage sees that other program's path, and would point the shims at it. So the
/// variables count only if this executable lives inside the mounted directory AND
/// that directory carries beisl's own AppStream component.
pub fn running() -> Option<Running> {
    let appimage = PathBuf::from(std::env::var_os("APPIMAGE")?);
    let appdir = PathBuf::from(std::env::var_os("APPDIR")?);
    let exe = std::env::current_exe().ok()?;
    validate(&exe, &appdir, &appimage)
}

fn validate(exe: &Path, appdir: &Path, appimage: &Path) -> Option<Running> {
    let exe = exe.canonicalize().ok()?;
    let dir = appdir.canonicalize().ok()?;
    if !exe.starts_with(&dir) || !appimage.is_absolute() || !appimage.is_file() {
        return None;
    }
    let id = assets::metainfo::ID;
    let component = dir.join(format!("usr/share/metainfo/{id}.metainfo.xml"));
    let xml = std::fs::read_to_string(component).ok()?;
    xml.contains(&format!("<id>{id}</id>")).then(|| Running {
        appimage: appimage.to_path_buf(),
        appdir: dir,
    })
}

/// Drop the runtime's variables from this process, so no child inherits them.
pub fn strip_runtime_env() {
    for var in RUNTIME_VARS {
        std::env::remove_var(var);
    }
}

/// Why the file's location lets somebody else decide what runs as this user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Issue {
    NotOwned,
    WritableByOthers,
    /// A directory on the way to the file where others may replace entries.
    DirWritableByOthers(PathBuf),
    /// Removable or network storage, by filesystem type or bus. Advisory: the shim
    /// does not check this at launch, since the file may simply be absent then.
    ForeignMedia(String),
}

impl Issue {
    pub fn explain(&self) -> String {
        match self {
            Issue::NotOwned => "The file is not owned by you.".into(),
            Issue::WritableByOthers => "Other users can write to the file.".into(),
            Issue::DirWritableByOthers(d) => {
                format!("Other users can replace files in {}.", d.display())
            }
            Issue::ForeignMedia(what) => {
                format!("It is on {what}, which may be absent or changed when a game starts.")
            }
        }
    }

    /// Whether the shim's own launch-time check trips on this.
    pub fn blocks_strict_shim(&self) -> bool {
        !matches!(self, Issue::ForeignMedia(_))
    }
}

fn my_uid() -> Option<u32> {
    std::fs::metadata("/proc/self").ok().map(|m| m.uid())
}

/// A group that is only this user (the "user private group" convention) does not
/// widen who can write. Read from /etc/group and /etc/passwd; when that cannot be
/// settled the answer is no, which errs toward telling the user.
fn group_is_private(gid: u32, uid: u32) -> bool {
    let field = |line: &str, n: usize| line.split(':').nth(n).map(str::to_string);
    let user = std::fs::read_to_string("/etc/passwd").ok().and_then(|p| {
        p.lines()
            .find(|l| field(l, 2).as_deref() == Some(&uid.to_string()))
            .and_then(|l| field(l, 0))
    });
    let group = std::fs::read_to_string("/etc/group").ok().and_then(|g| {
        g.lines()
            .find(|l| field(l, 2).as_deref() == Some(&gid.to_string()))
            .map(|l| (field(l, 0), field(l, 3).unwrap_or_default()))
    });
    match (user, group) {
        (Some(user), Some((Some(name), members))) => {
            name == user && members.split(',').all(|m| m.is_empty() || m == user)
        }
        _ => false,
    }
}

/// The same rule the shim applies at launch, so setup and the shim cannot disagree
/// about a location: group or others may write, and neither a sticky directory nor
/// a private group stands in the way.
fn others_can_replace(meta: &std::fs::Metadata, uid: u32) -> bool {
    let mode = meta.mode();
    if meta.is_dir() && mode & 0o1000 != 0 {
        return false;
    }
    mode & 0o002 != 0 || (mode & 0o020 != 0 && !group_is_private(meta.gid(), uid))
}

/// Everything about where `appimage` is kept that setup should put in front of the
/// user before the shims are told to trust it.
pub fn inspect(appimage: &Path) -> Vec<Issue> {
    let mut issues = Vec::new();
    let Some(uid) = my_uid() else {
        return issues;
    };
    let Ok(meta) = std::fs::metadata(appimage) else {
        return issues;
    };
    if meta.uid() != uid {
        issues.push(Issue::NotOwned);
    }
    if others_can_replace(&meta, uid) {
        issues.push(Issue::WritableByOthers);
    }
    let mut dir = appimage.parent();
    while let Some(d) = dir {
        if let Ok(m) = std::fs::metadata(d) {
            if others_can_replace(&m, uid) {
                issues.push(Issue::DirWritableByOthers(d.to_path_buf()));
                break;
            }
        }
        dir = d.parent();
    }
    if let Some(what) = foreign_media(appimage) {
        issues.push(Issue::ForeignMedia(what));
    }
    issues
}

/// Network filesystems by type, removable disks by what sysfs says about the device.
/// Mount points are not used: an internal disk under /media is ordinary.
fn foreign_media(path: &Path) -> Option<String> {
    let path = path.canonicalize().ok()?;
    let info = std::fs::read_to_string("/proc/self/mountinfo").ok()?;
    // The longest mount point that is a prefix of the path is the one it lives on.
    let (_, dev, fstype) = info
        .lines()
        .filter_map(|l| {
            let (left, right) = l.split_once(" - ")?;
            let mut f = left.split(' ');
            let dev = f.nth(2)?.to_string();
            let point = f.nth(1)?.replace("\\040", " ");
            let fstype = right.split(' ').next()?.to_string();
            path.starts_with(&point).then_some((point, dev, fstype))
        })
        .max_by_key(|(point, _, _)| point.len())?;
    const NETWORK: [&str; 8] = [
        "nfs",
        "nfs4",
        "cifs",
        "smb3",
        "fuse.sshfs",
        "9p",
        "ceph",
        "glusterfs",
    ];
    if NETWORK.contains(&fstype.as_str()) {
        return Some(format!("a network filesystem ({fstype})"));
    }
    let sys = std::fs::canonicalize(format!("/sys/dev/block/{dev}")).ok()?;
    let on_usb = sys.components().any(|c| {
        c.as_os_str()
            .to_str()
            .is_some_and(|s| s.starts_with("usb") && s[3..].chars().all(|c| c.is_ascii_digit()))
    });
    // A partition's `removable` flag lives on its parent disk.
    let removable = [sys.join("removable"), sys.join("../removable")]
        .iter()
        .any(|p| std::fs::read_to_string(p).is_ok_and(|v| v.trim() == "1"));
    (on_usb || removable).then(|| "a removable disk".to_string())
}

/// The shim for one tool. The path goes into a single-quoted shell string, so the
/// only character that needs care is the single quote itself.
pub fn shim_script(tool: &str, appimage: &Path, strict: bool) -> String {
    let quoted = appimage.display().to_string().replace('\'', "'\\''");
    assets::appimage_shim::TEMPLATE
        .replace("@APPIMAGE@", &quoted)
        .replace("@TOOL@", tool)
        .replace("@STRICT@", if strict { "1" } else { "0" })
}

/// What sits at a tool's place in `~/.local/bin`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Shim {
    Absent,
    /// One of ours, and what it was told.
    Points {
        appimage: PathBuf,
        strict: bool,
    },
    /// Something else: a copied binary from the tarball install, or a stranger.
    Other,
}

/// Read back what an installed shim records. Parses the two assignment lines the
/// template writes rather than executing anything.
pub fn read_shim(path: &Path) -> Shim {
    let Ok(bytes) = std::fs::read(path) else {
        return Shim::Absent;
    };
    let Ok(text) = String::from_utf8(bytes) else {
        return Shim::Other;
    };
    if text
        .lines()
        .nth(1)
        .map(|l| l.starts_with(assets::appimage_shim::MARKER))
        != Some(true)
    {
        return Shim::Other;
    }
    let value = |key: &str| text.lines().find_map(|l| l.strip_prefix(key));
    let appimage = value("appimage='")
        .and_then(|v| v.strip_suffix('\''))
        .map(|v| PathBuf::from(v.replace("'\\''", "'")));
    match (appimage, value("strict=")) {
        (Some(appimage), Some(strict)) => Shim::Points {
            appimage,
            strict: strict.trim() != "0",
        },
        _ => Shim::Other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("beisl-appimage-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// A mounted image as the runtime presents it: our component under usr/share,
    /// a binary under usr/bin, and the file the user double-clicked.
    fn fake_image(root: &Path, id: &str) -> (PathBuf, PathBuf, PathBuf) {
        let appdir = root.join("mount");
        std::fs::create_dir_all(appdir.join("usr/bin")).unwrap();
        std::fs::create_dir_all(appdir.join("usr/share/metainfo")).unwrap();
        std::fs::write(
            appdir.join(format!("usr/share/metainfo/{id}.metainfo.xml")),
            format!("<component><id>{id}</id></component>"),
        )
        .unwrap();
        let exe = appdir.join("usr/bin/beisl-setup");
        std::fs::write(&exe, b"x").unwrap();
        let appimage = root.join("Beisl.AppImage");
        std::fs::write(&appimage, b"x").unwrap();
        (exe, appdir, appimage)
    }

    #[test]
    fn an_inherited_appimage_variable_is_not_believed() {
        let root = scratch("inherit");
        let (exe, appdir, appimage) = fake_image(&root, assets::metainfo::ID);
        assert!(validate(&exe, &appdir, &appimage).is_some());

        // Started from another AppImage's terminal: the variables describe that
        // program, and our executable is nowhere inside its mount.
        let native = root.join("native-beisl-setup");
        std::fs::write(&native, b"x").unwrap();
        assert_eq!(validate(&native, &appdir, &appimage), None);

        // Inside a mount, but it is somebody else's image.
        let other = scratch("inherit-other");
        let (exe2, appdir2, appimage2) = fake_image(&other, "org.example.Editor");
        assert_eq!(validate(&exe2, &appdir2, &appimage2), None);

        // The file the variable names has to exist; a relative path is no path.
        assert_eq!(validate(&exe, &appdir, &root.join("gone.AppImage")), None);
        assert_eq!(validate(&exe, &appdir, Path::new("Beisl.AppImage")), None);
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&other);
    }

    #[test]
    fn a_shim_round_trips_what_it_was_told() {
        let root = scratch("roundtrip");
        // A path that needs every bit of the quoting.
        let appimage = root.join("it's here/Beisl 0.3.AppImage");
        for (tool, strict) in [("beisl", true), ("beisl-setup", false)] {
            let script = shim_script(tool, &appimage, strict);
            for hole in ["@APPIMAGE@", "@TOOL@", "@STRICT@"] {
                assert!(!script.contains(hole), "{hole} left unfilled for {tool}");
            }
            assert!(script.contains(&format!("tool='{tool}'")));
            let path = root.join(tool);
            std::fs::write(&path, &script).unwrap();
            assert_eq!(
                read_shim(&path),
                Shim::Points {
                    appimage: appimage.clone(),
                    strict
                }
            );
        }
        assert_eq!(read_shim(&root.join("nothing-here")), Shim::Absent);
        // A copied binary, as the tarball install leaves, is not ours to trust.
        let binary = root.join("real");
        std::fs::write(&binary, [0x7f, b'E', b'L', b'F', 0xff, 0xfe]).unwrap();
        assert_eq!(read_shim(&binary), Shim::Other);
        let script = root.join("stranger");
        std::fs::write(&script, "#!/bin/sh\nexec something-else\n").unwrap();
        assert_eq!(read_shim(&script), Shim::Other);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_location_others_can_write_is_named() {
        use std::os::unix::fs::PermissionsExt;
        let root = scratch("inspect");
        let set = |p: &Path, mode| {
            std::fs::set_permissions(p, std::fs::Permissions::from_mode(mode)).unwrap()
        };
        set(&root, 0o755);
        let file = root.join("Beisl.AppImage");
        std::fs::write(&file, b"x").unwrap();
        set(&file, 0o755);
        let hard: Vec<Issue> = inspect(&file)
            .into_iter()
            .filter(Issue::blocks_strict_shim)
            .collect();
        // The scratch dir sits under a sticky /tmp: nobody else can replace it.
        assert_eq!(hard, Vec::<Issue>::new());

        set(&file, 0o757);
        assert!(inspect(&file).contains(&Issue::WritableByOthers));
        set(&file, 0o755);
        set(&root, 0o777);
        assert!(inspect(&file).contains(&Issue::DirWritableByOthers(root.clone())));
        // Sticky is what makes a shared directory safe for a file you own.
        set(&root, 0o1777);
        assert!(!inspect(&file)
            .iter()
            .any(|i| matches!(i, Issue::DirWritableByOthers(_))));
        set(&root, 0o755);
        let _ = std::fs::remove_dir_all(&root);
    }
}
