//! C43 (solo view), end to end through a real PTY.
//!
//! Unit tests in `src/core/app.rs`/`src/ui/render.rs` prove the model and
//! chrome in isolation. What they cannot prove is the thing a real terminal
//! adds: that `Alt+Shift+t`'s bytes — a shifted letter, delivered as the
//! uppercase meta-ESC glyph (`ESC T`), exactly the delivery shape
//! `tests/rekeyed_chords.rs`'s `Alt+Shift+s` test already pins for the same
//! family of chord — actually reach `ToggleSolo`, that the rail really
//! draws, that `Alt+↓` really steps it, and that toggling back really
//! restores the tiled screen.

#[allow(dead_code)]
mod harness;

use std::time::Duration;

/// `ESC` + the shifted glyph — meta-ESC has no shift bit of its own, so the
/// uppercase codepoint carries it, same shape as `ALT_SHIFT_S` in
/// `tests/rekeyed_chords.rs`.
const ALT_SHIFT_T: &[u8] = b"\x1bT";
/// Alt+Down arrives as a CSI sequence with xterm modifier 3 (Alt), the same
/// delivery path `tests/rekeyed_chords.rs`'s `ALT_LEFT`/`ALT_RIGHT` use.
const ALT_DOWN: &[u8] = b"\x1b[1;3B";

/// How many panes the frame is drawing, counted by their top-left border
/// corner — `tests/rekeyed_chords.rs`'s own helper, reused here to prove
/// the tiled screen (two side-by-side borders) is really back after
/// toggling solo off.
fn pane_count(screen: &vt100::Screen) -> usize {
    let (rows, cols) = screen.size();
    (0..rows)
        .flat_map(|r| (0..cols).map(move |c| (r, c)))
        .filter(|&(r, c)| screen.cell(r, c).map(|cell| cell.contents()) == Some("┌".to_string()))
        .count()
}

/// The row (if any) whose rail marker column carries `▎` — the focused
/// row's marker (C8), scanned outside row 0 so it can't be confused with
/// the tab bar's own active-tab marker in the same glyph.
fn marked_row(screen: &vt100::Screen) -> Option<u16> {
    let (rows, _cols) = screen.size();
    (1..rows).find(|&r| screen.cell(r, 0).map(|cell| cell.contents()) == Some("▎".to_string()))
}

/// The whole C43 contract in one PTY run: toggle on, see the rail; step it;
/// toggle off, see the tiled screen exactly as it was. One test function
/// (per the plan) rather than four, since every step depends on the
/// previous one's state and a real PTY spawn is the expensive part.
#[test]
fn alt_shift_t_toggles_solo_steps_the_rail_and_tiles_back_through_a_real_terminal() {
    let cwd = std::env::temp_dir();
    let cwd = cwd.to_str().expect("temp dir is valid utf8");
    let Some(mut h) = harness::spawn_or_skip("solo view e2e", &harness::two_panes(cwd)) else {
        return;
    };
    assert!(h.settle(Duration::from_secs(15)), "roost never drew a first frame");
    assert!(
        h.wait_for(Duration::from_secs(15), |s| s.contents().contains("1 main")).is_some(),
        "roost never drew its tab bar",
    );
    assert_eq!(pane_count(h.screen()), 2, "the fixture starts as two tiled panes");
    assert!(
        !h.screen().contents().contains("SOLO"),
        "the fixture must not start solo:\n{}",
        h.screen().contents(),
    );

    // Alt+Shift+t: the rail comes up, header and all.
    h.write_bytes(ALT_SHIFT_T);
    assert!(
        h.wait_for(Duration::from_secs(5), |s| s.contents().contains("SOLO · 2 PANES")).is_some(),
        "ESC T never reached ToggleSolo — the rail header never drew:\n{}",
        h.screen().contents(),
    );
    let first_marker = marked_row(h.screen());
    assert!(
        first_marker.is_some(),
        "the focused row must carry the ▎ marker:\n{}",
        h.screen().contents()
    );

    // Alt+↓: the marker steps to the next rail row.
    h.write_bytes(ALT_DOWN);
    assert!(
        h.wait_for(Duration::from_secs(5), |s| marked_row(s).is_some()
            && marked_row(s) != first_marker)
            .is_some(),
        "Alt+↓ never moved the ▎ marker off row {first_marker:?}:\n{}",
        h.screen().contents(),
    );

    // Alt+Shift+t again: back to tiles, exactly the two panes the fixture
    // started with — the round trip C42's own ladder cannot offer for free.
    h.write_bytes(ALT_SHIFT_T);
    assert!(
        h.wait_for(Duration::from_secs(5), |s| !s.contents().contains("SOLO")
            && pane_count(s) == 2)
            .is_some(),
        "the second Alt+Shift+t never tiled the tab back:\n{}",
        h.screen().contents(),
    );

    assert!(h.quit_and_wait(Duration::from_secs(5)).is_some(), "roost did not exit cleanly");
}
