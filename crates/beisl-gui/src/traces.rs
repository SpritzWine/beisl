//! The trace library page: every past run under the traces root, one click from its
//! own session window.
//!
//! Self-contained Elm-style component — state, messages, update, view — so the setup
//! app's `main.rs` stays the checklist it started as and future pages (the v2 A/B
//! compare view, say) have a pattern to follow instead of growing that file further.
//! The parent maps [`Msg`] into its own message type and handles only [`Msg::Back`]
//! (navigation is the parent's business; everything else is ours).

use std::collections::HashSet;

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
    /// Flip one row's checkbox (refused for a live run — see `RunEntry::is_live`).
    ToggleSelect(usize),
    /// Selection helpers: ADD every matching non-live row to the selection. They never
    /// remove — re-clicking one after hand-deselecting a row should not fight the user.
    SelectAll,
    SelectUnanalyzed,
    SelectAnalyzed,
    ClearSelection,
    /// Arm the bulk-delete confirm (the file-manager pattern's deliberate second click).
    DeleteSelected,
    CancelBulk,
    ConfirmBulk,
    /// A row's info column was clicked; toggles selection only while Ctrl is held (a
    /// plain click stays inert — `Open` is its own button, not the row itself).
    RowPressed(usize),
    /// From the parent's modifier-tracking subscription: is Ctrl currently held.
    Modifiers(bool),
}

/// Which rows a selection helper adds. Live rows never match any rule — the recorder
/// or the game itself might still own the directory (the bug bulk delete must not
/// reintroduce; see `RunEntry::is_live`).
#[derive(Clone, Copy)]
enum SelectRule {
    All,
    Unanalyzed,
    Analyzed,
}

impl SelectRule {
    fn matches(self, run: &RunEntry) -> bool {
        if run.is_live() {
            return false;
        }
        match self {
            SelectRule::All => true,
            SelectRule::Unanalyzed => !run.has_artifact,
            SelectRule::Analyzed => run.has_artifact,
        }
    }
}

/// A selection helper button: add every non-live row matching `rule` to `selected` —
/// pure and widget-free so it's directly testable (see the tests below).
fn select_by_rule(runs: &[RunEntry], rule: SelectRule, selected: &mut HashSet<String>) {
    selected.extend(
        runs.iter()
            .filter(|r| rule.matches(r))
            .map(|r| r.id.clone()),
    );
}

/// One row's checkbox/Ctrl-click: flip its membership in `selected`, refusing a live
/// row outright (checked or not, it must never end up selected).
fn toggle_select(runs: &[RunEntry], i: usize, selected: &mut HashSet<String>) {
    let Some(run) = runs.get(i) else { return };
    if run.is_live() {
        return;
    }
    if !selected.remove(&run.id) {
        selected.insert(run.id.clone());
    }
}

/// Resolve selected ids to their run dirs against `entries`, skipping anything live —
/// used by `ConfirmBulk` against a FRESH rescan (not the page's cached rows), so a run
/// that went live between being selected and the confirm click is never deleted. This
/// closes the startup race: the wrapper creates the run dir before it writes
/// `session.json` (`crates/beisl/src/run.rs`), so a scan can briefly read a brand-new
/// run as `session: None` -> not-live, long enough to select it in that window.
fn resolve_deletable(entries: &[RunEntry], ids: &HashSet<String>) -> Vec<std::path::PathBuf> {
    entries
        .iter()
        .filter(|r| ids.contains(&r.id) && !r.is_live())
        .map(|r| r.dir.clone())
        .collect()
}

/// The row caption's state word. A stale run (session claims a live state, but it
/// started before the current boot — see `RunEntry::is_stale`: nothing can still own
/// that directory) is checked FIRST, so it reads honestly as never having finished
/// rather than "recording"/"in use" forever. Genuinely live states are named honestly
/// too — Starting/Running/Recording/Stopping, or Saved's own "still running" — so a
/// live row never reads like a finished one (e.g. a Recording run must not say "not
/// analyzed" while its checkbox is inert and its Delete button hidden, with nothing
/// explaining why). Settled runs (`Done`/`Error`) fall back to the analysis-state word;
/// a stale run's checkbox/Delete now behave like a settled one too (`is_live()`
/// excludes stale runs), so a stale run without an artifact is selectable via the
/// Unanalyzed helper below — its perf.data may still be analyzable, so that's desired,
/// even though its caption always reads "stale — never finished", not "not analyzed".
fn state_word(run: &RunEntry) -> &'static str {
    if run.is_stale() {
        return "stale — never finished";
    }
    if run.is_live() {
        return match run.session.as_ref().map(|s| s.state) {
            Some(session::State::Starting) => "starting",
            Some(session::State::Running) => "running",
            Some(session::State::Recording) => "recording",
            Some(session::State::Stopping) => "stopping",
            // Saved's own doc comment: the recorder stopped, but the game is still
            // running — say so, rather than reading like a finished run.
            Some(session::State::Saved) => "in use — game still running",
            // Unreachable: is_live() above already narrowed to exactly these states.
            _ => "recording",
        };
    }
    match &run.session {
        _ if run.has_artifact => "analyzed",
        Some(s) if s.elapsed_secs > 0 => "not analyzed",
        Some(_) => "incomplete",
        None => "no session data",
    }
}

/// The library's own state: the scanned rows plus the view state that belongs to this
/// page alone.
#[derive(Default)]
pub struct TracesTab {
    runs: Vec<RunEntry>,
    /// Page-local error (a failed window spawn / refused delete) — deliberately not a
    /// shared field, so nothing else's stale error ever haunts the library view.
    error: Option<String>,
    /// Row index with an armed delete confirm, if any.
    confirm_delete: Option<usize>,
    /// Run ids checked via the row checkboxes or Ctrl+click — ids (not indexes) so a
    /// selection survives a rescan reordering the list.
    selected: HashSet<String>,
    /// The bulk "Delete selected (N)" confirm is armed (mutually exclusive with
    /// `confirm_delete` — arming one cancels the other).
    confirm_bulk: bool,
    /// Live from the parent's modifier subscription: is Ctrl currently held. Drives
    /// whether a row click (`Msg::RowPressed`) toggles its checkbox.
    ctrl_held: bool,
}

impl TracesTab {
    /// Rescan the traces dir (cheap — a run finished since the last look should simply
    /// be there) and reset transient view state.
    pub fn refresh(&mut self) {
        self.runs = library::scan(&library::default_root());
        self.error = None;
        self.confirm_delete = None;
        self.confirm_bulk = false;
        // Drop selected ids the scan no longer lists, AND any that are now live. The
        // latter matters because of the startup race: the wrapper creates the run dir
        // before it writes session.json (crates/beisl/src/run.rs), so a scan can
        // briefly read a brand-new run as not-live and let it get selected; once
        // session.json lands its checkbox renders unchecked (disabled) here, so a
        // stale id must not keep inflating "Delete selected (N)" in the footer.
        // ConfirmBulk/ConfirmDelete re-check liveness against their own fresh scan
        // regardless — this just keeps what's displayed honest in the meantime.
        let runs = &self.runs;
        self.selected
            .retain(|id| runs.iter().any(|r| &r.id == id && !r.is_live()));
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
            Msg::Delete(i) => {
                // Defense in depth: the view no longer renders a Delete button on a
                // live row, but refuse arming the confirm here too rather than trust
                // that alone.
                if !self.runs.get(i).is_some_and(RunEntry::is_live) {
                    self.confirm_delete = Some(i);
                    // Mutually exclusive with the bulk confirm — arming one cancels
                    // the other.
                    self.confirm_bulk = false;
                }
            }
            Msg::CancelDelete => self.confirm_delete = None,
            Msg::ConfirmDelete => {
                if let Some(id) = self
                    .confirm_delete
                    .take()
                    .and_then(|i| self.runs.get(i))
                    .map(|r| r.id.clone())
                {
                    let root = library::default_root();
                    // Re-resolve against a FRESH scan, not the cached self.runs: the
                    // same startup race ConfirmBulk closes (see `resolve_deletable`)
                    // — the armed confirm can be stale by the time of this second
                    // click. delete_run itself is policy-free (containment only —
                    // see RunEntry::is_live's doc comment: a Saved run's *legitimate*
                    // discard is the session window's own Discard button, not this
                    // list), so the liveness gate has to live here.
                    let fresh = library::scan(&root);
                    if let Some(run) = fresh.iter().find(|r| r.id == id) {
                        if run.is_live() {
                            self.error =
                                Some("that trace just went live — refusing to delete".into());
                        } else if let Err(e) = library::delete_run(&root, &run.dir) {
                            self.error = Some(format!("couldn't delete the trace: {e}"));
                        }
                    }
                    self.runs = library::scan(&root);
                }
            }
            Msg::ToggleSelect(i) => toggle_select(&self.runs, i, &mut self.selected),
            Msg::SelectAll => select_by_rule(&self.runs, SelectRule::All, &mut self.selected),
            Msg::SelectUnanalyzed => {
                select_by_rule(&self.runs, SelectRule::Unanalyzed, &mut self.selected)
            }
            Msg::SelectAnalyzed => {
                select_by_rule(&self.runs, SelectRule::Analyzed, &mut self.selected)
            }
            Msg::ClearSelection => {
                self.selected.clear();
                self.confirm_bulk = false;
            }
            Msg::DeleteSelected => {
                if !self.selected.is_empty() {
                    self.confirm_bulk = true;
                    // Mutually exclusive with the per-row confirm — arming one cancels
                    // the other.
                    self.confirm_delete = None;
                }
            }
            Msg::CancelBulk => self.confirm_bulk = false,
            Msg::ConfirmBulk => {
                self.confirm_bulk = false;
                let root = library::default_root();
                // Resolve against a FRESH scan, not the cached self.runs: the wrapper
                // creates the run dir before it writes session.json
                // (crates/beisl/src/run.rs ~line 18 vs ~123) — a >100ms window where a
                // scan reads session:None -> not-live. Re-checking against stale
                // self.runs would still delete a row that has since started
                // recording; resolve_deletable's is_live skip has to see reality as
                // of right now.
                let fresh = library::scan(&root);
                let dirs = resolve_deletable(&fresh, &self.selected);
                for dir in &dirs {
                    // delete_run is the one containment-checked boundary — every
                    // deletion (single or bulk) routes through it, no new filesystem
                    // path here. Keep going on error so one bad entry doesn't strand
                    // the rest of the batch; remember only the first error.
                    if let Err(e) = library::delete_run(&root, dir) {
                        if self.error.is_none() {
                            self.error = Some(format!("couldn't delete the trace: {e}"));
                        }
                    }
                }
                self.selected.clear();
                self.runs = library::scan(&root);
            }
            Msg::RowPressed(i) => {
                if self.ctrl_held {
                    toggle_select(&self.runs, i, &mut self.selected);
                }
            }
            Msg::Modifiers(ctrl) => self.ctrl_held = ctrl,
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
        } else {
            // Selection helpers: "All" / "Unanalyzed" / "Analyzed" check matching rows
            // and leave the rest for the user to refine by hand — never a replace.
            let mut helpers = widget::Row::new()
                .spacing(8)
                .align_y(cosmic::iced::Alignment::Center)
                .push(widget::text::caption("Select:"))
                .push(widget::button::text("All").on_press(Msg::SelectAll))
                .push(widget::button::text("Unanalyzed").on_press(Msg::SelectUnanalyzed))
                .push(widget::button::text("Analyzed").on_press(Msg::SelectAnalyzed));
            if !self.selected.is_empty() {
                helpers = helpers.push(widget::button::text("Clear").on_press(Msg::ClearSelection));
            }
            content = content.push(helpers);
        }
        for (i, run) in self.runs.iter().enumerate() {
            let when = if run.started > 0 {
                fmt_unix_ts(run.started)
            } else {
                run.id.clone()
            };
            let state = state_word(run);
            let mut caption = format!("{when}   \u{b7}   {state}");
            if let Some(sess) = &run.session {
                if sess.elapsed_secs > 0 {
                    caption = format!(
                        "{when}   \u{b7}   {}   \u{b7}   {state}",
                        fmt_duration(sess.elapsed_secs)
                    );
                }
            }
            let live = run.is_live();
            // Interactive checkbox for a selectable row; a live run gets a display-only
            // one (always unchecked, no on_toggle) — same "read-only = status" idiom as
            // the checklist page's checkboxes.
            let checkbox = if live {
                widget::checkbox(false)
            } else {
                let checked = self.selected.contains(&run.id);
                widget::checkbox(checked).on_toggle(move |_| Msg::ToggleSelect(i))
            };
            // The info column is wrapped in a mouse_area so Ctrl+click toggles selection
            // without turning a plain click into a picker — that stays the row's own
            // Open button.
            let info = widget::mouse_area(
                widget::Column::new()
                    .spacing(2)
                    .push(widget::text::body(run.title().to_string()))
                    .push(widget::text::caption(caption))
                    .width(Length::Fill),
            )
            .on_press(Msg::RowPressed(i));
            let mut row = widget::Row::new()
                .spacing(12)
                .align_y(cosmic::iced::Alignment::Center)
                .push(checkbox)
                .push(info);
            if !live && self.confirm_delete == Some(i) {
                // The confirm pair replaces this row's actions, so the destructive
                // click is a deliberate second one — same idiom as the session
                // window's Discard.
                row = row
                    .push(widget::button::standard("Cancel").on_press(Msg::CancelDelete))
                    .push(widget::button::destructive("Delete trace").on_press(Msg::ConfirmDelete));
            } else {
                row = row.push(widget::button::text("Folder").on_press(Msg::OpenFolder(i)));
                // No Delete button on a live row: a "protected" inert checkbox next to
                // an active Delete button on the same recording row would be
                // incoherent. A Saved run's legitimate discard path stays the session
                // window's own Discard button (reached via Open) — that view is
                // explicit about the game still running; this list isn't, so it just
                // doesn't offer delete here.
                if !live {
                    row = row.push(widget::button::text("Delete").on_press(Msg::Delete(i)));
                }
                row = row.push(widget::button::standard("Open").on_press(Msg::Open(i)));
            }
            content = content.push(row);
        }

        if let Some(e) = &self.error {
            content = content.push(widget::text::body(format!("Something went wrong: {e}")));
        }

        let mut footer = widget::Row::new()
            .spacing(8)
            .push(widget::button::text("Back").on_press(Msg::Back))
            .push(widget::Space::new().width(Length::Fill));
        if self.confirm_bulk {
            // Same deliberate-second-click idiom as the per-row confirm, naming the count.
            footer = footer
                .push(widget::button::standard("Cancel").on_press(Msg::CancelBulk))
                .push(
                    widget::button::destructive(format!("Delete {} traces", self.selected.len()))
                        .on_press(Msg::ConfirmBulk),
                );
        } else if !self.selected.is_empty() {
            footer = footer.push(
                widget::button::destructive(format!("Delete selected ({})", self.selected.len()))
                    .on_press(Msg::DeleteSelected),
            );
        }
        footer = footer.push(widget::button::standard("Refresh").on_press(Msg::Refresh));

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
    use session::Session;
    use std::path::PathBuf;

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

    // -- selection helpers ---------------------------------------------------------------

    /// `is_live`/`is_stale` now consult the real boot time (`session::library::
    /// boot_time`, unreachable from here — it's crate-private to `session`), so a
    /// fixture's `started` has to be picked deliberately instead of an arbitrary `1`:
    /// far enough in the future that it's post-boot on any machine these tests run
    /// on, so a "live" fixture is genuinely live, never accidentally stale.
    const FAR_FUTURE: u64 = 4_102_444_800; // 2100-01-01 UTC

    fn run(id: &str, state: session::State, has_artifact: bool) -> RunEntry {
        RunEntry {
            dir: PathBuf::from(id),
            id: id.to_string(),
            started: FAR_FUTURE,
            session: Some(Session {
                state,
                ..Default::default()
            }),
            has_artifact,
            has_report: false,
        }
    }

    /// A run whose session claims a live state but started at the epoch's first
    /// second — always pre-boot on a real machine (the kernel's `btime` is never
    /// `1`), so this is deterministically stale on any machine these tests run on,
    /// without needing to inject a fake boot time.
    fn stale_run(id: &str, state: session::State, has_artifact: bool) -> RunEntry {
        RunEntry {
            dir: PathBuf::from(id),
            id: id.to_string(),
            started: 1,
            session: Some(Session {
                state,
                ..Default::default()
            }),
            has_artifact,
            has_report: false,
        }
    }

    /// A run with no session.json at all — an old, unanalyzed dir; never live.
    fn run_no_session(id: &str) -> RunEntry {
        RunEntry {
            dir: PathBuf::from(id),
            id: id.to_string(),
            started: 1,
            session: None,
            has_artifact: false,
            has_report: false,
        }
    }

    #[test]
    fn select_all_adds_every_non_live_row_and_skips_live_ones() {
        let runs = [
            run("run-1", session::State::Done, false),
            run("run-2", session::State::Recording, false), // live: must be skipped
            run_no_session("run-3"),
        ];
        let mut selected = HashSet::new();
        select_by_rule(&runs, SelectRule::All, &mut selected);
        assert_eq!(
            selected,
            HashSet::from(["run-1".to_string(), "run-3".to_string()])
        );
    }

    #[test]
    fn select_unanalyzed_vs_analyzed_split_and_dont_overlap() {
        let runs = [
            run("run-1", session::State::Done, false), // finished, not analyzed
            run("run-2", session::State::Done, true),  // finished, analyzed
            run("run-3", session::State::Saved, false), // live (game still running)
        ];

        let mut unanalyzed = HashSet::new();
        select_by_rule(&runs, SelectRule::Unanalyzed, &mut unanalyzed);
        assert_eq!(unanalyzed, HashSet::from(["run-1".to_string()]));

        let mut analyzed = HashSet::new();
        select_by_rule(&runs, SelectRule::Analyzed, &mut analyzed);
        assert_eq!(analyzed, HashSet::from(["run-2".to_string()]));
    }

    #[test]
    fn select_unanalyzed_includes_a_stale_recording_run() {
        // `is_live()` now excludes stale runs (RunEntry::is_stale), so a Recording run
        // stuck since a previous boot passes the Unanalyzed rule same as any settled,
        // un-analyzed run — its perf.data may still be analyzable, so that's desired,
        // not a bug the staleness work should have introduced.
        let runs = [stale_run("run-1", session::State::Recording, false)];
        let mut selected = HashSet::new();
        select_by_rule(&runs, SelectRule::Unanalyzed, &mut selected);
        assert_eq!(selected, HashSet::from(["run-1".to_string()]));
    }

    #[test]
    fn selection_helpers_only_add_never_remove() {
        let runs = [run("run-1", session::State::Done, false)];
        // Pre-seed with an id that will not match this run's rule.
        let mut selected: HashSet<String> = HashSet::from(["run-9".to_string()]);
        select_by_rule(&runs, SelectRule::Analyzed, &mut selected);
        // run-1 doesn't match Analyzed, so it's not added, but run-9 must survive.
        assert_eq!(selected, HashSet::from(["run-9".to_string()]));
    }

    #[test]
    fn toggle_select_flips_membership_but_refuses_live_rows() {
        let runs = [
            run("run-1", session::State::Done, false),
            run("run-2", session::State::Running, false), // live
        ];
        let mut selected = HashSet::new();

        toggle_select(&runs, 0, &mut selected);
        assert!(selected.contains("run-1"));
        toggle_select(&runs, 0, &mut selected);
        assert!(!selected.contains("run-1"));

        // A live row never enters the selection, no matter how it's clicked.
        toggle_select(&runs, 1, &mut selected);
        assert!(selected.is_empty());
    }

    #[test]
    fn resolve_deletable_skips_ids_that_are_live_in_the_fresh_scan() {
        // The startup race this guards: `run-2` was selected while it still read
        // not-live, but the fresh scan passed to resolve_deletable now shows it
        // Recording — it must be excluded even though its id is still `selected`.
        let fresh = [
            run("run-1", session::State::Done, false),
            run("run-2", session::State::Recording, false),
            run("run-3", session::State::Done, true),
        ];
        let selected: HashSet<String> = ["run-1", "run-2", "run-3"]
            .into_iter()
            .map(String::from)
            .collect();
        let dirs = resolve_deletable(&fresh, &selected);
        let mut got: Vec<_> = dirs.iter().map(|p| p.to_str().unwrap()).collect();
        got.sort_unstable();
        assert_eq!(got, ["run-1", "run-3"]);
    }

    #[test]
    fn resolve_deletable_ignores_ids_not_in_the_fresh_scan() {
        // An id that vanished from the fresh scan entirely (deleted elsewhere between
        // selection and confirm) must not produce a dir either.
        let fresh = [run("run-1", session::State::Done, false)];
        let selected: HashSet<String> =
            ["run-1", "run-404"].into_iter().map(String::from).collect();
        let dirs = resolve_deletable(&fresh, &selected);
        assert_eq!(dirs, vec![PathBuf::from("run-1")]);
    }

    #[test]
    fn state_word_names_live_states_honestly() {
        assert_eq!(
            state_word(&run("r", session::State::Starting, false)),
            "starting"
        );
        assert_eq!(
            state_word(&run("r", session::State::Running, false)),
            "running"
        );
        assert_eq!(
            state_word(&run("r", session::State::Recording, false)),
            "recording"
        );
        assert_eq!(
            state_word(&run("r", session::State::Stopping, false)),
            "stopping"
        );
        // Saved: recorder done, game still running — must not read like "finished".
        assert_eq!(
            state_word(&run("r", session::State::Saved, false)),
            "in use — game still running"
        );
    }

    #[test]
    fn state_word_falls_back_to_analysis_state_once_settled() {
        assert_eq!(
            state_word(&run("r", session::State::Done, true)),
            "analyzed"
        );
        // Done, no artifact, elapsed_secs still 0 (the `run` fixture's default).
        assert_eq!(
            state_word(&run("r", session::State::Done, false)),
            "incomplete"
        );
        assert_eq!(state_word(&run_no_session("r")), "no session data");

        let mut not_analyzed = run("r", session::State::Done, false);
        not_analyzed.session.as_mut().unwrap().elapsed_secs = 42;
        assert_eq!(state_word(&not_analyzed), "not analyzed");
    }

    #[test]
    fn state_word_reads_stale_never_finished_before_any_live_wording() {
        // A stale run's session still claims Recording — the exact caption a genuinely
        // live run gets — so the is_stale() check has to run first, or this row would
        // read "recording" forever even though nothing can still own its directory.
        assert_eq!(
            state_word(&stale_run("r", session::State::Recording, false)),
            "stale — never finished"
        );
        // Also checked ahead of Saved's own special-cased wording.
        assert_eq!(
            state_word(&stale_run("r", session::State::Saved, false)),
            "stale — never finished"
        );
    }
}
