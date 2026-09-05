//! `beisl-compat` - review the compat walls beisl found, and file them upstream.
//!
//! A second front-end over gamebus's report flow, and only that. The findings live in
//! gamebus-setup's stash, the scan that fills it and the drafts for ProtonDB and
//! AreWeAntiCheatYet are gamebus-setup's own subcommands, and marking a finding as
//! reported is one too. This window runs those commands and shows what they say; it
//! reimplements none of them, so the two UIs cannot drift apart on what a finding is
//! (the rule from the ecosystem plan: the logic lives in gamebus, beisl-setup drives it).
//!
//! It reads the stash file directly for display, as a plain map keyed by finding, so a
//! field this build does not know is ignored rather than fatal. Nothing here submits
//! anywhere: a draft is text to review, and the file-it link opens the browser.
//!
//! Same worker-and-tick pattern as beisl-setup: each gamebus-setup call runs on a thread
//! and the UI polls its captured output, keeping clear of the version-volatile async API.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use cosmic::app::{Core, Settings, Task};
use cosmic::iced::{Length, Subscription};
use cosmic::widget;
use cosmic::{Application, Element};

/// gamebus-setup's stash, relative to `$XDG_DATA_HOME` (its `CompatStash::default_path`).
const STASH_REL: &str = "gamebus-presenced/compat-findings.json";

/// Upstream targets as gamebus-setup names them, with what a person calls them and
/// whether gamebus-setup can draft for them (`--export`). gamedb has no draft: its
/// pages are written by `gamebus-setup gamedb`, so here it can only be marked done.
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
    signature: Option<String>,
    log: Option<String>,
    trace_dir: Option<String>,
}

/// One finding as the stash holds it. Field names follow gamebus's `CompatFinding`;
/// anything else in the file is left alone.
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
}

impl Finding {
    fn display_title(&self) -> &str {
        self.title.as_deref().unwrap_or(&self.key)
    }

    fn latest(&self) -> Option<&Observation> {
        self.observations.last()
    }

    /// gamebus-setup's own rule: a live finding a target has not seen, and
    /// AreWeAntiCheatYet only for anti-cheat walls (a Wine stub is not theirs).
    fn needs(&self, target: &str) -> bool {
        self.dismissed.is_none()
            && !self.observations.is_empty()
            && !self.reported.contains_key(target)
            && (target != "awacy" || self.is_anti_cheat())
    }

    fn is_anti_cheat(&self) -> bool {
        matches!(self.wall.as_str(), "kernel-anti-cheat" | "missing-runtime")
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

fn stash_path() -> Option<PathBuf> {
    let data = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))?;
    Some(data.join(STASH_REL))
}

fn str_of(v: &serde_json::Value, key: &str) -> Option<String> {
    v.get(key).and_then(|x| x.as_str()).map(str::to_string)
}

/// The stash, newest wall first. A missing file is an empty stash; a file that does not
/// parse is an error to show, never "no findings" (gamebus-setup's stance too).
fn load_stash(path: &Path) -> Result<Vec<Finding>, String> {
    let raw = match std::fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    let map: serde_json::Map<String, serde_json::Value> = serde_json::from_str(&raw)
        .map_err(|e| format!("{} exists but failed to parse: {e}", path.display()))?;
    let mut out: Vec<Finding> = map
        .iter()
        .map(|(key, v)| Finding {
            key: key.clone(),
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
        })
        .collect();
    out.sort_by(|a, b| b.last_seen.cmp(&a.last_seen).then(a.key.cmp(&b.key)));
    Ok(out)
}

/// `gamebus-setup` on PATH, or in `~/.local/bin` where its user-level install puts it
/// (a desktop launch does not always carry a login shell's PATH).
fn find_tool(name: &str) -> Option<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    if let Some(home) = std::env::var_os("HOME") {
        dirs.push(PathBuf::from(home).join(".local/bin"));
    }
    dirs.into_iter().map(|d| d.join(name)).find(|p| p.is_file())
}

// ---------------------------------------------------------------------------------------
// Background command worker
// ---------------------------------------------------------------------------------------

#[derive(Default)]
struct Shared {
    log: Vec<String>,
    finished: bool,
    ok: bool,
}

/// Run one argv, keeping stdout then stderr. gamebus-setup prints its drafts and lists on
/// stdout and its refusals on stderr, and a person wants both.
fn spawn_cmd(argv: Vec<String>) -> Arc<Mutex<Shared>> {
    let shared = Arc::new(Mutex::new(Shared::default()));
    let s2 = shared.clone();
    std::thread::spawn(move || {
        let result = std::process::Command::new(&argv[0]).args(&argv[1..]).output();
        let mut s = s2.lock().unwrap();
        match result {
            Ok(out) => {
                for stream in [&out.stdout, &out.stderr] {
                    for line in String::from_utf8_lossy(stream).lines() {
                        s.log.push(line.to_string());
                    }
                }
                s.ok = out.status.success();
            }
            Err(e) => {
                s.log.push(format!("could not run {}: {e}", argv[0]));
                s.ok = false;
            }
        }
        s.finished = true;
    });
    shared
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Job {
    Scan,
    Draft { key: String, target: String },
    MarkReported { key: String, target: String },
}

/// A draft as gamebus-setup printed it, plus the file-it URL pulled out for a button.
#[derive(Debug, Clone)]
struct Draft {
    target: String,
    text: String,
    url: Option<String>,
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
    stash_path: Option<PathBuf>,
    findings: Vec<Finding>,
    load_error: Option<String>,
    selected: Option<String>,
    job: Option<(Job, Arc<Mutex<Shared>>)>,
    /// What the last finished job said, shown under the toolbar.
    note: Option<String>,
    draft: Option<Draft>,
}

impl App {
    fn reload(&mut self) {
        match self.stash_path.as_deref().map(load_stash) {
            Some(Ok(findings)) => {
                self.findings = findings;
                self.load_error = None;
            }
            Some(Err(e)) => self.load_error = Some(e),
            None => self.load_error = Some("Cannot resolve the stash path (no HOME).".into()),
        }
        if let Some(sel) = &self.selected {
            if !self.findings.iter().any(|f| &f.key == sel) {
                self.selected = None;
            }
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

    /// A finished job's output becomes a note, a draft, or a reload - never silence.
    fn finish(&mut self, job: Job, log: Vec<String>, ok: bool) {
        match job {
            Job::Scan => {
                self.reload();
                let recorded = log.iter().filter(|l| l.starts_with("Recorded ")).count();
                let skipped = log.iter().filter(|l| l.starts_with("Skipped ")).count();
                self.note = Some(if !ok {
                    format!("Scan failed: {}", log.last().cloned().unwrap_or_default())
                } else if recorded == 0 && skipped == 0 {
                    "Scan done: no compat walls in beisl's traces.".into()
                } else {
                    format!("Scan done: {recorded} finding(s) recorded, {skipped} skipped.")
                });
            }
            Job::Draft { target, .. } => {
                if ok {
                    let url = log
                        .iter()
                        .find_map(|l| l.strip_prefix("File it at: "))
                        .map(|u| u.trim().to_string());
                    // Drop gamebus-setup's framing lines; the body is what gets reviewed.
                    let text: Vec<&str> = log
                        .iter()
                        .map(String::as_str)
                        .filter(|l| !l.starts_with("=== ") && !l.starts_with("Nothing was submitted"))
                        .collect();
                    self.draft = Some(Draft {
                        target,
                        text: text.join("\n").trim().to_string(),
                        url,
                    });
                } else {
                    self.note = Some(format!(
                        "Draft failed: {}",
                        log.last().cloned().unwrap_or_default()
                    ));
                }
            }
            Job::MarkReported { target, .. } => {
                self.reload();
                self.note = Some(if ok {
                    format!("Marked as reported to {target}.")
                } else {
                    format!("Could not mark: {}", log.last().cloned().unwrap_or_default())
                });
            }
        }
    }
}

impl Application for App {
    type Executor = cosmic::executor::Default;
    type Flags = ();
    type Message = Message;

    const APP_ID: &'static str = "org.spritzwine.BeislCompat";

    fn core(&self) -> &Core {
        &self.core
    }

    fn core_mut(&mut self) -> &mut Core {
        &mut self.core
    }

    fn init(core: Core, _flags: ()) -> (Self, Task<Self::Message>) {
        let mut app = App {
            core,
            gamebus_setup: find_tool("gamebus-setup"),
            mcp_found: find_tool("beisl-mcp").is_some(),
            presenced: preflight::gamebus_presence_check(),
            stash_path: stash_path(),
            findings: Vec::new(),
            load_error: None,
            selected: None,
            job: None,
            note: None,
            draft: None,
        };
        app.reload();
        (app, Task::none())
    }

    fn update(&mut self, message: Self::Message) -> Task<Self::Message> {
        match message {
            Message::Tick => {
                let done = self.job.as_ref().and_then(|(job, shared)| {
                    let s = shared.lock().unwrap();
                    s.finished.then(|| (job.clone(), s.log.clone(), s.ok))
                });
                if let Some((job, log, ok)) = done {
                    self.job = None;
                    self.finish(job, log, ok);
                }
            }
            Message::Reload => {
                self.reload();
                self.note = None;
            }
            Message::Scan => {
                if !self.busy() {
                    self.start(Job::Scan, &["--scan"]);
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
                    self.start(job, &["--export", &target, "--key", &key]);
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
        let page = match self.selected() {
            Some(f) => self.detail(f),
            None => self.list(),
        };
        widget::container(widget::scrollable(page))
            .padding(28)
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
    }
}

impl App {
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
            col = col.push(widget::text::body(format!(
                "The stash could not be read: {e}\nFix or move the file; nothing has \
                 overwritten it."
            )));
        }

        let mut section = widget::settings::section().title("Findings");
        if self.findings.is_empty() && self.load_error.is_none() {
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
            head = head.push(widget::button::standard("Open").on_press(Message::Select(f.key.clone())));
            let latest = f
                .latest()
                .map(|o| {
                    format!(
                        "  |  latest: {} on {}",
                        o.wine.as_deref().unwrap_or("unknown wine"),
                        o.gpu.as_deref().unwrap_or("unknown GPU")
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
            .push(
                widget::Row::new()
                    .spacing(12)
                    .push(widget::button::text("Back").on_press(Message::Back))
                    .push(widget::text::title1(f.display_title().to_string())),
            )
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
            let mut item = widget::Column::new().spacing(4).push(widget::text::body(format!(
                "{}  via {}  |  {} on {}",
                o.observed,
                o.source,
                o.wine.as_deref().unwrap_or("unknown wine"),
                o.gpu.as_deref().unwrap_or("unknown GPU")
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
                row = row.push(widget::text::caption(if *target == "awacy" {
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
                    widget::button::suggested("Open the form").on_press(Message::OpenUrl(url.clone())),
                );
            }
            let draft = widget::Column::new()
                .spacing(8)
                .push(widget::text::caption(
                    "Review and complete it before filing. gamebus-setup lists what it could \
                     not fill in; nothing was submitted.",
                ))
                .push(actions)
                .push(widget::text::monotext(d.text.clone()));
            col = col.push(widget::settings::section().title(format!("Draft for {label}")).add(draft));
        }
        col.into()
    }
}

fn main() -> cosmic::iced::Result {
    for a in std::env::args().skip(1) {
        match a.as_str() {
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
    cosmic::app::run::<App>(settings, ())
}
