//! C12/C20/C27 at the bottom of the size range: a fleet overlay that opens
//! must be *visible*, not merely in `Mode::Roster`.
//!
//! `feed_overlay_size` used to reach a zero-height rect on any terminal six
//! rows or shorter (the tab bar and hint bar take two, then the formula
//! subtracts four), so `Alt+Shift+a` and `Alt+e` flipped the mode word and
//! drew nothing else — a state change with no frame, indistinguishable from
//! a binding that does not work. The picker had already answered this in
//! `dialog_rect`: "an empty result still needs a frame to say so".
//!
//! The unit test `the_fleet_overlays_always_have_a_frame_to_say_so_with`
//! pins the geometry. This pins what the geometry is *for* — that a real
//! roost at a real six-row terminal actually puts the overlay on screen.

#[allow(dead_code)]
mod harness;

use harness::spawn_or_skip_sized;
use std::time::Duration;

/// Open `chord`'s overlay at `cols`x`rows` and require its **title** on
/// screen, not just its mode word.
///
/// The title, rather than a border glyph: the panes draw borders too, and a
/// baseline count taken at settle time is not a baseline at all — `settle`
/// returns as soon as two parses agree, which on a freshly spawned shell
/// happens before the panes have painted. Counting corners therefore passed
/// with the bug present, because the *panes'* corners arrived between the
/// two samples. The title is drawn by the overlay and by nothing else, and
/// the pre-check below makes the test demonstrate that rather than assume
/// it.
fn opens_visibly(what: &str, chord: &[u8], mode_word: &str, title: &str, rows: u16, cols: u16) {
    let cwd = std::env::temp_dir().to_str().unwrap().to_string();
    let ws = harness::two_panes(&cwd);
    let Some(mut h) = spawn_or_skip_sized(what, &ws, &[], rows, cols) else { return };
    // 10 s, not 5: roost spawns *login* shells (P18), and a user rc that
    // sources something slow (nvm, on this sandbox) puts ~1 s of startup
    // noise ahead of the first frame. The deadline is a liveness check, not
    // a latency budget — nothing here measures speed, so the only thing a
    // tight one buys is a flake on a loaded CI runner.
    assert!(h.settle(Duration::from_secs(10)), "{what}: roost never drew a first frame");
    // Both panes painted, so the screen this compares against is the real
    // steady state rather than a half-drawn one.
    assert!(
        h.wait_for(Duration::from_secs(10), |s| s.contents().matches('┌').count() >= 2).is_some(),
        "{what}: the panes never drew at {cols}x{rows}:\n{}",
        h.screen().contents(),
    );
    assert!(
        !h.screen().contents().contains(title),
        "{what}: {title:?} is on screen before the overlay opened, so it cannot \
         prove the overlay drew",
    );

    h.write_bytes(chord);
    assert!(
        h.wait_for(Duration::from_secs(3), |s| s.contents().contains(mode_word)).is_some(),
        "{what}: never entered {mode_word} at {cols}x{rows}",
    );
    assert!(
        h.wait_for(Duration::from_secs(3), |s| s.contents().contains(title)).is_some(),
        "{what}: {mode_word} at {cols}x{rows} drew nothing — the mode word changed \
         and the overlay did not appear:\n{}",
        h.screen().contents(),
    );
}

/// Six rows is where the old arithmetic hit zero; five and four are below
/// it, and eight is comfortably above. All four must draw something.
#[test]
fn the_roster_is_visible_at_the_smallest_terminals() {
    for rows in [8u16, 6, 5, 4] {
        opens_visibly("roster", b"\x1bA", "ROSTER", "fleet", rows, 40);
    }
}

#[test]
fn the_activity_feed_is_visible_at_the_smallest_terminals() {
    for rows in [8u16, 6, 5, 4] {
        opens_visibly("feed", b"\x1be", "FEED", "activity", rows, 40);
    }
}
