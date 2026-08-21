//! Scrollback search, end to end — SPEC-parity P21.
//!
//! Every peer multiplexer ships a way to find text that has scrolled off the
//! screen; roost's only route into its own history was `roost read --full`,
//! i.e. leaving the TUI. `/` in Scroll (or Copy) mode opens an incremental
//! search over the focused pane's scrollback + screen; typing filters, and a
//! jump parks the view on the hit.
//!
//! The scenario is the honest one: a pane prints 300 numbered lines — far
//! more than the 40-row harness screen — so the target is genuinely gone
//! from the visible grid, and only a search over *history* can bring it
//! back.

// The shared harness is compiled per test binary; helpers other tenants use
// are dead code from this binary's view — not real rot.
#[allow(dead_code)]
mod harness;

use std::time::Duration;

/// Alt+PageUp — scroll mode. xterm modified-key encoding, mod 3 = Alt.
const ALT_PGUP: &[u8] = b"\x1b[5;3~";

#[test]
fn slash_finds_a_line_that_has_scrolled_off_and_jumps_the_view_to_it() {
    let cwd = std::env::temp_dir();
    let cwd = cwd.to_str().expect("temp dir is valid utf8");
    let Some(mut h) = harness::spawn_or_skip("scrollback-search gate", &harness::one_pane(cwd))
    else {
        return;
    };
    assert!(h.settle(Duration::from_secs(5)), "initial frame never settled");

    // 300 numbered lines. The marker is spelled in two pieces so the tty's
    // echo of the typed command can never satisfy a wait on it.
    h.write_bytes(b"for i in $(seq 1 300); do echo \"mark-$i\"; done; echo 'DO''NE'\r");
    h.wait_for(Duration::from_secs(20), |s| s.contents().contains("DONE"))
        .expect("the pane never finished printing its history");

    // `mark-42` is ~258 lines back: long gone from a 40-row screen.
    assert!(
        !h.screen().contents().contains("mark-42 "),
        "the target must have scrolled off before the search runs:\n{}",
        h.screen().contents()
    );

    // Scroll mode, then the search prompt, then the query.
    h.write_bytes(ALT_PGUP);
    h.wait_for(Duration::from_secs(5), |s| s.contents().contains("SCROLL"))
        .expect("Alt+PageUp never reached scroll mode");
    h.write_bytes(b"/");
    if h.wait_for(Duration::from_secs(5), |s| s.contents().contains("SEARCH")).is_none() {
        panic!("`/` did not open a search prompt:\n{}", h.screen().contents());
    }
    h.write_bytes(b"mark-42");

    // The view must now be showing the line the search found. `mark-42 ` —
    // with the trailing space the grid pads a short row's tail against —
    // can only be the line itself, never `mark-420`-style neighbours (which
    // don't exist below 300 anyway) nor the query echoed on the hint bar
    // (that reads `/mark-42`).
    if h.wait_for(Duration::from_secs(5), |s| s.contents().contains("mark-42 ")).is_none() {
        panic!("the search never jumped the view to its hit:\n{}", h.screen().contents());
    }
    // The prompt reports where it is in the hit list, so an empty result
    // can't masquerade as a jump.
    let screen = h.screen().contents();
    assert!(screen.contains("1/1"), "the hit counter should read 1/1:\n{screen}");

    // Esc cancels back to the live tail's view, exactly where `/` was
    // pressed — the last line printed is on screen again.
    h.write_bytes(b"\x1b");
    if h.wait_for(Duration::from_secs(5), |s| s.contents().contains("DONE")).is_none() {
        panic!("Esc did not restore the pre-search view:\n{}", h.screen().contents());
    }

    assert!(h.quit_and_wait(Duration::from_secs(10)).is_some(), "roost did not exit cleanly");
}
