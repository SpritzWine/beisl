//! Audible feedback — the v0 stand-in for an in-game indicator.
//!
//! A short blip when Beisl is armed / recording starts / recording stops lets a
//! fullscreen player confirm their hotkey worked, without us drawing into the game
//! (which would mean becoming a Vulkan layer). The in-game overlay indicator is a
//! v2 nicety; see DESIGN.md.

pub enum Cue {
    /// Beisl is set up and about to launch the game.
    Ready,
    /// CPU capture started (used once the recorder toggles on).
    #[allow(dead_code)]
    RecordStart,
    /// CPU capture stopped.
    RecordStop,
}

pub fn play(cue: Cue) {
    // TODO: play a short bundled WAV (via `paplay`/`aplay`, or a tiny embedded PCM
    //   blip). Distinct tones for start vs stop.
    let name = match cue {
        Cue::Ready => "ready",
        Cue::RecordStart => "rec-start",
        Cue::RecordStop => "rec-stop",
    };
    eprintln!("beisl: \u{266a} cue:{name} (TODO: play sound)");
}
