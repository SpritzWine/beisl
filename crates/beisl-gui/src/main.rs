//! `beisl-setup` — the libcosmic setup page for Beisl.
//!
//! A read-only preflight checklist over the UI-agnostic `preflight` crate: it shows what
//! Beisl needs, ticks each item as it's satisfied, and runs the privileged fixes through
//! `pkexec` on a background thread (so the polkit prompt and the event loop don't block
//! each other). The UI polls the worker on a timer — the same pattern the SpritzWine
//! installer uses, avoiding the version-volatile parts of libcosmic's async API.
//!
//! When launched from an *extracted dist tree* (a `beisl` binary next to this one that is
//! not the installed copy), an install page comes first: a tiny fork of the SpritzWine
//! installer wizard, collapsed to one page over `beisl install` — so the standalone
//! tarball has the same double-click install/update story as the SpritzWine bundle.
//! Pass `--payload <dir>` to point at a tree explicitly (e.g. from a dev checkout).
//!
//! A rotating tip panel turns the setup wait into onboarding (shortcut, where traces land,
//! that it works with any wine). Mirrors the installer's widget idioms for the pinned rev.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use cosmic::app::{ApplicationExt, Core, Settings, Task};
use cosmic::iced::{Length, Subscription};
use cosmic::widget;
use cosmic::{Application, Element};

use preflight::gpu::GpuPinCandidate;
use preflight::{
    all_checks, detect_distro, detect_record_mode, detect_system_kind, disk_check,
    driver_debuginfo_check, fix_command, gpu_dump_check, locked_system_message,
    mangohud_hotkey_check, multi_gpu_check, ostree_install_note, perf_debuginfod_check,
    perf_record_fix, steam_visible_beisl_check, Check, CheckId, Config, Distro, DumpAccessPolicy,
    Fix, GpuPin, RecordMode, Status, SystemKind, STEAM_PATH_ISSUE_URL,
};

mod chart; // frametime timeline (plotters, drawn on cosmic's Canvas)
mod traces; // the trace library page (its own Elm-style component)
mod window; // the live session window (`--session`)

/// Onboarding tips, cycled while the user waits.
const TIPS: &[&str] = &[
    "Frametime logging runs on its own for the whole session — the MangoHud hotkey \
     (default Shift+F2) pauses and resumes it.",
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
                    // pkexec fixes report on stderr, `beisl install` on stdout — keep both.
                    for stream in [&out.stdout, &out.stderr] {
                        for line in String::from_utf8_lossy(stream).lines() {
                            s.log.push(line.to_string());
                        }
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

/// Which page is showing. `Install` only exists when launched from an extracted dist
/// tree; it hands over to the checklist once the bundle is installed (or skipped).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Page {
    Install,
    Checks,
    /// The trace library: past runs on disk. Each run opens in the session window it
    /// already had while recording (`beisl-setup --session <dir>`) — the ask, the
    /// results view, and live/error states all come for free by reusing that window
    /// instead of re-embedding its surfaces here (DESIGN.md: "reusing that same view").
    Traces,
}

#[derive(Debug, Clone)]
enum Message {
    /// Install page: run `<payload>/beisl install` on the worker.
    InstallBundle,
    /// Install page: move on to the checklist (skip, or continue after installing).
    ShowChecks,
    Recheck,
    FixAll,
    /// The optional GPU dump-access row's OWN button — a standing privilege change, always
    /// its own explicit click, never folded into `FixAll`.
    EnableDumpCapture,
    /// The Steam-launcher row's OWN button — writes a shim into /usr/bin (pkexec), so
    /// like the dump row it's always its own explicit opt-in, never folded into `FixAll`.
    InstallSteamLauncher,
    /// Open the upstream Valve bug behind the Steam-launcher row in the browser.
    OpenSteamPathIssue,
    /// Advanced: remove the installed rule.
    RemoveDumpRule,
    /// Advanced: pick the persisted policy (intent only — no privilege runs here).
    SetPolicy(DumpAccessPolicy),
    /// Advanced: pick (or clear) the GPU pin. `None` = "Don't pin"; `Some(i)` indexes
    /// `AppModel::gpu_candidates`. Always a plain config write — no privilege anywhere.
    SetGpuPin(Option<usize>),
    /// Advanced: on/off for on-demand debuginfod fetch of GPU-driver symbols. Default on;
    /// a plain config write, same as the GPU pin — nothing privileged to run either way.
    SetDebuginfodFetch(bool),
    ToggleAdvanced,
    /// Show the trace library (rescans on every entry — cheap, and a run finished
    /// since the last look should simply be there).
    ShowTraces,
    /// Everything inside the library page (see `traces::Msg`); only `Back` is ours.
    Traces(traces::Msg),
    Tick,
    Close,
}

/// Startup flags: the extracted dist tree to offer installing, when there is one.
struct Flags {
    payload: Option<PathBuf>,
}

struct AppModel {
    core: Core,
    distro: Distro,
    /// Package-mutability of this system — picks the install verb (or the SteamOS redirect).
    system: SystemKind,
    page: Page,
    /// The dist tree the install page installs from (`Some` iff launched from one).
    payload: Option<PathBuf>,
    /// `~/.local/bin/beisl` already existed at startup — wording flips to "Update".
    had_install: bool,
    install_worker: Option<Arc<Mutex<Shared>>>,
    /// The install worker ran to completion (successfully or not) at least once.
    install_done: bool,
    trace_dir: PathBuf,
    checks: Vec<Check>,
    worker: Option<Arc<Mutex<Shared>>>,
    /// Separate worker for the opt-in dump-access install/remove, so it can never ride
    /// along with the blocking `FixAll` sweep. Carries the policy to persist on success.
    dump_worker: Option<(Arc<Mutex<Shared>>, DumpAccessPolicy)>,
    config: Config,
    /// Detected GPU pin candidates for the Advanced picker (recomputed alongside `checks`).
    gpu_candidates: Vec<GpuPinCandidate>,
    /// The trace library page (state + view + actions live in `traces.rs`).
    traces: traces::TracesTab,
    show_advanced: bool,
    log: Vec<String>,
    error: Option<String>,
    tick: u64,
    tip: usize,
}

fn home() -> PathBuf {
    PathBuf::from(std::env::var_os("HOME").unwrap_or_default())
}

/// Where `beisl install` puts the binaries. Also the discriminator for bundle detection:
/// the installed layout has `beisl` next to `beisl-setup` too, so *location* is what
/// tells an extracted tarball apart from the installed copy.
fn bin_dir() -> PathBuf {
    home().join(".local/bin")
}

/// The extracted dist tree this binary runs from, if any: the executable's directory when
/// it carries a `beisl` binary and is not `~/.local/bin` itself.
fn detect_bundle() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?.to_path_buf();
    if !dir.join("beisl").is_file() {
        return None;
    }
    match (dir.canonicalize(), bin_dir().canonicalize()) {
        (Ok(a), Ok(b)) if a == b => None,
        _ => Some(dir),
    }
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
    fn detect_all(trace_dir: &Path) -> Vec<Check> {
        let mut checks = all_checks();
        for c in checks.iter_mut() {
            c.detect();
        }
        checks.push(disk_check(trace_dir));
        // The optional, opt-in GPU dump-access row. Kept separate so it never joins the
        // blocking "set everything up" sweep (see `pending_fixes`).
        checks.push(gpu_dump_check());
        // The optional, informational multi-GPU/dual-ICD row — its remedy is a config pick
        // in Advanced, not a privileged fix, so it never joins the sweep either.
        checks.push(multi_gpu_check());
        // The optional, informational driver debug-symbols row — remedy is a manual,
        // distro-specific install named in the why (fix is None, so no sweep).
        checks.push(driver_debuginfo_check());
        // Advisory: the MangoHud #2053 hotkey crash — a workaround in the why, no fix.
        checks.push(mangohud_hotkey_check());
        // Advisory: can the local `perf` fetch debug symbols via debuginfod at all? Distinct
        // from `driver_debuginfo_check` (are the files reachable) — this asks whether `perf`
        // was even built with the support Beisl's on-demand fetch depends on.
        checks.push(perf_debuginfod_check());
        // Steam's containerized client scrubs the launch-option PATH — without the
        // /usr/bin launcher, `beisl %command%` (the tool's whole entry point) silently
        // fails. That makes this the most actionable row on the page, so it goes FIRST
        // (right under the record-tier ladder), not buried below the advisories.
        checks.insert(0, steam_visible_beisl_check());
        checks
    }

    fn running(&self) -> bool {
        self.worker.is_some() || self.dump_worker.is_some() || self.install_worker.is_some()
    }

    /// All *blocking* checks satisfied → the user can trace. (The optional GPU-dump row is
    /// non-blocking, so it never gates readiness.)
    fn ready(&self) -> bool {
        self.checks
            .iter()
            .filter(|c| c.blocking)
            .all(|c| c.status == Status::Ok)
    }

    /// The pkexec commands for every fixable check that still needs setup — EXCLUDING the
    /// GPU dump-access row (a standing privilege) and the Steam launcher shim (a write
    /// into /usr/bin): system-level changes only ever applied by their own buttons.
    fn pending_fixes(&self) -> Vec<Vec<String>> {
        self.checks
            .iter()
            .filter(|c| {
                c.status == Status::NeedsFix
                    && c.id != CheckId::GpuDumpAccess
                    && c.id != CheckId::SteamVisibleBeisl
            })
            .filter_map(|c| {
                c.fix
                    .as_ref()
                    .map(|f| fix_command(f, self.distro, self.system))
            })
            .filter(|cmd| !cmd.is_empty())
            .collect()
    }

    fn gpu_dump_status(&self) -> Status {
        self.checks
            .iter()
            .find(|c| c.id == CheckId::GpuDumpAccess)
            .map(|c| c.status.clone())
            .unwrap_or(Status::NeedsFix)
    }

    /// The currently configured pin's index into `gpu_candidates`, matched by name — `None`
    /// both when nothing is pinned and when a previously-pinned candidate is no longer
    /// detected (e.g. the driver was uninstalled since).
    fn selected_gpu_pin(&self) -> Option<usize> {
        let pin = self.config.gpu_pin.as_ref()?;
        self.gpu_candidates.iter().position(|c| c.name == pin.name)
    }

    /// Start the dump-access install (or remove) on the dedicated worker, persisting the
    /// given policy once it succeeds. One polkit prompt; guarded by `running()`.
    fn spawn_dump(&mut self, fix: Fix, becomes: DumpAccessPolicy) {
        if self.running() {
            return;
        }
        let cmd = fix_command(&fix, self.distro, self.system);
        if cmd.is_empty() {
            return;
        }
        self.log.clear();
        self.error = None;
        // Reflect "working…" on the row.
        for c in self.checks.iter_mut() {
            if c.id == CheckId::GpuDumpAccess {
                c.status = Status::Fixing;
            }
        }
        self.dump_worker = Some((spawn_fixer(vec![cmd]), becomes));
    }
}

impl Application for AppModel {
    type Executor = cosmic::executor::Default;
    type Flags = Flags;
    type Message = Message;

    const APP_ID: &'static str = "org.spritzwine.Beisl";

    fn core(&self) -> &Core {
        &self.core
    }

    fn core_mut(&mut self) -> &mut Core {
        &mut self.core
    }

    fn init(core: Core, flags: Self::Flags) -> (Self, Task<Self::Message>) {
        let trace_dir = trace_dir();
        let checks = Self::detect_all(&trace_dir);
        let mut core = core;
        core.set_header_title("beisl setup".to_string());
        let app = AppModel {
            core,
            distro: detect_distro(),
            system: detect_system_kind(),
            page: if flags.payload.is_some() {
                Page::Install
            } else {
                Page::Checks
            },
            had_install: bin_dir().join("beisl").is_file(),
            payload: flags.payload,
            install_worker: None,
            install_done: false,
            trace_dir,
            checks,
            worker: None,
            dump_worker: None,
            config: Config::load(),
            gpu_candidates: preflight::gpu::detect_candidates(),
            traces: traces::TracesTab::default(),
            show_advanced: false,
            log: Vec::new(),
            error: None,
            tick: 0,
            tip: 0,
        };
        (app, Task::none())
    }

    fn update(&mut self, message: Self::Message) -> Task<Self::Message> {
        match message {
            Message::InstallBundle => {
                if !self.running() {
                    if let Some(p) = &self.payload {
                        self.log.clear();
                        self.error = None;
                        self.install_done = false;
                        self.install_worker = Some(spawn_fixer(vec![vec![
                            p.join("beisl").display().to_string(),
                            "install".into(),
                        ]]));
                    }
                }
            }
            Message::ShowChecks => {
                if !self.running() {
                    self.page = Page::Checks;
                    self.log.clear();
                    self.error = None;
                    // Fresh detection — the install just changed what's on PATH.
                    self.checks = Self::detect_all(&self.trace_dir);
                    self.gpu_candidates = preflight::gpu::detect_candidates();
                }
            }
            Message::Recheck => {
                if !self.running() {
                    self.checks = Self::detect_all(&self.trace_dir);
                    self.gpu_candidates = preflight::gpu::detect_candidates();
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
            Message::EnableDumpCapture => {
                self.spawn_dump(Fix::InstallDumpAccessRule, DumpAccessPolicy::RuleInstalled);
            }
            Message::OpenSteamPathIssue => {
                // Fire-and-forget: the portal/desktop handles the browser; a failure just
                // means no window opens — nothing beisl should abort or prompt over.
                let _ = std::process::Command::new("xdg-open")
                    .arg(STEAM_PATH_ISSUE_URL)
                    .stderr(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .spawn();
            }
            Message::InstallSteamLauncher => {
                // One explicit click, one pkexec prompt; nothing to persist — success is
                // simply /usr/bin/beisl existing, which the post-worker re-detect sees.
                if !self.running() {
                    let cmd = fix_command(&Fix::InstallSteamShim, self.distro, self.system);
                    if !cmd.is_empty() {
                        for c in self.checks.iter_mut() {
                            if c.id == CheckId::SteamVisibleBeisl {
                                c.status = Status::Fixing;
                            }
                        }
                        self.log.clear();
                        self.error = None;
                        self.worker = Some(spawn_fixer(vec![cmd]));
                    }
                }
            }
            Message::RemoveDumpRule => {
                self.spawn_dump(Fix::RemoveDumpAccessRule, DumpAccessPolicy::WarnOnly);
            }
            Message::SetPolicy(p) => {
                // Intent only — persist the preference; the privileged install/remove stays
                // behind the explicit Enable / Remove buttons.
                if !self.running() {
                    self.config.dump_access_policy = p;
                    let _ = self.config.save();
                }
            }
            Message::SetGpuPin(sel) => {
                // Plain config write — no privilege anywhere in this path.
                if !self.running() {
                    self.config.gpu_pin =
                        sel.and_then(|i| self.gpu_candidates.get(i))
                            .map(|c| GpuPin {
                                name: c.name.clone(),
                                icd_paths: c.icd_paths.clone(),
                                device_select: c.device_select.clone(),
                            });
                    let _ = self.config.save();
                }
            }
            Message::SetDebuginfodFetch(on) => {
                // Plain config write — no privilege anywhere in this path.
                if !self.running() {
                    self.config.debuginfod_fetch = on;
                    let _ = self.config.save();
                }
            }
            Message::ToggleAdvanced => self.show_advanced = !self.show_advanced,
            Message::ShowTraces => {
                self.traces.refresh();
                self.page = Page::Traces;
            }
            // Navigation is the parent's business; the rest is the page's own. Back
            // routes through ShowChecks so leaving the library behaves exactly like
            // every other way of reaching the checklist (clears stale error/log,
            // re-detects — and stays blocked while a fixer runs).
            Message::Traces(traces::Msg::Back) => return self.update(Message::ShowChecks),
            Message::Traces(m) => self.traces.update(m),
            Message::Tick => {
                self.tick += 1;
                // Set the taskbar/overview title once the window exists (not possible in init).
                if self.tick == 1 {
                    let _ = self.set_window_title("beisl setup".to_string());
                }
                if self.tick.is_multiple_of(24) {
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
                if let Some(w) = &self.install_worker {
                    let (log, finished, error) = {
                        let s = w.lock().unwrap();
                        (s.log.clone(), s.finished, s.error.clone())
                    };
                    self.log = log;
                    if finished {
                        self.install_worker = None;
                        self.install_done = true;
                        self.error = error;
                    }
                }
                if let Some((w, becomes)) = self.dump_worker.clone() {
                    let (log, finished, error) = {
                        let s = w.lock().unwrap();
                        (s.log.clone(), s.finished, s.error.clone())
                    };
                    self.log = log;
                    if finished {
                        self.dump_worker = None;
                        self.error = error.clone();
                        // Persist the new policy only if the privileged step actually worked.
                        if error.is_none() {
                            self.config.dump_access_policy = becomes;
                            let _ = self.config.save();
                        }
                        self.checks = Self::detect_all(&self.trace_dir);
                    }
                }
            }
            Message::Close => std::process::exit(0),
        }
        Task::none()
    }

    fn subscription(&self) -> Subscription<Self::Message> {
        // Tick for the worker log and to rotate tips, plus Ctrl-tracking for the trace
        // library's Ctrl+click row selection (see `traces::Msg::RowPressed`) — the
        // library page has no keyboard focus of its own, so the modifier state has to
        // come from a window-level subscription instead.
        Subscription::batch([
            cosmic::iced::time::every(Duration::from_millis(120)).map(|_| Message::Tick),
            cosmic::iced::event::listen_with(|event, _status, _window| match event {
                cosmic::iced::Event::Keyboard(cosmic::iced::keyboard::Event::ModifiersChanged(
                    modifiers,
                )) => Some(Message::Traces(traces::Msg::Modifiers(modifiers.control()))),
                _ => None,
            }),
        ])
    }

    fn view(&self) -> Element<'_, Self::Message> {
        let page = match self.page {
            Page::Install => self.install_page(),
            Page::Checks => self.checks_page(),
            Page::Traces => self.traces.view().map(Message::Traces),
        };
        widget::container(page)
            .padding(28)
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
    }
}

impl AppModel {
    fn checks_page(&self) -> Element<'_, Message> {
        // A locked immutable system (SteamOS) can't get a host `perf` — show the redirect
        // instead of a checklist that can't pass, matching `beisl --check`.
        if self.system == SystemKind::Locked {
            return widget::container(
                widget::Column::new()
                    .spacing(14)
                    .push(widget::text::title1("beisl can't run here"))
                    .push(widget::text::body(locked_system_message())),
            )
            .padding(24)
            .into();
        }

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

        // ostree systems layer fixes with rpm-ostree — they apply after a reboot.
        if self.system == SystemKind::OstreeAtomic {
            content = content.push(widget::text::caption(ostree_install_note()));
        }

        // Perf record tier — full/degraded/none with the current one marked, mirroring
        // `beisl --check`. Declining CAP_PERFMON isn't "broken", it's the degraded tier, so
        // this ladder replaces the old binary "perf can record" row (skipped in the loop
        // below). Computed here rather than stored: detection is cheap and always current
        // (a granted/revoked capability shows immediately on the next render).
        let mode = detect_record_mode();
        let mut tier = widget::Column::new().spacing(4).push(widget::text::body(
            "Perf record tier — what a trace captures here:",
        ));
        for t in [RecordMode::Full, RecordMode::Degraded, RecordMode::None] {
            tier = tier.push(
                widget::Row::new()
                    .spacing(10)
                    .push(widget::checkbox(t == mode).label(t.as_str()))
                    .push(widget::text::caption(t.describe())),
            );
        }
        // Below Full: show the grant-CAP_PERFMON command (described, run via the fix flow).
        if mode != RecordMode::Full {
            let cmd = fix_command(&perf_record_fix(), self.distro, self.system);
            if !cmd.is_empty() {
                tier = tier.push(widget::text::caption(format!(
                    "\u{2192} for full: {}",
                    cmd.join(" ")
                )));
            }
        }
        content = content.push(tier);

        for c in &self.checks {
            // The perf-record capability is shown as the tier ladder above, not a binary row.
            if c.id == CheckId::PerfParanoid {
                continue;
            }
            let is_ok = c.status == Status::Ok;
            // Read-only checkbox (no on_toggle) = the status indicator.
            let mut row = widget::Row::new()
                .spacing(12)
                .push(widget::checkbox(is_ok).label(c.label))
                .push(widget::Space::new().width(Length::Fill));

            // The optional GPU dump-access row carries its OWN Enable button — a standing
            // privilege change, deliberately kept out of "Set everything up".
            if c.id == CheckId::GpuDumpAccess && !is_ok {
                let mut btn = widget::button::standard("Enable");
                if !self.running() {
                    btn = btn.on_press(Message::EnableDumpCapture);
                }
                row = row.push(btn);
            } else if c.id == CheckId::SteamVisibleBeisl && !is_ok && c.fix.is_some() {
                // Same opt-in stance for the /usr/bin launcher shim (no fix on immutable
                // systems — the why carries the full-path fallback, so no button either).
                let mut btn = widget::button::standard("Install launcher");
                if !self.running() {
                    btn = btn.on_press(Message::InstallSteamLauncher);
                }
                row = row.push(btn);
            } else {
                row = row.push(widget::text::caption(status_word(c)));
            }
            content = content
                .push(row)
                .push(widget::text::caption(c.why.clone().into_owned()));
            // The why cites Valve's bug number — make it a real link, not a dead end.
            if c.id == CheckId::SteamVisibleBeisl && !is_ok {
                content = content.push(
                    widget::button::link("steam-for-linux #13389 \u{2197}")
                        .on_press(Message::OpenSteamPathIssue),
                );
            }
        }

        // Informational only — the decision lives in Advanced. A poweruser who turned it
        // off still gets an honest read here; everyone else just sees the good default
        // is on, with nothing to decide.
        content = content.push(self.debuginfod_indicator());

        // Advanced (opt-in details): collapsed by default. No expander widget at this
        // libcosmic rev, so a plain toggle-shown section (the brief allows this).
        content = content.push(self.advanced_section());

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
        // and always visible at the bottom. Right padding inside the scroll region keeps
        // the rows — and especially their trailing Enable/Install buttons — clear of the
        // scrollbar (wider than the session window's 14: this page puts interactive
        // targets at the right edge, and a button under a scrollbar feels cramped).
        widget::Column::new()
            .spacing(14)
            .push(
                widget::scrollable(
                    widget::container(content).padding(cosmic::iced::Padding::from([0, 28, 0, 0])),
                )
                .width(Length::Fill)
                .height(Length::Fill),
            )
            .push(widget::container(widget::text::body(TIPS[self.tip])).padding(12))
            .push(self.footer())
            .into()
    }

    /// The install page — the SpritzWine installer wizard collapsed to one page: offer,
    /// streamed `beisl install` log, then hand over to the checklist. Only reachable when
    /// launched from an extracted dist tree (see `detect_bundle`).
    fn install_page(&self) -> Element<'_, Message> {
        let verb = if self.had_install {
            "Update"
        } else {
            "Install"
        };
        let mut col = widget::Column::new()
            .spacing(14)
            .push(widget::text::title1(format!("{verb} Beisl")))
            .push(widget::text::body(
                "Copies beisl and this setup app into ~/.local/bin and adds \"Beisl Setup\" \
                 to your app menu, so `beisl %command%` works in a game's launch options.",
            ));
        if let Some(p) = &self.payload {
            col = col.push(widget::text::caption(format!("Bundle: {}", p.display())));
        }
        if self.had_install && !self.install_done {
            col = col.push(widget::text::caption(
                "Replaces the copy already in ~/.local/bin.",
            ));
        }

        // The streamed `beisl install` output (its success message doubles as the
        // launch-option guidance), then the verdict line.
        if self.running() || !self.log.is_empty() {
            let mut logcol = widget::Column::new().spacing(2);
            for line in &self.log {
                logcol = logcol.push(widget::text::monotext(line.clone()));
            }
            col = col.push(logcol);
        }
        if let Some(e) = &self.error {
            col = col.push(widget::text::body(format!("Something went wrong: {e}")));
        }

        let done_ok = self.install_done && self.error.is_none();
        let mut buttons = widget::Row::new().spacing(8);
        if done_ok {
            buttons = buttons.push(widget::Space::new().width(Length::Fill)).push(
                widget::button::suggested("Check prerequisites").on_press(Message::ShowChecks),
            );
        } else {
            let mut skip = widget::button::standard("Skip");
            let mut install = widget::button::suggested(verb);
            if !self.running() {
                skip = skip.on_press(Message::ShowChecks);
                install = install.on_press(Message::InstallBundle);
            }
            buttons = buttons
                .push(skip)
                .push(widget::Space::new().width(Length::Fill))
                .push(install);
        }

        col.push(widget::Space::new().height(Length::Fill))
            .push(buttons)
            .into()
    }

    /// Main-screen indicator for on-demand debuginfod fetch (see `Config::debuginfod_fetch`,
    /// default on): the average user should see a reassurance, not a decision — the actual
    /// toggle lives in Advanced (`debuginfod_toggle`). Read-only checkbox, same idiom as the
    /// checklist rows above.
    fn debuginfod_indicator(&self) -> Element<'_, Message> {
        let label = if self.config.debuginfod_fetch {
            "Debug symbols: fetched on demand ✓"
        } else {
            "Debug symbols: fetched on demand — off (see Advanced)"
        };
        widget::Row::new()
            .spacing(12)
            .push(widget::checkbox(self.config.debuginfod_fetch).label(label))
            .into()
    }

    /// The collapsed "Advanced" block: a toggle header, and when open, the GPU dump-access
    /// policy radios + a "Remove rule" button. The radios record intent only; the actual
    /// privileged install/remove are always the explicit Enable / Remove buttons.
    fn advanced_section(&self) -> Element<'_, Message> {
        let arrow = if self.show_advanced { "▾" } else { "▸" };
        let header =
            widget::button::text(format!("{arrow} Advanced")).on_press(Message::ToggleAdvanced);

        let mut col = widget::Column::new().spacing(8).push(header);
        if !self.show_advanced {
            return col.into();
        }

        let policy = self.config.dump_access_policy;
        col = col
            .push(widget::text::body("GPU crash-dump capture"))
            .push(widget::text::caption(
                "When a game crashes your GPU, this decides whether Beisl can save the \
                 evidence itself or just points you at a manual command. The dumps only ever \
                 describe your own crashed games.",
            ))
            .push(widget::radio(
                widget::text::body("Warn only — no system change (default)"),
                DumpAccessPolicy::WarnOnly,
                Some(policy),
                Message::SetPolicy,
            ))
            .push(widget::radio(
                widget::text::body("Auto-capture — install the access rule (recommended)"),
                DumpAccessPolicy::RuleInstalled,
                Some(policy),
                Message::SetPolicy,
            ));

        // The privileged actions, each its own labelled button.
        let mut actions = widget::Row::new().spacing(8);
        if self.gpu_dump_status() != Status::Ok {
            let mut enable = widget::button::suggested("Enable auto-capture");
            if !self.running() {
                enable = enable.on_press(Message::EnableDumpCapture);
            }
            actions = actions.push(enable);
        }
        let mut remove = widget::button::destructive("Remove rule");
        if !self.running() {
            remove = remove.on_press(Message::RemoveDumpRule);
        }
        actions = actions.push(remove);
        col = col.push(actions);

        col = col.push(self.gpu_pin_picker());
        col.push(self.debuginfod_toggle()).into()
    }

    /// The debuginfod on-demand fetch toggle. On by default (see `preflight::Config`); the
    /// main screen only shows that it's active (see `checks_page`) — this is the one place
    /// a poweruser can turn it off for a fully offline/hermetic resolve.
    fn debuginfod_toggle(&self) -> Element<'_, Message> {
        widget::Column::new()
            .spacing(8)
            .push(widget::text::body("GPU-driver debug symbols"))
            .push(widget::text::caption(
                "Fetches driver debug symbols on demand during analysis via debuginfod — \
                 cached locally, no privilege, no manual setup. Turn off to keep symbol \
                 resolution fully offline.",
            ))
            .push(
                widget::toggler(self.config.debuginfod_fetch)
                    .label("Fetch on demand".to_string())
                    .on_toggle(Message::SetDebuginfodFetch),
            )
            .into()
    }

    /// The GPU pin picker: "Don't pin (default)" plus one radio per detected candidate —
    /// disambiguated driver installs included (see `preflight::gpu::build_candidates`).
    /// Selecting persists straight to config; there is no privileged step to run.
    fn gpu_pin_picker(&self) -> Element<'_, Message> {
        let mut col = widget::Column::new()
            .spacing(8)
            .push(widget::text::body("GPU selection"))
            .push(widget::text::caption(
                "Force which GPU a trace runs on, so an A/B comparison can't silently land on \
                 a different one. Sets VK_DRIVER_FILES (and MESA_VK_DEVICE_SELECT when we can \
                 resolve the exact card) for the traced game only.",
            ));

        let selected = self.selected_gpu_pin();
        col = col.push(widget::radio(
            widget::text::body("Don't pin (default)"),
            None::<usize>,
            Some(selected),
            Message::SetGpuPin,
        ));

        if self.gpu_candidates.is_empty() {
            col = col.push(widget::text::caption("No additional GPUs detected to pin."));
        } else {
            for (i, cand) in self.gpu_candidates.iter().enumerate() {
                col = col.push(widget::radio(
                    widget::text::body(cand.name.clone()),
                    Some(i),
                    Some(selected),
                    Message::SetGpuPin,
                ));
            }
        }

        col.into()
    }

    fn footer(&self) -> Element<'_, Message> {
        let mut row = widget::Row::new().spacing(8);

        if self.ready() {
            row = row
                .push(widget::button::text("Traces").on_press(Message::ShowTraces))
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
            .push(widget::button::text("Traces").on_press(Message::ShowTraces))
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
    // `beisl-setup --session <dir>` opens the live session window; otherwise the setup
    // page, with the install page in front when a dist tree is (or is pointed) at hand.
    let mut args = std::env::args().skip(1);
    let mut payload = None;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--version" => {
                // The workspace version, carried across the workspace split by preflight.
                println!("beisl-setup {}", preflight::VERSION);
                return Ok(());
            }
            "--help" | "-h" | "help" => {
                // Single source with the man page: docs/beisl-setup.1.md's top,
                // cut at the help-end marker, `# ` section markers stripped.
                let doc = include_str!("../../../docs/beisl-setup.1.md");
                let top = doc.split("<!-- help-end -->").next().unwrap_or(doc);
                let lines: Vec<&str> = top
                    .lines()
                    .map(|l| l.strip_prefix("# ").unwrap_or(l))
                    .collect();
                println!("{}", lines.join("\n").trim());
                return Ok(());
            }
            "--session" => {
                let dir = PathBuf::from(args.next().unwrap_or_default());
                // Golden-ratio landscape (≈1.618:1): the results view's monospaced function /
                // issue rows need the width, and the charts read better wide than tall.
                let settings = Settings::default().size(cosmic::iced::Size::new(880.0, 544.0));
                return cosmic::app::run::<window::SessionApp>(settings, dir);
            }
            "--payload" => {
                // Dev-only escape hatch (run from a checkout against a built dist tree),
                // so a bad value is a plain CLI error rather than an in-GUI state.
                let dir = PathBuf::from(args.next().unwrap_or_default());
                if !dir.join("beisl").is_file() {
                    eprintln!(
                        "beisl-setup: --payload {} has no beisl binary (expected an extracted dist tree)",
                        dir.display()
                    );
                    std::process::exit(2);
                }
                payload = Some(dir);
            }
            _ => {}
        }
    }
    let flags = Flags {
        payload: payload.or_else(detect_bundle),
    };
    let settings = Settings::default().size(cosmic::iced::Size::new(640.0, 640.0));
    cosmic::app::run::<AppModel>(settings, flags)
}
