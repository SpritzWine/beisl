//! Live compat-wall watch for the session window.
//!
//! The wall detector proper is post-hoc: beisl-mcp reads a trace after the run. But a
//! person looking at the session window while the game's own error dialog is up deserves
//! better than "No problems detected." So the wrapper tails the same logs the scan reads
//! and pushes one warning per signature into `session.json` as it happens.
//!
//! Two logs, because Proton moves the evidence: with `PROTON_LOG` on, Wine's stderr goes
//! to `~/steam-<appid>.log` instead of the stderr beisl tees into game.log. beisl never
//! sets that variable (a tracer must not change the launch it traces); it reads the file a
//! user asked Proton for, and preserves it beside the trace at exit, since Proton deletes
//! it at the next launch.

use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// Bound on bytes swallowed per poll, so a log written at full tilt can't stall the
/// wrapper's tick; the rest is read next time round.
const MAX_BYTES_PER_POLL: u64 = 1 << 20;
const MAX_LINE_LEN: usize = 300;

/// Follows one log file line by line, tolerating the file not existing yet, being
/// recreated (Proton removes and reopens its log at every launch), or being truncated.
pub struct LogTail {
    path: PathBuf,
    offset: u64,
    carry: String,
    /// A file last written before this is a previous launch's leftover, not ours.
    not_before: SystemTime,
}

impl LogTail {
    pub fn new(path: PathBuf, not_before: SystemTime) -> Self {
        Self {
            path,
            offset: 0,
            carry: String::new(),
            not_before,
        }
    }

    /// Complete new lines since the last call. Empty on any error: a log we can't read is
    /// a log with nothing to say.
    pub fn drain(&mut self) -> Vec<String> {
        let Ok(meta) = std::fs::metadata(&self.path) else {
            return Vec::new();
        };
        if meta.modified().is_ok_and(|m| m < self.not_before) {
            return Vec::new();
        }
        if meta.len() < self.offset {
            self.offset = 0;
            self.carry.clear();
        }
        let Ok(mut file) = std::fs::File::open(&self.path) else {
            return Vec::new();
        };
        if file.seek(SeekFrom::Start(self.offset)).is_err() {
            return Vec::new();
        }
        let mut buf = Vec::new();
        let Ok(read) = file.by_ref().take(MAX_BYTES_PER_POLL).read_to_end(&mut buf) else {
            return Vec::new();
        };
        self.offset += read as u64;
        self.carry.push_str(&String::from_utf8_lossy(&buf));
        let mut lines = Vec::new();
        while let Some(nl) = self.carry.find('\n') {
            let line: String = self.carry.drain(..=nl).collect();
            lines.push(line.trim_end_matches(['\n', '\r']).to_string());
        }
        lines
    }
}

/// Where Proton writes its log for this launch, if the USER turned logging on. Mirrors
/// Proton's own `setup_logging`: `nonzero(PROTON_LOG)`, `PROTON_LOG_DIR` else `$HOME`,
/// `steam-<SteamGameId>.log`.
pub fn proton_log_path(env: impl Fn(&str) -> Option<String>) -> Option<PathBuf> {
    let flag = env("PROTON_LOG")?;
    if flag.is_empty() || flag == "0" {
        return None;
    }
    let id = env("SteamGameId")?;
    let dir = env("PROTON_LOG_DIR")
        .filter(|d| !d.is_empty())
        .or_else(|| env("HOME"))?;
    Some(PathBuf::from(dir).join(format!("steam-{id}.log")))
}

/// Tails every log a wall could show up in and reports each signature once.
pub struct WallWatch {
    tails: Vec<LogTail>,
    seen: Vec<&'static str>,
}

impl WallWatch {
    pub fn new(paths: impl IntoIterator<Item = PathBuf>, not_before: SystemTime) -> Self {
        Self {
            tails: paths
                .into_iter()
                .map(|p| LogTail::new(p, not_before))
                .collect(),
            seen: Vec::new(),
        }
    }

    /// New walls since the last poll: the signature and the line that showed it.
    pub fn poll(&mut self) -> Vec<(&'static str, String)> {
        let mut hits = Vec::new();
        for tail in &mut self.tails {
            for line in tail.drain() {
                let lower = line.to_lowercase();
                let Some(sig) = compat::wall_signature(&lower, compat::wine_builtin_drivers())
                else {
                    continue;
                };
                if !self.seen.contains(&sig) {
                    self.seen.push(sig);
                    hits.push((sig, clip(line.trim())));
                }
            }
        }
        hits
    }
}

/// Keep Proton's log beside the trace: Proton deletes it at the next launch, and it is the
/// only place the wall's evidence exists when `PROTON_LOG` is on. `None` when Proton wrote
/// no log for this run - an old file from a previous launch is not ours to keep.
pub fn preserve_proton_log(src: &Path, run_dir: &Path, not_before: SystemTime) -> Option<u64> {
    let meta = std::fs::metadata(src).ok()?;
    if meta.modified().ok()? < not_before {
        return None;
    }
    std::fs::copy(src, run_dir.join("proton.log")).ok()
}

fn clip(line: &str) -> String {
    if line.chars().count() <= MAX_LINE_LEN {
        line.to_string()
    } else {
        let mut s: String = line.chars().take(MAX_LINE_LEN).collect();
        s.push_str("...");
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::time::Duration;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("beisl-walls-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_tail_yields_only_complete_lines_and_survives_truncation() {
        let dir = scratch("tail");
        let path = dir.join("game.log");
        let mut tail = LogTail::new(path.clone(), SystemTime::UNIX_EPOCH);
        assert!(tail.drain().is_empty(), "no file yet is not an error");
        std::fs::write(&path, "one\ntwo\nthr").unwrap();
        assert_eq!(tail.drain(), ["one", "two"]);
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        f.write_all(b"ee\r\n").unwrap();
        drop(f);
        assert_eq!(tail.drain(), ["three"]);
        // Proton removes and recreates its log: a shorter file starts over.
        std::fs::write(&path, "fresh\n").unwrap();
        assert_eq!(tail.drain(), ["fresh"]);
    }

    #[test]
    fn a_leftover_from_a_previous_launch_is_ignored() {
        let dir = scratch("stale");
        let path = dir.join("steam-1.log");
        std::fs::write(&path, "old wall: unimplemented function x\n").unwrap();
        let later = SystemTime::now() + Duration::from_secs(3600);
        let mut tail = LogTail::new(path.clone(), later);
        assert!(tail.drain().is_empty());
        assert!(preserve_proton_log(&path, &dir, later).is_none());
        assert!(preserve_proton_log(&path, &dir, SystemTime::UNIX_EPOCH).is_some());
        assert!(dir.join("proton.log").is_file());
    }

    #[test]
    fn proton_log_path_follows_protons_own_rules() {
        let env = |vars: &'static [(&'static str, &'static str)]| {
            move |k: &str| {
                vars.iter()
                    .find(|(n, _)| *n == k)
                    .map(|(_, v)| v.to_string())
            }
        };
        assert_eq!(
            proton_log_path(env(&[
                ("PROTON_LOG", "1"),
                ("SteamGameId", "4809930"),
                ("HOME", "/home/f")
            ])),
            Some(PathBuf::from("/home/f/steam-4809930.log"))
        );
        assert_eq!(
            proton_log_path(env(&[
                ("PROTON_LOG", "+ntoskrnl"),
                ("SteamGameId", "4809930"),
                ("PROTON_LOG_DIR", "/tmp/logs"),
                ("HOME", "/home/f"),
            ])),
            Some(PathBuf::from("/tmp/logs/steam-4809930.log"))
        );
        // Proton's nonzero(): "0" and "" mean off; and no SteamGameId means no per-game log.
        assert_eq!(
            proton_log_path(env(&[
                ("PROTON_LOG", "0"),
                ("SteamGameId", "1"),
                ("HOME", "/h")
            ])),
            None
        );
        assert_eq!(
            proton_log_path(env(&[("PROTON_LOG", "1"), ("HOME", "/h")])),
            None
        );
        assert_eq!(
            proton_log_path(env(&[("SteamGameId", "1"), ("HOME", "/h")])),
            None
        );
    }

    #[test]
    fn a_watch_reports_each_signature_once() {
        let dir = scratch("watch");
        let path = dir.join("proton.log");
        std::fs::write(
            &path,
            "wine: Call from 0x7b00 to unimplemented function ntdll.dll.A, aborting\n\
             wine: Call from 0x7b00 to unimplemented function ntdll.dll.B, aborting\n",
        )
        .unwrap();
        let mut watch = WallWatch::new([path], SystemTime::UNIX_EPOCH);
        let hits = watch.poll();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].0, "wine-stub");
        assert!(watch.poll().is_empty());
    }
}
