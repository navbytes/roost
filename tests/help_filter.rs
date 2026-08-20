//! C39: the keymap overlay's type-ahead filter, driven through a real PTY.
//!
//! The unit tests pin the state machine and the layout; this is the check
//! that the two meet on a screen — that `/` visibly narrows the table, that
//! the title says what is happening, and that a reader can get back out.
//! `tests/modal_quit.rs`'s lesson applies: assert the overlay is up before
//! pressing anything at it, or the test reports on Normal mode.

#[allow(dead_code)]
mod harness;

use harness::spawn_or_skip;
use std::time::Duration;

/// Open the keymap and prove it is on screen by its C9 mode word.
fn open_keymap(what: &str) -> Option<harness::Harness> {
    let cwd = std::env::temp_dir().to_str().unwrap().to_string();
    let mut h = spawn_or_skip(what, &harness::two_panes(&cwd))?;
    assert!(h.settle(Duration::from_secs(10)), "{what}: roost never drew a first frame");
    h.write_bytes(b"\x1b?");
    assert!(
        h.wait_for(Duration::from_secs(10), |s| s.contents().contains("HELP")).is_some(),
        "{what}: the keymap never opened:\n{}",
        h.screen().contents(),
    );
    Some(h)
}

#[test]
fn slash_narrows_the_keymap_and_the_title_says_so() {
    let Some(mut h) = open_keymap("filter") else { return };

    // The unfiltered overlay advertises the filter, and shows groups a
    // query will remove.
    let before = h.screen().contents();
    assert!(before.contains("/ filters"), "the affordance is visible:\n{before}");
    assert!(before.contains("LAYOUT"), "the LAYOUT group is drawn:\n{before}");

    h.write_bytes(b"/stack");
    assert!(
        h.wait_for(Duration::from_secs(5), |s| s.contents().contains("/stack")).is_some(),
        "the query never reached the title:\n{}",
        h.screen().contents(),
    );

    let after = h.screen().contents();
    assert!(after.contains("HELP"), "still open while filtering:\n{after}");
    assert!(after.contains("Esc clears"), "the way out is on screen:\n{after}");
    // A real narrowing: a group with no matching row is gone entirely.
    assert!(!after.contains("READING THE SCREEN"), "the query removed groups:\n{after}");

    // Esc clears, a second Esc closes.
    //
    // The wait between them is load-bearing, and getting it wrong is what
    // made this test fail on macOS and pass on Linux. It first waited for
    // the screen to contain `"/ "` — which `Alt+/` and this very row's
    // "`/` filters it" already put there, so the predicate was true before
    // the first Esc was even parsed. `wait_for` returned instantly and the
    // second ESC byte went out on the heels of the first; two ESCs arriving
    // together fuse into one event (roost's README documents the same
    // fusion for ESC+key), so only one Esc landed: the query cleared and
    // the overlay stayed open.
    //
    // Wait for the *transition* instead — the query leaving the title — so
    // the first Esc is provably handled before the second is sent.
    h.write_bytes(b"\x1b");
    assert!(
        h.wait_for(Duration::from_secs(10), |s| !s.contents().contains("/stack")).is_some(),
        "the first Esc did not clear the query:\n{}",
        h.screen().contents(),
    );
    assert!(
        h.screen().contents().contains("HELP"),
        "...and it must clear the query without closing the overlay:\n{}",
        h.screen().contents(),
    );

    h.write_bytes(b"\x1b");
    assert!(
        h.wait_for(Duration::from_secs(10), |s| s.contents().contains("NORMAL")).is_some(),
        "the second Esc did not close the overlay:\n{}",
        h.screen().contents(),
    );
}

/// C15 unchanged: with no query open, a letter still dismisses — including
/// `q`, which only becomes text once `/` has been pressed.
#[test]
fn a_letter_still_closes_an_unfiltered_keymap() {
    let Some(mut h) = open_keymap("unfiltered") else { return };
    h.write_bytes(b"q");
    assert!(
        h.wait_for(Duration::from_secs(5), |s| s.contents().contains("NORMAL")).is_some(),
        "`q` must still close an overlay with no query open:\n{}",
        h.screen().contents(),
    );
}
