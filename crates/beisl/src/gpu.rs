//! GPU-hang watcher — the capture discipline from the Saints Row case study
//! (DESIGN.md): the xe KMD retains exactly ONE devcoredump; while it sits unread,
//! new hangs record nothing. So during a session we watch the devcoredump nodes,
//! auto-capture a pending dump into the trace dir, then free the node.
//!
//! Two kernel drivers, two shapes:
//! * xe (and other devcoredump users): `/sys/class/drm/cardN/device/devcoredump/`
//!   APPEARS when a dump is pending; reading `data` yields the dump, writing to it
//!   frees the slot.
//! * i915: no devcoredump — a persistent `/sys/class/drm/cardN/error` node that
//!   reads "No error state collected" when healthy. There is no existence-signal,
//!   so we can only try-read it (best-effort; it is root-only on most systems).
//!
//! Both nodes are usually root-owned, so as a plain user the watcher may only be
//! able to *detect* a pending dump. In that case it emits [`Event::Unreadable`]
//! with the exact command to save it — still valuable, because the alternative is
//! the user learning days later that dump #2 was never recorded.

use std::collections::HashSet;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

/// What the watcher found on a poll.
#[derive(Debug)]
pub enum Event {
    /// A pending dump was captured into the trace dir and the node was freed.
    Captured { card: String, dest: PathBuf },
    /// A pending dump exists but is not readable by this user.
    Unreadable { card: String, node: PathBuf },
}

pub struct GpuWatch {
    base: PathBuf,
    run_dir: PathBuf,
    /// Nodes we've captured + freed — done, never revisit.
    captured: HashSet<PathBuf>,
    /// Nodes we've already emitted an [`Event::Unreadable`] for — warn once, but KEEP
    /// re-attempting the read. So if the user enables auto-capture mid-session (the udev
    /// rule + a reload land while the game runs, and the xe dump survives until read), the
    /// very next poll succeeds and captures the still-pending dump instead of losing it.
    warned: HashSet<PathBuf>,
    captures: u32,
}

impl GpuWatch {
    pub fn new(run_dir: &Path) -> Self {
        Self::with_base(Path::new("/sys/class/drm"), run_dir)
    }

    /// Test seam: point the watcher at a fake sysfs tree.
    pub fn with_base(base: &Path, run_dir: &Path) -> Self {
        GpuWatch {
            base: base.to_path_buf(),
            run_dir: run_dir.to_path_buf(),
            captured: HashSet::new(),
            warned: HashSet::new(),
            captures: 0,
        }
    }

    /// Did a GPU hang show up during this run — whether we captured its dump or only saw
    /// an unreadable one? Either way a devcoredump appeared, so it's the "why did it crash"
    /// case that always warrants analysis regardless of the game's (launcher-laundered)
    /// exit code.
    pub fn hang_seen(&self) -> bool {
        !self.captured.is_empty() || !self.warned.is_empty()
    }

    /// Scan for pending devcoredumps; capture-and-free what we can.
    pub fn poll(&mut self) -> Vec<Event> {
        let mut events = Vec::new();
        for card in self.cards() {
            let node = self.base.join(&card).join("device/devcoredump/data");
            if !node.exists() || self.captured.contains(&node) {
                continue;
            }
            match std::fs::read(&node) {
                Ok(data) => {
                    self.captures += 1;
                    let dest = self
                        .run_dir
                        .join(format!("gpu-devcoredump-{card}-{}.txt", self.captures));
                    if std::fs::write(&dest, &data).is_ok() {
                        // Free the slot so the NEXT hang can record (xe keeps one).
                        let _ = std::fs::write(&node, "1");
                        self.captured.insert(node);
                        events.push(Event::Captured { card, dest });
                    }
                }
                Err(e) if e.kind() == ErrorKind::PermissionDenied => {
                    // Warn once, but do NOT give up: keep re-reading each poll so enabling
                    // auto-capture mid-session rescues this still-pending dump.
                    if self.warned.insert(node.clone()) {
                        events.push(Event::Unreadable { card, node });
                    }
                }
                Err(_) => {} /* transient (node vanished mid-read); retry next poll */
            }
        }
        events
    }

    /// DRM card directories proper (`card0`, `card1`, …) — connectors like
    /// `card0-DP-5` also live in this directory and must be skipped.
    fn cards(&self) -> Vec<String> {
        let mut cards = Vec::new();
        if let Ok(entries) = std::fs::read_dir(&self.base) {
            for e in entries.flatten() {
                let name = e.file_name().to_string_lossy().into_owned();
                if name
                    .strip_prefix("card")
                    .is_some_and(|r| !r.is_empty() && r.bytes().all(|b| b.is_ascii_digit()))
                {
                    cards.push(name);
                }
            }
        }
        cards.sort();
        cards
    }
}

/// The one-liner we hand the user when a dump is pending but root-only.
pub fn sudo_capture_hint(node: &Path, run_dir: &Path, card: &str) -> String {
    format!(
        "sudo sh -c 'cat {node} > {dir}/gpu-devcoredump-{card}.txt && echo 1 > {node}'",
        node = node.display(),
        dir = run_dir.display(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_sysfs(with_dump: &[&str], connectors: &[&str]) -> (tempdir::TempDir, PathBuf) {
        let tmp = tempdir::TempDir::new();
        let base = tmp.path().join("drm");
        for card in with_dump {
            let d = base.join(card).join("device/devcoredump");
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(d.join("data"), b"DUMP-DATA").unwrap();
        }
        for c in connectors {
            std::fs::create_dir_all(base.join(c)).unwrap();
        }
        (tmp, base)
    }

    /// Minimal tempdir (std-only, no dev-deps).
    mod tempdir {
        use std::path::{Path, PathBuf};
        pub struct TempDir(PathBuf);
        impl TempDir {
            pub fn new() -> Self {
                let d = std::env::temp_dir().join(format!(
                    "beisl-gpu-test-{}-{:?}",
                    std::process::id(),
                    std::thread::current().id()
                ));
                std::fs::create_dir_all(&d).unwrap();
                TempDir(d)
            }
            pub fn path(&self) -> &Path {
                &self.0
            }
        }
        impl Drop for TempDir {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
    }

    #[test]
    fn captures_and_frees_a_pending_dump() {
        let (tmp, base) = fake_sysfs(&["card1"], &[]);
        let run = tmp.path().join("run");
        std::fs::create_dir_all(&run).unwrap();

        let mut w = GpuWatch::with_base(&base, &run);
        let events = w.poll();
        assert_eq!(events.len(), 1);
        match &events[0] {
            Event::Captured { card, dest } => {
                assert_eq!(card, "card1");
                assert_eq!(std::fs::read(dest).unwrap(), b"DUMP-DATA");
            }
            other => panic!("expected Captured, got {other:?}"),
        }
        // Node was "freed" (our write lands in the fake file).
        let node = base.join("card1/device/devcoredump/data");
        assert_eq!(std::fs::read(&node).unwrap(), b"1");
        // One event per node per session.
        assert!(w.poll().is_empty());
    }

    #[test]
    fn ignores_connector_entries_and_dumpless_cards() {
        let (tmp, base) = fake_sysfs(&[], &["card0-DP-5", "card0-HDMI-A-1"]);
        std::fs::create_dir_all(base.join("card0/device")).unwrap(); // healthy card, no dump
        let run = tmp.path().join("run");
        std::fs::create_dir_all(&run).unwrap();

        let mut w = GpuWatch::with_base(&base, &run);
        assert!(w.poll().is_empty());
        assert_eq!(w.cards(), vec!["card0".to_string()]);
    }

    #[test]
    fn unreadable_dump_is_retried_and_captured_after_access_is_granted() {
        // Root bypasses file perms, so this can only assert the retry on a non-root run.
        if unsafe { libc::geteuid() } == 0 {
            return;
        }
        let (tmp, base) = fake_sysfs(&["card1"], &[]);
        let node = base.join("card1/device/devcoredump/data");
        // Make the pending dump unreadable (simulates the default root-only node).
        chmod(&node, 0o000);
        let run = tmp.path().join("run");
        std::fs::create_dir_all(&run).unwrap();

        let mut w = GpuWatch::with_base(&base, &run);
        // First poll: one Unreadable warning…
        let ev = w.poll();
        assert!(matches!(ev.as_slice(), [Event::Unreadable { .. }]));
        // …and it isn't spammed on the next poll while still unreadable.
        assert!(w.poll().is_empty());

        // User enables auto-capture mid-session → node becomes readable → next poll captures.
        chmod(&node, 0o644);
        let ev = w.poll();
        assert!(matches!(ev.as_slice(), [Event::Captured { .. }]));
    }

    fn chmod(path: &Path, mode: u32) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
    }

    #[test]
    fn hint_names_node_and_destination() {
        let hint = sudo_capture_hint(Path::new("/sys/x/data"), Path::new("/tmp/run"), "card1");
        assert!(hint.contains("/sys/x/data"));
        assert!(hint.contains("/tmp/run/gpu-devcoredump-card1.txt"));
        assert!(hint.contains("echo 1 >"));
    }
}
