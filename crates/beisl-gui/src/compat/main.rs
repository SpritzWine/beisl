//! `beisl-compat` - review the compat walls beisl found, and file them upstream.
//!
//! A second front-end over gamebus's report flow, and only that. The findings live in
//! gamebus-setup's stash, the scan that fills it and the drafts for ProtonDB and
//! AreWeAntiCheatYet are gamebus-setup's own subcommands, and marking a finding as
//! reported is one too. This window runs those commands and shows what they say; it
//! reimplements none of them, so the two UIs cannot drift apart on what a finding is
//! (the rule from the ecosystem plan: the logic lives in gamebus, beisl-setup drives it).
//!
//! Everything shown comes off `gamebus-setup compat --json`: the findings and what
//! they still need reporting to, what the drain just took, the scan's tally, the
//! drafts. gamebus grew that half for this window, so nothing here parses human output
//! or reads gamebus's stash file behind its back. Nothing here submits anywhere
//! either: a draft is text to review, and the file-it link opens the browser.
//!
//! Same worker-and-tick pattern as beisl-setup: each gamebus-setup call runs on a thread
//! and the UI polls its captured output, keeping clear of the version-volatile async API.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use cosmic::app::{ApplicationExt, Core, Settings, Task};
use cosmic::iced::{Length, Subscription};
use cosmic::widget;
use cosmic::{Application, Element};
use gamebus_coupler::{pickable_stores, CompatFinding, MissVerb, Observation, TARGETS};

mod identity;
mod mcp;
mod misses;

/// Lowercase, like `beisl setup`: the family's windows name themselves the same way.
const BASE_TITLE: &str = "beisl compat findings";

/// What a person calls each target, and whether gamebus-setup can draft for it
/// (`--export`). gamedb has no draft: its pages are written by `gamebus-setup gamedb`,
/// so here it can only be marked done.
///
/// The target NAMES are `gamebus_coupler::TARGETS` and are not repeated here - a list
/// of three strings had drifted into three copies. Only the wording is beisl's. The
/// draftable flag is still a guess about gamebus's CLI and belongs with them
/// eventually; raised with that side 2026-09-27, deliberately not in their first cut.
fn target_label(target: &str) -> (&'static str, bool) {
    match target {
        "protondb" => ("ProtonDB", true),
        "awacy" => ("AreWeAntiCheatYet", true),
        "gamedb" => ("gamebus-gamedb", false),
        _ => ("", false),
    }
}

/// beisl reports a vendor tag and never a device name, so the vendor stands in rather
/// than letting a beisl-only finding read "unknown GPU".
/// The selected tab reads as the suggested action, the others as plain buttons: the
/// same way beisl-setup marks where you are.
fn tab_button(label: &str, tab: Tab, current: Tab) -> Element<'static, Message> {
    let text = label.to_string();
    if tab == current {
        widget::button::suggested(text).into()
    } else {
        widget::button::standard(text)
            .on_press(Message::Tab(tab))
            .into()
    }
}

/// The setup TUI's own ordering, so the two front-ends list one stash the same way:
/// dismissed parked at the bottom, then newest first, then by title, then by key.
///
/// Deliberately `title` and not `effective_title`: this mirrors gamebus-setup's
/// `spawn_misses` exactly, and a sort that disagreed with the TUI would be worse than
/// one that sorts an overridden title under the daemon's original.
fn tui_order(rows: &mut [&gamebus_coupler::MissRow]) {
    rows.sort_by(|a, b| {
        a.miss
            .dismissed
            .is_some()
            .cmp(&b.miss.dismissed.is_some())
            .then_with(|| b.miss.last_seen.cmp(&a.miss.last_seen))
            .then_with(|| a.miss.title.cmp(&b.miss.title))
            .then_with(|| a.key.cmp(&b.key))
    });
}

/// The store ids a picker may offer, in the data set's order. gamebus compiles these
/// from gamebus-gamedb's `stores.toml`, and which ids are offerable is a flag in that
/// data - so this is one call, never a list kept in step by hand.
fn stores() -> Vec<&'static str> {
    pickable_stores().collect()
}

fn gpu_label(o: &Observation) -> String {
    match (&o.gpu, &o.gpu_vendor) {
        (Some(g), _) => g.clone(),
        (None, Some(v)) => format!("{v} GPU"),
        (None, None) => "unknown GPU".to_string(),
    }
}

/// The wrapper's wording for a wall, where the two vocabularies meet. gamebus names
/// the wall; `compat` is beisl's own crate and says what the log line meant.
fn explanation(wall: &str) -> &'static str {
    match wall {
        "kernel-anti-cheat" => compat::explain("kernel-driver"),
        "wine-stub" => compat::explain("wine-stub"),
        "volume-guid-path" => compat::explain("volume-guid"),
        "missing-runtime" => {
            "The game needs an anti-cheat runtime (EAC or BattlEye for Proton) that is \
             not installed here."
        }
        _ => compat::explain(""),
    }
}

/// One row of `compat --json`: gamebus's own record, plus the two fields the CLI
/// computes onto it. The record is `gamebus_coupler::CompatFinding` itself rather
/// than a copy of its fields, so a field this build does not know still round-trips
/// and no shape can drift between the two windows.
#[derive(Debug, Clone)]
struct Finding {
    key: String,
    /// Targets that still want this finding. Read from the envelope, NOT recomputed
    /// with the coupler's `pending_targets`: the authority is the gamebus binary that
    /// will do the marking, not the crate version linked here, and those two can drift
    /// once this is a rev-pinned dependency.
    needs: Vec<String>,
    record: CompatFinding,
}

impl Finding {
    fn display_title(&self) -> &str {
        self.record.title.as_deref().unwrap_or(&self.key)
    }

    fn needs(&self, target: &str) -> bool {
        self.needs.iter().any(|t| t == target)
    }
}

fn str_of(v: &serde_json::Value, key: &str) -> Option<String> {
    v.get(key).and_then(|x| x.as_str()).map(str::to_string)
}

fn strs_of(v: &serde_json::Value, key: &str) -> Vec<String> {
    v.get(key)
        .and_then(|x| x.as_array())
        .map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect())
        .unwrap_or_default()
}

/// What a bare `compat --json` answers: the whole stash, plus what this very call took
/// off beisl's inbox on the way (gamebus drains before every command).
#[derive(Debug, Default)]
struct Listing {
    findings: Vec<Finding>,
    took: Vec<String>,
    refused: Vec<(String, String)>,
}

fn parse_listing(out: &str) -> Result<Listing, String> {
    let v: serde_json::Value = serde_json::from_str(out).map_err(|e| e.to_string())?;
    let rows = v
        .get("findings")
        .and_then(|f| f.as_array())
        .ok_or_else(|| "no findings array".to_string())?;
    // Each row is a CompatFinding with `key` and `needs` added on top, so the record
    // deserializes with gamebus's own serde attributes and the two extras are read
    // beside it. A row that will not deserialize is an error, never a silent skip.
    let mut findings: Vec<Finding> = rows
        .iter()
        .map(|v| {
            let record: CompatFinding = serde_json::from_value(v.clone())
                .map_err(|e| format!("a finding did not parse: {e}"))?;
            Ok(Finding {
                key: str_of(v, "key").unwrap_or_default(),
                needs: strs_of(v, "needs"),
                record,
            })
        })
        .collect::<Result<_, String>>()?;
    // gamebus sorts by key; a review list wants the freshest wall on top.
    findings.sort_by(|a, b| {
        b.record
            .last_seen
            .cmp(&a.record.last_seen)
            .then(a.key.cmp(&b.key))
    });

    let inbox = v.get("inbox").cloned().unwrap_or_default();
    Ok(Listing {
        findings,
        took: strs_of(&inbox, "took"),
        refused: inbox
            .get("refused")
            .and_then(|r| r.as_array())
            .map(|a| {
                a.iter()
                    .map(|r| {
                        (
                            str_of(r, "path").unwrap_or_default(),
                            str_of(r, "why").unwrap_or_default(),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default(),
    })
}

/// What `--scan --json` answers. `truncated` means walls were left unread, which the
/// text output only ever mentioned in passing.
#[derive(Debug, Default)]
struct Scan {
    recorded: Vec<String>,
    skipped: Vec<String>,
    truncated: bool,
}

fn parse_scan(out: &str) -> Result<Scan, String> {
    let v: serde_json::Value = serde_json::from_str(out).map_err(|e| e.to_string())?;
    if !v.is_object() {
        return Err("not an object".into());
    }
    Ok(Scan {
        recorded: strs_of(&v, "recorded"),
        skipped: strs_of(&v, "skipped"),
        truncated: v.get("truncated").and_then(|t| t.as_bool()).unwrap_or(false),
    })
}

/// One draft out of `--export <target> --key <key> --json`. `missing` is the list of
/// facts no trace can supply, which the text output buried under the body.
#[derive(Debug, Default)]
struct Report {
    body: String,
    url: Option<String>,
    missing: Vec<String>,
}

fn parse_report(out: &str) -> Result<Option<Report>, String> {
    let v: serde_json::Value = serde_json::from_str(out).map_err(|e| e.to_string())?;
    let reports = v
        .get("reports")
        .and_then(|r| r.as_array())
        .ok_or_else(|| "no reports array".to_string())?;
    Ok(reports.first().map(|r| Report {
        body: str_of(r, "body").unwrap_or_default(),
        url: str_of(r, "url"),
        missing: strs_of(r, "missing"),
    }))
}

/// Everything gamebus-setup said on stderr, not just the last line.
///
/// Its refusals are written as cause then reassurance ("Refused the payload: ..." /
/// "Nothing was written."), and the auth refusals add a cure line. Taking only the last
/// line showed the reassurance and dropped the reason, which is the half a person needs.
/// Capped so a pathological run cannot fill the window.
fn why(err: &[String]) -> String {
    let lines: Vec<&str> = err
        .iter()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .collect();
    if lines.is_empty() {
        return "no output".into();
    }
    let keep = lines.len().saturating_sub(6);
    lines[keep..].join("\n")
}

/// What to show when a call did not answer in JSON. A gamebus-setup older than 0.4.0
/// ignores the flag and prints its human list instead, so a parse failure is how this
/// window finds out the two halves are out of step.
fn json_failure(parse_error: &str, err: &[String], ok: bool) -> String {
    if ok {
        format!(
            "gamebus-setup answered, but not in JSON ({parse_error}). This window drives \
             `gamebus-setup compat --json`, which gamebus-presenced grew in 0.4.0. Update \
             it and reopen."
        )
    } else {
        format!("gamebus-setup failed: {}", why(err))
    }
}

// ---------------------------------------------------------------------------------------
// Background command worker
// ---------------------------------------------------------------------------------------

#[derive(Default)]
struct Shared {
    /// stdout verbatim: gamebus-setup's JSON answer, which must not be interleaved
    /// with anything else to stay parseable.
    out: String,
    /// stderr, line by line. gamebus-setup keeps its refusals and failures here, and
    /// they are what a person needs to read when the answer did not come.
    err: Vec<String>,
    finished: bool,
    ok: bool,
}

fn spawn_cmd(argv: Vec<String>) -> Arc<Mutex<Shared>> {
    let shared = Arc::new(Mutex::new(Shared::default()));
    let s2 = shared.clone();
    std::thread::spawn(move || {
        let result = std::process::Command::new(&argv[0])
            .args(&argv[1..])
            .output();
        let mut s = s2.lock().unwrap();
        match result {
            Ok(out) => {
                s.out = String::from_utf8_lossy(&out.stdout).into_owned();
                s.err = String::from_utf8_lossy(&out.stderr)
                    .lines()
                    .map(str::to_string)
                    .collect();
                s.ok = out.status.success();
            }
            Err(e) => {
                s.err.push(format!("could not run {}: {e}", argv[0]));
                s.ok = false;
            }
        }
        s.finished = true;
    });
    shared
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Job {
    /// A bare `compat --json` run, which is both halves of a refresh at once: gamebus
    /// drains its inbox before every command, so this is how a finding beisl dropped at
    /// game exit reaches the stash without the user pressing anything, and the same
    /// answer carries the stash back. Runs at open and after anything that writes.
    List,
    Scan,
    Draft {
        key: String,
        target: String,
    },
    MarkReported {
        key: String,
        target: String,
    },
}

/// A draft as gamebus-setup rendered it: the body to review, where to file it, and the
/// facts it could not fill in from a trace.
#[derive(Debug, Clone)]
struct Draft {
    target: String,
    text: String,
    url: Option<String>,
    missing: Vec<String>,
}

// ---------------------------------------------------------------------------------------
// Application
// ---------------------------------------------------------------------------------------

/// Which half of gamebus's review this window is showing. gamedb joins when its read
/// tool lands; the compat tab stays on the CLI, which is the only path that drains the
/// inbox (gamebus's MCP reads deliberately never write).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tab {
    Compat,
    Misses,
}

#[derive(Debug, Clone)]
enum Message {
    Tab(Tab),
    MissVerb(String, &'static str),
    ShowDismissed(bool),
    /// Open the details pane on one miss, or on whatever is selected (the header
    /// button, which is why the key is optional).
    Details(Option<String>),
    CloseDetails,
    TitleDraft(String),
    SaveTitle,
    StorePicked(usize),
    Tick,
    Reload,
    Scan,
    Select(String),
    Back,
    Draft(String),
    MarkReported(String),
    OpenUrl(String),
    CopyDraft,
}

struct App {
    core: Core,
    tab: Tab,
    /// Started on the first visit to the misses tab, not at open: it spawns a
    /// gamebus-setup child, and a user who never leaves the compat tab should not pay
    /// for one.
    mcp: Option<misses::Worker>,
    /// Dismissed misses are parked, not deleted, and most of the stash here is parked.
    /// Hidden by default so the list shows what still wants a decision.
    show_dismissed: bool,
    /// The miss the details pane is showing. The pane is a context drawer, so COSMIC
    /// owns whether it is visible; this owns what is in it.
    details: Option<String>,
    /// The title field's contents while it is being edited. Separate from the record so
    /// a half-typed correction is never mistaken for a saved one.
    title_draft: String,
    gamebus_setup: Option<PathBuf>,
    mcp_found: bool,
    presenced: preflight::Check,
    findings: Vec<Finding>,
    /// Why the last list call produced nothing usable. Never "no findings": an answer
    /// that did not arrive must not read as an empty stash.
    load_error: Option<String>,
    selected: Option<String>,
    job: Option<(Job, Arc<Mutex<Shared>>)>,
    /// A finding to select as soon as it exists, from `--key`. Held until the opening
    /// drain lands, because the wall we were opened for may still be sitting in the
    /// inbox unread.
    pending_key: Option<String>,
    /// What the last finished job said, shown under the toolbar.
    note: Option<String>,
    /// What the header and taskbar currently say, so the title is pushed on change and
    /// not on every tick.
    title: String,
    draft: Option<Draft>,
}

impl App {
    fn reload(&mut self) {
        self.start(Job::List, &["--json"]);
    }

    /// Select the `--key` finding once the stash actually holds it. Silent while it does
    /// not: the drop may still be in the inbox, and the opening drain will bring it.
    fn take_pending_key(&mut self) {
        let Some(key) = self.pending_key.clone() else {
            return;
        };
        if self.findings.iter().any(|f| f.key == key) {
            self.selected = Some(key);
            self.pending_key = None;
        }
    }

    fn start_misses(&mut self) {
        let Some(bin) = self.gamebus_setup.clone() else {
            return;
        };
        let w = self.mcp.get_or_insert_with(|| misses::Worker::start(bin));
        w.send(misses::Job::List);
    }

    /// One miss by key, from whatever the worker last fetched.
    fn miss(&self, key: &str) -> Option<gamebus_coupler::MissRow> {
        let w = self.mcp.as_ref()?;
        let out = w.out.lock().ok()?;
        out.misses.iter().find(|r| r.key == key).cloned()
    }

    /// The details pane: the facts gamebus holds about one miss, and the corrections
    /// that need a field rather than a button. It folds in from the side instead of
    /// widening every row, which is what COSMIC's own file manager does with file
    /// details.
    fn details_pane(&self, row: &gamebus_coupler::MissRow) -> Element<'_, Message> {
        let m = &row.miss;
        let fact = |k: &str, v: String| {
            widget::Row::new()
                .spacing(12)
                .push(widget::text::caption(k.to_string()))
                .push(widget::Space::new().width(Length::Fill))
                .push(widget::text::body(v))
        };

        let mut facts = widget::settings::section().title("What gamebus knows");
        facts = facts
            .add(fact("Key", row.key.clone()))
            .add(fact("Store", m.effective_store().to_string()))
            .add(fact(
                "Codename",
                m.effective_codename().unwrap_or("-").to_string(),
            ))
            .add(fact("First seen", m.first_seen.clone()))
            .add(fact("Last seen", m.last_seen.clone()));
        if let Some(exe) = &m.executable {
            facts = facts.add(fact("Executable", exe.clone()));
        }
        if let Some(l) = &m.launcher {
            facts = facts.add(fact("Launcher", l.clone()));
        }
        // Where the name came from matters: a title the user set is not a resolution,
        // and the pane should not present it as one.
        let provenance = if m.title_override.is_some() {
            "set by you".to_string()
        } else {
            m.title_source.clone().unwrap_or_else(|| "unresolved".into())
        };
        facts = facts.add(fact("Title from", provenance));

        let title = widget::Column::new()
            .spacing(8)
            .push(widget::text::caption(
                "A correction, not a resolution: it overrides what the daemon worked \
                 out and survives its later writes.",
            ))
            .push(
                widget::text_input("Game title", &self.title_draft)
                    .on_input(Message::TitleDraft)
                    .on_submit(|_| Message::SaveTitle),
            )
            .push(
                widget::Row::new().spacing(8).push(
                    widget::button::suggested("Save title").on_press(Message::SaveTitle),
                ),
            );

        // The ids and their order are gamebus's (by how often the upstream database
        // uses each), not a list retyped here.
        let all = stores();
        let picked = all.iter().position(|s| *s == m.effective_store());
        let guess = if m.store_override.is_some() {
            format!(
                "Overridden. The daemon guessed \"{}\"; picking that again clears the \
                 override.",
                m.store
            )
        } else {
            "The daemon's own guess. Picking another records a correction.".to_string()
        };
        let store = widget::Column::new()
            .spacing(8)
            .push(widget::text::caption(guess))
            .push(widget::dropdown(all, picked, Message::StorePicked));

        widget::Column::new()
            .spacing(16)
            .push(facts)
            .push(widget::settings::section().title("Title").add(title))
            .push(widget::settings::section().title("Store").add(store))
            .into()
    }

    fn selected(&self) -> Option<&Finding> {
        let key = self.selected.as_ref()?;
        self.findings.iter().find(|f| &f.key == key)
    }

    fn busy(&self) -> bool {
        self.job.is_some()
    }

    fn start(&mut self, job: Job, args: &[&str]) {
        let Some(tool) = &self.gamebus_setup else {
            self.note = Some("gamebus-setup is not installed.".into());
            return;
        };
        let mut argv = vec![tool.display().to_string(), "compat".to_string()];
        argv.extend(args.iter().map(|a| a.to_string()));
        self.note = None;
        self.job = Some((job, spawn_cmd(argv)));
    }

    /// A finished job's answer becomes a note, a draft, or a fresh list - never
    /// silence. Every job answers in JSON on stdout; `start` clears the note, so a
    /// chained refresh is kicked off before the note that describes it is set.
    fn finish(&mut self, job: Job, out: String, err: Vec<String>, ok: bool) {
        match job {
            Job::List => {
                if !ok && out.trim().is_empty() {
                    self.load_error = Some(json_failure("", &err, false));
                    return;
                }
                match parse_listing(&out) {
                    Ok(listing) => {
                        self.findings = listing.findings;
                        self.load_error = None;
                        if let Some(sel) = &self.selected {
                            if !self.findings.iter().any(|f| &f.key == sel) {
                                self.selected = None;
                            }
                        }
                        self.take_pending_key();
                        let mut said = Vec::new();
                        if !listing.took.is_empty() {
                            said.push(format!(
                                "{} finding(s) just arrived from beisl.",
                                listing.took.len()
                            ));
                        }
                        // A drop gamebus refused is a wall the user has and would never
                        // hear about otherwise; it is kept on disk, not deleted.
                        for (path, why) in &listing.refused {
                            said.push(format!("A drop was refused ({why}): {path}"));
                        }
                        if !said.is_empty() {
                            self.note = Some(said.join("\n"));
                        }
                    }
                    Err(e) => self.load_error = Some(json_failure(&e, &err, ok)),
                }
            }
            Job::Scan => {
                if !ok {
                    self.note = Some(format!("Scan failed: {}", why(&err)));
                    return;
                }
                match parse_scan(&out) {
                    Ok(scan) => {
                        let mut said = if scan.recorded.is_empty() && scan.skipped.is_empty() {
                            "Scan done: no compat walls in beisl's traces.".to_string()
                        } else {
                            format!(
                                "Scan done: {} finding(s) recorded, {} skipped.",
                                scan.recorded.len(),
                                scan.skipped.len()
                            )
                        };
                        // Loud on purpose, as gamebus is: a wall it could not key is a
                        // wall the user has and does not know about.
                        for why in &scan.skipped {
                            said.push_str(&format!("\nSkipped: {why}"));
                        }
                        if scan.truncated {
                            said.push_str(
                                "\nThe event list was truncated; scan again for the rest.",
                            );
                        }
                        self.reload();
                        self.note = Some(said);
                    }
                    Err(e) => self.note = Some(json_failure(&e, &err, ok)),
                }
            }
            Job::Draft { target, .. } => {
                if !ok {
                    self.note = Some(format!("Draft failed: {}", why(&err)));
                    return;
                }
                match parse_report(&out) {
                    // One key in, so at most one draft out.
                    Ok(Some(r)) => {
                        self.draft = Some(Draft {
                            target,
                            text: r.body,
                            url: r.url,
                            missing: r.missing,
                        })
                    }
                    Ok(None) => {
                        self.note = Some(format!("gamebus-setup had nothing to draft for {target}."))
                    }
                    Err(e) => self.note = Some(json_failure(&e, &err, ok)),
                }
            }
            Job::MarkReported { target, .. } => {
                if ok {
                    self.reload();
                    self.note = Some(format!("Marked as reported to {target}."));
                } else {
                    self.note = Some(format!("Could not mark: {}", why(&err)));
                }
            }
        }
    }
}

impl Application for App {
    type Executor = cosmic::executor::Default;
    type Flags = Option<String>;
    type Message = Message;

    const APP_ID: &'static str = "org.spritzwine.BeislCompat";

    fn core(&self) -> &Core {
        &self.core
    }

    fn core_mut(&mut self) -> &mut Core {
        &mut self.core
    }

    fn init(core: Core, key: Self::Flags) -> (Self, Task<Self::Message>) {
        let mut core = core;
        core.set_header_title(BASE_TITLE.to_string());
        let mut app = App {
            core,
            tab: Tab::Compat,
            mcp: None,
            show_dismissed: false,
            details: None,
            title_draft: String::new(),
            gamebus_setup: preflight::find_tool("gamebus-setup"),
            mcp_found: preflight::find_tool("beisl-mcp").is_some(),
            presenced: preflight::gamebus_presence_check(),
            findings: Vec::new(),
            load_error: None,
            selected: None,
            job: None,
            pending_key: key,
            note: None,
            title: String::new(),
            draft: None,
        };
        // The list call drains the inbox on its way, so the wall this window was opened
        // for reaches the stash before anything tries to select it.
        app.reload();
        (app, Task::none())
    }

    fn update(&mut self, message: Self::Message) -> Task<Self::Message> {
        match message {
            Message::Tick => {
                // The window exists only after init, so the taskbar title is pushed here -
                // same place beisl-setup pushes its own.
                let want = self.window_title();
                if want != self.title {
                    self.title = want.clone();
                    self.set_header_title(want.clone());
                    if let Some(id) = self.core.main_window_id() {
                        let _ = self.set_window_title(want, id);
                    }
                }
                let done = self.job.as_ref().and_then(|(job, shared)| {
                    let s = shared.lock().unwrap();
                    s.finished
                        .then(|| (job.clone(), s.out.clone(), s.err.clone(), s.ok))
                });
                if let Some((job, out, err, ok)) = done {
                    self.job = None;
                    self.finish(job, out, err, ok);
                }
            }
            Message::Tab(tab) => {
                self.tab = tab;
                self.note = None;
                if tab == Tab::Misses {
                    self.start_misses();
                }
            }
            Message::ShowDismissed(v) => self.show_dismissed = v,
            Message::Details(key) => {
                if let Some(key) = key.or_else(|| self.details.clone()) {
                    self.title_draft = self
                        .miss(&key)
                        .and_then(|r| r.miss.effective_title().map(str::to_string))
                        .unwrap_or_default();
                    self.details = Some(key);
                    self.core.window.show_context = true;
                }
            }
            Message::CloseDetails => {
                self.core.window.show_context = false;
                self.details = None;
            }
            Message::TitleDraft(t) => self.title_draft = t,
            Message::StorePicked(i) => {
                let all = stores();
                if let (Some(w), Some(key), Some(store)) =
                    (&self.mcp, self.details.clone(), all.get(i))
                {
                    // The clearing rule is gamebus's: sending the daemon's own guess
                    // drops the override. beisl just sends what was picked.
                    w.send(misses::Job::Apply {
                        key,
                        verb: MissVerb::SetStore {
                            store: (*store).to_string(),
                        },
                    });
                }
            }
            Message::SaveTitle => {
                let draft = self.title_draft.trim().to_string();
                if let (Some(w), Some(key), false) =
                    (&self.mcp, self.details.clone(), draft.is_empty())
                {
                    w.send(misses::Job::Apply {
                        key,
                        verb: MissVerb::SetTitle { title: draft },
                    });
                }
            }
            Message::MissVerb(key, verb) => {
                if let Some(w) = &self.mcp {
                    let verb = match verb {
                        "dismiss" => MissVerb::Dismiss,
                        "undismiss" => MissVerb::Undismiss,
                        "promote" => MissVerb::Promote,
                        _ => MissVerb::Demote,
                    };
                    w.send(misses::Job::Apply { key, verb });
                }
            }
            Message::Reload => {
                if !self.busy() {
                    self.reload();
                }
            }
            Message::Scan => {
                if !self.busy() {
                    self.start(Job::Scan, &["--scan", "--json"]);
                }
            }
            Message::Select(key) => {
                self.selected = Some(key);
                self.draft = None;
                self.note = None;
            }
            Message::Back => {
                self.selected = None;
                self.draft = None;
                self.note = None;
            }
            Message::Draft(target) => {
                if let (Some(key), false) = (self.selected.clone(), self.busy()) {
                    self.draft = None;
                    let job = Job::Draft {
                        key: key.clone(),
                        target: target.clone(),
                    };
                    self.start(job, &["--export", &target, "--key", &key, "--json"]);
                }
            }
            Message::MarkReported(target) => {
                if let (Some(key), false) = (self.selected.clone(), self.busy()) {
                    let job = Job::MarkReported {
                        key: key.clone(),
                        target: target.clone(),
                    };
                    self.start(job, &["--reported", &key, "--target", &target]);
                }
            }
            Message::OpenUrl(url) => {
                // The desktop's own opener; the URL came from gamebus-setup's draft.
                let _ = std::process::Command::new("xdg-open").arg(url).spawn();
            }
            Message::CopyDraft => {
                if let Some(d) = &self.draft {
                    return cosmic::iced::clipboard::write(d.text.clone());
                }
            }
        }
        Task::none()
    }

    fn context_drawer(&self) -> Option<cosmic::app::context_drawer::ContextDrawer<'_, Message>> {
        let row = self.details.as_ref().and_then(|k| self.miss(k))?;
        let title = row
            .miss
            .effective_title()
            .unwrap_or(&row.key)
            .to_string();
        Some(
            cosmic::app::context_drawer::context_drawer(
                self.details_pane(&row),
                Message::CloseDetails,
            )
            .title(title),
        )
    }

    /// The pane's own toggle, where COSMIC's file manager keeps it. Always present on
    /// the misses tab, so the pane can be reopened without hunting for the row it was
    /// last on.
    fn header_start(&self) -> Vec<Element<'_, Message>> {
        if self.tab != Tab::Misses || self.details.is_none() {
            return Vec::new();
        }
        let showing = self.core.window.show_context;
        vec![widget::button::icon(widget::icon::from_name(
            "sidebar-show-right-symbolic",
        ))
        .on_press(if showing {
            Message::CloseDetails
        } else {
            Message::Details(None)
        })
        .into()]
    }

    fn subscription(&self) -> Subscription<Self::Message> {
        cosmic::iced::time::every(Duration::from_millis(120)).map(|_| Message::Tick)
    }

    fn view(&self) -> Element<'_, Self::Message> {
        // Same frame as the setup app's traces page: the content scrolls, and navigation
        // sits in a footer under it, where the rest of the beisl windows keep it.
        if self.tab == Tab::Misses {
            let content = self.misses_page();
            return widget::container(
                widget::Column::new()
                    .spacing(14)
                    .push(
                        widget::Row::new()
                            .spacing(8)
                            .push(tab_button("Compat findings", Tab::Compat, self.tab))
                            .push(tab_button("Identity misses", Tab::Misses, self.tab)),
                    )
                    .push(
                        widget::scrollable(widget::container(content).padding(
                            cosmic::iced::Padding::from([0, 14, 0, 0]),
                        ))
                        .width(Length::Fill)
                        .height(Length::Fill),
                    ),
            )
            .padding(28)
            .width(Length::Fill)
            .height(Length::Fill)
            .into();
        }
        let (content, footer) = match self.selected() {
            Some(f) => (
                self.detail(f),
                Some(
                    widget::Row::new()
                        .spacing(8)
                        .push(widget::button::text("Back").on_press(Message::Back))
                        .push(widget::Space::new().width(Length::Fill)),
                ),
            ),
            None => (self.list(), None),
        };
        let tabs = widget::Row::new()
            .spacing(8)
            .push(tab_button("Compat findings", Tab::Compat, self.tab))
            .push(tab_button("Identity misses", Tab::Misses, self.tab));
        let mut col = widget::Column::new().spacing(14).push(tabs).push(
            widget::scrollable(
                widget::container(content).padding(cosmic::iced::Padding::from([0, 14, 0, 0])),
            )
            .width(Length::Fill)
            .height(Length::Fill),
        );
        if let Some(footer) = footer {
            col = col.push(footer);
        }
        widget::container(col)
            .padding(28)
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
    }
}

impl App {
    /// The header and taskbar name this window by what it is, plus the finding in view -
    /// several of these can be open beside a session window and a setup page.
    fn window_title(&self) -> String {
        match self.selected() {
            Some(f) => format!("{} - {BASE_TITLE}", f.display_title()),
            None => BASE_TITLE.to_string(),
        }
    }

    /// One line on what this window can reach: the tool it drives, the daemon that names
    /// games, and the server the scan reads.
    fn status_line(&self) -> String {
        let setup = match &self.gamebus_setup {
            Some(p) => format!("gamebus-setup: {}", p.display()),
            None => "gamebus-setup: not found".to_string(),
        };
        let presenced = if self.presenced.status == preflight::Status::Ok {
            "gamebus-presenced: installed"
        } else {
            "gamebus-presenced: not installed"
        };
        let mcp = if self.mcp_found {
            "beisl-mcp: found"
        } else {
            "beisl-mcp: not found (the scan needs it)"
        };
        format!("{setup}  |  {presenced}  |  {mcp}")
    }

    fn list(&self) -> Element<'_, Message> {
        let mut col = widget::Column::new()
            .spacing(14)
            .push(widget::text::title1("Compat findings"))
            .push(widget::text::caption(self.status_line()));

        if self.gamebus_setup.is_none() {
            col = col.push(widget::text::body(
                "This window drives gamebus-setup, which is not installed. It ships with \
                 gamebus-presenced; install that and reopen.",
            ));
            return col.into();
        }

        let mut scan = widget::button::suggested("Scan beisl traces");
        let mut reload = widget::button::standard("Reload");
        if !self.busy() {
            scan = scan.on_press(Message::Scan);
            reload = reload.on_press(Message::Reload);
        }
        col = col
            .push(widget::Row::new().spacing(12).push(scan).push(reload))
            .push(widget::text::caption(
                "The scan reads beisl's traces through beisl-mcp and records every compat \
                 wall in gamebus's stash. Nothing leaves this machine.",
            ));
        if self.busy() {
            col = col.push(widget::text::caption("Running gamebus-setup..."));
        }
        if let Some(note) = &self.note {
            col = col.push(widget::text::body(note.clone()));
        }
        if let Some(e) = &self.load_error {
            col = col.push(widget::text::body(e.clone()));
        }

        let mut section = widget::settings::section().title("Findings");
        if self.findings.is_empty() && self.load_error.is_none() && !self.busy() {
            section = section.add(widget::text::caption(
                "No compat findings yet. Run a game under beisl, then scan.",
            ));
        }
        for f in &self.findings {
            let mut head = widget::Row::new()
                .spacing(12)
                .push(widget::text::body(f.display_title().to_string()))
                .push(widget::Space::new().width(Length::Fill));
            if f.record.dismissed.is_some() {
                head = head.push(widget::text::caption("dismissed"));
            }
            head = head
                .push(widget::button::standard("Open").on_press(Message::Select(f.key.clone())));
            let latest = f
                .record
                .latest()
                .map(|o| {
                    format!(
                        "  |  latest: {} on {}",
                        o.wine.as_deref().unwrap_or("unknown wine"),
                        gpu_label(o)
                    )
                })
                .unwrap_or_default();
            let pending: Vec<&str> = TARGETS
                .iter()
                .copied()
                .filter(|t| f.needs(t))
                .collect();
            let mut item = widget::Column::new()
                .spacing(4)
                .push(head)
                .push(widget::text::caption(format!(
                    "{}  |  wall: {}  |  seen {} .. {}{latest}",
                    f.key,
                    f.record.wall.as_str(),
                    f.record.first_seen,
                    f.record.last_seen
                )));
            if !pending.is_empty() {
                item = item.push(widget::text::caption(format!(
                    "not yet reported to: {}",
                    pending.join(", ")
                )));
            }
            section = section.add(item);
        }
        col.push(section).into()
    }

    /// gamebus's identity misses: launches it could not name authoritatively. The
    /// review verbs that need no typing are here; correcting a title or a store needs a
    /// text field and comes with the rest of the tab.
    fn misses_page(&self) -> Element<'_, Message> {
        let mut col = widget::Column::new()
            .spacing(14)
            .push(widget::text::title1("Identity misses"))
            .push(widget::text::caption(
                "Games gamebus saw launch without an authoritative identity. Dismissing \
                 parks one; promoting opts a umu launch into the umu-database pipeline. \
                 Nothing here is submitted anywhere.",
            ));

        if self.gamebus_setup.is_none() {
            return col
                .push(widget::text::body(
                    "This tab drives gamebus-setup, which is not installed.",
                ))
                .into();
        }

        let Some(w) = &self.mcp else {
            return col.push(widget::text::caption("Connecting to gamebus...")).into();
        };
        let out = w.out.lock().unwrap();

        // The one alarm worth raising, and only when it means something.
        if let Some(note) = out.ledger.as_ref().and_then(misses::ledger_note) {
            col = col.push(widget::text::body(note));
        }
        if out.busy {
            col = col.push(widget::text::caption("Talking to gamebus-setup..."));
        }
        if let Some(e) = &out.error {
            col = col.push(widget::text::body(e.clone()));
        }

        // gamebus's MCP server returns misses in key order, which is a stable wire
        // order rather than a reading order. This is the setup TUI's own sort, so the
        // two front-ends list the same stash the same way.
        let mut rows: Vec<&gamebus_coupler::MissRow> = out.misses.iter().collect();
        tui_order(&mut rows);
        let parked = rows.iter().filter(|r| r.miss.dismissed.is_some()).count();
        if !self.show_dismissed {
            rows.retain(|r| r.miss.dismissed.is_none());
        }
        if parked > 0 {
            col = col.push(
                widget::toggler(self.show_dismissed)
                    .label(format!("Show {parked} dismissed"))
                    .on_toggle(Message::ShowDismissed),
            );
        }

        let mut section = widget::settings::section().title("Misses");
        if rows.is_empty() && !out.busy && out.error.is_none() {
            section = section.add(widget::text::caption(if parked > 0 {
                "Every miss here has been dismissed. Switch them on above to review one."
            } else {
                "Nothing recorded. gamebus notes a miss when a game launches with no \
                 identity it can trust."
            }));
        }
        for row in rows {
            let m = &row.miss;
            let dismissed = m.dismissed.is_some();
            let promoted = m.umu_promoted.is_some();
            let mut head = widget::Row::new()
                .spacing(12)
                .push(widget::text::body(
                    m.effective_title().unwrap_or(&row.key).to_string(),
                ))
                .push(widget::Space::new().width(Length::Fill));
            if dismissed {
                head = head.push(widget::text::caption("dismissed"));
            }
            if row.umu_candidate {
                head = head.push(widget::text::caption("umu candidate"));
            }
            let mut actions = widget::Row::new().spacing(8);
            let (label, verb) = if dismissed {
                ("Restore", "undismiss")
            } else {
                ("Dismiss", "dismiss")
            };
            actions = actions.push(
                widget::button::standard("Details")
                    .on_press(Message::Details(Some(row.key.clone()))),
            );
            actions = actions.push(
                widget::button::standard(label)
                    .on_press(Message::MissVerb(row.key.clone(), verb)),
            );
            // Promotion is a umu-database act, so it is offered only where it means
            // something: a launcher launch that never went through umu has nothing to
            // promote, and gamebus would refuse it.
            if row.is_umu_miss {
                let (label, verb) = if promoted {
                    ("Take back", "demote")
                } else {
                    ("Promote to umu", "promote")
                };
                actions = actions.push(
                    widget::button::text(label)
                        .on_press(Message::MissVerb(row.key.clone(), verb)),
                );
            }
            let item = widget::Column::new()
                .spacing(4)
                .push(head)
                .push(widget::text::caption(format!(
                    "{}  |  store: {}  |  codename: {}  |  seen {} .. {}",
                    row.key,
                    m.effective_store(),
                    m.effective_codename().unwrap_or("-"),
                    m.first_seen,
                    m.last_seen
                )))
                .push(actions);
            section = section.add(item);
        }
        col.push(section).into()
    }

    fn detail<'a>(&'a self, f: &'a Finding) -> Element<'a, Message> {
        let mut col = widget::Column::new()
            .spacing(14)
            .push(widget::text::title1(f.display_title().to_string()))
            .push(widget::text::caption(format!(
                "{}{}  |  seen {} .. {}",
                f.key,
                f.record
                    .steam_appid
                    .as_deref()
                    .map(|a| format!("  |  Steam appid {a}"))
                    .unwrap_or_default(),
                f.record.first_seen,
                f.record.last_seen
            )));

        let wall = widget::Column::new()
            .spacing(6)
            .push(widget::text::body(format!("Wall: {}", f.record.wall.as_str())))
            .push(widget::text::caption(explanation(f.record.wall.as_str())));
        col = col.push(widget::settings::section().title("What happened").add(wall));

        let mut obs = widget::settings::section().title("Evidence, newest first");
        for o in f.record.observations.iter().rev() {
            let mut item = widget::Column::new()
                .spacing(4)
                .push(widget::text::body(format!(
                    "{}  via {}  |  {} on {}",
                    o.observed,
                    o.source,
                    o.wine.as_deref().unwrap_or("unknown wine"),
                    gpu_label(o)
                )));
            if let Some(sig) = &o.signature {
                item = item.push(widget::text::caption(format!("signature: {sig}")));
            }
            if let Some(log) = &o.log {
                item = item.push(widget::text::caption(format!("log: {log}")));
            }
            if let Some(dir) = &o.trace_dir {
                item = item.push(widget::text::caption(format!("trace: {dir}")));
            }
            obs = obs.add(item);
        }
        col = col.push(obs);

        // Report: one row per target. Drafting prints text to review; filing is the
        // user's, in the browser; marking done is what gamebus-setup remembers.
        let mut report = widget::settings::section().title("Report it");
        for target in TARGETS.iter().copied() {
            let (label, draftable) = target_label(target);
            let mut row = widget::Row::new()
                .spacing(12)
                .push(widget::text::body(label.to_string()))
                .push(widget::Space::new().width(Length::Fill));
            if let Some(when) = f.record.reported.get(target) {
                row = row.push(widget::text::caption(format!("reported {when}")));
            } else if !f.needs(target) {
                row = row.push(widget::text::caption(if f.record.dismissed.is_some() {
                    "dismissed"
                } else if target == "awacy" {
                    "not theirs: they track anti-cheat behaviour"
                } else {
                    "nothing to submit"
                }));
            } else {
                if draftable {
                    let mut b = widget::button::standard("Draft");
                    if !self.busy() {
                        b = b.on_press(Message::Draft(target.to_string()));
                    }
                    row = row.push(b);
                }
                let mut done = widget::button::text("Mark reported");
                if !self.busy() {
                    done = done.on_press(Message::MarkReported(target.to_string()));
                }
                row = row.push(done);
            }
            report = report.add(row);
        }
        col = col.push(report);

        if self.busy() {
            col = col.push(widget::text::caption("Running gamebus-setup..."));
        }
        if let Some(note) = &self.note {
            col = col.push(widget::text::body(note.clone()));
        }
        if let Some(d) = &self.draft {
            let label = match target_label(&d.target) {
                ("", _) => d.target.as_str(),
                (l, _) => l,
            };
            let mut actions = widget::Row::new()
                .spacing(12)
                .push(widget::button::standard("Copy text").on_press(Message::CopyDraft));
            if let Some(url) = &d.url {
                actions = actions.push(
                    widget::button::suggested("Open the form")
                        .on_press(Message::OpenUrl(url.clone())),
                );
            }
            let mut draft = widget::Column::new()
                .spacing(8)
                .push(widget::text::caption(
                    "Review and complete it before filing. Nothing was submitted.",
                ))
                .push(actions);
            // The facts no trace can supply, which the text output buried under the body.
            if !d.missing.is_empty() {
                draft = draft.push(widget::text::body(format!(
                    "Before filing, this draft still needs: {}",
                    d.missing.join(", ")
                )));
            }
            let draft = draft.push(widget::text::monotext(d.text.clone()));
            col = col.push(
                widget::settings::section()
                    .title(format!("Draft for {label}"))
                    .add(draft),
            );
        }
        col.into()
    }
}

fn main() -> cosmic::iced::Result {
    let mut key = None;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            // Open straight on one finding, as beisl-setup's Compatibility section does
            // when it hands a wall over. An unknown key just opens the list.
            "--key" => key = args.next().filter(|k| !k.is_empty()),
            "--version" => {
                println!("beisl-compat {}", preflight::VERSION);
                return Ok(());
            }
            "--help" | "-h" | "help" => {
                // Single source with the man page: docs/beisl-compat.1.md's top, cut at the
                // help-end marker, `# ` section markers stripped (as beisl-setup does).
                let doc = include_str!("../../../../docs/beisl-compat.1.md");
                let top = doc.split("<!-- help-end -->").next().unwrap_or(doc);
                let lines: Vec<&str> = top
                    .lines()
                    .map(|l| l.strip_prefix("# ").unwrap_or(l))
                    .collect();
                println!("{}", lines.join("\n").trim());
                return Ok(());
            }
            _ => {}
        }
    }
    let settings = Settings::default().size(cosmic::iced::Size::new(880.0, 640.0));
    cosmic::app::run::<App>(settings, key)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Trimmed from a real `gamebus-setup compat --json`, unknown fields kept: this
    /// window must not care that gamebus records a layer split it does not show.
    const LIST: &str = r#"{
      "findings": [
        {
          "awacy_slug": null,
          "dismissed": null,
          "first_seen": "2026-09-05",
          "key": "steam:4809930",
          "last_seen": "2026-09-05",
          "needs": ["protondb", "awacy"],
          "observations": [
            {
              "attributed_pct": 74.68,
              "a_field_from_a_newer_gamebus": 1,
              "gpu_vendor": "amd",
              "layer_split": {"game": 74.16},
              "log": "beisl:run-1788635431",
              "observed": "2026-09-05",
              "signature": "fixme:ntdll:NtLoadDriver",
              "source": "beisl",
              "trace_dir": "/home/x/.local/share/beisl/traces/run-1788635431",
              "wine": "SpritzWine-Prater"
            }
          ],
          "reported": {"gamedb": "2026-09-06"},
          "steam_appid": "4809930",
          "title": "WARDOGS Playtest",
          "wall": "kernel-anti-cheat"
        },
        {
          "first_seen": "2026-08-01",
          "key": "steam:1",
          "last_seen": "2026-09-09",
          "needs": [],
          "observations": [],
          "reported": {},
          "title": null,
          "wall": "wine-stub"
        }
      ],
      "inbox": {
        "took": ["steam:4809930"],
        "refused": [{"path": "/x/0001-run-42.json", "why": "not JSON"}]
      }
    }"#;

    #[test]
    fn listing_carries_key_needs_and_inbox() {
        let l = parse_listing(LIST).unwrap();
        // Freshest wall first, not gamebus's key order.
        assert_eq!(l.findings[0].key, "steam:1");
        let f = &l.findings[1];
        assert_eq!(f.display_title(), "WARDOGS Playtest");
        assert_eq!(f.record.steam_appid.as_deref(), Some("4809930"));
        assert_eq!(
            f.record.reported.get("gamedb").map(String::as_str),
            Some("2026-09-06")
        );
        // The record round-trips through gamebus's own type: fields this window never
        // reads are still parsed (layer_split is a real field there now), and a field
        // NO build knows lands in the catch-all rather than failing the row.
        assert!(f.record.observations[0].layer_split.is_some());
        assert!(f.record.observations[0]
            .extra
            .contains_key("a_field_from_a_newer_gamebus"));
        // The rule that stays gamebus's: needs is read, never recomputed.
        assert!(f.needs("protondb") && f.needs("awacy") && !f.needs("gamedb"));
        assert_eq!(l.took, ["steam:4809930"]);
        assert_eq!(l.refused[0].1, "not JSON");
    }

    #[test]
    fn a_finding_with_no_needs_is_offered_nothing() {
        let l = parse_listing(LIST).unwrap();
        let f = &l.findings[0];
        assert!(TARGETS.iter().all(|t| !f.needs(t)));
    }

    /// A gamebus-setup older than 0.4.0 ignores `--json` and prints its list. That must
    /// read as version skew, never as an empty stash.
    #[test]
    fn human_output_is_an_error_not_an_empty_stash() {
        let human = "* WARDOGS Playtest  [steam:4809930]\n    wall: kernel-anti-cheat\n";
        assert!(parse_listing(human).is_err());
        assert!(json_failure("expected value", &[], true).contains("0.4.0"));
    }

    #[test]
    fn scan_and_report_shapes() {
        let s = parse_scan(r#"{"recorded":["steam:7"],"skipped":["a wall with no key"],"truncated":true}"#).unwrap();
        assert_eq!(s.recorded, ["steam:7"]);
        assert_eq!(s.skipped, ["a wall with no key"]);
        assert!(s.truncated);

        let r = parse_report(
            r#"{"reports":[{"key":"steam:7","target":"protondb","url":"https://p/app/7",
                "body":"text","missing":["GPU actually used: unknown."]}]}"#,
        )
        .unwrap()
        .unwrap();
        assert_eq!(r.url.as_deref(), Some("https://p/app/7"));
        assert_eq!(r.missing.len(), 1);
        assert!(parse_report(r#"{"reports":[]}"#).unwrap().is_none());
    }

    /// Every tiebreaker in gamebus-setup's `spawn_misses`, in order. Drift here means
    /// the two windows list one stash differently, which is what this was fixing.
    #[test]
    fn misses_list_in_the_setup_tuis_order() {
        let row = |key: &str, title: Option<&str>, last: &str, dismissed: bool| {
            let mut v = serde_json::json!({
                "key": key, "is_umu_miss": true, "umu_candidate": false,
                "store": "steam", "umu_id": "umu-0",
                "first_seen": "2026-01-01", "last_seen": last,
            });
            let m = v.as_object_mut().unwrap();
            if let Some(t) = title {
                m.insert("title".into(), serde_json::json!(t));
            }
            if dismissed {
                m.insert("dismissed".into(), serde_json::json!("2026-02-02"));
            }
            serde_json::from_value::<gamebus_coupler::MissRow>(v).expect("row")
        };
        let all = vec![
            row("k-old", Some("Zed"), "2026-01-02", false),
            row("k-dismissed", Some("Aaa"), "2026-09-09", true),
            row("k-b", Some("Beta"), "2026-05-05", false),
            row("k-a", Some("Alpha"), "2026-05-05", false),
            row("k-z", Some("Alpha"), "2026-05-05", false),
        ];
        let mut rows: Vec<&gamebus_coupler::MissRow> = all.iter().collect();
        tui_order(&mut rows);
        let order: Vec<&str> = rows.iter().map(|r| r.key.as_str()).collect();
        assert_eq!(
            order,
            [
                // newest undismissed first; ties by title, then by key
                "k-a", "k-z", "k-b", "k-old",
                // dismissed parks at the bottom however new it is
                "k-dismissed",
            ]
        );
    }

    /// gamebus writes a refusal as cause then reassurance, and the auth refusals add a
    /// cure. Showing only the last line lost the reason, which is the half worth reading.
    #[test]
    fn a_refusal_keeps_its_reason_not_just_its_reassurance() {
        let two_line = [
            "Refused the payload: delivery 1: missing field `wall`".to_string(),
            "Nothing was written.".to_string(),
        ];
        let shown = why(&two_line);
        assert!(shown.contains("missing field"), "the cause was dropped: {shown}");
        assert!(shown.contains("Nothing was written."));

        // A cause-plus-cure refusal, the shape the MCP work adds to the CLI.
        let with_cure = [
            "Refused: the ledger could not be verified.".to_string(),
            "Re-initialise the passphrase: gamebus-setup auth init.".to_string(),
        ];
        assert!(why(&with_cure).contains("auth init"));
        assert!(why(&with_cure).contains("could not be verified"));

        assert_eq!(why(&[]), "no output");
        assert_eq!(why(&["  ".to_string(), "real".to_string()]), "real");
        // Capped, newest kept.
        let many: Vec<String> = (0..20).map(|i| i.to_string()).collect();
        assert_eq!(why(&many).lines().count(), 6);
        assert!(why(&many).ends_with("19"));
    }

    /// beisl supplies a vendor and no device, and the window used to print "unknown GPU"
    /// over a finding that knew perfectly well it ran on an AMD card. Built through the
    /// wire rather than a struct literal: `gamebus_coupler::Observation` has no
    /// `Default`, and JSON is how one ever actually reaches this window.
    #[test]
    fn vendor_stands_in_for_a_missing_device() {
        let obs = |extra: serde_json::Value| -> Observation {
            let mut base = serde_json::json!({"source": "beisl", "observed": "2026-09-05"});
            let (b, e) = (base.as_object_mut().unwrap(), extra.as_object().unwrap().clone());
            b.extend(e);
            serde_json::from_value(base).expect("observation")
        };
        assert_eq!(gpu_label(&obs(serde_json::json!({"gpu_vendor": "amd"}))), "amd GPU");
        assert_eq!(
            gpu_label(&obs(serde_json::json!({
                "gpu": "Radeon RX 6750 XT", "gpu_vendor": "amd"
            }))),
            "Radeon RX 6750 XT"
        );
        assert_eq!(gpu_label(&obs(serde_json::json!({}))), "unknown GPU");
    }
}
