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

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use cosmic::app::{ApplicationExt, Core, Settings, Task};
use cosmic::iced::{Length, Subscription};
use cosmic::widget;
use cosmic::{Application, Element};

/// Lowercase, like `beisl setup`: the family's windows name themselves the same way.
const BASE_TITLE: &str = "beisl compat findings";

/// Upstream targets as gamebus-setup names them, with what a person calls them and
/// whether gamebus-setup can draft for them (`--export`). gamedb has no draft: its
/// pages are written by `gamebus-setup gamedb`, so here it can only be marked done.
/// Which of them a finding still wants is not decided here - `needs` says.
const TARGETS: &[(&str, &str, bool)] = &[
    ("protondb", "ProtonDB", true),
    ("awacy", "AreWeAntiCheatYet", true),
    ("gamedb", "gamebus-gamedb", false),
];

#[derive(Debug, Clone, Default)]
struct Observation {
    source: String,
    observed: String,
    wine: Option<String>,
    gpu: Option<String>,
    gpu_vendor: Option<String>,
    signature: Option<String>,
    log: Option<String>,
    trace_dir: Option<String>,
}

impl Observation {
    /// beisl reports a vendor tag and never a device name, so the vendor stands in
    /// rather than letting a beisl-only finding read "unknown GPU".
    fn gpu_label(&self) -> String {
        match (&self.gpu, &self.gpu_vendor) {
            (Some(g), _) => g.clone(),
            (None, Some(v)) => format!("{v} GPU"),
            (None, None) => "unknown GPU".to_string(),
        }
    }
}

/// One finding as `--json` reports it. Field names follow gamebus's `CompatFinding`,
/// plus the two it computes onto each record: `key` and `needs`. Anything else in the
/// record is left alone.
#[derive(Debug, Clone, Default)]
struct Finding {
    key: String,
    title: Option<String>,
    wall: String,
    steam_appid: Option<String>,
    first_seen: String,
    last_seen: String,
    observations: Vec<Observation>,
    reported: BTreeMap<String, String>,
    dismissed: Option<String>,
    /// Targets that still want this finding, as gamebus decides it. Not recomputed
    /// here: the AreWeAntiCheatYet rule (a Wine stub is Wine's bug, not theirs) is
    /// gamebus's to own, and two copies of it would drift.
    needs: Vec<String>,
}

impl Finding {
    fn display_title(&self) -> &str {
        self.title.as_deref().unwrap_or(&self.key)
    }

    fn latest(&self) -> Option<&Observation> {
        self.observations.last()
    }

    fn needs(&self, target: &str) -> bool {
        self.needs.iter().any(|t| t == target)
    }

    /// The wrapper's wording for this wall, where the two vocabularies meet.
    fn explanation(&self) -> &'static str {
        match self.wall.as_str() {
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
    let mut findings: Vec<Finding> = v
        .get("findings")
        .and_then(|f| f.as_array())
        .ok_or_else(|| "no findings array".to_string())?
        .iter()
        .map(|v| Finding {
            key: str_of(v, "key").unwrap_or_default(),
            title: str_of(v, "title"),
            wall: str_of(v, "wall").unwrap_or_default(),
            steam_appid: str_of(v, "steam_appid"),
            first_seen: str_of(v, "first_seen").unwrap_or_default(),
            last_seen: str_of(v, "last_seen").unwrap_or_default(),
            observations: v
                .get("observations")
                .and_then(|o| o.as_array())
                .map(|arr| {
                    arr.iter()
                        .map(|o| Observation {
                            source: str_of(o, "source").unwrap_or_default(),
                            observed: str_of(o, "observed").unwrap_or_default(),
                            wine: str_of(o, "wine"),
                            gpu: str_of(o, "gpu"),
                            gpu_vendor: str_of(o, "gpu_vendor"),
                            signature: str_of(o, "signature"),
                            log: str_of(o, "log"),
                            trace_dir: str_of(o, "trace_dir"),
                        })
                        .collect()
                })
                .unwrap_or_default(),
            reported: v
                .get("reported")
                .and_then(|r| r.as_object())
                .map(|r| {
                    r.iter()
                        .filter_map(|(t, d)| d.as_str().map(|d| (t.clone(), d.to_string())))
                        .collect()
                })
                .unwrap_or_default(),
            dismissed: str_of(v, "dismissed"),
            needs: strs_of(v, "needs"),
        })
        .collect();
    // gamebus sorts by key; a review list wants the freshest wall on top.
    findings.sort_by(|a, b| b.last_seen.cmp(&a.last_seen).then(a.key.cmp(&b.key)));

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
        format!(
            "gamebus-setup failed: {}",
            err.last().cloned().unwrap_or_else(|| "no output".into())
        )
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

#[derive(Debug, Clone)]
enum Message {
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
                    self.note = Some(format!(
                        "Scan failed: {}",
                        err.last().cloned().unwrap_or_default()
                    ));
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
                    self.note = Some(format!(
                        "Draft failed: {}",
                        err.last().cloned().unwrap_or_default()
                    ));
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
                    self.note = Some(format!(
                        "Could not mark: {}",
                        err.last().cloned().unwrap_or_default()
                    ));
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

    fn subscription(&self) -> Subscription<Self::Message> {
        cosmic::iced::time::every(Duration::from_millis(120)).map(|_| Message::Tick)
    }

    fn view(&self) -> Element<'_, Self::Message> {
        // Same frame as the setup app's traces page: the content scrolls, and navigation
        // sits in a footer under it, where the rest of the beisl windows keep it.
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
        let mut col = widget::Column::new().spacing(14).push(
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
            if f.dismissed.is_some() {
                head = head.push(widget::text::caption("dismissed"));
            }
            head = head
                .push(widget::button::standard("Open").on_press(Message::Select(f.key.clone())));
            let latest = f
                .latest()
                .map(|o| {
                    format!(
                        "  |  latest: {} on {}",
                        o.wine.as_deref().unwrap_or("unknown wine"),
                        o.gpu_label()
                    )
                })
                .unwrap_or_default();
            let pending: Vec<&str> = TARGETS
                .iter()
                .filter(|(t, _, _)| f.needs(t))
                .map(|(t, _, _)| *t)
                .collect();
            let mut item = widget::Column::new()
                .spacing(4)
                .push(head)
                .push(widget::text::caption(format!(
                    "{}  |  wall: {}  |  seen {} .. {}{latest}",
                    f.key, f.wall, f.first_seen, f.last_seen
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

    fn detail<'a>(&'a self, f: &'a Finding) -> Element<'a, Message> {
        let mut col = widget::Column::new()
            .spacing(14)
            .push(widget::text::title1(f.display_title().to_string()))
            .push(widget::text::caption(format!(
                "{}{}  |  seen {} .. {}",
                f.key,
                f.steam_appid
                    .as_deref()
                    .map(|a| format!("  |  Steam appid {a}"))
                    .unwrap_or_default(),
                f.first_seen,
                f.last_seen
            )));

        let wall = widget::Column::new()
            .spacing(6)
            .push(widget::text::body(format!("Wall: {}", f.wall)))
            .push(widget::text::caption(f.explanation()));
        col = col.push(widget::settings::section().title("What happened").add(wall));

        let mut obs = widget::settings::section().title("Evidence, newest first");
        for o in f.observations.iter().rev() {
            let mut item = widget::Column::new()
                .spacing(4)
                .push(widget::text::body(format!(
                    "{}  via {}  |  {} on {}",
                    o.observed,
                    o.source,
                    o.wine.as_deref().unwrap_or("unknown wine"),
                    o.gpu_label()
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
        for (target, label, draftable) in TARGETS {
            let mut row = widget::Row::new()
                .spacing(12)
                .push(widget::text::body(label.to_string()))
                .push(widget::Space::new().width(Length::Fill));
            if let Some(when) = f.reported.get(*target) {
                row = row.push(widget::text::caption(format!("reported {when}")));
            } else if !f.needs(target) {
                row = row.push(widget::text::caption(if f.dismissed.is_some() {
                    "dismissed"
                } else if *target == "awacy" {
                    "not theirs: they track anti-cheat behaviour"
                } else {
                    "nothing to submit"
                }));
            } else {
                if *draftable {
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
            let label = TARGETS
                .iter()
                .find(|(t, _, _)| *t == d.target)
                .map(|(_, l, _)| *l)
                .unwrap_or(d.target.as_str());
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
        assert_eq!(f.steam_appid.as_deref(), Some("4809930"));
        assert_eq!(f.reported.get("gamedb").map(String::as_str), Some("2026-09-06"));
        // The rule that stays gamebus's: needs is read, never recomputed.
        assert!(f.needs("protondb") && f.needs("awacy") && !f.needs("gamedb"));
        assert_eq!(l.took, ["steam:4809930"]);
        assert_eq!(l.refused[0].1, "not JSON");
    }

    #[test]
    fn a_finding_with_no_needs_is_offered_nothing() {
        let l = parse_listing(LIST).unwrap();
        let f = &l.findings[0];
        assert!(TARGETS.iter().all(|(t, _, _)| !f.needs(t)));
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

    /// beisl supplies a vendor and no device, and the window used to print "unknown GPU"
    /// over a finding that knew perfectly well it ran on an AMD card.
    #[test]
    fn vendor_stands_in_for_a_missing_device() {
        let vendor = Observation {
            gpu_vendor: Some("amd".into()),
            ..Observation::default()
        };
        assert_eq!(vendor.gpu_label(), "amd GPU");
        let device = Observation {
            gpu: Some("Radeon RX 6750 XT".into()),
            gpu_vendor: Some("amd".into()),
            ..Observation::default()
        };
        assert_eq!(device.gpu_label(), "Radeon RX 6750 XT");
        assert_eq!(Observation::default().gpu_label(), "unknown GPU");
    }
}
