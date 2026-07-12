//! The live session window: `beisl-setup --session <dir>`.
//!
//! Three surfaces over one trace dir, in the order a run moves through them:
//!
//! 1. **Live** — while the game runs: settings, recording state, and "what's wrong",
//!    with one control flowing back ("Stop & save" sets `stop_requested`).
//! 2. **The ask** — a *kept* clean run the wrapper left un-analyzed (`analysis_offer`,
//!    no `artifact.json`): "Analyze" (resolve on a background thread) or "Discard".
//! 3. **The results view** — an `artifact.json` exists (a crash auto-analyzed, or the
//!    user hit Analyze): the layer split + frametime health + issues.
//!
//! The window does *no* analysis itself: surfaces 2→3 run and render `artifact::Trace-
//! Artifact`, the same code path `beisl symbols` and the crash teardown use. It only
//! binds to the result, exactly as the setup page binds to `preflight`.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use cosmic::app::{ApplicationExt, Core, Task};
use cosmic::iced::{Length, Subscription};
use cosmic::widget;
use cosmic::{Application, Element};

use artifact::{TraceArtifact, SCHEMA_VERSION};
use preflight::{detect_distro, detect_system_kind, fix_command, Config, DumpAccessPolicy, Fix};
use session::{Session, State};

#[derive(Debug, Clone)]
pub enum Msg {
    Tick,
    Stop,
    /// The live "your GPU crashed — help gather evidence" offer: install the access rule
    /// now (one polkit prompt). The wrapper is still polling the pending dump, so once the
    /// rule lands its next read captures it — no reverse channel needed for the retry.
    EnableDumpCapture,
    /// The ask: resolve the kept trace now (background thread), then show the results.
    Analyze,
    /// The ask: throw the trace away — a two-step, since it deletes the run dir.
    Discard,
    ConfirmDiscard,
    CancelDiscard,
    OpenFolder,
    Close,
}

/// Tracks the background `pkexec` install so the polkit prompt doesn't block the UI.
#[derive(Default)]
struct Enable {
    finished: bool,
    error: Option<String>,
}

/// Tracks the background `TraceArtifact::build` so the ~seconds-long `perf script` resolve
/// never blocks the event loop (the setup app's worker + `Tick`-poll pattern).
#[derive(Default)]
struct Analyze {
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
    /// The resolved trace, once `artifact.json` exists (crash auto-analyzed, or Analyze ran).
    artifact: Option<TraceArtifact>,
    /// `perf.data` still on disk — the gate for offering analysis on a finished run
    /// (without it there is nothing to resolve, however the run ended).
    raw_present: bool,
    analyzing: Option<Arc<Mutex<Analyze>>>,
    analyze_note: Option<String>,
    /// Discard is destructive (removes the run dir), so it takes a confirm click.
    confirm_discard: bool,
    /// The window/taskbar title — "Beisl" alone until we know the game, then "Beisl — <game>".
    title: String,
    /// Whether the xdg window title has been pushed yet (only possible once the window
    /// exists, i.e. on the first Tick, not in `init`).
    title_pushed: bool,
}

/// The window title: the product name, then what the window is currently *doing* — the live
/// state while a game runs, or which surface it's showing afterwards. The current activity
/// reads better in a taskbar than a raw launcher exe name (`upc.exe`), and it never repeats
/// the game name the results view already shows.
fn window_title(has_artifact: bool, analysis_offer: bool, state: State) -> String {
    let activity = if has_artifact {
        "Results"
    } else if analysis_offer {
        "Clean run"
    } else {
        match state {
            State::Starting => "Starting",
            State::Running => "Running",
            State::Recording => "Recording",
            State::Stopping => "Stopping",
            State::Saved => "Saved",
            State::Done => "Finished",
            State::Error => "Error",
        }
    };
    // Lowercase "beisl" always — the wordmark is never title-cased (it's a label, not prose).
    format!("beisl ∷ {activity}")
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
        let artifact = load_artifact(&dir);
        let raw_present = dir.join("perf.data").is_file();
        let title = window_title(artifact.is_some(), session.analysis_offer, session.state);
        let mut core = core;
        core.set_header_title(title.clone());
        (
            Self {
                core,
                dir,
                session,
                have,
                config: Config::load(),
                enabling: None,
                enable_note: None,
                artifact,
                raw_present,
                analyzing: None,
                analyze_note: None,
                confirm_discard: false,
                title,
                title_pushed: false,
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
                // Push the title once the window exists, and refresh it as the activity
                // changes (live state → the ask → results). The xdg title (taskbar/overview)
                // can only be set once the window is up, so it happens here, not in `init`.
                let title = window_title(
                    self.artifact.is_some(),
                    self.session.analysis_offer,
                    self.session.state,
                );
                if title != self.title || !self.title_pushed {
                    self.title = title.clone();
                    self.title_pushed = true;
                    self.set_header_title(title.clone());
                    let _ = self.set_window_title(title);
                }
                // A crash auto-writes artifact.json before the window even opens; poll for it
                // (and for one Analyze produces) until we have it. Cheap: one stat+read of a
                // small file, and only until it's loaded.
                if self.artifact.is_none()
                    && (self.session.analysis_offer
                        || self.is_finished()
                        || self.session.state == State::Saved)
                {
                    self.artifact = load_artifact(&self.dir);
                    self.raw_present = self.dir.join("perf.data").is_file();
                }
                self.poll_enable();
                self.poll_analyze();
            }
            Msg::EnableDumpCapture => {
                if self.enabling.is_none() {
                    let cmd = fix_command(
                        &Fix::InstallDumpAccessRule,
                        detect_distro(),
                        detect_system_kind(),
                    );
                    if !cmd.is_empty() {
                        self.enable_note = Some("Enabling capture…".to_string());
                        self.enabling = Some(spawn_enable(cmd));
                    }
                }
            }
            Msg::Analyze => {
                if self.analyzing.is_none() && self.artifact.is_none() {
                    self.analyze_note = Some("Analyzing the trace…".to_string());
                    self.analyzing = Some(spawn_analyze(
                        self.dir.clone(),
                        self.session.game.clone(),
                        self.session.elapsed_secs as f64,
                    ));
                }
            }
            Msg::Discard => self.confirm_discard = true,
            Msg::CancelDiscard => self.confirm_discard = false,
            Msg::ConfirmDiscard => {
                // The user chose to throw the trace away; remove the run dir, then close
                // (there's nothing left for this window to show). Through delete_run, the
                // one deletion boundary: anything that isn't a run-* dir under the traces
                // root (say, a dev dir handed to --session) is refused, not deleted.
                let _ = session::library::delete_run(&session::library::default_root(), &self.dir);
                std::process::exit(0);
            }
            Msg::Stop => {
                if let Some(mut s) = Session::read(&self.dir) {
                    s.stop_requested = true;
                    let _ = s.write(&self.dir);
                    self.session = s;
                }
            }
            Msg::OpenFolder => {
                // Quiet the child: KDE's kioclient chats on stderr and reads like an error.
                let _ = std::process::Command::new("xdg-open")
                    .arg(&self.dir)
                    .stderr(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .spawn();
            }
            Msg::Close => std::process::exit(0),
        }
        Task::none()
    }

    fn subscription(&self) -> Subscription<Self::Message> {
        cosmic::iced::time::every(Duration::from_millis(400)).map(|_| Msg::Tick)
    }

    fn view(&self) -> Element<'_, Self::Message> {
        // Pick the surface: a resolved trace shows results; a kept-but-un-analyzed clean run
        // shows the ask; anything else is the live view.
        let (content, buttons) = if let Some(a) = &self.artifact {
            (self.results_content(a), self.results_buttons())
        } else if self.session.analysis_offer || self.stale_unanalyzed() {
            // The second arm is the trace library's analyze-on-demand: an old run
            // whose ask was never answered (or whose analysis never happened) still
            // deserves the same Analyze/Discard choice, as long as perf.data exists.
            (self.ask_content(), self.ask_buttons())
        } else {
            (self.live_content(), self.live_buttons())
        };

        // Content scrolls; the buttons stay pinned and visible at the bottom. A little right
        // padding inside the scroll region keeps full-width charts off the scrollbar. The
        // product name lives in the header/title bar (set via `set_header_title`), so the
        // content doesn't repeat it.
        let page = widget::Column::new()
            .spacing(10)
            .push(
                widget::scrollable(
                    widget::container(content).padding(cosmic::iced::Padding::from([0, 14, 0, 0])),
                )
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

impl SessionApp {
    fn is_finished(&self) -> bool {
        matches!(self.session.state, State::Done | State::Error)
    }

    /// A finished run that was never analyzed but still has its raw trace: the
    /// library's analyze-on-demand case (the wrapper only sets `analysis_offer` on a
    /// kept clean exit; runs whose analysis failed or never ran end up here). A
    /// "Stop & save" mid-game counts too — the recorder is stopped, so the trace is
    /// as analyzable as it will ever be; the ask just keeps saying the game runs.
    fn stale_unanalyzed(&self) -> bool {
        let settled = self.is_finished() || self.session.state == State::Saved;
        settled && self.artifact.is_none() && self.raw_present
    }

    /// Poll the background dump-access install (mirrors the setup app's worker pattern).
    fn poll_enable(&mut self) {
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
                    // Persist intent + tell the user; the wrapper's watcher will capture the
                    // still-pending dump on its next poll.
                    self.config.dump_access_policy = DumpAccessPolicy::RuleInstalled;
                    let _ = self.config.save();
                    self.enable_note =
                        Some("Capture enabled — saving this crash's dump now.".to_string());
                }
            }
        }
    }

    /// Poll the background Analyze; on success the artifact is now on disk, so load it and
    /// flip to the results view.
    fn poll_analyze(&mut self) {
        if let Some(w) = &self.analyzing {
            let (finished, error) = {
                let a = w.lock().unwrap();
                (a.finished, a.error.clone())
            };
            if finished {
                self.analyzing = None;
                match error {
                    Some(err) => self.analyze_note = Some(format!("Couldn't analyze: {err}")),
                    None => {
                        self.artifact = load_artifact(&self.dir);
                        self.analyze_note = None;
                    }
                }
            }
        }
    }

    // -- Surface 1: live -----------------------------------------------------------------

    fn live_content(&self) -> Element<'_, Msg> {
        let s = &self.session;
        let badge = match s.state {
            State::Starting => "Starting…",
            State::Running => "Game running",
            State::Recording => "● Recording",
            State::Stopping => "Stopping…",
            State::Saved => "Saved — game still running",
            State::Done => "Finished",
            State::Error => "Error",
        };

        let title = if s.game.is_empty() {
            "Beisl".to_string()
        } else {
            s.game.clone()
        };
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

        // The wrapper stamps the pinned GPU's label when a pin is configured, so an A/B
        // viewer never has to guess which GPU a trace ran on.
        if let Some(name) = &s.gpu_pin {
            col = col.push(widget::text::caption(format!("GPU: {name} (pinned)")));
        }

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
        if s.dump_capture_offer && self.config.dump_access_policy == DumpAccessPolicy::WarnOnly {
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
        col.into()
    }

    fn live_buttons(&self) -> Element<'_, Msg> {
        // "Stop & save" only exists while a capture is actually running, so show it only then
        // rather than as a permanently-disabled control.
        let mut buttons = widget::Row::new()
            .spacing(8)
            .push(widget::button::text("Open folder").on_press(Msg::OpenFolder))
            .push(widget::Space::new().width(Length::Fill));
        if self.session.state == State::Recording {
            buttons = buttons.push(widget::button::standard("Stop & save").on_press(Msg::Stop));
        }
        buttons
            .push(widget::button::suggested("Close").on_press(Msg::Close))
            .into()
    }

    // -- Surface 2: the ask --------------------------------------------------------------

    fn ask_content(&self) -> Element<'_, Msg> {
        let title = if self.session.game.is_empty() {
            "Clean run".to_string()
        } else {
            self.session.game.clone()
        };
        let mut col = widget::Column::new()
            .spacing(10)
            .push(widget::text::title3(title))
            .push(widget::text::body(
                match (self.session.analysis_offer, self.session.state) {
                    (true, _) => "Clean run — not analyzed yet",
                    (false, State::Saved) => "Trace saved — the game is still running",
                    _ => "Finished run — not analyzed yet",
                },
            ))
            .push(widget::text::caption(
                "The trace was kept as-is. Analyze it to see which layer of the stack ate the \
                 frame and where it hitched, or discard it to reclaim the disk.",
            ))
            .push(widget::text::caption(format!(
                "Trace: {}",
                human_bytes(self.session.trace_bytes)
            )));

        // A "Stop & save" keeps the session live behind this ask — analysis is safe
        // (the recorder is stopped), but hang warnings must not disappear with the
        // live view, so they ride along here.
        if self.session.state == State::Saved && !self.session.warnings.is_empty() {
            for w in &self.session.warnings {
                col = col.push(widget::text::caption(w.clone()));
            }
        }

        if self.analyzing.is_some() {
            col = col.push(widget::text::body(
                "Analyzing the trace… (this takes a few seconds)",
            ));
        }
        if let Some(note) = &self.analyze_note {
            col = col.push(widget::text::caption(note.clone()));
        }
        if self.confirm_discard {
            col = col.push(widget::text::body(
                "Discard this trace? This permanently deletes the run directory.",
            ));
        }
        col.into()
    }

    fn ask_buttons(&self) -> Element<'_, Msg> {
        // The confirm step replaces the normal buttons so the destructive action is a
        // deliberate second click, never a stray one.
        if self.confirm_discard {
            return widget::Row::new()
                .spacing(8)
                .push(widget::button::standard("Cancel").on_press(Msg::CancelDiscard))
                .push(widget::Space::new().width(Length::Fill))
                .push(widget::button::destructive("Delete trace").on_press(Msg::ConfirmDiscard))
                .into();
        }

        let busy = self.analyzing.is_some();
        let mut analyze = widget::button::suggested("Analyze");
        let mut discard = widget::button::destructive("Discard");
        if !busy {
            analyze = analyze.on_press(Msg::Analyze);
            discard = discard.on_press(Msg::Discard);
        }
        widget::Row::new()
            .spacing(8)
            .push(widget::button::text("Open folder").on_press(Msg::OpenFolder))
            .push(widget::Space::new().width(Length::Fill))
            .push(discard)
            .push(analyze)
            .into()
    }

    // -- Surface 3: the results view -----------------------------------------------------

    fn results_content(&self, a: &TraceArtifact) -> Element<'_, Msg> {
        let mut col = widget::Column::new().spacing(10);

        // A trace from a newer schema than we understand: say so plainly rather than render a
        // half-read view. (Older/defaulted fields still load — that's the serde-default path.)
        if a.schema_version > SCHEMA_VERSION {
            col = col.push(widget::text::body(format!(
                "This trace was written by a newer Beisl (schema {} > {}). Update to read it \
                 fully; showing what this version understands.",
                a.schema_version, SCHEMA_VERSION
            )));
        }

        // 1. Header — game · wine · tier, with the Exports caveat inline.
        col = col
            .push(widget::text::title3(a.game.clone()))
            .push(widget::text::caption(format!(
                "wine: {}   ·   fidelity: {}   ·   {:.0}s",
                a.wine_flavor, a.diagnostics.fidelity, a.duration_s
            )));
        if a.diagnostics.fidelity == "Exports" {
            col = col.push(widget::text::caption(
                "Exports tier — PE symbols are nearest-export: A/B-comparable, not full.",
            ));
        }

        // 2. Frametime health + spikes.
        col = col.push(widget::text::title4("Frametimes"));
        if let Some(f) = &a.frame_stats {
            col = col.push(widget::text::body(format!(
                "avg {:.0} fps   ·   1% low {:.1} ms   ·   worst {:.1} ms",
                f.avg_fps, f.p99_frametime_ms, f.worst_frametime_ms
            )));
            // The timeline chart (plotters, drawn on Canvas) with the worst spikes marked.
            if !a.timeline.is_empty() {
                let points = a.timeline.iter().map(|&(t, ms)| (t, ms as f64)).collect();
                let spikes = a
                    .spikes
                    .iter()
                    .map(|s| (s.t_seconds, s.frametime_ms as f64))
                    .collect();
                col = col.push(crate::chart::FrametimeChart::new(points, spikes).view::<Msg>());
            }
            if !a.spikes.is_empty() {
                col = col.push(widget::text::caption("Worst hitches:"));
                for s in &a.spikes {
                    col = col.push(widget::text::caption(format!(
                        "   {:.0} ms at {:.1}s",
                        s.frametime_ms, s.t_seconds
                    )));
                }
            }
        } else {
            col = col.push(widget::text::caption(
                "No MangoHud frametime log for this run.",
            ));
        }

        // 3. The culprit — the layer split (who ate the frame), normalized over attributed
        //    samples, each layer with its own symbol coverage.
        col = col.push(widget::text::title4("Who ate the frame"));
        if a.layers.is_empty() {
            col = col.push(widget::text::caption("No CPU samples to attribute."));
        } else {
            col = col.push(widget::text::caption(format!(
                "CPU by layer — of {:.0}% of samples we could place:",
                a.attributed_pct
            )));
            // The colored stacked bar (GPU driver segment tinted by vendor), each segment
            // with a hover card, then the precise per-layer legend below it.
            let segments = a
                .layers
                .iter()
                .map(|l| crate::chart::LayerSegment {
                    label: if l.layer == "driver" && !a.gpu_vendor.is_empty() {
                        format!("driver · {}", a.gpu_vendor)
                    } else {
                        l.layer.clone()
                    },
                    share: l.pct,
                    named_pct: l.symbol_pct,
                    color: crate::chart::layer_color(&l.layer, &a.gpu_vendor),
                })
                .collect();
            col = col.push(crate::chart::layer_bar::<Msg>(segments));
            // Legend: a color swatch matching the bar segment, then the precise numbers.
            for l in &a.layers {
                let row = widget::Row::new()
                    .spacing(8)
                    .align_y(cosmic::iced::Alignment::Center)
                    .push(crate::chart::swatch::<Msg>(crate::chart::layer_color(
                        &l.layer,
                        &a.gpu_vendor,
                    )))
                    .push(widget::text::monotext(format!(
                        "{:<11} {:4.1}%   {:>3.0}% named",
                        l.layer, l.pct, l.symbol_pct
                    )));
                col = col.push(row);
            }
            let gap = 100.0 - a.attributed_pct;
            if gap >= 0.5 {
                col = col.push(widget::text::caption(format!(
                    "{gap:.0}% of samples were unmapped — could belong to any layer, \
                     graphics/driver included.",
                )));
            }
        }

        // Top hot functions within the layers (the ones we could name). Same header level and
        // monospaced, column-aligned rows as Issues below, so the two read as one family.
        if !a.hot_symbols.is_empty() {
            col = col.push(widget::text::title4("Hottest functions"));
            col = col.push(widget::text::caption(
                "Self time across all layers — the functions we could name.",
            ));
            for h in a.hot_symbols.iter().take(8) {
                col = col.push(widget::text::monotext(format!(
                    "{:>5.1}%  {:<26}  {}",
                    h.pct, h.module, h.symbol
                )));
            }
        }

        // 4. Issues — anonymous hot modules tied to their fix. The same signal the preflight
        //    debuginfo checks predict, now confirmed against the real trace.
        if !a.diagnostics.anonymous_modules.is_empty() {
            col = col.push(widget::text::title4("Issues"));
            col = col.push(widget::text::caption(
                "Hot modules we couldn't see inside — install the fix to name them:",
            ));
            for (module, pct) in &a.diagnostics.anonymous_modules {
                let hint = if module.to_ascii_lowercase().ends_with(".dll") {
                    "needs the Full tier (retained-DWARF wine build)"
                } else {
                    "install its debuginfo package"
                };
                col = col.push(widget::text::monotext(format!(
                    "{pct:>5.1}%  {module:<26}  {hint}"
                )));
            }
        }

        if self.confirm_discard {
            col = col.push(widget::text::body(
                "Delete this trace? This permanently deletes the run directory.",
            ));
        }

        col.into()
    }

    fn results_buttons(&self) -> Element<'_, Msg> {
        // The confirm pair replaces the normal buttons so the destructive action is a
        // deliberate second click — the same idiom as the ask and the library rows.
        // ConfirmDiscard goes through delete_run (the one deletion boundary) and closes:
        // a judged-worthless trace leaves through the same door everywhere.
        if self.confirm_discard {
            // The pair stays on the left, where the Delete that armed it sits — the
            // confirming click lands under the pointer instead of across the window
            // (the ask's confirm is right-aligned for the same reason: its trigger is).
            return widget::Row::new()
                .spacing(8)
                .push(widget::button::standard("Cancel").on_press(Msg::CancelDiscard))
                .push(widget::button::destructive("Delete trace").on_press(Msg::ConfirmDiscard))
                .push(widget::Space::new().width(Length::Fill))
                .into();
        }
        widget::Row::new()
            .spacing(8)
            .push(widget::button::text("Open folder").on_press(Msg::OpenFolder))
            .push(widget::button::destructive("Delete").on_press(Msg::Discard))
            .push(widget::Space::new().width(Length::Fill))
            .push(widget::button::suggested("Close").on_press(Msg::Close))
            .into()
    }
}

/// Read `artifact.json` from a trace dir, or `None` if it's missing/unparseable. A schema
/// too new to fully understand still loads (unknown fields are ignored, missing ones
/// default) — the results view flags that case rather than refusing to render.
fn load_artifact(dir: &Path) -> Option<TraceArtifact> {
    TraceArtifact::read_json(&dir.join(session::library::ARTIFACT_FILE)).ok()
}

/// Run `TraceArtifact::build` (resolve `perf.data` + fold in the CSV) on a background
/// thread, writing `artifact.json` on success. The UI polls the returned handle each Tick.
fn spawn_analyze(dir: PathBuf, game: String, duration_s: f64) -> Arc<Mutex<Analyze>> {
    let state = Arc::new(Mutex::new(Analyze::default()));
    let s2 = state.clone();
    std::thread::spawn(move || {
        let result = TraceArtifact::build(&dir, &game, duration_s)
            .and_then(|a| a.write_json(&dir.join(session::library::ARTIFACT_FILE)));
        let mut a = s2.lock().unwrap();
        if let Err(e) = result {
            a.error = Some(e.to_string());
        }
        a.finished = true;
    });
    state
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
