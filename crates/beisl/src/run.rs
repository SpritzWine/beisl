//! Orchestrates one trace: set up the environment, spawn the game, open the session
//! window, and manage the `perf` recorder across the game's lifetime — publishing live
//! state to `session.json` for the window to display.

use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use preflight::RecordMode;
use session::{Session, State};

use crate::mangohud::TOGGLE_KEY;
use crate::{cue, gpu, mangohud, perf, presence, steam, walls};

/// Run `cmd` under tracing; returns the game's exit code.
pub fn trace(cmd: &[String]) -> io::Result<i32> {
    let run_dir = new_run_dir()?;
    let mut log = Log::open(&run_dir);
    log.line(&format!("launch argv = {cmd:?}"));

    // We run *inside* Steam's own launch environment here, so — unlike preflight's
    // `.steamrt64`-directory guess — we can see the actual truth of whether it scrubbed
    // our PATH. Persist that reading so preflight can retire the launcher-shim row once
    // Valve's bug is confirmed fixed, or re-promote it if a later Steam update regresses.
    observe_steam_path(&mut log);

    // Steam runs launch options through a shell, and some %command% expansions carry
    // leading `VAR=value` env assignments; peel those off so we run the real program, not
    // a literal "VAR=value". A no-op when there are none (e.g. the usual reaper command).
    let (envs, prog) = split_env_prefix(cmd);
    if prog.is_empty() {
        log.line("error: nothing to run after env assignments");
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "no program to run",
        ));
    }
    if !envs.is_empty() {
        log.line(&format!("env prefix = {envs:?}"));
    }
    log.line(&format!(
        "program = {} (exists: {}), args = {:?}",
        prog[0],
        Path::new(&prog[0]).exists(),
        &prog[1..]
    ));

    // Pick how perf will capture this session up front — Full (system-wide), Degraded
    // (own-process userspace-only, when paranoia blocks -a but not own profiling), or None
    // — because Degraded changes what we spawn: `perf` wrapping the game, not the game
    // directly (see `launch_argv`).
    let perf_data = run_dir.join("perf.data");
    let mode = select_record_mode();
    let (program, args) = launch_argv(mode, &perf_data, &prog);

    let mut child = Command::new(&program);
    child.args(&args);
    if mode == RecordMode::Degraded {
        // Degraded has no Recorder::start() of its own (perf IS `child` here) — arm the
        // same death-signal backstop `Recorder::start` gives Full mode.
        perf::arm_pdeathsig(&mut child);
    }
    for (k, v) in &envs {
        child.env(k, v);
    }

    // Force Vulkan enumeration onto a specific GPU, if the user pinned one —
    // countering the "enumeration roulette" trap from DESIGN.md's Saints Row case study,
    // where two Vulkan-visible drivers silently invalidated an A/B. Plain config, no
    // privilege involved.
    let cfg = preflight::Config::load();
    if let Some(pin) = &cfg.gpu_pin {
        for (k, v) in gpu_pin_envs(pin) {
            log.line(&format!("gpu pin: {k}={v}"));
            child.env(k, v);
        }
    }

    // Frametime side-channel + toggle hotkey. Also aim MangoHud's GPU sampler at the card
    // the game actually renders on (the pinned GPU, else the sole GPU) — left to itself
    // the sampler picks a card independently of Vulkan device selection, and on a
    // multi-GPU box it happily logs an idle iGPU's stats as the game's.
    let mango_pci_dev = mangohud::sample_pci_dev(cfg.gpu_pin.as_ref());
    if let Some(addr) = &mango_pci_dev {
        log.line(&format!("mangohud: sample GPU pci_dev={addr}"));
    }
    if let Some(note) = mangohud::configure(&mut child, &run_dir, mango_pci_dev.as_deref()) {
        log.line(&note);
    }

    // Tee the child's stdout+stderr into game.log — Wine/vkd3d/DXVK diagnostics
    // (err: lines, device-lost messages) land on stderr, and inherited stdio alone is
    // gone once the launcher window closes. Tee, don't redirect: our own stdio IS the
    // Lutris log window / Steam console, and a beisl injected into launch options must
    // not swallow the game's output. Two handles onto the same file; interleaving of
    // the two streams is acceptable.
    // Everything this run writes is younger than this; a log file older than it is a
    // previous launch's leftover (see `walls::LogTail`).
    let started_at = std::time::SystemTime::now();
    let game_log = match std::fs::File::create(run_dir.join("game.log")) {
        Ok(f) => {
            child.stdout(Stdio::piped());
            child.stderr(Stdio::piped());
            Some(f)
        }
        Err(e) => {
            // Inherited stdio stays in place — the game still runs, just uncaptured.
            log.line(&format!(
                "game.log: couldn't create ({e}) — child output not captured"
            ));
            None
        }
    };

    let mut recorder = perf::Recorder::new(&run_dir);
    // Watch for GPU hangs: xe keeps exactly ONE devcoredump, so capture-and-free
    // during the session or the next hang records nothing (see DESIGN.md case study).
    let mut gpuwatch = gpu::GpuWatch::new(&run_dir);

    // Publish the initial state so the window has something to show immediately.
    // `wrapper` is OUR identity (this process IS the wrapper), so a reader can later
    // tell a same-boot crash from a genuinely live run (see session::WrapperProc) -
    // `None` on failure just degrades to the pre-boot-only staleness rule.
    let mut sess = Session {
        game: game_name(&prog),
        trace_dir: run_dir.display().to_string(),
        hotkey: TOGGLE_KEY.to_string(),
        state: State::Starting,
        warnings: preflight_warnings(),
        gpu_pin: cfg.gpu_pin.as_ref().map(|p| p.name.clone()),
        record_mode: mode.as_str().to_string(),
        wrapper: session::WrapperProc::current(),
        ..Default::default()
    };
    // Steam's own word on what this is, before anything runs: the appid a compat
    // finding is keyed on, and the manifest's product name. Presence may refine the
    // name later; nothing may take the appid away.
    let mut name_source = NameSource::Argv;
    if let Some(id) = steam::from_launch(&prog) {
        log.line(&format!(
            "game identity from Steam: appid {}{}",
            id.appid,
            match &id.name {
                Some(name) => format!(", {name}"),
                None => ", no manifest found for a name".to_string(),
            }
        ));
        sess.steam_appid = id.appid;
        if let Some(name) = id.name {
            sess.game = name;
            name_source = NameSource::Steam;
        }
    }
    let _ = sess.write(&run_dir);

    cue::play(cue::Cue::Ready);
    let mut proc = match child.spawn() {
        Ok(p) => p,
        Err(e) => {
            log.line(&format!("error: failed to launch {}: {e}", prog[0]));
            sess.state = State::Error;
            sess.message = format!("Failed to launch {}: {e}", prog[0]);
            let _ = sess.write(&run_dir);
            spawn_monitor(&run_dir); // open the window so the error is visible
            return Err(e);
        }
    };
    log.line(&format!("launched pid {}", proc.id()));

    // Wire up the tees now that the pipes exist. Detached threads: they end on pipe EOF
    // (child and any launcher descendants holding it gone), and at latest with us.
    if let Some(f) = game_log {
        let err_file = f.try_clone();
        if let Some(out) = proc.stdout.take() {
            tee(out, io::stdout(), f);
        }
        if let Some(err) = proc.stderr.take() {
            match err_file {
                Ok(f) => tee(err, io::stderr(), f),
                // Clone failed — keep at least the launcher-window side alive.
                Err(_) => tee_discard_file(err, io::stderr()),
            }
        }
    }

    // Open the session window alongside the game (best-effort; headless still works).
    spawn_monitor(&run_dir);

    // Start/acknowledge recording per the chosen mode. Full drives perf as our own child
    // via Recorder; Degraded's perf is already live (it's `child`, spawned above — wrapping
    // the game); None just tells the user why nothing's being captured.
    match mode {
        RecordMode::Full => match recorder.start() {
            Ok(()) => {
                sess.state = State::Recording;
                log.line("perf: recording started");
            }
            Err(e) => {
                sess.state = State::Running;
                sess.warnings.push(e.clone());
                log.line(&format!("perf: {e}"));
            }
        },
        RecordMode::Degraded => {
            sess.state = State::Recording;
            perf::set_perf_pid(proc.id());
            log.line("perf: recording started (degraded / userspace-only)");
        }
        RecordMode::None => {
            sess.state = State::Running;
            let warning =
                "perf isn't available on this machine — run beisl-setup (game runs un-traced)"
                    .to_string();
            sess.warnings.push(warning.clone());
            log.line(&format!("perf: {warning}"));
        }
    }
    // Stop perf cleanly if beisl is signalled (Steam's Stop button = SIGTERM, or Ctrl-C).
    perf::install_teardown();
    let _ = sess.write(&run_dir);

    // Watch the logs for a compat wall while the game runs, so the window can say what the
    // game's own error dialog won't. With PROTON_LOG on, the user's own choice, the
    // evidence lands in Proton's file rather than game.log; we read it, never enable it.
    let proton_log = walls::proton_log_path(|k| std::env::var(k).ok());
    if let Some(p) = &proton_log {
        log.line(&format!(
            "proton log: PROTON_LOG is set by the user - watching {} for wall signatures",
            p.display()
        ));
    }
    let mut walls = walls::WallWatch::new(
        std::iter::once(run_dir.join("game.log")).chain(proton_log.clone()),
        started_at,
    );

    let start = Instant::now();
    let mut stopped = false;
    let mut last_presence_ask: Option<Duration> = None;
    let mut presence_asks = 0u32;
    let mut presence_reachable = false;
    let mut stop_refused = false;
    loop {
        if let Some(status) = proc.try_wait()? {
            // Close the pid-reuse window: whatever PID we published (Recorder's own child,
            // or `proc` in Degraded) is about to become stale now that it's exited.
            perf::set_perf_pid(0);
            if !stopped {
                recorder.stop();
                cue::play(cue::Cue::RecordStop);
            }
            // A hang often coincides with the game dying — one last look.
            drain_gpu_events(&mut gpuwatch, &mut log, &mut sess, &run_dir);
            note_walls(walls.poll(), &mut log, &mut sess);
            if let Some(src) = &proton_log {
                match walls::preserve_proton_log(src, &run_dir, started_at) {
                    Some(bytes) => log.line(&format!(
                        "proton log: preserved as proton.log ({bytes} bytes) - Proton deletes \
                         the original at the next launch"
                    )),
                    None => log.line(
                        "proton log: PROTON_LOG was set but Proton wrote no log for this run",
                    ),
                }
            }
            sess.state = State::Done;
            sess.elapsed_secs = start.elapsed().as_secs();
            sess.trace_bytes = trace_size(&run_dir);
            sess.message = if sess.trace_bytes > 0 {
                // Decide what the trace is worth: a crash (non-zero code, or a GPU hang the
                // watcher saw) is always analyzed for forensics; a clean exit follows the
                // user's `clean-run-analysis` preference so a boring session doesn't burn
                // seconds resolving and keep tens of MB for nothing.
                let crashed = status.code() != Some(0) || gpuwatch.hang_seen();
                match decide_post_run(crashed, cfg.clean_run_analysis) {
                    PostRun::Analyze => {
                        let why = if crashed {
                            "abnormal exit"
                        } else {
                            "clean exit"
                        };
                        log.line(&format!("post-run: analyzing ({why})"));
                        // This resolve runs inside Steam's launch session, which lacks an
                        // interactive shell's DEBUGINFOD_URLS — set it explicitly so the
                        // crash trace's driver layer gets symbols instead of anonymous
                        // addresses (the whole point of auto-analyzing a crash).
                        let urls = crate::symbols::debuginfod_urls(&cfg);
                        match write_report(
                            &run_dir,
                            &sess.game,
                            sess.elapsed_secs,
                            &sess.record_mode,
                            urls.as_deref(),
                            &mut log,
                        ) {
                            Some(headline) => format!("Game exited — trace saved. {headline}"),
                            None => "Game exited — trace saved.".to_string(),
                        }
                    }
                    PostRun::Keep => {
                        log.line("post-run: clean exit — leaving raw trace in place");
                        sess.analysis_offer = true;
                        "Game exited — trace saved (not analyzed). Analyze it any time in \
                         the window, or run `beisl symbols`."
                            .to_string()
                    }
                    PostRun::Discard => {
                        let _ = std::fs::remove_file(run_dir.join("perf.data"));
                        sess.trace_bytes = 0;
                        // A healthy run, not a recorder failure — state_word must not
                        // read the empty perf.data as "wrote no data".
                        sess.trace_discarded = true;
                        log.line("post-run: clean exit — perf trace discarded (per preference)");
                        "Game exited — clean run, perf trace discarded (per your setting)."
                            .to_string()
                    }
                }
            } else {
                "Game exited.".to_string()
            };
            let _ = sess.write(&run_dir);
            // The one place a silent presence miss gets said out loud: a run named from
            // argv alone is the first thing anyone debugging the integration looks for.
            if name_source < NameSource::Presence && presence_asks > 0 {
                log.line(&format!(
                    "gamebus presence: no answer in {presence_asks} asks ({})",
                    if presence_reachable {
                        "service up, no matching activity - its /proc sweep runs every \
                         15 s, and a short run can end before it looks"
                    } else {
                        "service not on the bus"
                    }
                ));
            }
            // With launcher chains (umu-run → launcher → game) this is the *wrapper's*
            // status — it can be 0 even when the real game process died. The marker
            // itself is `session::EXIT_LOG_MARKER`, shared with beisl-mcp's crash
            // judgment — don't reword this line without updating that constant.
            log.line(&format!(
                "{} {status} (with launcher chains this is the \
                 wrapper's status, not the game's), trace {} bytes",
                session::EXIT_LOG_MARKER,
                sess.trace_bytes
            ));
            return Ok(status.code().unwrap_or(0));
        }

        sess.elapsed_secs = start.elapsed().as_secs();
        sess.trace_bytes = trace_size(&run_dir);
        drain_gpu_events(&mut gpuwatch, &mut log, &mut sess, &run_dir);

        note_walls(walls.poll(), &mut log, &mut sess);

        // MangoHud names its CSV after the *real* game process; with launcher chains
        // that beats the argv guess (which names the wrapper, e.g. "upc.exe").
        if let Some(game) = mangohud_game_name(&run_dir) {
            if sess.game != game && name_source <= NameSource::MangoHud {
                log.line(&format!(
                    "game name from MangoHud csv: {game} (was: {})",
                    sess.game
                ));
                sess.game = game;
                name_source = NameSource::MangoHud;
            }
        }

        // gamebus-presenced, if it's running, knows what a *human* calls this game -
        // Discord's title, not the manifest's - so it outranks everything above. Polled
        // until it answers once (it publishes only after its own sweep has seen the
        // process, which is after we start), then left alone.
        if name_source < NameSource::Presence && presence_due(start.elapsed(), last_presence_ask) {
            last_presence_ask = Some(start.elapsed());
            presence_asks += 1;
            let known = (!sess.steam_appid.is_empty()).then_some(sess.steam_appid.as_str());
            match presence::lookup(known) {
                presence::Lookup::Found(found) => {
                    log.line(&format!(
                        "game identity from gamebus presence: {} (was: {}){}",
                        found.name,
                        sess.game,
                        match &found.steam_appid {
                            Some(id) => format!(", steam appid {id}"),
                            None => String::new(),
                        }
                    ));
                    sess.game = found.name;
                    // Steam's own appid stays when presence has none to offer.
                    if let Some(id) = found.steam_appid {
                        sess.steam_appid = id;
                    }
                    name_source = NameSource::Presence;
                }
                presence::Lookup::Nothing => presence_reachable = true,
                presence::Lookup::Unreachable => {}
            }
        }

        // The window can ask us to stop the trace while the game keeps running. We must
        // keep living until the game exits (Steam waits on the launch-options process),
        // so we only stop the recorder, not the game. Degraded can't honor this at all:
        // its one `perf` process IS the game's parent, so "stop" would have to kill perf
        // and take the game down with it — the opposite of "keep the game running".
        if mode != RecordMode::Degraded {
            if !stopped {
                if let Some(disk) = Session::read(&run_dir) {
                    if disk.stop_requested {
                        recorder.stop();
                        cue::play(cue::Cue::RecordStop);
                        stopped = true;
                        sess.stop_requested = true;
                        sess.trace_bytes = trace_size(&run_dir);
                        // Saved, not Done: the trace is complete but the game lives on —
                        // the window can offer analysis while still saying so.
                        sess.state = State::Saved;
                        sess.message = "Stopped — trace saved. Game still running.".to_string();
                    }
                }
            }
        } else if !stop_refused {
            if let Some(disk) = Session::read(&run_dir) {
                if disk.stop_requested {
                    stop_refused = true;
                    log.line(
                        "stop: degraded mode records the whole session — ignoring stop to \
                         keep the game running",
                    );
                    sess.message = "Stop isn't available in degraded mode — it records the \
                                     whole session, so it stays on until the game exits."
                        .to_string();
                    sess.warnings.push(
                        "Degraded recording mode captures the whole session, so it can't be \
                         stopped early without ending the game too — ignoring the stop \
                         request and continuing to record."
                            .to_string(),
                    );
                }
            }
        }
        let _ = sess.write(&run_dir);
        std::thread::sleep(Duration::from_millis(800));
    }
}

/// Surface GPU-hang captures/warnings live: into the log and the session window.
///
/// User-facing wording is layered (DESIGN + Florian's spec): plain diagnosis first ("your
/// GPU, not the game or Wine"), the offer to help gather evidence second, the technical
/// detail (card, filename, reboot advice) last.
fn drain_gpu_events(watch: &mut gpu::GpuWatch, log: &mut Log, sess: &mut Session, run_dir: &Path) {
    for ev in watch.poll() {
        match ev {
            gpu::Event::Captured { card, dest } => {
                log.line(&format!(
                    "gpu: devcoredump on {card} captured -> {}",
                    dest.display()
                ));
                let file = dest
                    .file_name()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default();
                sess.warnings.push(format!(
                    "Something went wrong and your GPU was the cause — not the game's code and \
                     not Wine. Beisl saved the crash evidence for you ({file}). \
                     Heads-up: the GPU had to be reset, so reboot before your next comparison \
                     run — results until then can't be trusted. (GPU: {card})"
                ));
            }
            gpu::Event::Unreadable { card, node } => {
                log.line(&format!(
                    "gpu: devcoredump pending on {card} but root-only ({})",
                    node.display()
                ));
                // Offer the one-click enable only under the warn-only policy.
                let offer = preflight::Config::load().dump_access_policy
                    == preflight::DumpAccessPolicy::WarnOnly;
                sess.dump_capture_offer = offer;
                let bridge = if offer {
                    "Want to help get this fixed? Turn on crash-dump capture and Beisl will save \
                     the evidence — this crash's dump can still be rescued."
                } else {
                    "Beisl couldn't read the crash dump this time."
                };
                sess.warnings.push(format!(
                    "Something went wrong and your GPU was the cause — not the game's code and \
                     not Wine. {bridge} To grab it by hand instead: {} (GPU: {card})",
                    gpu::sudo_capture_hint(&node, run_dir, &card)
                ));
            }
        }
    }
}

/// The child-process env vars a GPU pin translates to: `VK_DRIVER_FILES` (the pin's ICD
/// manifest paths, colon-joined — the format Vulkan's loader expects) and, only when we
/// could resolve it, `MESA_VK_DEVICE_SELECT`. Split out from `trace` so it's testable
/// without spawning a real process.
fn gpu_pin_envs(pin: &preflight::GpuPin) -> Vec<(String, String)> {
    let mut envs = vec![("VK_DRIVER_FILES".to_string(), pin.icd_paths.join(":"))];
    if let Some(sel) = &pin.device_select {
        envs.push(("MESA_VK_DEVICE_SELECT".to_string(), sel.clone()));
    }
    envs
}

/// Env vars the Proton *compat-tool contract* sets on every launch — but Heroic, Lutris,
/// and Bottles all route non-Steam games through the very same contract, so their presence
/// alone only proves "some compat tool launched us", not "Steam specifically". Enough to
/// trust a `Scrubbed` reading (see [`decide_observation`]'s doc for why that side stays
/// permissive), NOT enough on its own to trust `Fixed`.
const STEAM_COMPAT_MARKERS: &[&str] = &[
    "SteamAppId",
    "SteamGameId",
    "STEAM_COMPAT_APP_ID",
    "STEAM_COMPAT_CLIENT_INSTALL_PATH",
];

/// Env vars only the real Steam *client* sets, outside the compat-tool contract above —
/// third-party launchers don't set these. Required before trusting a `Fixed` reading (see
/// [`decide_observation`]). Not empirically verified against a real Steam launch in this
/// dev environment (headless, no Steam client installed) — confirm `SteamEnv=1` /
/// `SteamClientLaunch=1` actually survive into `%command%` before relying on this gate.
const STEAM_CLIENT_ONLY_MARKERS: &[&str] = &["SteamEnv", "SteamClientLaunch"];

fn launched_by_compat_tool() -> bool {
    STEAM_COMPAT_MARKERS
        .iter()
        .any(|k| std::env::var_os(k).is_some())
}

fn launched_by_steam_client() -> bool {
    STEAM_CLIENT_ONLY_MARKERS
        .iter()
        .any(|k| std::env::var_os(k).is_some())
}

/// Does this process's own PATH include the user's `~/.local/bin`? That's the entry a bare
/// `beisl %command%` launch option needs Steam to have passed through untouched (see
/// `preflight::steam_client_scrubs_path`'s doc for the containerized client's scrub).
fn path_has_user_local_bin() -> bool {
    let target = PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/bin");
    path_contains_dir(std::env::var_os("PATH").as_deref(), &target)
}

/// Testable core of [`path_has_user_local_bin`]: does `path_var` (colon-joined, as `PATH`
/// always is) contain exactly `target`? Split out so the decision can be tested without
/// mutating the process's real `PATH`/`HOME`.
fn path_contains_dir(path_var: Option<&std::ffi::OsStr>, target: &Path) -> bool {
    path_var
        .map(|p| std::env::split_paths(p).any(|dir| dir == target))
        .unwrap_or(false)
}

/// Pure decision, asymmetrically gated: the two outcomes have very different failure
/// costs. A false `Scrubbed` is harmless and self-healing (it just triggers today's
/// ordinary NeedsFix heuristic, corrected the moment a real Steam trace runs) — so
/// `path_fixed = false` always resolves regardless of `client_marker`. A false `Fixed` is
/// sticky and self-sealing: persisted with no shim installed, it retires the row with an
/// authoritative "works without a shim" — but if that reading actually came from a
/// Heroic/Lutris/Bottles launch (same compat-tool contract, user's own already-full PATH),
/// the NEXT real Steam launch still can't resolve `beisl %command%`, and now never even
/// runs to record the correction. So `path_fixed = true` only resolves to `Fixed` when
/// `client_marker` also holds; otherwise `None` — don't record anything, leave the stored
/// observation untouched.
fn decide_observation(
    path_fixed: bool,
    client_marker: bool,
) -> Option<preflight::SteamPathObservation> {
    if path_fixed {
        client_marker.then_some(preflight::SteamPathObservation::Fixed)
    } else {
        Some(preflight::SteamPathObservation::Scrubbed)
    }
}

/// Record what THIS Steam-launched trace saw of the launch PATH into the preflight config
/// (see `preflight::SteamPathObservation`), so `beisl-setup` can retire the launcher-shim
/// row once Valve's bug looks fixed here, or re-promote it on a later regression. A no-op
/// off a manual terminal run, and also a no-op when a PATH-looks-fixed reading can't be
/// trusted yet (see [`decide_observation`]). A failed config write must never abort the
/// trace — logged, not propagated.
fn observe_steam_path(log: &mut Log) {
    if !launched_by_compat_tool() {
        return;
    }
    let Some(observed) = decide_observation(path_has_user_local_bin(), launched_by_steam_client())
    else {
        log.line(
            "steam path observation: PATH looks fixed but no Steam-client-only marker \
             present (could be a non-Steam launcher on the same compat-tool contract) — \
             not recording, to avoid a sticky false \"fixed\"",
        );
        return;
    };
    let mut cfg = preflight::Config::load();
    if cfg.steam_path_observation != observed {
        log.line(&format!(
            "steam path observation: {:?} -> {observed:?}",
            cfg.steam_path_observation
        ));
        cfg.steam_path_observation = observed;
        // beisl-setup also loads+saves this same file (e.g. a GPU pin pick); if it read an
        // older snapshot before this trace ran and saves after, its write clobbers this
        // fresh observation with the stale one. Benign — no locking here — because the
        // worst case is one lost observation, corrected by the next Steam-launched trace.
        if let Err(e) = cfg.save() {
            log.line(&format!("steam path observation: config save failed ({e})"));
        }
    }
}

/// Parse `BEISL_FORCE_RECORD_MODE`'s value (case-insensitive `full`/`degraded`/`none`) —
/// used by the `verify` skill / manual testing to force a mode without faking the
/// underlying `perf_event_paranoid`/CAP_PERFMON state. `None` covers both "unset" and
/// "unrecognized", so the caller can fall through to real detection either way.
fn parse_forced_mode(v: Option<&str>) -> Option<RecordMode> {
    match v?.to_ascii_lowercase().as_str() {
        "full" => Some(RecordMode::Full),
        "degraded" => Some(RecordMode::Degraded),
        "none" => Some(RecordMode::None),
        _ => None,
    }
}

/// Which [`RecordMode`] to use for this launch: `BEISL_FORCE_RECORD_MODE` overrides
/// detection when set to a recognized value; otherwise ask preflight what this machine
/// can actually do. The env read lives here (not in `preflight`) — forcing a mode is a
/// wrapper-level testing knob, not something the detection crate should know about.
fn select_record_mode() -> RecordMode {
    if let Some(v) = std::env::var_os("BEISL_FORCE_RECORD_MODE") {
        if let Some(m) = parse_forced_mode(v.to_str()) {
            return m;
        }
        eprintln!("beisl: BEISL_FORCE_RECORD_MODE={v:?} not recognized — detecting instead");
    }
    preflight::detect_record_mode()
}

/// The program + args to actually spawn for `mode`. Full and None launch the game
/// directly (today's behavior, unchanged); Degraded wraps it in `perf record` (own-process
/// cycles, no `-a`) so perf's lifetime is the game's — no separate Recorder needed.
fn launch_argv(mode: RecordMode, perf_data: &Path, prog: &[String]) -> (String, Vec<String>) {
    match mode {
        RecordMode::Full | RecordMode::None => (prog[0].clone(), prog[1..].to_vec()),
        RecordMode::Degraded => {
            let mut args = perf::degraded_args(perf_data);
            args.push("--".to_string());
            args.extend(prog.iter().cloned());
            ("perf".to_string(), args)
        }
    }
}

/// Peel leading `VAR=value` tokens (Steam/user env assignments) off the front of the
/// command, returning them plus the remaining program + args. Stops at the first token
/// that isn't an assignment, so a normal `reaper …` command is left untouched.
fn split_env_prefix(cmd: &[String]) -> (Vec<(String, String)>, Vec<String>) {
    let mut envs = Vec::new();
    let mut i = 0;
    while i < cmd.len() {
        match cmd[i].split_once('=') {
            Some((k, v)) if is_env_name(k) => {
                envs.push((k.to_string(), v.to_string()));
                i += 1;
            }
            _ => break,
        }
    }
    (envs, cmd[i..].to_vec())
}

fn is_env_name(s: &str) -> bool {
    let mut chars = s.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// A small append-only diagnostic log in the trace dir. Steam swallows the wrapper's
/// stderr, so this is where a failed launch actually surfaces; mirrored to stderr too.
struct Log {
    file: Option<std::fs::File>,
}

impl Log {
    fn open(dir: &Path) -> Self {
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join("beisl.log"))
            .ok();
        Log { file }
    }

    fn line(&mut self, s: &str) {
        eprintln!("beisl: {s}");
        if let Some(f) = self.file.as_mut() {
            use std::io::Write;
            let _ = writeln!(f, "{s}");
        }
    }
}

/// Launch the session window: prefer the `beisl-setup` sibling of this binary (same
/// build, no version skew), else fall back to PATH.
fn spawn_monitor(dir: &Path) {
    // Headless / testing: `BEISL_NO_UI=1` runs the wrapper without the window.
    if std::env::var_os("BEISL_NO_UI").is_some() {
        return;
    }
    // Sibling first, PATH second: the beisl-setup sitting next to this binary is by
    // construction the same build (one bundle, one dist tree), while PATH may hold an
    // older install. A version-skewed window silently freezes on session states it
    // doesn't know (a fresh wrapper's "Saved" froze an 11-o'clock GUI at "Recording"
    // during review, 2026-07-11) — skew-proofing beats install-preferring.
    if let Ok(exe) = std::env::current_exe() {
        if let Some(sibling) = exe.parent().map(|p| p.join("beisl-setup")) {
            if sibling.is_file()
                && Command::new(&sibling)
                    .arg("--session")
                    .arg(dir)
                    .spawn()
                    .is_ok()
            {
                return;
            }
        }
    }
    let _ = Command::new("beisl-setup")
        .arg("--session")
        .arg(dir)
        .spawn();
}

/// Blocking preflight checks that still need setup — surfaced live as "what's wrong".
fn preflight_warnings() -> Vec<String> {
    let mut warns = Vec::new();
    let mut checks = preflight::all_checks();
    for c in checks.iter_mut() {
        c.detect();
        if c.blocking && c.status == preflight::Status::NeedsFix {
            warns.push(format!("{} needs setup — run beisl-setup", c.label));
        }
    }
    warns
}

fn trace_size(dir: &Path) -> u64 {
    std::fs::metadata(dir.join("perf.data"))
        .map(|m| m.len())
        .unwrap_or(0)
}

/// What to do with a finished trace: [`decide_post_run`] picks it from the exit status
/// and the user's clean-run preference.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PostRun {
    /// Resolve now and write the report (crash, or the `always` preference).
    Analyze,
    /// Leave the raw trace in place, untouched — the default. Analyze later via the window
    /// or `beisl symbols`; nothing is discarded.
    Keep,
    /// Drop the perf trace to reclaim space (clean exit, `discard` preference).
    Discard,
}

/// Crash → always analyze (that's the whole point of the tool); clean exit → the user's
/// preference. `crashed` folds the game's exit code together with a detected GPU hang,
/// because with launcher chains (umu-run → … → game) the code we see can be 0 even when
/// the game died — the hang signal catches that.
fn decide_post_run(crashed: bool, policy: preflight::CleanRunAnalysis) -> PostRun {
    if crashed {
        return PostRun::Analyze;
    }
    match policy {
        preflight::CleanRunAnalysis::Always => PostRun::Analyze,
        preflight::CleanRunAnalysis::Keep => PostRun::Keep,
        preflight::CleanRunAnalysis::Discard => PostRun::Discard,
    }
}

/// Resolve the finished trace and drop `artifact.json` (the exportable, GUI/MCP-ready
/// merge) + `report.txt` (human-readable) into the run dir. Best-effort — a resolution
/// failure (no `perf` at analysis time, unreadable data) must never turn a captured trace
/// into a wrapper error. Returns the one-line headline for the session message.
fn write_report(
    run_dir: &Path,
    game: &str,
    duration_s: u64,
    record_mode: &str,
    debuginfod_urls: Option<&str>,
    log: &mut Log,
) -> Option<String> {
    log.line("post-run: resolving symbols");
    let art = match artifact::TraceArtifact::build_with_debuginfod(
        run_dir,
        game,
        duration_s as f64,
        record_mode,
        debuginfod_urls,
    ) {
        Ok(a) => a,
        Err(e) => {
            log.line(&format!("post-run: resolution failed ({e})"));
            // Same door as `beisl symbols`/the window's Analyze: say so on disk, but
            // never disable a later retry (see `mark_invalid`'s doc comment).
            let _ = session::library::mark_invalid(run_dir, &e.to_string());
            return None;
        }
    };
    // Best-effort, same stance as the resolve above: a write failure (disk full, a
    // read-only run dir) must never turn a captured trace into a wrapper error.
    let _ = art.persist(run_dir);
    // A retry succeeded on the same perf.data — drop any stale failure verdict.
    session::library::clear_invalid(run_dir);
    let headline = art.headline();
    log.line(&format!("post-run: {headline}"));
    Some(headline)
}

/// A compat wall just showed up in a log: say so in beisl.log and in the window's
/// warning list, once per signature. The wall's own evidence line goes to the log, the
/// human explanation to the window.
fn note_walls(hits: Vec<(&'static str, String)>, log: &mut Log, sess: &mut Session) {
    for (signature, line) in hits {
        log.line(&format!("compat wall: {signature}: {line}"));
        sess.warnings.push(format!(
            "Compat wall ({signature}): {}",
            compat::explain(signature)
        ));
    }
}

/// Where a run's name came from, in rising order of trust; a name is only replaced from
/// a higher rung. Argv names the launcher, MangoHud the process, Steam's manifest the
/// product, presence the product as Discord and Steam together call it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum NameSource {
    Argv,
    MangoHud,
    Steam,
    Presence,
}

/// Whether to ask gamebus presence again, `last_ask` being when we last did (as an
/// offset from the run's start). Every ask costs a `busctl` process, and a presence
/// record takes a sweep or two to appear: once a second for the first half minute,
/// then every five - past that the service is either absent or has nothing for us,
/// and a trace can run for hours.
fn presence_due(elapsed: Duration, last_ask: Option<Duration>) -> bool {
    let gap = Duration::from_secs(if elapsed.as_secs() < 30 { 1 } else { 5 });
    last_ask.is_none_or(|t| elapsed.saturating_sub(t) >= gap)
}

/// Best-effort human name for the game: the last `.exe` argument, else the command's
/// basename.
fn game_name(cmd: &[String]) -> String {
    cmd.iter()
        .rev()
        .find(|a| a.to_ascii_lowercase().ends_with(".exe"))
        .or_else(|| cmd.first())
        .map(|a| {
            Path::new(a)
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| a.clone())
        })
        .unwrap_or_default()
}

/// Pump `src` into both `term` and `file` until EOF, on a detached thread. Raw chunks,
/// flushed to the terminal side per read so the game's output reaches the Lutris/Steam
/// log view as promptly as it would without us in between.
fn tee<R, W>(mut src: R, mut term: W, mut file: std::fs::File)
where
    R: io::Read + Send + 'static,
    W: io::Write + Send + 'static,
{
    use io::Write as _;
    std::thread::spawn(move || {
        let mut buf = [0u8; 8192];
        loop {
            match src.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    let _ = term.write_all(&buf[..n]);
                    let _ = term.flush();
                    let _ = file.write_all(&buf[..n]);
                }
            }
        }
    });
}

/// [`tee`] without the file sink, for when cloning the game.log handle failed: the
/// launcher-window pass-through is the part we must never lose.
fn tee_discard_file<R, W>(mut src: R, mut term: W)
where
    R: io::Read + Send + 'static,
    W: io::Write + Send + 'static,
{
    std::thread::spawn(move || {
        let _ = io::copy(&mut src, &mut term);
    });
}

/// The real game's name from a MangoHud frametime CSV in the run dir, if one has
/// appeared yet. MangoHud writes it mid-session, named after the game process.
fn mangohud_game_name(run_dir: &Path) -> Option<String> {
    std::fs::read_dir(run_dir)
        .ok()?
        .flatten()
        .find_map(|e| csv_game_name(&e.file_name().to_string_lossy()))
}

/// `"Outlaws_2026-07-11_07-15-40.csv"` → `Some("Outlaws")`. The suffix is MangoHud's
/// `_%Y-%m-%d_%H-%M-%S` timestamp; filenames that don't match are ignored.
fn csv_game_name(file_name: &str) -> Option<String> {
    let stem = file_name.strip_suffix(".csv")?;
    let (rest, time) = stem.rsplit_once('_')?;
    let (game, date) = rest.rsplit_once('_')?;
    fn is_stamp(s: &str, lens: &[usize]) -> bool {
        let parts: Vec<&str> = s.split('-').collect();
        parts.len() == lens.len()
            && parts
                .iter()
                .zip(lens)
                .all(|(p, l)| p.len() == *l && p.bytes().all(|c| c.is_ascii_digit()))
    }
    (!game.is_empty() && is_stamp(date, &[4, 2, 2]) && is_stamp(time, &[2, 2, 2]))
        .then(|| game.to_string())
}

/// `$XDG_DATA_HOME/beisl/traces/run-<unix_ts>/` (falls back to `~/.local/share`).
fn new_run_dir() -> io::Result<PathBuf> {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let dir = data_root().join("traces").join(format!("run-{ts}"));
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

pub(crate) fn data_root() -> PathBuf {
    // One XDG resolution for writer and readers: the session crate owns it now — its
    // library module lists the very dirs this side creates, so the two must agree.
    session::library::data_root()
}

#[cfg(test)]
mod tests {
    use super::*;
    use preflight::{CleanRunAnalysis, GpuPin};

    // The Arc A770 from the case-study machine as a MESA_VK_DEVICE_SELECT selector
    // (same fixture naming as the mangohud/preflight test modules).
    const ARC_SELECT: &str = "8086:56a0!";

    #[test]
    fn a_crash_is_always_analyzed_whatever_the_preference() {
        for policy in [
            CleanRunAnalysis::Keep,
            CleanRunAnalysis::Always,
            CleanRunAnalysis::Discard,
        ] {
            assert_eq!(
                decide_post_run(true, policy),
                PostRun::Analyze,
                "{policy:?}"
            );
        }
    }

    #[test]
    fn a_clean_exit_follows_the_preference() {
        // The default keeps the trace in place — a clean run is never discarded by default.
        assert_eq!(
            decide_post_run(false, CleanRunAnalysis::default()),
            PostRun::Keep
        );
        assert_eq!(
            decide_post_run(false, CleanRunAnalysis::Keep),
            PostRun::Keep
        );
        assert_eq!(
            decide_post_run(false, CleanRunAnalysis::Always),
            PostRun::Analyze
        );
        assert_eq!(
            decide_post_run(false, CleanRunAnalysis::Discard),
            PostRun::Discard
        );
    }

    #[test]
    fn gpu_pin_envs_joins_icd_paths_and_sets_device_select() {
        let pin = GpuPin {
            name: "Intel — intel_icd (/usr/share/vulkan/icd.d)".into(),
            icd_paths: vec![
                "/usr/share/vulkan/icd.d/intel_icd.x86_64.json".into(),
                "/usr/share/vulkan/icd.d/intel_icd.i686.json".into(),
            ],
            device_select: Some(ARC_SELECT.into()),
        };
        let envs = gpu_pin_envs(&pin);
        assert_eq!(
            envs,
            vec![
                (
                    "VK_DRIVER_FILES".to_string(),
                    "/usr/share/vulkan/icd.d/intel_icd.x86_64.json:\
                     /usr/share/vulkan/icd.d/intel_icd.i686.json"
                        .to_string()
                ),
                ("MESA_VK_DEVICE_SELECT".to_string(), ARC_SELECT.to_string()),
            ]
        );
    }

    #[test]
    fn gpu_pin_envs_omits_device_select_when_unresolved() {
        let pin = GpuPin {
            name: "nouveau_icd (/usr/share/vulkan/icd.d)".into(),
            icd_paths: vec!["/usr/share/vulkan/icd.d/nouveau_icd.x86_64.json".into()],
            device_select: None,
        };
        let envs = gpu_pin_envs(&pin);
        assert_eq!(envs.len(), 1);
        assert_eq!(envs[0].0, "VK_DRIVER_FILES");
    }

    #[test]
    fn presence_is_asked_once_a_second_then_every_five() {
        let s = Duration::from_secs;
        assert!(presence_due(s(0), None));
        assert!(!presence_due(
            s(10),
            Some(s(9) + Duration::from_millis(800))
        ));
        assert!(presence_due(s(10), Some(s(9))));
        assert!(!presence_due(s(63), Some(s(60))));
        assert!(presence_due(s(65), Some(s(60))));
    }

    #[test]
    fn csv_game_name_parses_mangohud_filenames() {
        assert_eq!(
            csv_game_name("Outlaws_2026-07-11_07-15-40.csv").as_deref(),
            Some("Outlaws")
        );
        // Underscores in the game name survive — only the timestamp suffix is peeled.
        assert_eq!(
            csv_game_name("Blood_West_2026-07-11_07-15-40.csv").as_deref(),
            Some("Blood_West")
        );
    }

    #[test]
    fn csv_game_name_rejects_non_mangohud_files() {
        for name in [
            "perf.data",
            "beisl.log",
            "game.log",
            "notes.csv",                       // no timestamp suffix
            "Outlaws_2026-07-11.csv",          // date only
            "_2026-07-11_07-15-40.csv",        // empty game name
            "Outlaws_2026-7-11_07-15-40.csv",  // malformed date
            "Outlaws_2026-07-11_07-15-4x.csv", // non-digit in time
        ] {
            assert_eq!(csv_game_name(name), None, "{name}");
        }
    }

    /// A configured pin must always survive into the session's human-readable label
    /// (`session::Session::gpu_pin`), the same field the session window shows. This locks
    /// down the mapping `trace()` does inline (`cfg.gpu_pin.as_ref().map(|p| p.name...)`).
    #[test]
    fn session_gpu_pin_label_mirrors_the_configured_pins_name() {
        let pin = GpuPin {
            name: "Intel — intel_icd (/usr/share/vulkan/icd.d)".into(),
            icd_paths: vec!["/a.json".into()],
            device_select: None,
        };
        let cfg = preflight::Config {
            gpu_pin: Some(pin.clone()),
            ..Default::default()
        };
        let label = cfg.gpu_pin.as_ref().map(|p| p.name.clone());
        assert_eq!(label.as_deref(), Some(pin.name.as_str()));
    }

    #[test]
    fn path_contains_dir_matches_the_exact_entry_only() {
        use std::ffi::OsStr;
        let target = Path::new("/home/tester/.local/bin");
        assert!(path_contains_dir(
            Some(OsStr::new("/usr/bin:/home/tester/.local/bin:/bin")),
            target
        ));
        // Steam's scrub: only the container's minimal PATH survives.
        assert!(!path_contains_dir(
            Some(OsStr::new("/usr/bin:/bin")),
            target
        ));
        // A same-named directory for a DIFFERENT user must not count as a match.
        assert!(!path_contains_dir(
            Some(OsStr::new("/usr/bin:/home/someone-else/.local/bin")),
            target
        ));
        assert!(!path_contains_dir(None, target));
    }

    #[test]
    fn steam_compat_markers_cover_native_and_proton_launches() {
        // Sanity on the marker set itself — the actual env reads (`launched_by_compat_tool`,
        // `launched_by_steam_client`) aren't unit-tested (it'd mean mutating the process's
        // real env, racy under parallel tests); this locks down that both launch shapes
        // have a marker in the compat-tool list.
        assert!(STEAM_COMPAT_MARKERS.contains(&"SteamAppId"));
        assert!(STEAM_COMPAT_MARKERS.contains(&"STEAM_COMPAT_APP_ID"));
    }

    #[test]
    fn client_only_markers_are_disjoint_from_the_compat_tool_contract() {
        // The whole point of the second marker set: it must name vars a third-party
        // launcher on the same compat-tool contract would NOT also set.
        for m in STEAM_CLIENT_ONLY_MARKERS {
            assert!(
                !STEAM_COMPAT_MARKERS.contains(m),
                "{m} must not double as a compat-tool marker"
            );
        }
    }

    #[test]
    fn decide_observation_gates_fixed_on_the_client_only_marker() {
        use preflight::SteamPathObservation::{Fixed, Scrubbed};
        // Scrubbed stays permissive either way — a false Scrubbed is harmless and
        // self-healing, so it doesn't need the stronger client-only marker.
        assert_eq!(decide_observation(false, false), Some(Scrubbed));
        assert_eq!(decide_observation(false, true), Some(Scrubbed));
        // Fixed needs the client-only marker: compat markers alone (a Heroic/Lutris/
        // Bottles launch) must NOT be enough to record the sticky, self-sealing "fixed".
        assert_eq!(decide_observation(true, false), None);
        assert_eq!(decide_observation(true, true), Some(Fixed));
    }

    #[test]
    fn parse_forced_mode_is_case_insensitive_and_rejects_garbage() {
        assert_eq!(
            parse_forced_mode(Some("degraded")),
            Some(RecordMode::Degraded)
        );
        assert_eq!(parse_forced_mode(Some("FULL")), Some(RecordMode::Full));
        assert_eq!(parse_forced_mode(Some("none")), Some(RecordMode::None));
        assert_eq!(parse_forced_mode(Some("bogus")), None);
        assert_eq!(parse_forced_mode(None), None);
    }

    #[test]
    fn launch_argv_full_and_none_run_the_game_directly() {
        let perf_data = Path::new("/tmp/beisl-test/perf.data");
        let prog = vec!["game.exe".to_string(), "-windowed".to_string()];
        for mode in [RecordMode::Full, RecordMode::None] {
            let (program, args) = launch_argv(mode, perf_data, &prog);
            assert_eq!(program, "game.exe");
            assert_eq!(args, vec!["-windowed".to_string()]);
        }
    }

    #[test]
    fn launch_argv_degraded_wraps_the_game_in_perf() {
        let perf_data = Path::new("/tmp/beisl-test/perf.data");
        let prog = vec!["game.exe".to_string(), "-windowed".to_string()];
        let (program, args) = launch_argv(RecordMode::Degraded, perf_data, &prog);
        assert_eq!(program, "perf");
        // perf's own flags, then the `--` separator, then the game's argv verbatim.
        let sep = args.iter().position(|a| a == "--").expect("-- separator");
        assert_eq!(&args[sep + 1..], &prog[..]);
        assert!(args[..sep].contains(&"-q".to_string()));
        assert!(!args[..sep].contains(&"-a".to_string()));
    }
}
