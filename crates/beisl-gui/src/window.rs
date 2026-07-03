//! The live session window: `beisl-setup --session <dir>`.
//!
//! Polls the wrapper's `session.json` and shows the settings, the recording state, and
//! "what's wrong" while the game runs. One control flows back — "Stop & save" sets
//! `stop_requested`, which the wrapper picks up. Mirrors the setup page's Application shape.

use std::path::PathBuf;
use std::time::Duration;

use cosmic::app::{Core, Task};
use cosmic::iced::{Length, Subscription};
use cosmic::widget;
use cosmic::{Application, Element};

use session::{Session, State};

#[derive(Debug, Clone)]
pub enum Msg {
    Tick,
    Stop,
    OpenFolder,
    Close,
}

pub struct SessionApp {
    core: Core,
    dir: PathBuf,
    session: Session,
    have: bool,
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
        (Self { core, dir, session, have }, Task::none())
    }

    fn update(&mut self, message: Self::Message) -> Task<Self::Message> {
        match message {
            Msg::Tick => {
                if let Some(s) = Session::read(&self.dir) {
                    self.session = s;
                    self.have = true;
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
            State::Recording => "● Recording",
            State::Stopping => "Stopping…",
            State::Done => "Saved",
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

        if !self.have {
            col = col.push(widget::text::caption("(waiting for the session to start…)"));
        }

        let mut stop = widget::button::standard("Stop & save");
        if s.state == State::Recording {
            stop = stop.on_press(Msg::Stop);
        }
        let buttons = widget::Row::new()
            .spacing(8)
            .push(widget::button::text("Open folder").on_press(Msg::OpenFolder))
            .push(widget::Space::new().width(Length::Fill))
            .push(stop)
            .push(widget::button::suggested("Close").on_press(Msg::Close));

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
