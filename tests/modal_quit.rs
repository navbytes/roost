//! U1 · C24b: `Alt+q` quits from inside a modal, not just from Normal mode.
//!
//! C24b's "nothing else changes" bullet — an Alt chord that is not the
//! mode's own leaves the mode and dispatches globally — at the instance that
//! matters most. `src/core/app.rs` pins the rule across all eleven modes and
//! the whole chord space; this is the other end of the same wire: a real
//! roost in a real PTY, a modal genuinely on screen, and the process
//! actually gone.
//!
//! Written after a simulation agent reported Alt+q hanging inside every
//! modal. It does not. Its harness pressed the opening chord 100 ms after
//! spawn — before roost had drawn anything, so no modal was ever open — and
//! then gave the quit a 1 s deadline when a real quit here takes 0.9–1.9 s.
//! The two mistakes are what these tests exist to make impossible to repeat:
//! **assert the modal opened before pressing anything at it**, and give the
//! exit a deadline with room in it. A modal test that never opened a modal
//! is worse than no test — it reports on a screen it never reached.

#[allow(dead_code)]
mod harness;

use harness::spawn_or_skip;
use std::time::Duration;

fn workspace() -> String {
    let cwd = std::env::temp_dir().to_str().unwrap().to_string();
    // Two panes: the roster is a fleet surface, and one pane makes for a
    // thin test of it.
    harness::two_panes(&cwd)
}

/// Open the modal `chord` summons, prove it is on screen by its C9 mode
/// word, then press Alt+q and require the process to be gone.
///
/// The mode word is the marker on purpose. Overlay body text is a bad one:
/// "keys", "shell" and "edit" all sit in the always-visible hint bar, so a
/// test keyed on them passes whether or not anything opened — the exact
/// failure being guarded against. `render::mode_word` prints one word per
/// mode and nothing else prints it, which the pre-check below makes the
/// test itself demonstrate.
fn quits_from(what: &str, chord: &[u8], mode_word: &str) {
    let Some(mut h) = spawn_or_skip(what, &workspace()) else { return };
    assert!(h.settle(Duration::from_secs(5)), "{what}: roost never drew a first frame");

    assert!(
        !h.screen().contents().contains(mode_word),
        "{what}: {mode_word:?} is on screen in Normal mode, so it cannot prove \
         a modal opened — pick a marker unique to the overlay",
    );

    h.write_bytes(chord);
    assert!(
        h.wait_for(Duration::from_secs(3), |s| s.contents().contains(mode_word)).is_some(),
        "{what}: the modal never opened — {mode_word:?} is not on screen, so what \
         follows would be testing Normal mode:\n{}",
        h.screen().contents(),
    );

    // Generous, and deliberately so: this asserts roost exits at all, not
    // how fast. `quit_and_wait` re-presses at 700 ms to answer U1's
    // busy-fleet confirm, so any deadline under ~1 s tests the harness's
    // own timing rather than roost's behaviour.
    assert!(
        h.quit_and_wait(Duration::from_secs(10)).is_some(),
        "{what}: Alt+q did not quit from inside the modal",
    );
}

#[test]
fn alt_q_quits_from_the_broadcast_composer() {
    quits_from("broadcast composer", b"\x1b'", "BROADCAST");
}

#[test]
fn alt_q_quits_from_the_help_overlay() {
    quits_from("help overlay", b"\x1b?", "HELP");
}

#[test]
fn alt_q_quits_from_the_roster() {
    quits_from("roster", b"\x1bA", "ROSTER");
}

#[test]
fn alt_q_quits_from_the_pane_editor() {
    quits_from("pane editor", b"\x1br", "EDIT");
}

#[test]
fn alt_q_quits_from_the_launch_picker() {
    quits_from("launch picker", b"\x1b\r", "PICKER");
}
