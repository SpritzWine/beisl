//! The live session window: `beisl-setup --session <dir>`.
//!
//! Polls the wrapper's `session.json` and shows the settings, the recording state, and
//! "what's wrong" while the game runs. One control flows back — "Stop & save" sets
//! `stop_requested`, which the wrapper picks up. Mirrors the setup page's Application shape.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use cosmic::app::{Core, Task};
use cosmic::iced::{Length, Subscription};
use cosmic::widget;
use cosmic::{Application, Element};

use preflight::{detect_distro, fix_command, Config, DumpAccessPolicy, Fix};
use session::{Session, State};

#[derive(Debug, Clone)]
pub enum Msg {
    Tick,
    Stop,
    /// The live "your GPU crashed — help gather evidence" offer: install the access rule
    /// now (one polkit prompt). The wrapper is still polling the pending dump, so once the
    /// rule lands its next read captures it — no reverse channel needed for the retry.
    EnableDumpCapture,
    OpenFolder,
    Close,
}

/// Tracks the background `pkexec` install so the polkit prompt doesn't block the UI.
#[derive(Default)]
struct Enable {
    finished: bool,
    error: Option<String>,
}

pub struct SessionApp {
    core: Core,
    dir: PathBuf,
    session: Session,
    have: bool,
    config: Config,
    enabling: Option<Arc<Mutex<Enable>>>,
    enable_note: Option<String>,
}

impl Application for SessionApp {
    type Executor = cosmic::executor::Default;
    type Flags = PathBuf;
    type Message = Msg;

    const APP_ID: &'static str = "org.spritzwine.BeislSession";

    fn core(&self) -> &Core {
        &self.core
    }

    fn core_mut(&mut self) -> &mut Core {
        &mut self.core
    }

    fn init(core: Core, dir: Self::Flags) -> (Self, Task<Self::Message>) {
        let read = Session::read(&dir);
        let have = read.is_some();
        let session = read.unwrap_or_default();
        (
            Self {
                core,
                dir,
                session,
                have,
                config: Config::load(),
                enabling: None,
                enable_note: None,
            },
            Task::none(),
        )
    }

    fn update(&mut self, message: Self::Message) -> Task<Self::Message> {
        match message {
            Msg::Tick => {
                if let Some(s) = Session::read(&self.dir) {
                    self.session = s;
                    self.have = true;
                }
                if let Some(w) = &self.enabling {
                    let (finished, error) = {
                        let e = w.lock().unwrap();
                        (e.finished, e.error.clone())
                    };
                    if finished {
                        self.enabling = None;
                        if let Some(err) = error {
                            self.enable_note = Some(format!("Couldn't enable capture: {err}"));
                        } else {
                            // Persist intent + tell the user; the wrapper's watcher will
                            // capture the still-pending dump on its next poll.
                            self.config.dump_access_policy = DumpAccessPolicy::RuleInstalled;
                            let _ = self.config.save();
                            self.enable_note = Some(
                                "Capture enabled — saving this crash's dump now.".to_string(),
                            );
                        }
                    }
                }
            }
            Msg::EnableDumpCapture => {
                if self.enabling.is_none() {
                    let cmd = fix_command(&Fix::InstallDumpAccessRule, detect_distro());
                    if !cmd.is_empty() {
                        self.enable_note = Some("Enabling capture…".to_string());
                        self.enabling = Some(spawn_enable(cmd));
                    }
                }
            }
            Msg::Stop => {
                if let Some(mut s) = Session::read(&self.dir) {
                    s.stop_requested = true;
                    let _ = s.write(&self.dir);
                    self.session = s;
                }
            }
            Msg::OpenFolder => {
                let _ = std::process::Command::new("xdg-open").arg(&self.dir).spawn();
            }
            Msg::Close => std::process::exit(0),
        }
        Task::none()
    }

    fn subscription(&self) -> Subscription<Self::Message> {
        cosmic::iced::time::every(Duration::from_millis(400)).map(|_| Msg::Tick)
    }

    fn view(&self) -> Element<'_, Self::Message> {
        let s = &self.session;
        let badge = match s.state {
            State::Starting => "Starting…",
            State::Running => "Game running",
            State::Recording => "● Recording",
            State::Stopping => "Stopping…",
            State::Done => "Finished",
            State::Error => "Error",
        };

        let title = if s.game.is_empty() { "Beisl".to_string() } else { s.game.clone() };
        let mut col = widget::Column::new()
            .spacing(10)
            .push(widget::text::title3(title))
            .push(widget::text::body(badge))
            .push(widget::text::caption(format!(
                "Elapsed {}   ·   trace {}",
                mmss(s.elapsed_secs),
                human_bytes(s.trace_bytes)
            )))
            .push(widget::text::caption(format!("Hotkey: {}", s.hotkey)))
            .push(widget::text::caption(format!("Output: {}", s.trace_dir)));

        if !s.message.is_empty() {
            col = col.push(widget::text::body(s.message.clone()));
        }

        // "What's wrong" — the live warning list.
        if s.warnings.is_empty() {
            col = col.push(widget::text::caption("No problems detected."));
        } else {
            col = col.push(widget::text::body("Heads up:"));
            for w in &s.warnings {
                col = col.push(widget::text::caption(format!("• {w}")));
            }
        }

        // The live "help gather evidence" affordance: only under the warn-only policy, and
        // only once a crash has left a dump we couldn't read (the wrapper sets the flag).
        // The headline diagnosis is already in the warning text above; this is the action.
        if s.dump_capture_offer
            && self.config.dump_access_policy == DumpAccessPolicy::WarnOnly
        {
            let mut b = widget::button::suggested("Enable auto-capture");
            if self.enabling.is_none() {
                b = b.on_press(Msg::EnableDumpCapture);
            }
            col = col.push(b);
        }
        if let Some(note) = &self.enable_note {
            col = col.push(widget::text::caption(note.clone()));
        }

        if !self.have {
            col = col.push(widget::text::caption("(waiting for the session to start…)"));
        }

        // "Stop & save" only exists while a capture is actually running, so show it only
        // then rather than as a permanently-disabled control.
        let mut buttons = widget::Row::new()
            .spacing(8)
            .push(widget::button::text("Open folder").on_press(Msg::OpenFolder))
            .push(widget::Space::new().width(Length::Fill));
        if s.state == State::Recording {
            buttons = buttons.push(widget::button::standard("Stop & save").on_press(Msg::Stop));
        }
        let buttons = buttons.push(widget::button::suggested("Close").on_press(Msg::Close));

        // Content scrolls; the buttons stay pinned and visible at the bottom.
        let page = widget::Column::new()
            .spacing(10)
            .push(
                widget::scrollable(col)
                    .width(Length::Fill)
                    .height(Length::Fill),
            )
            .push(buttons);

        widget::container(page)
            .padding(24)
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
    }
}

/// Run the (single) `pkexec …` install argv on a background thread so the polkit prompt
/// doesn't block the event loop.
fn spawn_enable(cmd: Vec<String>) -> Arc<Mutex<Enable>> {
    let state = Arc::new(Mutex::new(Enable::default()));
    let s2 = state.clone();
    std::thread::spawn(move || {
        let result = std::process::Command::new(&cmd[0]).args(&cmd[1..]).output();
        let mut e = s2.lock().unwrap();
        match result {
            Ok(out) if out.status.success() => {}
            Ok(out) => {
                let msg = String::from_utf8_lossy(&out.stderr);
                e.error = Some(msg.lines().last().unwrap_or("failed").to_string());
            }
            Err(err) => e.error = Some(err.to_string()),
        }
        e.finished = true;
    });
    state
}

fn mmss(secs: u64) -> String {
    format!("{:02}:{:02}", secs / 60, secs % 60)
}

fn human_bytes(b: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;
    if b >= GB {
        format!("{:.1} GB", b as f64 / GB as f64)
    } else if b >= MB {
        format!("{:.1} MB", b as f64 / MB as f64)
    } else if b >= KB {
        format!("{:.0} KB", b as f64 / KB as f64)
    } else {
        format!("{b} B")
    }
}
