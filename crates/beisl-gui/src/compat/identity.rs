//! This window's key pair, and what it remembers of gamebus's ledger.
//!
//! gamebus identifies each MCP client by an Ed25519 key and attributes every change to
//! it. The key is an IDENTITY, not a secret that protects anything: anything running as
//! the user can read it, which gamebus's design says out loud. It separates cooperating
//! clients; it does not prove who is at the keyboard.
//!
//! Both files live beside beisl's own config and NEVER in a trace directory: beisl's
//! "export a trace" is literally "reveal the run dir in the file manager", and users
//! hand whole run dirs to strangers for help. A key in one would be mailed out.

// Like the transport beside it: the key and the ledger anchor exist before the tabs
// that will use them. The allow goes when the handshake is wired into the window.
#![allow(dead_code)]

use std::io::{ErrorKind, Read, Result, Write};
use std::path::PathBuf;

use ed25519_dalek::{Signer, SigningKey};

/// Beside `config.toml`, mode 0600.
const KEY_FILE: &str = "gamebus-client.key";
/// The last `(ledger_id, head)` this window saw, for the second-party check.
const LEDGER_FILE: &str = "gamebus-ledger.json";

fn config_dir() -> Option<PathBuf> {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .map(|b| b.join("beisl"))
}

fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn from_hex(s: &str) -> Option<Vec<u8>> {
    let s = s.trim();
    if !s.len().is_multiple_of(2) {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok())
        .collect()
}

pub struct Identity {
    key: SigningKey,
}

impl Identity {
    /// Load the key, or make one on first use. Written 0600 by creating it that way
    /// rather than chmod-ing after: a key that is briefly world-readable is a key that
    /// was world-readable.
    pub fn load_or_create() -> Result<Self> {
        let dir = config_dir()
            .ok_or_else(|| std::io::Error::new(ErrorKind::NotFound, "no HOME for the config dir"))?;
        Self::load_or_create_in(&dir)
    }

    /// The directory is a parameter so a test never has to set `XDG_CONFIG_HOME`:
    /// the environment is process-global and tests run in parallel, which is a race,
    /// not a fixture.
    pub fn load_or_create_in(dir: &std::path::Path) -> Result<Self> {
        let path = dir.join(KEY_FILE);
        match std::fs::read_to_string(&path) {
            Ok(text) => {
                let raw = from_hex(&text).filter(|b| b.len() == 32).ok_or_else(|| {
                    std::io::Error::new(
                        ErrorKind::InvalidData,
                        format!("{} is not 32 hex-encoded bytes", path.display()),
                    )
                })?;
                let mut seed = [0u8; 32];
                seed.copy_from_slice(&raw);
                Ok(Self {
                    key: SigningKey::from_bytes(&seed),
                })
            }
            Err(e) if e.kind() == ErrorKind::NotFound => {
                std::fs::create_dir_all(dir)?;
                let mut seed = [0u8; 32];
                std::fs::File::open("/dev/urandom")?.read_exact(&mut seed)?;
                let key = SigningKey::from_bytes(&seed);
                write_private(&path, &to_hex(&seed))?;
                Ok(Self { key })
            }
            Err(e) => Err(e),
        }
    }

    pub fn public_hex(&self) -> String {
        to_hex(self.key.verifying_key().as_bytes())
    }

    /// Sign the RAW challenge bytes, not their hex text. gamebus hex-decodes before
    /// verifying, and signing the wrong one fails in a way that looks like a bad key.
    pub fn sign_hex(&self, challenge: &[u8]) -> String {
        to_hex(&self.key.sign(challenge).to_bytes())
    }
}

fn write_private(path: &std::path::Path, text: &str) -> Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    f.write_all(text.as_bytes())?;
    f.write_all(b"\n")
}

/// What gamebus reported about its ledger at handshake time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Seen {
    pub id: String,
    pub head: String,
}

/// What `ledger_contains` answered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Contains {
    pub present: bool,
    pub verified: bool,
}

/// The second-party check: beisl remembers a ledger head so that a chain with no other
/// party gains one. Two things make it usable rather than noise. `ledger_id` separates
/// an honest reset from a rewrite, and `ledger_contains` answers the question the head
/// alone could not - whether the head beisl remembers is still IN the chain. Without
/// that second half, every ordinary edit looks exactly like tampering.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Check {
    /// No verified ledger on gamebus's side. Its own state, not an alarm.
    NoLedger,
    /// Nothing remembered yet, or a different ledger: anchor quietly.
    Anchored,
    /// Same ledger, same head. Nothing moved since the last look.
    Unchanged,
    /// Same ledger, the head moved on, and the remembered one is still in the chain.
    /// The normal case after any edit.
    Moved,
    /// The chain does not verify right now. gamebus blocks its own edits in this state.
    Unverified,
    /// Same ledger, and the remembered head is GONE. History was rewritten. The only
    /// state that is an alarm, which is what makes it worth showing.
    Rewritten { remembered: String },
    /// Same ledger and a moved head, but the question could not be put. Not an alarm:
    /// an unanswered check is not a failed one.
    Unknown,
}

/// The hash to ask `ledger_contains` about, if asking is needed at all. `None` means the
/// state is already decided without a call.
pub fn lookup_needed(remembered: Option<&Seen>, seen: Option<&Seen>) -> Option<String> {
    match (remembered, seen) {
        (Some(old), Some(new)) if old.id == new.id && old.head != new.head => {
            Some(old.head.clone())
        }
        _ => None,
    }
}

pub fn check(remembered: Option<&Seen>, seen: Option<&Seen>, got: Option<Contains>) -> Check {
    match (remembered, seen) {
        (_, None) => Check::NoLedger,
        (None, Some(_)) => Check::Anchored,
        (Some(old), Some(new)) if old.id != new.id => Check::Anchored,
        (Some(old), Some(new)) if old.head == new.head => match got {
            Some(c) if !c.verified => Check::Unverified,
            _ => Check::Unchanged,
        },
        (Some(old), Some(_)) => match got {
            // A broken chain is the stronger statement: gamebus refuses its own edits
            // in this state, so say that rather than reasoning about one line in it.
            Some(c) if !c.verified => Check::Unverified,
            Some(c) if c.present => Check::Moved,
            Some(_) => Check::Rewritten {
                remembered: old.head.clone(),
            },
            None => Check::Unknown,
        },
    }
}

pub fn remembered() -> Option<Seen> {
    let text = std::fs::read_to_string(config_dir()?.join(LEDGER_FILE)).ok()?;
    let v: serde_json::Value = serde_json::from_str(&text).ok()?;
    Some(Seen {
        id: v.get("id")?.as_str()?.to_string(),
        head: v.get("head")?.as_str()?.to_string(),
    })
}

pub fn remember(seen: &Seen) -> Result<()> {
    let dir = config_dir()
        .ok_or_else(|| std::io::Error::new(ErrorKind::NotFound, "no HOME for the config dir"))?;
    std::fs::create_dir_all(&dir)?;
    let body = serde_json::json!({"id": seen.id, "head": seen.head}).to_string();
    // Stage and rename, this repo's overwrite idiom: a half-written anchor would read
    // as a rewritten ledger on the next look.
    let tmp = dir.join(format!("{LEDGER_FILE}.tmp"));
    std::fs::write(&tmp, body)?;
    std::fs::rename(tmp, dir.join(LEDGER_FILE))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_round_trips_and_rejects_rubbish() {
        assert_eq!(to_hex(&[0x00, 0x0f, 0xff]), "000fff");
        assert_eq!(from_hex("000fff").unwrap(), vec![0x00, 0x0f, 0xff]);
        assert!(from_hex("abc").is_none(), "odd length");
        assert!(from_hex("zz").is_none(), "not hex");
    }

    #[test]
    fn the_ledger_check_separates_reset_from_movement() {
        let a = Seen { id: "L1".into(), head: "h1".into() };
        let a2 = Seen { id: "L1".into(), head: "h2".into() };
        let b = Seen { id: "L2".into(), head: "h9".into() };
        let here = Some(Contains { present: true, verified: true });
        let gone = Some(Contains { present: false, verified: true });
        let broken = Some(Contains { present: true, verified: false });

        assert_eq!(check(Some(&a), None, None), Check::NoLedger);
        assert_eq!(check(None, Some(&a), None), Check::Anchored);
        // A reset or newly enabled ledger re-anchors quietly rather than alarming, and
        // never needs the lookup.
        assert_eq!(check(Some(&a), Some(&b), None), Check::Anchored);
        assert_eq!(lookup_needed(Some(&a), Some(&b)), None);
        assert_eq!(check(Some(&a), Some(&a), None), Check::Unchanged);
        assert_eq!(lookup_needed(Some(&a), Some(&a)), None);

        // A moved head is the one case that has to ask.
        assert_eq!(lookup_needed(Some(&a), Some(&a2)).as_deref(), Some("h1"));
        assert_eq!(check(Some(&a), Some(&a2), here), Check::Moved);
        assert_eq!(check(Some(&a), Some(&a2), None), Check::Unknown);
        assert_eq!(check(Some(&a), Some(&a2), broken), Check::Unverified);
        assert_eq!(
            check(Some(&a), Some(&a2), gone),
            Check::Rewritten { remembered: "h1".into() }
        );
    }

    /// The signature must cover the raw challenge, not its hex text: gamebus hex-decodes
    /// before verifying, and getting it backwards fails like a bad key.
    #[test]
    fn signs_the_raw_challenge_bytes() {
        let dir = std::env::temp_dir().join(format!("beisl-id-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let id = Identity::load_or_create_in(&dir).expect("first use creates a key");
        let hex_challenge = "0f1e2d3c4b5a69788796a5b4c3d2e1f00f1e2d3c4b5a69788796a5b4c3d2e1f0";
        let raw = from_hex(hex_challenge).unwrap();
        let sig = from_hex(&id.sign_hex(&raw)).unwrap();

        use ed25519_dalek::{Signature, Verifier, VerifyingKey};
        let pk: [u8; 32] = from_hex(&id.public_hex()).unwrap().try_into().unwrap();
        let vk = VerifyingKey::from_bytes(&pk).unwrap();
        let sig = Signature::from_bytes(&sig.try_into().unwrap());
        assert!(vk.verify(&raw, &sig).is_ok(), "raw bytes must verify");
        assert!(
            vk.verify(hex_challenge.as_bytes(), &sig).is_err(),
            "the hex text must NOT verify, or we signed the wrong thing"
        );

        // Same key on the next open, and not readable by anyone else.
        use std::os::unix::fs::PermissionsExt;
        let key_path = dir.join(KEY_FILE);
        let mode = std::fs::metadata(&key_path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "the client key must not be group/world readable");
        let again = Identity::load_or_create_in(&dir).unwrap();
        assert_eq!(again.public_hex(), id.public_hex());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
