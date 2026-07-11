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
//! Tools (all read-only): `list_traces`, `get_artifact`, `get_report`, `get_log`.
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
                })
            })
            .collect();
        json!({ "traces": runs, "traces_root": self.traces_root.display().to_string() }).to_string()
    }

    /// Read one well-known file out of one run dir. The run_id gate mirrors
    /// `library::delete_run`'s stance: only `run-*` names, no path fragments — the
    /// filename itself is never user input.
    fn read_run_file(&self, args: &Value, file: &str, tail: Option<u64>) -> Result<String, String> {
        let id = args
            .get("run_id")
            .and_then(Value::as_str)
            .ok_or("run_id is required (see list_traces)")?;
        if !id.starts_with("run-") || id.contains('/') || id.contains("..") {
            return Err(format!("not a run id: {id}"));
        }
        let path = self.traces_root.join(id).join(file);
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

fn tools_list() -> Value {
    let run_id =
        json!({ "type": "string", "description": "A run id from list_traces (run-<unix_ts>)" });
    json!({ "tools": [
        {
            "name": "list_traces",
            "description": "List all recorded game-performance runs, newest first: run_id, \
                            game, start time, duration, and whether an analysis artifact \
                            exists.",
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
    fn the_four_tools_are_listed() {
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
            ["list_traces", "get_artifact", "get_report", "get_log"]
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
    fn unknown_method_is_a_jsonrpc_error() {
        let server = Server::new(fixture_root("err"));
        let resp = call(
            &server,
            json!({"jsonrpc":"2.0","id":8,"method":"resources/list"}),
        );
        assert_eq!(resp["error"]["code"], -32601);
    }
}
