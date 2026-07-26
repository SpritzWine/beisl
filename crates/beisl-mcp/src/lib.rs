//! The MCP face of the trace library: serve past runs and their artifacts to an AI
//! client (DESIGN.md "The flywheel" — a trace nobody can read is a trace nobody fixes).
//!
//! A deliberately small server: JSON-RPC 2.0 over stdio (the MCP stdio transport is
//! newline-delimited JSON), one client, read-only. Hand-rolled on `serde_json` rather
//! than the official SDK: that SDK's async runtime schedules many concurrent sockets,
//! and this server has exactly one pipe and microsecond file reads — all weight, no
//! lift. Shipped as its own binary so the wrapper stays a wrapper; nothing here runs
//! unless a user wires `beisl-mcp` into their client's config.
//!
//! Tools (all read-only): `list_traces`, `get_setup_status`, `get_artifact`,
//! `get_report`, `get_log`, `get_crash_summary`, `list_events`.
//! Deleting and analyzing stay in the GUI/CLI, where a human confirms them.

use std::io::{self, BufRead, Write};
use std::path::PathBuf;

use serde_json::{json, Value};

use session::library::{self, ARTIFACT_FILE, REPORT_FILE};

/// The MCP revision this server was written against. We echo the client's requested
/// version instead of insisting on ours — every revision to date speaks our tiny
/// subset (initialize / tools/list / tools/call) identically.
pub const PROTOCOL_VERSION: &str = "2025-06-18";

/// `get_log` returns at most this much from the end of a log — launcher chains spam
/// game.log with megabytes of loader noise, and the useful lines (errors, device-lost,
/// MangoHud asserts) cluster at the tail.
const LOG_TAIL_BYTES: u64 = 64 * 1024;

/// A game.log line is worth surfacing when it smells like one of these. Lowercase;
/// matched case-insensitively. Shared by `get_crash_summary` (the deep-dive dump, up to
/// `MAX_SIGNAL_LINES`) and `list_events` (only cares whether any exist at all, to gate
/// its conservative `log-signal` kind — see [`Server::list_events`]).
const LOG_SIGNALS: &[&str] = &[
    "err:",   // wine's own error channel
    "error",  // MANGOHUD [error], generic loaders
    "assert", // assertion failures (the #2053 shape)
    "abort",
    "panic",
    "traceback",
    "exception",
    "segfault",
    "sigsegv",
    "device lost",
    "device_lost",
    "out of memory",
];
const MAX_SIGNAL_LINES: usize = 40;
const MAX_LINE_LEN: usize = 300;

pub struct Server {
    traces_root: PathBuf,
}

impl Server {
    pub fn new(traces_root: PathBuf) -> Self {
        Self { traces_root }
    }

    /// Pump the stdio transport until EOF (the client hanging up is the shutdown).
    pub fn serve(&self, input: impl BufRead, mut out: impl Write) -> io::Result<()> {
        for line in input.lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            if let Some(resp) = self.handle_line(&line) {
                writeln!(out, "{resp}")?;
                out.flush()?;
            }
        }
        Ok(())
    }

    /// One JSON-RPC message in, at most one out. Notifications (no `id`) get no
    /// response by definition — that includes `notifications/initialized`.
    pub fn handle_line(&self, line: &str) -> Option<String> {
        let msg: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => return Some(error_response(Value::Null, -32700, "parse error")),
        };
        let method = msg
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let id = match msg.get("id") {
            Some(i) if !i.is_null() => i.clone(),
            _ => return None,
        };
        let params = msg.get("params").cloned().unwrap_or(Value::Null);

        let result = match method {
            "initialize" => Ok(self.initialize(&params)),
            "ping" => Ok(json!({})),
            "tools/list" => Ok(tools_list()),
            "tools/call" => Ok(self.tools_call(&params)),
            other => Err(format!("method not found: {other}")),
        };
        Some(match result {
            Ok(r) => json!({"jsonrpc": "2.0", "id": id, "result": r}).to_string(),
            Err(m) => error_response(id, -32601, &m),
        })
    }

    fn initialize(&self, params: &Value) -> Value {
        let requested = params
            .get("protocolVersion")
            .and_then(Value::as_str)
            .unwrap_or(PROTOCOL_VERSION);
        json!({
            "protocolVersion": requested,
            "capabilities": { "tools": {} },
            "serverInfo": { "name": "beisl", "version": env!("CARGO_PKG_VERSION") },
            "instructions":
                "beisl records game performance traces (frametimes + CPU sampling, \
                 wine-agnostic). Start with list_traces; an analyzed run has an \
                 artifact (get_artifact: layer split of who ate the frame, frametime \
                 spikes, hottest functions, diagnostics) and a human-readable report \
                 (get_report). get_log serves the run's beisl/game/perf logs for \
                 crash forensics."
        })
    }

    fn tools_call(&self, params: &Value) -> Value {
        let name = params
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let args = params.get("arguments").cloned().unwrap_or(json!({}));
        match self.dispatch(name, &args) {
            Ok(text) => json!({"content": [{"type": "text", "text": text}], "isError": false}),
            Err(e) => json!({"content": [{"type": "text", "text": e}], "isError": true}),
        }
    }

    fn dispatch(&self, name: &str, args: &Value) -> Result<String, String> {
        match name {
            "list_traces" => Ok(self.list_traces()),
            "get_setup_status" => Ok(self.setup_status()),
            "get_crash_summary" => self.crash_summary(args),
            "list_events" => Ok(self.list_events(args)),
            "get_artifact" => self.read_run_file(args, ARTIFACT_FILE, None),
            "get_report" => self.read_run_file(args, REPORT_FILE, None),
            "get_log" => {
                // A fixed name map — the run_id is the only user-shaped path input.
                let file = match args.get("log").and_then(Value::as_str) {
                    Some("beisl") => "beisl.log",
                    Some("game") => "game.log",
                    Some("perf") => "perf.log",
                    other => {
                        return Err(format!(
                            "unknown log {other:?} — expected \"beisl\", \"game\" or \"perf\""
                        ))
                    }
                };
                self.read_run_file(args, file, Some(LOG_TAIL_BYTES))
            }
            other => Err(format!("unknown tool: {other}")),
        }
    }

    /// "Which runs are stuck?" (Florian, 2026-07-19, working this machine's actual
    /// traces root: 5 runs sat in live session states — Recording/Saved — from before
    /// the current boot, and this tool had no way to say so) was unanswerable over
    /// MCP before `state`/`stale` existed here: an AI client could see `analyzed`
    /// but not the session state a stuck run was left in, nor tell a genuinely live
    /// run from one whose wrapper died - with a previous boot, or (via the pid probe)
    /// within the current one.
    fn list_traces(&self) -> String {
        let runs: Vec<Value> = library::scan(&self.traces_root)
            .iter()
            .map(|r| {
                json!({
                    "run_id": r.id,
                    "game": r.title(),
                    "started_unix": r.started,
                    "elapsed_secs": r.session.as_ref().map(|s| s.elapsed_secs),
                    "analyzed": r.has_artifact,
                    "has_report": r.has_report,
                    // `None` (-> JSON null) mirrors the crate's existing optional-field
                    // idiom (see elapsed_secs above): missing/corrupt session.json.
                    // Serialized via serde, not Debug: the API vocabulary must stay
                    // structurally identical to what session.json itself contains.
                    "state": r.session.as_ref().map(|s| serde_json::json!(s.state)),
                    "stale": r.is_stale(),
                    // A prior failed analysis attempt's error, or null — see
                    // `session::library::invalid_of` (cleared once perf.data changes).
                    "invalid": r.invalid,
                })
            })
            .collect();
        json!({ "traces": runs, "traces_root": self.traces_root.display().to_string() }).to_string()
    }

    /// The preflight checklist, read-only — the same assembly `beisl --check` and the
    /// setup page render, so an AI can explain *why* a trace looks the way it does
    /// ("60% of driver samples unnamed → the debug-symbols row is red") and name the
    /// remedy. Fix commands are returned as text and never executed here: advising is
    /// MCP's job, privilege stays behind the GUI's pkexec click (the consent stance).
    fn setup_status(&self) -> String {
        let distro = preflight::detect_distro();
        let system = preflight::detect_system_kind();
        let mut checks = preflight::all_checks();
        for c in checks.iter_mut() {
            c.detect();
        }
        checks.push(preflight::disk_check(&self.traces_root));
        checks.push(preflight::gpu_dump_check());
        checks.push(preflight::multi_gpu_check());
        checks.push(preflight::driver_debuginfo_check());
        checks.push(preflight::perf_debuginfod_check());
        checks.push(preflight::mangohud_hotkey_check());
        checks.push(preflight::steam_visible_beisl_check());

        let rows: Vec<Value> = checks
            .iter()
            .map(|c| {
                let status = match &c.status {
                    preflight::Status::Ok => "ok".to_string(),
                    preflight::Status::NeedsFix => "needs setup".to_string(),
                    preflight::Status::Failed(e) => format!("failed: {e}"),
                    other => format!("{other:?}"),
                };
                let fix = c.fix.as_ref().map(|f| {
                    let cmd = preflight::fix_command(f, distro, system);
                    if cmd.is_empty() {
                        Value::String("(install manually — unknown for this distro)".into())
                    } else {
                        Value::String(cmd.join(" "))
                    }
                });
                json!({
                    "id": format!("{:?}", c.id),
                    "label": c.label,
                    "status": status,
                    "blocking": c.blocking,
                    // Informational-only rows the setup GUI shows in its Advanced section
                    // rather than the main checklist (currently only a confirmed-fixed
                    // Steam launcher shim) — surfaced so an advisor doesn't overstate it.
                    "advanced": c.advanced,
                    "why": c.why,
                    "fix_command": fix,
                })
            })
            .collect();
        // Surfaced separately from the PerfParanoid row: that check's why now explains the
        // degraded fallback in prose, but an advisor asking "can it record right now" wants
        // the answer as data, not text-mined out of a sentence.
        let mode = preflight::detect_record_mode();
        json!({
            "distro_family": format!("{distro:?}"),
            "checks": rows,
            "record_mode": { "mode": mode.as_str(), "note": mode.describe() },
            "note": "fix_command is advice for the user to run themselves (or via the \
                     setup app) — this server never executes anything."
        })
        .to_string()
    }

    /// Pre-chewed crash forensics: the ten lines that matter instead of megabytes of
    /// launcher spam — the wrapper's verdict lines, the session's own warnings (that's
    /// where GPU-hang findings land), and the error-shaped lines from the game log's
    /// tail. Shaped by a real case: a MangoHud ImGui assert in game.log was the whole
    /// story of a "the game just died" report (upstream #2053).
    fn crash_summary(&self, args: &Value) -> Result<String, String> {
        let id = self.run_id(args)?;
        let dir = self.traces_root.join(&id);
        let session = session::Session::read(&dir);

        // The wrapper's own log: launch, exit status, post-run verdict, GPU events.
        let wrapper =
            wrapper_lines(&std::fs::read_to_string(dir.join("beisl.log")).unwrap_or_default());

        // Error-shaped lines from the tail of the game/launcher output.
        let (errors, dropped) = signal_lines(&log_tail(&dir.join("game.log")));

        Ok(json!({
            "run_id": id,
            "game": session.as_ref().map(|s| s.game.clone()),
            // Serde, not Debug — same session.json-vocabulary rule as list_traces.
            "state": session.as_ref().map(|s| serde_json::json!(s.state)),
            "message": session.as_ref().map(|s| s.message.clone()),
            "elapsed_secs": session.as_ref().map(|s| s.elapsed_secs),
            "warnings": session.as_ref().map(|s| s.warnings.clone()),
            "wrapper_log": wrapper,
            "game_log_errors": errors,
            "game_log_errors_dropped": dropped,
            "analyzed": dir.join(ARTIFACT_FILE).is_file(),
        })
        .to_string())
    }

    /// "Anything crash since we last talked?" as one cheap call — an AI client (or a
    /// cron'd agent) polls this at session start with a `since_unix` cursor instead of
    /// walking `list_traces` + a `get_crash_summary` per run itself. Deliberately
    /// conservative: a clean run must produce ZERO events, or an agent polling this on
    /// every session start learns to ignore it. Each event's `detail` is a short
    /// pointer, not a dump — `get_crash_summary` is still the deep dive.
    ///
    /// Cursor semantics: a run is considered when its LAST ACTIVITY (see
    /// [`last_activity`]: the latest of its start time and the mtimes of its
    /// `session.json`/`beisl.log`) is `>= since_unix` — NOT just its start time. Filtering
    /// on start time alone would permanently drop any run still alive at poll time: the
    /// cursor advances past its (already-passed) start, so a crash that lands later —
    /// including staleness, which can surface anywhere from moments after the wrapper
    /// dies (the pid-probe half of `RunEntry::is_stale`) to a whole reboot later (the
    /// pre-boot half) - is never re-examined on a later poll. The `>=`
    /// (kept, not tightened to `>`) means a run whose last activity lands exactly on a
    /// cursor value can reappear on the next poll that reuses that value: at-least-once
    /// delivery, not exactly-once. Clients should dedupe by `(run_id, kind)`.
    ///
    /// Event kinds, newest run first:
    /// * `crash` — the run is stale (`library::RunEntry::is_stale`: a still-live-looking
    ///   state whose wrapper is provably gone - either it started before the current
    ///   boot, or its recorded pid/starttime no longer matches `/proc`) in anything
    ///   OTHER than `Saved` (recorder stopped, game kept running - the session window
    ///   treats Saved+stale as "trace saved, the game has since exited", not a crash,
    ///   and so does this: an ordinary Stop-and-keep-playing followed by an ordinary
    ///   reboot must stay silent);
    ///   or `session.json` is `State::Error` (the wrapper failed to spawn the game at
    ///   all); or the wrapper's own `session::EXIT_LOG_MARKER` log line (see `run.rs`)
    ///   reports anything other than a clean `exit status: 0`.
    /// * `gpu` — a session warning mentions a GPU hang/devcoredump (`run.rs`'s
    ///   `drain_gpu_events` always folds "GPU" into that wording, so a case-insensitive
    ///   "gpu" match is exact, not a guess — and no OTHER warning text in this codebase
    ///   mentions a GPU, so it can't misfire on an unrelated warning).
    /// * `log-signal` — the game log's tail has an error-shaped line (`LOG_SIGNALS`),
    ///   but ONLY when a `crash`/`gpu` event already fired for the same run, or the run
    ///   is `invalid` (a prior analysis attempt failed against this `perf.data`).
    ///   Without that gate, ordinary loader noise ("err:" in a benign wine warning, say)
    ///   would turn every noisy-but-healthy run into an event — exactly the spam this
    ///   tool exists to avoid.
    fn list_events(&self, args: &Value) -> String {
        const MAX_EVENTS: usize = 100;

        let since = args.get("since_unix").and_then(Value::as_u64).unwrap_or(0);
        let now = now_unix();

        let mut events: Vec<Value> = Vec::new();
        for run in library::scan(&self.traces_root) {
            if last_activity(&run) < since {
                continue;
            }
            let beisl_log = std::fs::read_to_string(run.dir.join("beisl.log")).unwrap_or_default();
            let session = run.session.as_ref();
            let state = session.map(|s| s.state);
            // Saved+stale is the benign "trace saved — the game has since exited" case
            // (mirrors the session window's own stale handling): only the OTHER live
            // states (Starting/Running/Recording/Stopping) mean the wrapper that was
            // recording it is actually gone (pre-boot, or its pid provably dead now).
            let stale_crash = run.is_stale() && state != Some(session::State::Saved);
            let spawn_failed = state == Some(session::State::Error);
            let exit_crashed = wrapper_exit_crashed(&beisl_log).unwrap_or(false);
            let crash = stale_crash || spawn_failed || exit_crashed;
            let gpu_detail = session.and_then(gpu_detail);
            let game = run.title().to_string();

            if crash {
                events.push(json!({
                    "run_id": run.id, "unix": run.started, "game": game,
                    "kind": "crash", "detail": crash_detail(session, &beisl_log, stale_crash),
                }));
            }
            if let Some(detail) = &gpu_detail {
                events.push(json!({
                    "run_id": run.id, "unix": run.started, "game": game,
                    "kind": "gpu", "detail": detail,
                }));
            }
            if crash || gpu_detail.is_some() || run.invalid.is_some() {
                let (lines, _) = signal_lines(&log_tail(&run.dir.join("game.log")));
                if let Some(newest) = lines.last() {
                    events.push(json!({
                        "run_id": run.id, "unix": run.started, "game": game,
                        "kind": "log-signal", "detail": newest,
                    }));
                }
            }
        }

        let total = events.len();
        events.truncate(MAX_EVENTS);
        json!({ "events": events, "now_unix": now, "truncated": total > MAX_EVENTS }).to_string()
    }

    /// Read one well-known file out of one run dir. The run_id gate mirrors
    /// `library::delete_run`'s stance: only `run-*` names, no path fragments — the
    /// filename itself is never user input.
    fn read_run_file(&self, args: &Value, file: &str, tail: Option<u64>) -> Result<String, String> {
        let id = self.run_id(args)?;
        let path = self.traces_root.join(&id).join(file);
        let data = std::fs::read(&path)
            .map_err(|e| format!("{id}/{file}: {e} (is the run analyzed? see list_traces)"))?;
        let text = match tail {
            Some(max) if (data.len() as u64) > max => {
                let cut = data.len() - max as usize;
                format!(
                    "[…{cut} earlier bytes omitted — tail of {file}]\n{}",
                    String::from_utf8_lossy(&data[cut..])
                )
            }
            _ => String::from_utf8_lossy(&data).into_owned(),
        };
        Ok(text)
    }
}

impl Server {
    /// The run_id gate every path-touching tool goes through — mirrors
    /// `library::delete_run`'s stance: only `run-*` names, no path fragments.
    fn run_id(&self, args: &Value) -> Result<String, String> {
        let id = args
            .get("run_id")
            .and_then(Value::as_str)
            .ok_or("run_id is required (see list_traces)")?;
        if !id.starts_with("run-") || id.contains('/') || id.contains("..") {
            return Err(format!("not a run id: {id}"));
        }
        Ok(id.to_string())
    }
}

fn clip(line: &str, max: usize) -> String {
    if line.len() <= max {
        return line.to_string();
    }
    let mut cut = max;
    while !line.is_char_boundary(cut) {
        cut -= 1;
    }
    format!("{}…", &line[..cut])
}

/// The last `LOG_TAIL_BYTES` of `path`, lossy-UTF8 decoded — cheap even for a
/// launcher-chain log that's grown to megabytes of loader noise. Empty (not an error)
/// when the file is missing, mirroring the rest of this server's "a run dir is never
/// let down by one missing file" stance.
fn log_tail(path: &std::path::Path) -> String {
    let data = std::fs::read(path).unwrap_or_default();
    let start = data.len().saturating_sub(LOG_TAIL_BYTES as usize);
    String::from_utf8_lossy(&data[start..]).into_owned()
}

/// The wrapper-log lines worth surfacing to a human forensics reader: launch/exit/
/// post-run verdicts, wrapper-level errors, and GPU events. Shared by `get_crash_summary`
/// (the deep dive) and `crash_detail` below (the one-liner).
fn wrapper_lines(beisl_log: &str) -> Vec<String> {
    beisl_log
        .lines()
        .filter(|l| {
            let ll = l.to_lowercase();
            ll.contains("exited")
                || ll.contains("post-run")
                || ll.contains("error")
                || ll.contains("gpu")
        })
        .map(|l| clip(l, MAX_LINE_LEN))
        .collect()
}

/// Filter `text`'s lines to the error-shaped ones (see `LOG_SIGNALS`), clipped and capped
/// at `MAX_SIGNAL_LINES` — dropping the OLDEST over the cap, since the most recent lines
/// are the ones most likely to explain "why did it just die". Returns (kept, dropped
/// count). Shared by `get_crash_summary` (the deep dive) and `list_events` (which only
/// cares whether any exist, and about the newest one — see `Server::list_events`).
fn signal_lines(text: &str) -> (Vec<String>, usize) {
    let mut lines: Vec<String> = text
        .lines()
        .filter(|l| {
            let ll = l.to_lowercase();
            LOG_SIGNALS.iter().any(|s| ll.contains(s))
        })
        .map(|l| clip(l, MAX_LINE_LEN))
        .collect();
    let dropped = lines.len().saturating_sub(MAX_SIGNAL_LINES);
    if dropped > 0 {
        lines.drain(..dropped);
    }
    (lines, dropped)
}

/// Whether the wrapper's own `session::EXIT_LOG_MARKER` log line (see `run.rs`) reports
/// anything other than a clean `exit status: 0` — nonzero codes and signals both count.
/// `None` when that line hasn't been written yet (the run is still live, or the wrapper
/// never got that far — see `State::Error`, handled separately by `list_events`): no
/// evidence either way, so the caller's default must be "not a crash".
fn wrapper_exit_crashed(beisl_log: &str) -> Option<bool> {
    let line = beisl_log
        .lines()
        .find(|l| l.contains(session::EXIT_LOG_MARKER))?;
    let status = line
        .split(session::EXIT_LOG_MARKER)
        .nth(1)?
        .trim_start()
        .split(" (with launcher chains")
        .next()?;
    Some(status != "exit status: 0")
}

/// `list_events`'s `crash` detail: which evidence tripped it, in priority order — a
/// stale/stuck run first (there's no exit line to explain those), then the wrapper's own
/// spawn failure (`State::Error`, set before the game ever ran), then the wrapper's exit-
/// status line itself. Always a short pointer, never the forensics — that's
/// `get_crash_summary`'s job.
fn crash_detail(session: Option<&session::Session>, beisl_log: &str, stale: bool) -> String {
    if stale {
        let state = session
            .map(|s| format!("{:?}", s.state))
            .unwrap_or_else(|| "unknown".to_string());
        return format!("stuck in {state} - the wrapper that was recording it is gone");
    }
    if session.map(|s| s.state) == Some(session::State::Error) {
        return match session.map(|s| s.message.as_str()) {
            Some(m) if !m.is_empty() => clip(m, MAX_LINE_LEN),
            _ => "wrapper failed to launch the game".to_string(),
        };
    }
    match beisl_log
        .lines()
        .find(|l| l.contains(session::EXIT_LOG_MARKER))
    {
        Some(line) => clip(line, MAX_LINE_LEN),
        None => "did not exit cleanly".to_string(),
    }
}

/// `list_events`'s `gpu` detail — and, via `is_some()`, its `gpu` gate itself: the first
/// session warning that mentions a GPU hang/devcoredump (see `run.rs::drain_gpu_events`
/// for the exact wording; no other warning text in this codebase mentions a GPU, so a
/// case-insensitive "gpu" match is exact here, not a guess). `None` when the run has no
/// such warning.
fn gpu_detail(session: &session::Session) -> Option<String> {
    session
        .warnings
        .iter()
        .find(|w| w.to_lowercase().contains("gpu"))
        .map(|w| clip(w, MAX_LINE_LEN))
}

/// A run's LAST ACTIVITY, for `list_events`'s `since_unix` cursor (see that method's doc
/// for why start time alone isn't enough): the latest of the run's start time and the
/// mtimes of `session.json` and `beisl.log` — both files the wrapper keeps rewriting for
/// as long as a run is live, and both touched by every kind of crash evidence this tool
/// looks for (a spawn failure rewrites session.json once more; a normal or abnormal exit
/// rewrites both).
fn last_activity(run: &library::RunEntry) -> u64 {
    run.started
        .max(mtime_secs(&run.dir.join(session::FILE)))
        .max(mtime_secs(&run.dir.join("beisl.log")))
}

/// `path`'s mtime as unix seconds, `0` on any failure (missing file, unreadable
/// metadata, clock before the epoch) — same conservative-fallback shape as
/// `session::library`'s own `perf_data_stamp`.
fn mtime_secs(path: &std::path::Path) -> u64 {
    std::fs::metadata(path)
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Wall-clock "now" as unix seconds — the cursor a client should store and pass back as
/// `since_unix` on its next `list_events` call. `0` on the (essentially unreachable) case
/// of a clock before the epoch, so a broken clock degrades to "next poll sees everything
/// again" rather than a panic.
fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn tools_list() -> Value {
    let run_id =
        json!({ "type": "string", "description": "A run id from list_traces (run-<unix_ts>)" });
    json!({ "tools": [
        {
            "name": "list_traces",
            "description": "List all recorded game-performance runs, newest first: run_id, \
                            game, start time, duration, whether an analysis artifact \
                            exists, the session state, and whether the run is stale \
                            (a live-looking state whose wrapper is provably gone - \
                            either it started before the current boot, or its pid died \
                            within this one - so nothing can still own it: a stuck run, \
                            not an active one).",
            "inputSchema": { "type": "object", "properties": {}, "additionalProperties": false }
        },
        {
            "name": "get_setup_status",
            "description": "The preflight checklist, read-only: perf, symbolizer, paranoid \
                            level, MangoHud, disk, GPU dump access, multi-GPU, driver debug \
                            symbols — each with status, why it matters, and the fix command \
                            as text (never executed). Use it to explain missing symbol names \
                            or to advise setup.",
            "inputSchema": { "type": "object", "properties": {}, "additionalProperties": false }
        },
        {
            "name": "get_artifact",
            "description": "The analyzed trace as JSON: frame stats and spikes, the per-layer \
                            CPU split (who ate the frame: kernel/driver/dxvk/wine/game), \
                            hottest functions, timeline, diagnostics. Only analyzed runs \
                            have one.",
            "inputSchema": { "type": "object", "properties": { "run_id": run_id },
                             "required": ["run_id"], "additionalProperties": false }
        },
        {
            "name": "get_report",
            "description": "The human-readable analysis report for a run (same content as \
                            the artifact, prose-shaped).",
            "inputSchema": { "type": "object", "properties": { "run_id": run_id },
                             "required": ["run_id"], "additionalProperties": false }
        },
        {
            "name": "get_log",
            "description": "Tail of one of the run's logs: 'beisl' (the wrapper's own log), \
                            'game' (the game/launcher stdout+stderr — crash forensics live \
                            here), or 'perf' (the recorder).",
            "inputSchema": { "type": "object",
                             "properties": {
                                 "run_id": run_id,
                                 "log": { "type": "string", "enum": ["beisl", "game", "perf"] }
                             },
                             "required": ["run_id", "log"], "additionalProperties": false }
        },
        {
            "name": "get_crash_summary",
            "description": "Pre-chewed crash forensics for a run: the wrapper's exit/\
                            post-run verdict lines, the session's warnings (GPU hangs \
                            land there), and the error-shaped lines from the game log \
                            tail (wine err:, asserts, aborts, device-lost, MangoHud \
                            errors). Start here when a game 'just died'.",
            "inputSchema": { "type": "object", "properties": { "run_id": run_id },
                             "required": ["run_id"], "additionalProperties": false }
        },
        {
            "name": "list_events",
            "description": "\"Anything crash since we last talked?\" as one cheap call: \
                            crash/hang events from every run active at or after \
                            since_unix (default 0 = everything; a run counts as active \
                            up to the last time its session.json or beisl.log was \
                            written, not just when it started — a run still alive at \
                            your last poll is never silently skipped), newest first — no \
                            need to walk list_traces yourself. Cursor pattern: pass the \
                            response's now_unix back as since_unix on your next call to \
                            see only what's new; this is at-least-once delivery (a run \
                            active exactly on the cursor can repeat once), so dedupe by \
                            (run_id, kind) if that matters to you. Conservative by design \
                            (a clean run is silent — no events): 'crash' (abnormal exit, \
                            wrapper launch failure, or a stuck/stale run whose wrapper is \
                            provably gone - from a previous boot, or its pid dying within \
                            this one - but NOT a run that was cleanly stopped and kept \
                            playing before an ordinary reboot), 'gpu' (a GPU hang/\
                            devcoredump warning), 'log-signal' (an error-shaped game-log \
                            line, only alongside a harder signal or an invalid run). Each \
                            event's detail is a short pointer — get_crash_summary is the \
                            deep dive. Capped at the newest 100 events (truncated: true \
                            if hit).",
            "inputSchema": { "type": "object",
                             "properties": {
                                 "since_unix": { "type": "integer", "minimum": 0,
                                     "description": "Unix seconds; only runs active at or \
                                         after this (see the tool description) are \
                                         considered. Default 0 (every run). Pass the \
                                         previous call's now_unix here to poll for what's \
                                         new." }
                             },
                             "additionalProperties": false }
        }
    ]})
}

fn error_response(id: Value, code: i64, message: &str) -> String {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}}).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn wire_identifiers_are_pinned() {
        // These Debug strings ARE the MCP wire format: setup_status row ids and the
        // crash summary's session state. From 0.1.0 on they're a compatibility
        // promise — renaming a variant must be a conscious wire-format decision
        // (breaking this test), never silent refactoring fallout.
        use preflight::CheckId::*;
        let ids: Vec<String> = [
            Perf,
            LlvmSymbolizer,
            PerfParanoid,
            MangoHud,
            DiskSpace,
            GpuDumpAccess,
            MangoHudHotkey,
            MultiGpu,
            KernelSymbols,
            DriverDebuginfo,
            PerfDebuginfod,
            SteamVisibleBeisl,
        ]
        .iter()
        .map(|i| format!("{i:?}"))
        .collect();
        assert_eq!(
            ids,
            [
                "Perf",
                "LlvmSymbolizer",
                "PerfParanoid",
                "MangoHud",
                "DiskSpace",
                "GpuDumpAccess",
                "MangoHudHotkey",
                "MultiGpu",
                "KernelSymbols",
                "DriverDebuginfo",
                "PerfDebuginfod",
                "SteamVisibleBeisl",
            ]
        );
        use session::State::*;
        let states: Vec<String> = [Starting, Running, Recording, Stopping, Saved, Done, Error]
            .iter()
            .map(|s| format!("{s:?}"))
            .collect();
        assert_eq!(
            states,
            [
                "Starting",
                "Running",
                "Recording",
                "Stopping",
                "Saved",
                "Done",
                "Error"
            ]
        );
    }

    fn fixture_root(tag: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("beisl-mcp-{tag}-{}", std::process::id()));
        std::fs::remove_dir_all(&root).ok();
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    fn put_run(root: &Path, id: &str, analyzed: bool) {
        let dir = root.join(id);
        std::fs::create_dir_all(&dir).unwrap();
        session::Session {
            game: "Blood West.exe".into(),
            elapsed_secs: 120,
            ..Default::default()
        }
        .write(&dir)
        .unwrap();
        std::fs::write(dir.join("beisl.log"), b"launch argv = ...\n").unwrap();
        if analyzed {
            std::fs::write(dir.join(ARTIFACT_FILE), br#"{"schema_version":1}"#).unwrap();
            std::fs::write(dir.join(REPORT_FILE), b"who ate the frame: kernel\n").unwrap();
        }
    }

    fn call(server: &Server, req: Value) -> Value {
        serde_json::from_str(&server.handle_line(&req.to_string()).expect("a response")).unwrap()
    }

    #[test]
    fn initialize_echoes_the_clients_protocol_version() {
        let server = Server::new(fixture_root("init"));
        let resp = call(
            &server,
            json!({"jsonrpc":"2.0","id":1,"method":"initialize",
                   "params":{"protocolVersion":"2024-11-05"}}),
        );
        assert_eq!(resp["result"]["protocolVersion"], "2024-11-05");
        assert_eq!(resp["result"]["serverInfo"]["name"], "beisl");
        assert!(resp["result"]["capabilities"]["tools"].is_object());
    }

    #[test]
    fn notifications_get_no_response() {
        let server = Server::new(fixture_root("notif"));
        let none = server.handle_line(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#);
        assert!(none.is_none());
    }

    #[test]
    fn the_seven_tools_are_listed() {
        let server = Server::new(fixture_root("list"));
        let resp = call(
            &server,
            json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}),
        );
        let names: Vec<&str> = resp["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        assert_eq!(
            names,
            [
                "list_traces",
                "get_setup_status",
                "get_artifact",
                "get_report",
                "get_log",
                "get_crash_summary",
                "list_events"
            ]
        );
    }

    #[test]
    fn list_and_read_round_trip() {
        let root = fixture_root("rt");
        put_run(&root, "run-100", true);
        put_run(&root, "run-200", false);
        let server = Server::new(root);

        let resp = call(
            &server,
            json!({"jsonrpc":"2.0","id":3,"method":"tools/call",
                   "params":{"name":"list_traces","arguments":{}}}),
        );
        assert_eq!(resp["result"]["isError"], false);
        let listing: Value =
            serde_json::from_str(resp["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(listing["traces"][0]["run_id"], "run-200"); // newest first
        assert_eq!(listing["traces"][1]["analyzed"], true);

        let resp = call(
            &server,
            json!({"jsonrpc":"2.0","id":4,"method":"tools/call",
                   "params":{"name":"get_artifact","arguments":{"run_id":"run-100"}}}),
        );
        assert_eq!(resp["result"]["isError"], false);
        assert!(resp["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("schema_version"));

        // Un-analyzed run: a tool-level error that says why, not a protocol error.
        let resp = call(
            &server,
            json!({"jsonrpc":"2.0","id":5,"method":"tools/call",
                   "params":{"name":"get_artifact","arguments":{"run_id":"run-200"}}}),
        );
        assert_eq!(resp["result"]["isError"], true);
    }

    #[test]
    fn list_traces_reports_session_state_and_staleness() {
        // The real motivating case (2026-07-19): "which runs are stuck?" was
        // unanswerable over MCP — list_traces had `analyzed` but nothing about the
        // session state a stuck run was left in, nor a way to tell a genuinely live
        // run from one whose wrapper died - with a previous boot, or (via the pid
        // probe) within the current one.
        let root = fixture_root("state");
        // put_run's Session defaults to State::Starting (a live state) with a
        // `started` — from the run-* dir name — of 100: always long before this
        // (or any) real machine's boot time, so this run is stale.
        put_run(&root, "run-100", false);
        // A missing/corrupt session.json: state is null, the crate's existing
        // optional-field idiom (see elapsed_secs/game above) rather than a
        // made-up state — and never live, so never stale either.
        std::fs::create_dir_all(root.join("run-900")).unwrap();
        // A run whose analysis was attempted and failed (CarX's zero-byte perf.data
        // case): `invalid` should carry the recorded error.
        put_run(&root, "run-500", false);
        std::fs::write(root.join("run-500").join("perf.data"), b"").unwrap();
        library::mark_invalid(&root.join("run-500"), "perf script produced no samples").unwrap();

        let server = Server::new(root);
        let resp = call(
            &server,
            json!({"jsonrpc":"2.0","id":11,"method":"tools/call",
                   "params":{"name":"list_traces","arguments":{}}}),
        );
        assert_eq!(resp["result"]["isError"], false);
        let listing: Value =
            serde_json::from_str(resp["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
        let traces = listing["traces"].as_array().unwrap();

        let run_100 = traces.iter().find(|t| t["run_id"] == "run-100").unwrap();
        assert_eq!(run_100["state"], "Starting");
        assert_eq!(run_100["stale"], true);
        assert!(
            run_100["invalid"].is_null(),
            "no marker written for run-100"
        );

        let run_900 = traces.iter().find(|t| t["run_id"] == "run-900").unwrap();
        assert!(run_900["state"].is_null());
        assert_eq!(run_900["stale"], false);

        let run_500 = traces.iter().find(|t| t["run_id"] == "run-500").unwrap();
        assert_eq!(run_500["invalid"], "perf script produced no samples");
    }

    #[test]
    fn setup_status_reports_every_check_with_advice_shape() {
        let server = Server::new(fixture_root("setup"));
        let resp = call(
            &server,
            json!({"jsonrpc":"2.0","id":9,"method":"tools/call",
                   "params":{"name":"get_setup_status","arguments":{}}}),
        );
        assert_eq!(resp["result"]["isError"], false);
        let status: Value =
            serde_json::from_str(resp["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
        let checks = status["checks"].as_array().unwrap();
        // Statuses are host-dependent; the shape and the roster are not.
        assert!(checks.len() >= 8, "expected the full checklist");
        let ids: Vec<&str> = checks.iter().map(|c| c["id"].as_str().unwrap()).collect();
        for expected in ["Perf", "MangoHud", "DiskSpace", "MultiGpu"] {
            assert!(ids.contains(&expected), "missing {expected} in {ids:?}");
        }
        for c in checks {
            assert!(c["label"].is_string() && c["status"].is_string() && c["why"].is_string());
        }
        assert!(status["distro_family"].is_string());
        // Record mode is host-dependent too, but the shape (and a known-mode string) isn't.
        let mode = status["record_mode"]["mode"].as_str().unwrap();
        assert!(["full", "degraded", "none"].contains(&mode), "{mode}");
        assert!(status["record_mode"]["note"].is_string());
    }

    #[test]
    fn path_shaped_run_ids_are_refused() {
        let root = fixture_root("sec");
        put_run(&root, "run-100", true);
        let server = Server::new(root);
        for bad in ["../run-100", "run-100/../..", "/etc", "run-100/.."] {
            let resp = call(
                &server,
                json!({"jsonrpc":"2.0","id":6,"method":"tools/call",
                       "params":{"name":"get_report","arguments":{"run_id": bad}}}),
            );
            assert_eq!(resp["result"]["isError"], true, "must refuse {bad}");
        }
    }

    #[test]
    fn logs_are_tailed_not_dumped() {
        let root = fixture_root("tail");
        put_run(&root, "run-100", false);
        let big = "x".repeat(2 * LOG_TAIL_BYTES as usize);
        std::fs::write(root.join("run-100").join("game.log"), &big).unwrap();
        let server = Server::new(root);
        let resp = call(
            &server,
            json!({"jsonrpc":"2.0","id":7,"method":"tools/call",
                   "params":{"name":"get_log","arguments":{"run_id":"run-100","log":"game"}}}),
        );
        let text = resp["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.starts_with("[…"));
        assert!(text.len() < big.len());
    }

    #[test]
    fn crash_summary_surfaces_the_signal_not_the_spam() {
        let root = fixture_root("crash");
        put_run(&root, "run-100", false);
        let dir = root.join("run-100");
        std::fs::write(
            dir.join("beisl.log"),
            "12.3s launched pid 4242\n\
             99.1s launched command exited: exit status: 0 (with launcher chains this is the wrapper's status, not the game's), trace 16319876 bytes\n\
             99.1s post-run: clean exit — leaving raw trace in place\n",
        )
        .unwrap();
        // Real-world shape: thousands of benign loader lines around one fatal assert
        // (the MangoHud #2053 crash as captured in an actual game.log).
        let mut game = String::new();
        for _ in 0..500 {
            game.push_str(
                "gamemodeauto: dlopen failed - libgamemode.so: cannot open shared object file\n",
            );
        }
        game.push_str("../subprojects/imgui-1.91.6/imgui.cpp:10783: bool ImGui::ItemAdd(const ImRect&, ImGuiID, const ImRect*, ImGuiItemFlags): Assertion `id != window->ID && \"Cannot have an empty ID\"' failed.\n");
        game.push_str("some ordinary shutdown line\n");
        std::fs::write(dir.join("game.log"), game).unwrap();

        let server = Server::new(root);
        let resp = call(
            &server,
            json!({"jsonrpc":"2.0","id":10,"method":"tools/call",
                   "params":{"name":"get_crash_summary","arguments":{"run_id":"run-100"}}}),
        );
        assert_eq!(resp["result"]["isError"], false);
        let sum: Value =
            serde_json::from_str(resp["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
        let errors = sum["game_log_errors"].as_array().unwrap();
        assert!(
            errors
                .iter()
                .any(|l| l.as_str().unwrap().contains("Assertion")),
            "the assert must surface: {errors:?}"
        );
        // dlopen spam matches no signal; the summary stays context-cheap.
        assert!(
            errors.len() <= 2,
            "spam leaked through: {} lines",
            errors.len()
        );
        let wrapper = sum["wrapper_log"].as_array().unwrap();
        assert!(wrapper
            .iter()
            .any(|l| l.as_str().unwrap().contains("exited: exit status: 0")));
        assert_eq!(sum["analyzed"], false);
    }

    // -- list_events ----------------------------------------------------------------

    /// A run dir with full control over the bits `list_events` judges: state, warnings,
    /// and the two logs. Its `session.json`/`beisl.log` mtimes land at "now" (real
    /// wall-clock time), which is fine for every test that passes `since_unix: 0` — the
    /// cutoff test and friends that need a deterministic LAST ACTIVITY use
    /// `put_events_run_at` instead, which pins those mtimes explicitly.
    fn put_events_run(
        root: &Path,
        id: &str,
        state: session::State,
        warnings: Vec<String>,
        beisl_log: &str,
        game_log: &str,
    ) {
        let dir = root.join(id);
        std::fs::create_dir_all(&dir).unwrap();
        session::Session {
            game: "Blood West.exe".into(),
            state,
            warnings,
            ..Default::default()
        }
        .write(&dir)
        .unwrap();
        std::fs::write(dir.join("beisl.log"), beisl_log).unwrap();
        std::fs::write(dir.join("game.log"), game_log).unwrap();
    }

    /// Like `put_events_run`, but additionally pins the mtimes of `session.json` and
    /// `beisl.log` to `started` — so this run's LAST ACTIVITY (see `last_activity`) is
    /// deterministically exactly `started`, instead of whatever wall-clock instant the
    /// test happened to run at. Needed for anything that asserts a `since_unix` cutoff
    /// or boundary; `put_events_run` alone is fine when only a run's EVENT CONTENT, not
    /// its cursor timing, is under test. Returns the run id (`run-<started>`) it wrote.
    fn put_events_run_at(
        root: &Path,
        started: u64,
        state: session::State,
        warnings: Vec<String>,
        beisl_log: &str,
        game_log: &str,
    ) -> String {
        let id = format!("run-{started}");
        put_events_run(root, &id, state, warnings, beisl_log, game_log);
        let dir = root.join(&id);
        set_mtime(&dir.join(session::FILE), started);
        set_mtime(&dir.join("beisl.log"), started);
        id
    }

    /// Set `path`'s mtime to `unix_secs` — `File::set_modified` needs a handle opened
    /// for writing (utimes requires write access on Unix), which is why this isn't just
    /// `std::fs::metadata`'s read-only sibling.
    fn set_mtime(path: &Path, unix_secs: u64) {
        let f = std::fs::OpenOptions::new().write(true).open(path).unwrap();
        f.set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(unix_secs))
            .unwrap();
    }

    const CLEAN_EXIT: &str = "launched pid 1\n\
        99.0s launched command exited: exit status: 0 (with launcher chains this is the \
        wrapper's status, not the game's), trace 12345 bytes\n\
        99.0s post-run: clean exit — leaving raw trace in place\n";

    fn crashed_exit(code: i32) -> String {
        format!(
            "launched pid 1\n\
             99.0s launched command exited: exit status: {code} (with launcher chains this \
             is the wrapper's status, not the game's), trace 12345 bytes\n"
        )
    }

    fn events_of(server: &Server, since_unix: u64) -> Value {
        let resp = call(
            server,
            json!({"jsonrpc":"2.0","id":20,"method":"tools/call",
                   "params":{"name":"list_events",
                             "arguments":{"since_unix": since_unix}}}),
        );
        assert_eq!(resp["result"]["isError"], false);
        serde_json::from_str(resp["result"]["content"][0]["text"].as_str().unwrap()).unwrap()
    }

    #[test]
    fn clean_run_produces_no_events() {
        let root = fixture_root("events-clean");
        put_events_run(
            &root,
            "run-100",
            session::State::Done,
            vec![],
            CLEAN_EXIT,
            "ordinary shutdown line\n",
        );
        let listing = events_of(&Server::new(root), 0);
        assert_eq!(listing["events"].as_array().unwrap(), &Vec::<Value>::new());
        assert_eq!(listing["truncated"], false);
        assert!(listing["now_unix"].as_u64().unwrap() > 0);
    }

    #[test]
    fn nonzero_exit_is_a_crash_event() {
        let root = fixture_root("events-crash");
        put_events_run(
            &root,
            "run-100",
            session::State::Done,
            vec![],
            &crashed_exit(139),
            "ordinary shutdown line\n", // no log-signal spam, so this stays one event
        );
        let listing = events_of(&Server::new(root), 0);
        let events = listing["events"].as_array().unwrap();
        assert_eq!(events.len(), 1, "{events:?}");
        assert_eq!(events[0]["run_id"], "run-100");
        assert_eq!(events[0]["kind"], "crash");
        assert!(events[0]["detail"]
            .as_str()
            .unwrap()
            .contains("exit status: 139"));
    }

    #[test]
    fn gpu_warning_is_a_gpu_event() {
        let root = fixture_root("events-gpu");
        put_events_run(
            &root,
            "run-100",
            session::State::Done,
            vec![
                "Something went wrong and your GPU was the cause — not the game's code \
                  and not Wine. Beisl saved the crash evidence for you (dump.txt). \
                  (GPU: card0)"
                    .to_string(),
            ],
            CLEAN_EXIT, // a clean exit: this is GPU-only, not also a crash event
            "ordinary shutdown line\n",
        );
        let listing = events_of(&Server::new(root), 0);
        let events = listing["events"].as_array().unwrap();
        assert_eq!(events.len(), 1, "{events:?}");
        assert_eq!(events[0]["kind"], "gpu");
        assert!(events[0]["detail"].as_str().unwrap().contains("GPU"));
    }

    #[test]
    fn since_unix_excludes_runs_whose_last_activity_predates_it() {
        // Mtimes pinned via put_events_run_at: without that, both runs' session.json/
        // beisl.log would land at "now" (real wall-clock time), which is always well
        // past since_unix: 500 regardless of the run's nominal start — defeating the
        // very cutoff this test means to check. See `Server::list_events`'s doc for why
        // the comparison is against LAST ACTIVITY, not just start time.
        let root = fixture_root("events-cutoff");
        put_events_run_at(
            &root,
            100,
            session::State::Done,
            vec![],
            &crashed_exit(1),
            "",
        );
        put_events_run_at(
            &root,
            900,
            session::State::Done,
            vec![],
            &crashed_exit(1),
            "",
        );
        let listing = events_of(&Server::new(root), 500);
        let events = listing["events"].as_array().unwrap();
        assert_eq!(events.len(), 1, "{events:?}");
        assert_eq!(events[0]["run_id"], "run-900");
    }

    #[test]
    fn since_unix_equal_to_last_activity_is_included() {
        // The inclusive `>=` boundary (kept deliberately, not tightened to `>` — see the
        // doc comment on `Server::list_events`): a run whose last activity lands EXACTLY
        // on the cursor value must still be returned, not skipped.
        let root = fixture_root("events-boundary");
        let id = put_events_run_at(
            &root,
            12_345,
            session::State::Done,
            vec![],
            &crashed_exit(1),
            "",
        );
        let listing = events_of(&Server::new(root), 12_345);
        let events = listing["events"].as_array().unwrap();
        assert_eq!(events.len(), 1, "{events:?}");
        assert_eq!(events[0]["run_id"], id);
    }

    #[test]
    fn events_are_newest_run_first() {
        let root = fixture_root("events-order");
        for id in ["run-100", "run-300", "run-200"] {
            put_events_run(
                &root,
                id,
                session::State::Done,
                vec![],
                &crashed_exit(1),
                "",
            );
        }
        let listing = events_of(&Server::new(root), 0);
        let ids: Vec<&str> = listing["events"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["run_id"].as_str().unwrap())
            .collect();
        assert_eq!(ids, ["run-300", "run-200", "run-100"]);
    }

    #[test]
    fn noisy_log_with_clean_exit_is_not_an_event() {
        // The log-signal kind is gated on a harder signal (crash/gpu) or an invalid run
        // already being present — a healthy run that merely LOGS something error-shaped
        // (e.g. a benign "err:" from a loader) must stay silent, or polling this every
        // session start turns into spam an agent learns to ignore.
        let root = fixture_root("events-noisy-clean");
        let mut game_log = String::new();
        for _ in 0..5 {
            game_log.push_str("err: harmless one-off warning from a loader\n");
        }
        put_events_run(
            &root,
            "run-100",
            session::State::Done,
            vec![],
            CLEAN_EXIT,
            &game_log,
        );
        let listing = events_of(&Server::new(root), 0);
        assert_eq!(listing["events"].as_array().unwrap(), &Vec::<Value>::new());
    }

    #[test]
    fn crash_plus_noisy_log_also_surfaces_a_log_signal_event() {
        // The converse of the gating test above: once a harder signal (crash) already
        // fired for the run, the log-signal event is allowed to ride along.
        let root = fixture_root("events-crash-and-noisy");
        put_events_run(
            &root,
            "run-100",
            session::State::Done,
            vec![],
            &crashed_exit(1),
            "err: the real error line\n",
        );
        let listing = events_of(&Server::new(root), 0);
        let kinds: Vec<&str> = listing["events"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["kind"].as_str().unwrap())
            .collect();
        assert!(kinds.contains(&"crash"), "{kinds:?}");
        assert!(kinds.contains(&"log-signal"), "{kinds:?}");
    }

    #[test]
    fn stale_non_saved_run_is_a_crash_event() {
        // Mirrors list_traces_reports_session_state_and_staleness's own fixture: a
        // run-100 dir is always long before any real machine's boot time, so a run left
        // in a live, non-Saved state there is stale — whatever was recording it died
        // with that boot, and there's no exit line to say otherwise.
        let root = fixture_root("events-stale-crash");
        put_events_run(
            &root,
            "run-100",
            session::State::Recording,
            vec![],
            "launched pid 1\n", // died before ever writing an exit line
            "",
        );
        let listing = events_of(&Server::new(root), 0);
        let events = listing["events"].as_array().unwrap();
        assert_eq!(events.len(), 1, "{events:?}");
        assert_eq!(events[0]["kind"], "crash");
        assert!(events[0]["detail"].as_str().unwrap().contains("Recording"));
    }

    #[test]
    fn saved_and_stale_is_not_a_crash_event() {
        // The benign case the session window itself special-cases: Stop&Save leaves the
        // recorder done but the game still running; if the machine then reboots
        // normally before the game exits, Saved+stale must read as "trace saved — the
        // game has since exited", not a crash, or every ordinary Stop-and-keep-playing
        // session becomes a false positive.
        let root = fixture_root("events-saved-stale");
        put_events_run(
            &root,
            "run-100",
            session::State::Saved,
            vec![],
            "launched pid 1\n\
             50.0s stop requested — trace saved, game still running\n",
            "",
        );
        let listing = events_of(&Server::new(root), 0);
        assert_eq!(listing["events"].as_array().unwrap(), &Vec::<Value>::new());
    }

    #[test]
    fn spawn_failure_state_error_is_a_crash_event() {
        // The wrapper never writes an exit line in this path — it returns before the
        // game ever spawns (see run.rs::trace's spawn-failure branch) — so this must be
        // caught by session.json's State::Error alone, independent of beisl.log.
        let root = fixture_root("events-spawn-fail");
        let dir = root.join("run-100");
        std::fs::create_dir_all(&dir).unwrap();
        session::Session {
            game: "Blood West.exe".into(),
            state: session::State::Error,
            message: "Failed to launch reaper: No such file or directory (os error 2)".into(),
            ..Default::default()
        }
        .write(&dir)
        .unwrap();
        std::fs::write(
            dir.join("beisl.log"),
            "error: failed to launch reaper: No such file or directory (os error 2)\n",
        )
        .unwrap();
        std::fs::write(dir.join("game.log"), "").unwrap();

        let listing = events_of(&Server::new(root), 0);
        let events = listing["events"].as_array().unwrap();
        assert_eq!(events.len(), 1, "{events:?}");
        assert_eq!(events[0]["kind"], "crash");
        assert!(events[0]["detail"]
            .as_str()
            .unwrap()
            .contains("Failed to launch"));
    }

    #[test]
    fn invalid_run_gates_a_log_signal_event_on_its_own() {
        // A prior failed analysis attempt (mark_invalid) is, on its own, enough to admit
        // the log-signal kind — no crash or gpu event needs to fire alongside it.
        let root = fixture_root("events-invalid");
        put_events_run(
            &root,
            "run-100",
            session::State::Done,
            vec![],
            CLEAN_EXIT,
            "err: analysis-relevant error line\n",
        );
        let dir = root.join("run-100");
        std::fs::write(dir.join("perf.data"), b"").unwrap();
        library::mark_invalid(&dir, "perf script produced no samples").unwrap();

        let listing = events_of(&Server::new(root), 0);
        let kinds: Vec<&str> = listing["events"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["kind"].as_str().unwrap())
            .collect();
        assert_eq!(kinds, ["log-signal"], "{kinds:?}");
    }

    #[test]
    fn more_than_the_cap_is_truncated_but_flagged() {
        // 105 tiny fixtures (3 small file writes each) is cheap enough to exercise the
        // real cap/truncation path instead of just asserting the constant exists.
        let root = fixture_root("events-cap");
        for i in 0..105u64 {
            put_events_run(
                &root,
                &format!("run-{}", 1_000_000 + i),
                session::State::Done,
                vec![],
                &crashed_exit(1),
                "",
            );
        }
        let listing = events_of(&Server::new(root), 0);
        let events = listing["events"].as_array().unwrap();
        assert_eq!(events.len(), 100, "{events:?}");
        assert_eq!(listing["truncated"], true);
        // The newest 100 survive the cap (scan's own newest-first order), not an
        // arbitrary 100 — the very oldest run is the one dropped.
        let ids: Vec<&str> = events
            .iter()
            .map(|e| e["run_id"].as_str().unwrap())
            .collect();
        assert!(!ids.contains(&"run-1000000"), "{ids:?}");
        assert!(ids.contains(&"run-1000104"), "{ids:?}");
    }

    #[test]
    fn unknown_method_is_a_jsonrpc_error() {
        let server = Server::new(fixture_root("err"));
        let resp = call(
            &server,
            json!({"jsonrpc":"2.0","id":8,"method":"resources/list"}),
        );
        assert_eq!(resp["error"]["code"], -32601);
    }
}
