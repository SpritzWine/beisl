//! A minimal MCP client over `gamebus-setup mcp`.
//!
//! The mirror of gamebus's own client (`src/setup/mcp.rs`), which speaks to `beisl-mcp`:
//! same subset, same transport, pointed the other way. Newline-delimited JSON-RPC over a
//! child's stdio - no ports, no sockets, no network, no async runtime.
//!
//! Unlike the rest of this window's work, the child is LONG-LIVED: the handshake happens
//! once and every later call reuses it, because `initialize` is what identifies this
//! client to gamebus and repeating it per action would re-handshake on every keypress.
//!
//! A wedged server must not wedge the window, so a reader thread owns the pipe and the
//! caller waits on a channel with a timeout - gamebus's answer to the same problem, and
//! there is no reason to invent a second one.

// The transport landed ahead of its consumers: gamebus's MCP server is phase 1 (read
// tools only), so the tabs that will call this do not exist yet. The allow goes when the
// misses and gamedb tabs land; until then the test below is what exercises it.
#![allow(dead_code)]

use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::Duration;

use gamebus_coupler::{ErrorReason, ToolError};
use serde_json::{json, Value};

/// How long one call may take. Matches gamebus's own client: a server that has wedged
/// must not hold the window.
const CALL_TIMEOUT: Duration = Duration::from_secs(30);

/// The protocol version this client speaks. The server echoes it back.
const PROTOCOL: &str = "2025-06-18";

/// This client's name to gamebus, per binary rather than per project: attribution in
/// gamebus's ledger is the point of the handshake, and a family name would blur it.
pub const CLIENT_NAME: &str = "beisl-compat";

/// Why a call did not return a result.
#[derive(Debug, Clone)]
pub enum Failure {
    /// gamebus refused, and said which of its cures applies.
    Refused(ToolError),
    /// The server never answered, answered something unparseable, or is not there.
    /// Always the transport's fault, never a refusal - the two must not be conflated,
    /// because a refusal has a cure to show and a transport fault has none.
    Transport(String),
}

impl Failure {
    /// The cure to put in front of the user. gamebus guarantees exactly one per reason.
    pub fn cure(&self) -> String {
        let Failure::Refused(e) = self else {
            return String::new();
        };
        match e.reason {
            ErrorReason::EditsNotEnabled => {
                // beisl spawns the server itself, so this is a bug here, not a user problem.
                "This is a beisl bug: the window started gamebus-setup without --allow-edits."
                    .into()
            }
            ErrorReason::NetworkNotEnabled => {
                "This action needs the network, and the window started gamebus-setup \
                 without --allow-network."
                    .into()
            }
            ErrorReason::AuthUninitialized => {
                "gamebus has no owner key yet. Run: gamebus-setup auth init".into()
            }
            ErrorReason::ClientUnapproved => {
                format!("Approve the client \"{CLIENT_NAME}\" in gamebus-setup, then retry.")
            }
            ErrorReason::ClientKeyMismatch => format!(
                "gamebus knows a different key for \"{CLIENT_NAME}\". Re-approve it in \
                 gamebus-setup."
            ),
            ErrorReason::LedgerMissing | ErrorReason::LedgerUnverified => {
                "Re-initialise the passphrase in gamebus-setup.".into()
            }
            ErrorReason::StashUnreadable => {
                // Never "no findings": a file that failed to parse reads as data loss.
                "Fix or move the file gamebus named; nothing has overwritten it.".into()
            }
            _ => String::new(),
        }
    }
}

pub struct Client {
    child: Child,
    stdin: ChildStdin,
    lines: Receiver<String>,
    next_id: u64,
}

impl Client {
    /// Spawn the server and complete the handshake. `flags` are the permission tiers this
    /// window needs; the server offers only what they permit, so a tool missing from
    /// `tools/list` is a capability answer rather than a failure at call time.
    pub fn start(bin: &Path, flags: &[&str]) -> Result<Self, Failure> {
        let mut child = Command::new(bin)
            .arg("mcp")
            .args(flags)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| Failure::Transport(format!("could not start {}: {e}", bin.display())))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| Failure::Transport("no stdin on the server".into()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| Failure::Transport("no stdout on the server".into()))?;

        // The pipe is owned by a thread so a silent server cannot block the UI poll.
        let (tx, lines) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if tx.send(line).is_err() {
                    break;
                }
            }
        });

        let mut c = Client {
            child,
            stdin,
            lines,
            next_id: 0,
        };
        c.request(
            "initialize",
            json!({
                "protocolVersion": PROTOCOL,
                "capabilities": {},
                "clientInfo": {"name": CLIENT_NAME, "version": preflight::VERSION},
            }),
        )?;
        c.notify("notifications/initialized")?;
        Ok(c)
    }

    /// The tools this server is offering, which is what the permission flags allowed.
    pub fn tools(&mut self) -> Result<Vec<String>, Failure> {
        let v = self.request("tools/list", json!({}))?;
        Ok(v.get("tools")
            .and_then(|t| t.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|t| t.get("name").and_then(|n| n.as_str()).map(str::to_string))
                    .collect()
            })
            .unwrap_or_default())
    }

    /// Call one tool. A refusal comes back as `Failure::Refused` carrying gamebus's
    /// reason, so the caller can show its one cure.
    pub fn call(&mut self, tool: &str, args: Value) -> Result<Value, Failure> {
        let v = self.request("tools/call", json!({"name": tool, "arguments": args}))?;
        let text = v
            .pointer("/content/0/text")
            .and_then(|t| t.as_str())
            .unwrap_or_default();
        if v.get("isError").and_then(Value::as_bool).unwrap_or(false) {
            return Err(match serde_json::from_str::<ToolError>(text) {
                Ok(e) => Failure::Refused(e),
                // An error that is not a ToolError is a contract break, not a refusal.
                Err(e) => Failure::Transport(format!("{tool} failed unparseably: {e}: {text}")),
            });
        }
        serde_json::from_str(text)
            .map_err(|e| Failure::Transport(format!("{tool} answered unparseable JSON: {e}")))
    }

    fn notify(&mut self, method: &str) -> Result<(), Failure> {
        let line = json!({"jsonrpc": "2.0", "method": method}).to_string();
        writeln!(self.stdin, "{line}").map_err(|e| Failure::Transport(e.to_string()))
    }

    fn request(&mut self, method: &str, params: Value) -> Result<Value, Failure> {
        self.next_id += 1;
        let id = self.next_id;
        let line =
            json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}).to_string();
        writeln!(self.stdin, "{line}").map_err(|e| Failure::Transport(e.to_string()))?;

        // Skip anything that is not the answer to this id: notifications and any server
        // chatter must not be mistaken for a result.
        loop {
            let raw = match self.lines.recv_timeout(CALL_TIMEOUT) {
                Ok(raw) => raw,
                Err(RecvTimeoutError::Timeout) => {
                    return Err(Failure::Transport(format!("{method} timed out")))
                }
                Err(RecvTimeoutError::Disconnected) => {
                    return Err(Failure::Transport(format!("the server exited during {method}")))
                }
            };
            let Ok(v) = serde_json::from_str::<Value>(&raw) else {
                continue;
            };
            if v.get("id").and_then(Value::as_u64) != Some(id) {
                continue;
            }
            if let Some(e) = v.get("error") {
                return Err(Failure::Transport(format!("{method}: {e}")));
            }
            return Ok(v.get("result").cloned().unwrap_or(Value::Null));
        }
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        // The server lives exactly as long as the window that spawned it.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Where gamebus's server lives. Its branch is uncommitted in its own working tree,
    /// so this is a local path by necessity until the coupler and server are pushed.
    fn server() -> Option<std::path::PathBuf> {
        let p = std::env::var_os("GAMEBUS_SETUP")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| {
                "/media/Data/Projekte/gamebus-presenced/target/debug/gamebus-setup".into()
            });
        p.exists().then_some(p)
    }

    /// Drives the REAL server, because a client tested against a fixture of its own
    /// making proves only that it agrees with itself - the failure this project already
    /// had once, when two test suites agreed and the contract between them did not hold.
    #[test]
    fn handshake_and_read_tools_against_the_real_server() {
        let Some(bin) = server() else {
            // Loud on purpose: a silently skipped integration test looks green while
            // testing nothing.
            eprintln!("SKIPPED: no gamebus-setup at GAMEBUS_SETUP or the default path");
            return;
        };
        let mut c = Client::start(&bin, &[]).expect("handshake");

        let tools = c.tools().expect("tools/list");
        for want in ["list_misses", "get_miss", "list_findings", "get_finding"] {
            assert!(tools.contains(&want.to_string()), "missing {want} in {tools:?}");
        }
        // Read-only spawn: the edit tier must not be offered.
        assert!(
            !tools.iter().any(|t| t.starts_with("apply_")),
            "edit tools offered without --allow-edits: {tools:?}"
        );

        let v = c.call("list_findings", serde_json::json!({})).expect("list_findings");
        assert!(v.get("inbox_pending").is_some(), "no inbox_pending: {v}");
        let rows: Vec<gamebus_coupler::FindingRow> =
            serde_json::from_value(v["findings"].clone()).expect("rows deserialize");
        for r in &rows {
            // key and needs ride on top of a flattened record; the nesting is the part
            // worth proving, since flatten over a flatten catch-all is a serde sharp edge.
            assert!(!r.key.is_empty());
            assert!(!r.finding.wall.as_str().is_empty());
        }

        // A refusal must arrive as a typed reason with a cure, never as a transport fault.
        match c.call("get_finding", serde_json::json!({"key": "steam:does-not-exist"})) {
            Err(Failure::Refused(e)) => assert_eq!(e.reason, ErrorReason::NotFound),
            other => panic!("expected a typed refusal, got {other:?}"),
        }
        match c.call("no_such_tool", serde_json::json!({})) {
            Err(Failure::Refused(e)) => assert_eq!(e.reason, ErrorReason::UnknownTool),
            other => panic!("expected unknown-tool, got {other:?}"),
        }
    }
}
