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

use super::identity::{self, Identity, Seen};
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
            ErrorReason::NotAuthenticated => {
                // Like edits-not-enabled: if beisl sees this, beisl skipped its handshake.
                "This is a beisl bug: the window tried to edit before authenticating.".into()
            }
            ErrorReason::StashUnreadable => {
                // Never "no findings": a file that failed to parse reads as data loss.
                "Fix or move the file gamebus named; nothing has overwritten it.".into()
            }
            // A coupler refusal, a bad key, a missing record, a tool we should not have
            // called: gamebus's message is the whole story and inventing a cure would
            // be guessing.
            ErrorReason::Refused
            | ErrorReason::NotFound
            | ErrorReason::BadArguments
            | ErrorReason::UnknownTool => String::new(),
        }
    }
}

/// What gamebus said about itself during the handshake.
#[derive(Debug, Clone, Default)]
pub struct Greeting {
    /// The one-time challenge to sign. Absent only from a server too old to ask.
    pub challenge: Option<Vec<u8>>,
    /// gamebus's ledger, or None when it has no verified one.
    pub ledger: Option<Seen>,
}

pub struct Client {
    child: Child,
    stdin: ChildStdin,
    lines: Receiver<String>,
    next_id: u64,
    greeting: Greeting,
}

impl Client {
    /// Spawn the server and complete the handshake. `flags` are the permission tiers this
    /// window needs; the server offers only what they permit, so a tool missing from
    /// `tools/list` is a capability answer rather than a failure at call time.
    pub fn start(bin: &Path, flags: &[&str]) -> Result<Self, Failure> {
        let mut cmd = Command::new(bin);
        cmd.arg("mcp").args(flags);
        Self::start_with(cmd)
    }

    /// The same, from a `Command` the caller has already shaped - the environment is the
    /// only way a test can point gamebus at a scratch stash instead of the real one.
    pub fn start_with(mut cmd: Command) -> Result<Self, Failure> {
        let mut child = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| Failure::Transport(format!("could not start the server: {e}")))?;
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
            greeting: Greeting::default(),
        };
        let hello = c.request(
            "initialize",
            json!({
                "protocolVersion": PROTOCOL,
                "capabilities": {},
                "clientInfo": {"name": CLIENT_NAME, "version": preflight::VERSION},
            }),
        )?;
        c.notify("notifications/initialized")?;
        let meta = hello.get("_meta").cloned().unwrap_or(Value::Null);
        c.greeting = Greeting {
            challenge: meta
                .get("gamebus/challenge")
                .and_then(|v| v.as_str())
                .and_then(identity::from_hex),
            ledger: meta.get("gamebus/ledger").and_then(|l| {
                Some(Seen {
                    id: l.get("id")?.as_str()?.to_string(),
                    head: l.get("head")?.as_str()?.to_string(),
                })
            }),
        };
        Ok(c)
    }

    pub fn greeting(&self) -> &Greeting {
        &self.greeting
    }

    /// Prove which client this is. Edit tools stay refused until this succeeds; reads
    /// never need it. The challenge is single-use, so a retry means a fresh connection.
    pub fn authenticate(&mut self, id: &Identity) -> Result<Seen, Failure> {
        let challenge = self
            .greeting
            .challenge
            .clone()
            .ok_or_else(|| Failure::Transport("the server offered no challenge".into()))?;
        let v = self.request(
            "gamebus/authenticate",
            json!({
                "name": CLIENT_NAME,
                "public_key": id.public_hex(),
                "signature": id.sign_hex(&challenge),
            }),
        )?;
        let l = v.get("ledger").cloned().unwrap_or(Value::Null);
        Ok(Seen {
            id: l
                .get("id")
                .and_then(|s| s.as_str())
                .unwrap_or_default()
                .to_string(),
            head: l
                .get("head")
                .and_then(|s| s.as_str())
                .unwrap_or_default()
                .to_string(),
        })
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

    /// beisl's half of gamebus's tamper evidence: a chain kept by one party is only a
    /// record, so beisl remembers a head and asks, on the next connection, whether it is
    /// still in the chain. Read tier, so this works before authenticating.
    pub fn ledger_check(&mut self, remembered: Option<&Seen>) -> identity::Check {
        let seen = self.greeting.ledger.clone();
        let Some(hash) = identity::lookup_needed(remembered, seen.as_ref()) else {
            return identity::check(remembered, seen.as_ref(), None);
        };
        // An unanswered question is not a failed check: fall through to Unknown rather
        // than reporting tampering because a call did not land.
        let got = self
            .call("ledger_contains", json!({"hash": hash}))
            .ok()
            .map(|v| identity::Contains {
                present: v.get("present").and_then(Value::as_bool).unwrap_or(false),
                verified: v.get("verified").and_then(Value::as_bool).unwrap_or(true),
            });
        identity::check(remembered, seen.as_ref(), got)
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
                    return Err(Failure::Transport(format!(
                        "the server exited during {method}"
                    )))
                }
            };
            let Ok(v) = serde_json::from_str::<Value>(&raw) else {
                continue;
            };
            if v.get("id").and_then(Value::as_u64) != Some(id) {
                continue;
            }
            if let Some(e) = v.get("error") {
                // authenticate refuses through a JSON-RPC error rather than a tool
                // result, with the ToolError in `data`. Same refusal, same cure.
                if let Some(te) = e
                    .get("data")
                    .and_then(|d| serde_json::from_value::<ToolError>(d.clone()).ok())
                {
                    return Err(Failure::Refused(te));
                }
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
            assert!(
                tools.contains(&want.to_string()),
                "missing {want} in {tools:?}"
            );
        }
        // Read-only spawn: the edit tier must not be offered.
        assert!(
            !tools.iter().any(|t| t.starts_with("apply_")),
            "edit tools offered without --allow-edits: {tools:?}"
        );

        let v = c
            .call("list_findings", serde_json::json!({}))
            .expect("list_findings");
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
        match c.call(
            "get_finding",
            serde_json::json!({"key": "steam:does-not-exist"}),
        ) {
            Err(Failure::Refused(e)) => assert_eq!(e.reason, ErrorReason::NotFound),
            other => panic!("expected a typed refusal, got {other:?}"),
        }
        match c.call("no_such_tool", serde_json::json!({})) {
            Err(Failure::Refused(e)) => assert_eq!(e.reason, ErrorReason::UnknownTool),
            other => panic!("expected unknown-tool, got {other:?}"),
        }
    }

    /// The handshake against the real server, with gamebus's auth state pointed at a
    /// scratch dir so this can never touch the maintainer's own. Without `auth init`
    /// there is no owner key, so authenticate must refuse with the one reason whose cure
    /// is a single command - the first-run cliff this design was shaped around.
    #[test]
    fn authenticate_refuses_before_auth_init_and_says_so() {
        let Some(bin) = server() else {
            eprintln!("SKIPPED: no gamebus-setup at GAMEBUS_SETUP or the default path");
            return;
        };
        let scratch = std::env::temp_dir().join(format!("beisl-mcp-auth-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&scratch);
        std::fs::create_dir_all(&scratch).unwrap();

        let mut child = std::process::Command::new(&bin);
        child
            .arg("mcp")
            .arg("--allow-edits")
            .env("XDG_DATA_HOME", &scratch)
            .env("XDG_CONFIG_HOME", &scratch);
        let mut c = Client::start_with(child).expect("handshake");

        // A challenge must arrive, or the client half cannot be exercised at all.
        let g = c.greeting().clone();
        assert_eq!(
            g.challenge.as_ref().map(Vec::len),
            Some(32),
            "expected a 32-byte challenge, got {:?}",
            g.challenge.as_ref().map(Vec::len)
        );
        // Load-bearing, not a sanity check: this is the LAST assertion before
        // authenticate, and authenticating against the real home would register this
        // test's key under "beisl-compat" for good, leaving the actual window a
        // client-key-mismatch until the owner re-approved it. The real home has a
        // ledger, a scratch one does not, so a leaked environment stops here.
        assert!(
            g.ledger.is_none(),
            "the environment leaked: this is not a scratch home, refusing to authenticate"
        );

        let dir = scratch.join("cfg");
        std::fs::create_dir_all(&dir).unwrap();
        let id = Identity::load_or_create_in(&dir).expect("client key");

        match c.authenticate(&id) {
            Err(Failure::Refused(e)) => {
                assert_eq!(e.reason, ErrorReason::AuthUninitialized);
                let cure = Failure::Refused(e).cure();
                assert!(
                    cure.contains("auth init"),
                    "the cure must name the command: {cure}"
                );
            }
            other => panic!("expected auth-uninitialized, got {other:?}"),
        }

        // An edit before a successful handshake is beisl's bug, and gamebus says so.
        match c.call("drain_inbox", serde_json::json!({})) {
            Err(Failure::Refused(e)) => assert_eq!(e.reason, ErrorReason::NotAuthenticated),
            other => panic!("expected not-authenticated, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&scratch);
    }

    /// The four-state check against a REAL chain. Entirely read-only: `ledger_contains`
    /// is read tier, so this needs no authenticate, no --allow-edits and no owner
    /// command, and it writes nothing. It uses whatever ledger this machine has, and
    /// skips when there is none rather than creating one.
    #[test]
    fn the_ledger_check_against_a_real_chain() {
        let Some(bin) = server() else {
            eprintln!("SKIPPED: no gamebus-setup at GAMEBUS_SETUP or the default path");
            return;
        };
        let mut c = Client::start(&bin, &[]).expect("handshake");
        if c.greeting().ledger.is_none() {
            eprintln!("SKIPPED: this machine has no initialised gamebus ledger");
            return;
        }
        let first = c.greeting().ledger.clone().expect("checked just above");
        // Nothing remembered yet: anchor, and never ask.
        assert_eq!(c.ledger_check(None), identity::Check::Anchored);
        // Remembered and unmoved.
        assert_eq!(c.ledger_check(Some(&first)), identity::Check::Unchanged);
        // A different ledger re-anchors quietly instead of alarming.
        let other = Seen {
            id: "not-this-ledger".into(),
            head: first.head.clone(),
        };
        assert_eq!(c.ledger_check(Some(&other)), identity::Check::Anchored);
        // A head this chain never had is the alarm, and it is reached by really asking
        // the server, not by assuming what it would say.
        let forged = Seen {
            id: first.id.clone(),
            head: "00".repeat(32),
        };
        assert_eq!(
            c.ledger_check(Some(&forged)),
            identity::Check::Rewritten {
                remembered: "00".repeat(32)
            }
        );
    }
}
