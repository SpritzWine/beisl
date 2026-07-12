//! The trace library page: every past run under the traces root, one click from its
//! own session window.
//!
//! Self-contained Elm-style component — state, messages, update, view — so the setup
//! app's `main.rs` stays the checklist it started as and future pages (the v2 A/B
//! compare view, say) have a pattern to follow instead of growing that file further.
//! The parent maps [`Msg`] into its own message type and handles only [`Msg::Back`]
//! (navigation is the parent's business; everything else is ours).

use cosmic::iced::Length;
use cosmic::{widget, Element};
use session::library::{self, RunEntry};

#[derive(Debug, Clone)]
pub enum Msg {
    /// Navigation back to the checklist — handled by the parent, not here.
    Back,
    Refresh,
    /// Open one row's session window; indexes the current scan.
    Open(usize),
    /// v1 "export": reveal the run dir in the file manager — every artifact
    /// (artifact.json, report.txt, perf.data, logs) is right there to copy.
    OpenFolder(usize),
    /// Arm the per-row delete confirm (the destructive click is always the second one).
    Delete(usize),
    CancelDelete,
    ConfirmDelete,
}

/// The library's own state: the scanned rows plus the two bits of view state that
/// belong to this page alone.
#[derive(Default)]
pub struct TracesTab {
    runs: Vec<RunEntry>,
    /// Page-local error (a failed window spawn / refused delete) — deliberately not a
    /// shared field, so nothing else's stale error ever haunts the library view.
    error: Option<String>,
    /// Row index with an armed delete confirm, if any.
    confirm_delete: Option<usize>,
}

impl TracesTab {
    /// Rescan the traces dir (cheap — a run finished since the last look should simply
    /// be there) and reset transient view state.
    pub fn refresh(&mut self) {
        self.runs = library::scan(&library::default_root());
        self.error = None;
        self.confirm_delete = None;
    }

    pub fn update(&mut self, msg: Msg) {
        match msg {
            // The parent's: listed here so the match stays exhaustive when Msg grows.
            Msg::Back => {}
            Msg::Refresh => self.refresh(),
            Msg::Open(i) => {
                if let Some(run) = self.runs.get(i) {
                    // The session window is this same binary — hand the run dir to a new
                    // instance rather than growing a second results renderer in here.
                    let spawned = std::env::current_exe().and_then(|exe| {
                        std::process::Command::new(exe)
                            .arg("--session")
                            .arg(&run.dir)
                            .spawn()
                    });
                    if let Err(e) = spawned {
                        self.error = Some(format!("couldn't open the trace window: {e}"));
                    }
                }
            }
            Msg::OpenFolder(i) => {
                if let Some(run) = self.runs.get(i) {
                    // Quiet the child: KDE's kioclient chats on stderr (env-var
                    // forwarding notes) and reads like an error in a terminal.
                    let _ = std::process::Command::new("xdg-open")
                        .arg(&run.dir)
                        .stderr(std::process::Stdio::null())
                        .stdout(std::process::Stdio::null())
                        .spawn();
                }
            }
            Msg::Delete(i) => self.confirm_delete = Some(i),
            Msg::CancelDelete => self.confirm_delete = None,
            Msg::ConfirmDelete => {
                if let Some(run) = self.confirm_delete.take().and_then(|i| self.runs.get(i)) {
                    // delete_run is the safety boundary: it refuses anything that isn't
                    // a run-* dir directly under the traces root.
                    if let Err(e) = library::delete_run(&library::default_root(), &run.dir) {
                        self.error = Some(format!("couldn't delete the trace: {e}"));
                    }
                    self.runs = library::scan(&library::default_root());
                }
            }
        }
    }

    /// The page: every past run, newest first. Rows open the run's own session window;
    /// analysis state comes from the scan (artifact.json present = the results view
    /// shows immediately).
    pub fn view(&self) -> Element<'_, Msg> {
        let mut content = widget::Column::new()
            .spacing(14)
            .push(widget::text::title1("Traces"))
            .push(widget::text::caption(format!(
                "Past runs in {}",
                library::default_root().display()
            )));

        if self.runs.is_empty() {
            content = content.push(widget::text::body(
                "No traces yet. Run a game with `beisl %command%` in its launch options \
                 and it will show up here.",
            ));
        }
        for (i, run) in self.runs.iter().enumerate() {
            let when = if run.started > 0 {
                fmt_unix_ts(run.started)
            } else {
                run.id.clone()
            };
            let state = match &run.session {
                _ if run.has_artifact => "analyzed",
                Some(s) if s.elapsed_secs > 0 => "not analyzed",
                Some(_) => "incomplete",
                None => "no session data",
            };
            let mut caption = format!("{when}   \u{b7}   {state}");
            if let Some(sess) = &run.session {
                if sess.elapsed_secs > 0 {
                    caption = format!(
                        "{when}   \u{b7}   {}   \u{b7}   {state}",
                        fmt_duration(sess.elapsed_secs)
                    );
                }
            }
            let mut row = widget::Row::new()
                .spacing(12)
                .align_y(cosmic::iced::Alignment::Center)
                .push(
                    widget::Column::new()
                        .spacing(2)
                        .push(widget::text::body(run.title().to_string()))
                        .push(widget::text::caption(caption))
                        .width(Length::Fill),
                );
            if self.confirm_delete == Some(i) {
                // The confirm pair replaces this row's actions, so the destructive
                // click is a deliberate second one — same idiom as the session
                // window's Discard.
                row = row
                    .push(widget::button::standard("Cancel").on_press(Msg::CancelDelete))
                    .push(widget::button::destructive("Delete trace").on_press(Msg::ConfirmDelete));
            } else {
                row = row
                    .push(widget::button::text("Folder").on_press(Msg::OpenFolder(i)))
                    .push(widget::button::text("Delete").on_press(Msg::Delete(i)))
                    .push(widget::button::standard("Open").on_press(Msg::Open(i)));
            }
            content = content.push(row);
        }

        if let Some(e) = &self.error {
            content = content.push(widget::text::body(format!("Something went wrong: {e}")));
        }

        let footer = widget::Row::new()
            .spacing(8)
            .push(widget::button::text("Back").on_press(Msg::Back))
            .push(widget::Space::new().width(Length::Fill))
            .push(widget::button::standard("Refresh").on_press(Msg::Refresh));

        widget::Column::new()
            .spacing(14)
            .push(
                widget::scrollable(content)
                    .width(Length::Fill)
                    .height(Length::Fill),
            )
            .push(footer)
            .into()
    }
}

/// `run-<unix_ts>` timestamps as a readable date. UTC (std has no timezone database);
/// close enough to place "which evening was that" and honest about its offset.
fn fmt_unix_ts(ts: u64) -> String {
    let days = ts / 86_400;
    let (h, min) = ((ts % 86_400) / 3_600, (ts % 3_600) / 60);
    // Civil-from-days (Howard Hinnant's algorithm), valid for the unix era.
    let z = days as i64 + 719_468;
    let era = z / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02} {h:02}:{min:02} UTC")
}

/// Session length for a library row: "47m", "1h02m", "38s".
fn fmt_duration(secs: u64) -> String {
    match (secs / 3600, (secs % 3600) / 60) {
        (0, 0) => format!("{secs}s"),
        (0, m) => format!("{m}m"),
        (h, m) => format!("{h}h{m:02}m"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unix_ts_formats_civil_utc() {
        assert_eq!(fmt_unix_ts(0), "1970-01-01 00:00 UTC");
        // CLAUDE.md's own case-study trace, run-1783746830.
        assert_eq!(fmt_unix_ts(1_783_746_830), "2026-07-11 05:13 UTC");
        // Leap-year day.
        assert_eq!(fmt_unix_ts(1_709_164_800), "2024-02-29 00:00 UTC");
    }

    #[test]
    fn durations_read_like_a_library_row() {
        assert_eq!(fmt_duration(38), "38s");
        assert_eq!(fmt_duration(47 * 60 + 12), "47m");
        assert_eq!(fmt_duration(3_720), "1h02m");
    }
}
