//! The identity-miss half: gamebus's `umu-misses` review, driven over MCP.
//!
//! A resident worker owns one [`mcp::Client`] for the window's life. The handshake is
//! what identifies this window to gamebus, and the ledger check runs once against it, so
//! a client per button press would re-handshake and re-anchor on every keystroke.
//!
//! Reads need no authentication; edits do, and the first successful one registers this
//! window's key in gamebus's ledger for good. So authentication is deferred to the first
//! edit rather than done at open: a user who only ever looks should not be enrolled.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};

use gamebus_coupler::{MissRow, MissVerb};

use super::identity::{self, Check, Identity};
use super::mcp::{Client, Failure};

pub enum Job {
    List,
    Apply { key: String, verb: MissVerb },
}

/// What the worker has produced so far. The UI polls this on its tick, as it does for
/// the CLI jobs beside it.
#[derive(Default)]
pub struct Out {
    pub misses: Vec<MissRow>,
    /// Set when the last job failed, already worded with gamebus's cure.
    pub error: Option<String>,
    /// What the ledger check said at open, shown once rather than per action.
    pub ledger: Option<Check>,
    pub busy: bool,
    /// Bumped per finished job so the UI can tell "done" from "never ran".
    pub done: u64,
}

pub struct Worker {
    tx: Sender<Job>,
    pub out: Arc<Mutex<Out>>,
}

impl Worker {
    pub fn start(bin: PathBuf) -> Self {
        let (tx, rx) = mpsc::channel::<Job>();
        let out = Arc::new(Mutex::new(Out::default()));
        let mine = out.clone();
        std::thread::spawn(move || {
            let mut conn: Option<Client> = None;
            let mut authed = false;
            for job in rx {
                mine.lock().unwrap().busy = true;
                let r = run(&bin, &mut conn, &mut authed, job, &mine);
                let mut o = mine.lock().unwrap();
                if let Err(e) = r {
                    o.error = Some(e);
                    // A broken pipe must not poison every later call: drop the
                    // connection and let the next job start a fresh one.
                    conn = None;
                    authed = false;
                }
                o.busy = false;
                o.done += 1;
            }
        });
        Worker { tx, out }
    }

    pub fn send(&self, job: Job) {
        let _ = self.tx.send(job);
    }
}

/// One job against a connection that is opened lazily and reused.
fn run(
    bin: &Path,
    conn: &mut Option<Client>,
    authed: &mut bool,
    job: Job,
    out: &Arc<Mutex<Out>>,
) -> Result<(), String> {
    if conn.is_none() {
        // Both flags: this window does the editing, and asking for less would mean a
        // second connection later. What the user may do is gated by gamebus's own
        // approval, not by which flags beisl passed.
        let mut c = Client::start(bin, &["--allow-edits"]).map_err(word)?;
        let remembered = identity::remembered();
        let check = c.ledger_check(remembered.as_ref());
        // Re-anchor on anything but the alarm: a rewritten chain must not have its
        // evidence quietly overwritten by the very check that noticed.
        if !matches!(check, Check::Rewritten { .. }) {
            if let Some(seen) = c.greeting().ledger.clone() {
                let _ = identity::remember(&seen);
            }
        }
        out.lock().unwrap().ledger = Some(check);
        *conn = Some(c);
        *authed = false;
    }
    let c = conn.as_mut().expect("just opened");

    match job {
        Job::List => {
            let v = c.call("list_misses", serde_json::json!({})).map_err(word)?;
            let rows: Vec<MissRow> = serde_json::from_value(v["misses"].clone())
                .map_err(|e| format!("gamebus sent misses this build cannot read: {e}"))?;
            let mut o = out.lock().unwrap();
            o.misses = rows;
            o.error = None;
        }
        Job::Apply { key, verb } => {
            // Deferred to the first edit on purpose: authenticating enrols this window
            // in gamebus's ledger permanently, and only an editor needs to be named.
            if !*authed {
                let id = Identity::load_or_create()
                    .map_err(|e| format!("could not read beisl's client key: {e}"))?;
                c.authenticate(&id).map_err(word)?;
                *authed = true;
            }
            c.call("apply_miss", serde_json::json!({"key": key, "verb": verb}))
                .map_err(word)?;
            let v = c.call("list_misses", serde_json::json!({})).map_err(word)?;
            if let Ok(rows) = serde_json::from_value::<Vec<MissRow>>(v["misses"].clone()) {
                let mut o = out.lock().unwrap();
                o.misses = rows;
                o.error = None;
            }
        }
    }
    Ok(())
}

/// gamebus's refusal plus its one cure, as a person should read it.
fn word(f: Failure) -> String {
    match &f {
        Failure::Refused(e) => {
            let cure = f.cure();
            if cure.is_empty() {
                e.message.clone()
            } else {
                format!("{}\n{cure}", e.message)
            }
        }
        Failure::Transport(e) => format!("gamebus-setup could not be reached: {e}"),
    }
}

/// What to say about the ledger, or nothing at all. Only the rewrite is worth a line:
/// an alarm that fires on ordinary edits is one nobody reads.
pub fn ledger_note(check: &Check) -> Option<String> {
    match check {
        Check::Rewritten { remembered } => Some(format!(
            "gamebus's audit history was rewritten: the entry beisl last saw ({}) is no \
             longer in the chain. Nothing here is wrong, but the record of what changed \
             is no longer trustworthy.",
            &remembered[..remembered.len().min(12)]
        )),
        Check::Unverified => Some(
            "gamebus's audit chain does not verify, so it is refusing its own edits. \
             Re-initialise the passphrase in gamebus-setup."
                .into(),
        ),
        _ => None,
    }
}
