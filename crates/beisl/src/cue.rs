//! Audible feedback — the v0 stand-in for an in-game indicator.
//!
//! A short blip when Beisl is armed / recording starts / recording stops lets a
//! fullscreen player confirm their hotkey worked, without us drawing into the game
//! (which would mean becoming a Vulkan layer). The in-game overlay indicator is a
//! v2 nicety; see DESIGN.md.
//!
//! No bundled WAV: the blips are a few hundred milliseconds of synthesized sine,
//! rendered in-process and piped raw into `paplay` (PulseAudio/PipeWire) or `aplay`
//! (bare ALSA) — no asset to ship, no temp file, and a missing player just means a
//! silent cue (`let _ =`, feedback must never break a trace).

use std::io::Write as _;
use std::process::{Command, Stdio};

pub enum Cue {
    /// Beisl is set up and about to launch the game.
    Ready,
    /// CPU capture started (used once the recorder toggles on).
    #[allow(dead_code)]
    RecordStart,
    /// CPU capture stopped.
    RecordStop,
}

const RATE: u32 = 48_000;

/// Distinct, short, and polite: rising two-tone for "armed", single mid tone for
/// "recording", falling two-tone for "stopped" — recognizable over game audio without
/// startling anyone (C5/E5/G5, ~0.3 amplitude). A 0 Hz entry is a rest (renders as
/// silence through the same math). Two rests are load-bearing, both listen-tested on
/// the PipeWire desktop (2026-07-11):
///
/// * the 200 ms **preamble**: every sink here sits SUSPENDED when idle, and the
///   wake-up eats the head of a short stream — without the preamble the first note
///   (or the whole blip) is swallowed;
/// * the 90 ms **gap** between notes: played back-to-back the two tones fuse into
///   one perceived blip, defeating the rising-vs-falling distinction.
fn notes(cue: &Cue) -> &'static [(f32, u32)] {
    match cue {
        Cue::Ready => &[(0.0, 200), (523.25, 90), (0.0, 90), (783.99, 110)],
        Cue::RecordStart => &[(0.0, 200), (659.25, 130)],
        Cue::RecordStop => &[(0.0, 200), (783.99, 90), (0.0, 90), (523.25, 110)],
    }
}

pub fn play(cue: Cue) {
    // One stderr line per cue: the only diagnostic for "I hear nothing" reports (all
    // playback failures are deliberately silent) and a cheap timestamp in the
    // Lutris/Steam console log to correlate against.
    let name = match cue {
        Cue::Ready => "ready",
        Cue::RecordStart => "rec-start",
        Cue::RecordStop => "rec-stop",
    };
    eprintln!("beisl: \u{266a} cue:{name}");
    let pcm = render(notes(&cue));
    // Detached thread: rendering is instant but the player invocation isn't, and a cue
    // must never delay the game launch. The thread waits on the child so the ~200 ms
    // blip doesn't leave a zombie for the (game-length) life of the wrapper.
    std::thread::spawn(move || {
        for (bin, args) in PLAYERS {
            if pipe_to(bin, args, &pcm).is_ok() {
                return;
            }
        }
    });
}

/// Raw-PCM players to try, in order: `paplay` speaks to PulseAudio *and* PipeWire
/// (via pipewire-pulse, the desktop default); `aplay` covers bare-ALSA setups.
const PLAYERS: &[(&str, &[&str])] = &[
    (
        "paplay",
        &["--raw", "--format=s16le", "--rate=48000", "--channels=1"],
    ),
    (
        "aplay",
        &["-q", "-t", "raw", "-f", "S16_LE", "-r", "48000", "-c", "1"],
    ),
];

fn pipe_to(bin: &str, args: &[&str], pcm: &[u8]) -> std::io::Result<()> {
    let mut child = Command::new(bin)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(pcm);
        // Drop closes the pipe so the player hits EOF and exits.
    }
    // Gate on the exit status, not just the spawn: an installed paplay with no
    // daemon to talk to exits nonzero without a sound — fall through to aplay then.
    // (Free to wait for: we're on a detached thread.)
    if child.wait()?.success() {
        Ok(())
    } else {
        Err(std::io::Error::other(format!("{bin} exited nonzero")))
    }
}

/// Render `(freq_hz, duration_ms)` notes as s16le mono at [`RATE`], with a 5 ms linear
/// attack/decay per note so the blip doesn't click.
fn render(notes: &[(f32, u32)]) -> Vec<u8> {
    let mut out = Vec::new();
    for &(freq, ms) in notes {
        let total = (RATE * ms / 1000) as usize;
        let ramp = (RATE / 200) as usize; // 5 ms
        for i in 0..total {
            let t = i as f32 / RATE as f32;
            let env = if i < ramp {
                i as f32 / ramp as f32
            } else if i + ramp > total {
                (total - i) as f32 / ramp as f32
            } else {
                1.0
            };
            let s = (t * freq * std::f32::consts::TAU).sin() * env * 0.3;
            out.extend_from_slice(&((s * i16::MAX as f32) as i16).to_le_bytes());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_produces_the_right_amount_of_audible_audio() {
        // 200 ms preamble + 90 + 90 gap + 110 ms at 48 kHz mono s16le
        // = 490 ms * 48 samples/ms * 2 bytes.
        let pcm = render(notes(&Cue::Ready));
        assert_eq!(pcm.len(), 490 * 48 * 2);
        // The wake-up preamble is genuinely silent: the sink may eat all of it.
        assert!(
            pcm[..200 * 48 * 2].iter().all(|&b| b == 0),
            "preamble must be pure silence"
        );
        // Actually audible after it, not silence: a peak well above the noise floor…
        let peak = pcm
            .chunks_exact(2)
            .map(|c| i16::from_le_bytes([c[0], c[1]]).unsigned_abs())
            .max()
            .unwrap();
        assert!(peak > i16::MAX as u16 / 4, "peak {peak} too quiet");
        // …but enveloped: the first voiced sample starts at (near) zero, no click.
        let first = i16::from_le_bytes([pcm[200 * 48 * 2], pcm[200 * 48 * 2 + 1]]);
        assert_eq!(first, 0);
    }

    #[test]
    fn cues_are_audibly_distinct_and_players_speak_raw_pcm() {
        // Distinct tones (DESIGN.md, "Surfaces"): the three cues must not render identically.
        let ready = render(notes(&Cue::Ready));
        let stop = render(notes(&Cue::RecordStop));
        assert_ne!(ready, stop);
        assert_ne!(render(notes(&Cue::RecordStart)), ready);
        // Player argv must request exactly the format render() emits (s16le mono 48k).
        for (bin, args) in PLAYERS {
            let joined = args.join(" ");
            assert!(!bin.is_empty());
            assert!(
                joined.contains("s16le") || joined.contains("S16_LE"),
                "{bin}: {joined}"
            );
            assert!(joined.contains("48000"), "{bin}: {joined}");
        }
    }
}
