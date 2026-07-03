//! `beisl-setup` — the libcosmic setup page for Beisl.
//!
//! A read-only preflight checklist over the UI-agnostic `preflight` crate: it shows what
//! Beisl needs, ticks each item as it's satisfied, and runs the privileged fixes through
//! `pkexec` on a background thread (so the polkit prompt and the event loop don't block
//! each other). The UI polls the worker on a timer — the same pattern the SpritzWine
//! installer uses, avoiding the version-volatile parts of libcosmic's async API.
//!
//! A rotating tip panel turns the setup wait into onboarding (shortcut, where traces land,
//! that it works with any wine). Mirrors the installer's widget idioms for the pinned rev.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use cosmic::app::{Core, Settings, Task};
use cosmic::iced::{Length, Subscription};
use cosmic::widget;
use cosmic::{Application, Element};

use preflight::{all_checks, detect_distro, disk_check, fix_command, Check, Distro, Status};

mod window; // the live session window (`--session`)

/// Onboarding tips, cycled while the user waits.
const TIPS: &[&str] = &[
    "Press your MangoHud logging hotkey (default Shift+F2) to start and stop a trace.",
    "Traces are saved under ~/.local/share/beisl/traces/ — the app lists them for you.",
    "Beisl works with any Proton or Wine, not just SpritzWine — so you can compare them.",
    "If a game runs great, Beisl shows it's the game or GPU, not wine. That's a real answer.",
    "We grant perf one capability instead of weakening your whole system.",
];

// ---------------------------------------------------------------------------------------
// Background fix worker (runs the pkexec commands sequentially)
// ---------------------------------------------------------------------------------------

#[derive(Default)]
struct Shared {
    log: Vec<String>,
    finished: bool,
    error: Option<String>,
}

/// Run each `pkexec …` argv in turn, streaming a small log. Each raises its own polkit
/// prompt; a non-zero exit stops the run and surfaces the error.
fn spawn_fixer(cmds: Vec<Vec<String>>) -> Arc<Mutex<Shared>> {
    let shared = Arc::new(Mutex::new(Shared::default()));
    let s2 = shared.clone();
    std::thread::spawn(move || {
        for cmd in &cmds {
            {
                let mut s = s2.lock().unwrap();
                s.log.push(format!("==> {}", cmd.join(" ")));
            }
            match std::process::Command::new(&cmd[0]).args(&cmd[1..]).output() {
                Ok(out) => {
                    let mut s = s2.lock().unwrap();
                    for line in String::from_utf8_lossy(&out.stderr).lines() {
                        s.log.push(line.to_string());
                    }
                    if !out.status.success() {
                        s.error = Some(format!("`{}` failed", cmd.join(" ")));
                        s.finished = true;
                        return;
                    }
                }
                Err(e) => {
                    let mut s = s2.lock().unwrap();
                    s.error = Some(format!("could not run `{}`: {e}", cmd[0]));
                    s.finished = true;
                    return;
                }
            }
        }
        s2.lock().unwrap().finished = true;
    });
    shared
}

// ---------------------------------------------------------------------------------------
// Application
// ---------------------------------------------------------------------------------------

#[derive(Debug, Clone)]
enum Message {
    Recheck,
    FixAll,
    Tick,
    Close,
}

struct AppModel {
    core: Core,
    distro: Distro,
    trace_dir: PathBuf,
    checks: Vec<Check>,
    worker: Option<Arc<Mutex<Shared>>>,
    log: Vec<String>,
    error: Option<String>,
    tick: u64,
    tip: usize,
}

fn trace_dir() -> PathBuf {
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/share")
        });
    base.join("beisl").join("traces")
}

impl AppModel {
    fn detect_all(trace_dir: &PathBuf) -> Vec<Check> {
        let mut checks = all_checks();
        for c in checks.iter_mut() {
            c.detect();
        }
        checks.push(disk_check(trace_dir));
        checks
    }

    fn running(&self) -> bool {
        self.worker.is_some()
    }

    /// All *blocking* checks satisfied → the user can trace.
    fn ready(&self) -> bool {
        self.checks
            .iter()
            .filter(|c| c.blocking)
            .all(|c| c.status == Status::Ok)
    }

    /// The pkexec commands for every fixable check that still needs setup.
    fn pending_fixes(&self) -> Vec<Vec<String>> {
        self.checks
            .iter()
            .filter(|c| c.status == Status::NeedsFix)
            .filter_map(|c| c.fix.as_ref().map(|f| fix_command(f, self.distro)))
            .filter(|cmd| !cmd.is_empty())
            .collect()
    }
}

impl Application for AppModel {
    type Executor = cosmic::executor::Default;
    type Flags = ();
    type Message = Message;

    const APP_ID: &'static str = "org.spritzwine.Beisl";

    fn core(&self) -> &Core {
        &self.core
    }

    fn core_mut(&mut self) -> &mut Core {
        &mut self.core
    }

    fn init(core: Core, _flags: Self::Flags) -> (Self, Task<Self::Message>) {
        let trace_dir = trace_dir();
        let checks = Self::detect_all(&trace_dir);
        let app = AppModel {
            core,
            distro: detect_distro(),
            trace_dir,
            checks,
            worker: None,
            log: Vec::new(),
            error: None,
            tick: 0,
            tip: 0,
        };
        (app, Task::none())
    }

    fn update(&mut self, message: Self::Message) -> Task<Self::Message> {
        match message {
            Message::Recheck => {
                if !self.running() {
                    self.checks = Self::detect_all(&self.trace_dir);
                }
            }
            Message::FixAll => {
                if !self.running() {
                    let cmds = self.pending_fixes();
                    if !cmds.is_empty() {
                        // Show the ones we're about to fix as "working…".
                        for c in self.checks.iter_mut() {
                            if c.status == Status::NeedsFix && c.fix.is_some() {
                                c.status = Status::Fixing;
                            }
                        }
                        self.log.clear();
                        self.error = None;
                        self.worker = Some(spawn_fixer(cmds));
                    }
                }
            }
            Message::Tick => {
                self.tick += 1;
                if self.tick % 24 == 0 {
                    self.tip = (self.tip + 1) % TIPS.len();
                }
                if let Some(w) = &self.worker {
                    let (log, finished, error) = {
                        let s = w.lock().unwrap();
                        (s.log.clone(), s.finished, s.error.clone())
                    };
                    self.log = log;
                    if finished {
                        self.worker = None;
                        self.error = error;
                        self.checks = Self::detect_all(&self.trace_dir);
                    }
                }
            }
            Message::Close => std::process::exit(0),
        }
        Task::none()
    }

    fn subscription(&self) -> Subscription<Self::Message> {
        // Tick for the worker log and to rotate tips.
        cosmic::iced::time::every(Duration::from_millis(120)).map(|_| Message::Tick)
    }

    fn view(&self) -> Element<'_, Self::Message> {
        let title = if self.ready() {
            "You're ready to trace"
        } else {
            "Getting Beisl ready"
        };

        // The growing part (checklist + wrapping `why` lines + log) — this scrolls, so the
        // tip and footer below never get pushed out of the window.
        let mut content = widget::Column::new()
            .spacing(14)
            .push(widget::text::title1(title))
            .push(widget::text::caption(format!(
                "Traces will be saved to {}",
                self.trace_dir.display()
            )));

        for c in &self.checks {
            let is_ok = c.status == Status::Ok;
            // Read-only checkbox (no on_toggle) = the status indicator.
            let row = widget::Row::new()
                .spacing(12)
                .push(widget::checkbox(is_ok).label(c.label))
                .push(widget::Space::new().width(Length::Fill))
                .push(widget::text::caption(status_word(c)));
            content = content.push(row).push(widget::text::caption(c.why));
        }

        // While fixing, show the command log (rides along in the same scroll region).
        if self.running() || !self.log.is_empty() {
            let mut logcol = widget::Column::new().spacing(2);
            for line in self.log.iter().rev().take(80).rev() {
                logcol = logcol.push(widget::text::monotext(line.clone()));
            }
            content = content.push(logcol);
        }

        if let Some(e) = &self.error {
            content = content.push(widget::text::body(format!("Something went wrong: {e}")));
        }

        // Scrollable content fills the space; the rotating tip and the footer stay pinned
        // and always visible at the bottom.
        let page = widget::Column::new()
            .spacing(14)
            .push(
                widget::scrollable(content)
                    .width(Length::Fill)
                    .height(Length::Fill),
            )
            .push(widget::container(widget::text::body(TIPS[self.tip])).padding(12))
            .push(self.footer());

        widget::container(page)
            .padding(28)
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
    }
}

impl AppModel {
    fn footer(&self) -> Element<'_, Message> {
        let mut row = widget::Row::new().spacing(8);

        if self.ready() {
            row = row
                .push(widget::Space::new().width(Length::Fill))
                .push(widget::button::suggested("Done").on_press(Message::Close));
            return row.into();
        }

        // Re-check is always available when idle.
        let mut recheck = widget::button::standard("Re-check");
        let mut fix = widget::button::suggested("Set everything up");
        if !self.running() {
            recheck = recheck.on_press(Message::Recheck);
            if !self.pending_fixes().is_empty() {
                fix = fix.on_press(Message::FixAll);
            }
        }

        row.push(widget::button::text("Close").on_press(Message::Close))
            .push(widget::Space::new().width(Length::Fill))
            .push(recheck)
            .push(fix)
            .into()
    }
}

fn status_word(c: &Check) -> String {
    match &c.status {
        Status::Ok => "ready".into(),
        Status::NeedsFix => {
            if c.blocking {
                "needs setup".into()
            } else {
                "optional".into()
            }
        }
        Status::Fixing => "working…".into(),
        Status::Failed(e) => format!("error: {e}"),
        Status::Pending | Status::Checking => "checking…".into(),
    }
}

fn main() -> cosmic::iced::Result {
    // `beisl-setup --session <dir>` opens the live session window; otherwise the setup page.
    let mut args = std::env::args().skip(1);
    if args.next().as_deref() == Some("--session") {
        let dir = PathBuf::from(args.next().unwrap_or_default());
        let settings = Settings::default().size(cosmic::iced::Size::new(440.0, 560.0));
        return cosmic::app::run::<window::SessionApp>(settings, dir);
    }
    let settings = Settings::default().size(cosmic::iced::Size::new(640.0, 640.0));
    cosmic::app::run::<AppModel>(settings, ())
}
