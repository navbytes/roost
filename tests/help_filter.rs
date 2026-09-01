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
    assert!(before.contains("type to filter"), "the affordance is visible:\n{before}");
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
    // the screen to contain `"/ "` — which `Alt+/` (and, at the time, the
    // `Alt+?` row's "`/` filters it") already put there, so the predicate
    // was true before
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

/// [Amended 2026-09-01] A bare letter no longer dismisses: it OPENS the
/// filter seeded with itself — `q` included, the letter the old contract
/// reserved for closing. The way out while un-filtered is Esc (or Space),
/// pinned in the second half.
#[test]
fn a_letter_opens_the_filter_seeded_with_itself() {
    let Some(mut h) = open_keymap("seeded") else { return };
    h.write_bytes(b"q");
    assert!(
        h.wait_for(Duration::from_secs(5), |s| s.contents().contains("keys — /q")).is_some(),
        "`q` must open the filter already holding itself:\n{}",
        h.screen().contents(),
    );
    assert!(h.screen().contents().contains("Esc clears"), "…in the filtering state");

    // Esc clears the seeded query (transition-waited, per the fused-ESC
    // lesson in the slash test above), a second Esc closes.
    h.write_bytes(b"\x1b");
    assert!(
        h.wait_for(Duration::from_secs(10), |s| !s.contents().contains("keys — /q")).is_some(),
        "the first Esc did not clear the seeded query:\n{}",
        h.screen().contents(),
    );
    h.write_bytes(b"\x1b");
    assert!(
        h.wait_for(Duration::from_secs(10), |s| s.contents().contains("NORMAL")).is_some(),
        "the second Esc did not close the overlay:\n{}",
        h.screen().contents(),
    );
}

/// …and the keys that are not typing keep C15's "closes it": Space is the
/// poster's escape hatch, un-filtered, exactly as before.
#[test]
fn space_still_closes_an_unfiltered_keymap() {
    let Some(mut h) = open_keymap("space-closes") else { return };
    h.write_bytes(b" ");
    assert!(
        h.wait_for(Duration::from_secs(5), |s| s.contents().contains("NORMAL")).is_some(),
        "Space must still close an overlay with no query open:\n{}",
        h.screen().contents(),
    );
}

/// [C41] The palette end to end: type until the row is there, press `↵`,
/// and the thing happens. The unit tests pin the dispatch and the drawing
/// separately; this is the check that they meet on a screen — that the
/// title offers `↵ runs`, that a row is visibly marked as the one it will
/// run, and that pressing it both closes the overlay and fires the action.
///
/// `toggle the hint bar` is the target because its effect is *visible in
/// the same frame that proves the overlay closed*: the C9 bar is either
/// drawn or it is not. A verb whose result lives in the layout would need
/// a second, softer assertion about pane geometry.
#[test]
fn enter_runs_the_command_under_the_cursor() {
    let Some(mut h) = open_keymap("palette") else { return };
    // The C9 bar is *behind* the overlay, so there is nothing to observe
    // until this closes. `enter_still_just_closes_when_the_query_names_no_
    // command` below is the control for the final assertion: it closes the
    // same overlay the same way and finds the bar still there.
    h.write_bytes(b"/toggle the hint bar");
    assert!(
        h.wait_for(Duration::from_secs(10), |s| s.contents().contains("↵ runs")).is_some(),
        "the title never offered the palette's key:\n{}",
        h.screen().contents(),
    );
    let armed = h.screen().contents();
    assert!(armed.contains('❯'), "and the row it will run is marked:\n{armed}");
    assert!(armed.contains("↵ run"), "the hint bar says so too:\n{armed}");

    // Waited on the overlay's own title rather than the C9 mode word,
    // because the command under test *removes the bar the mode word lives
    // in* — `NORMAL` can never appear here, and waiting for it would time
    // out on a screen that is already correct.
    h.write_bytes(b"\r");
    assert!(
        h.wait_for(Duration::from_secs(10), |s| !s.contents().contains("keys — ")).is_some(),
        "Enter did not close the overlay:\n{}",
        h.screen().contents(),
    );
    let after = h.screen().contents();
    assert!(
        !after.contains("Alt+n new"),
        "...and it must have RUN the row, not merely closed on it:\n{after}",
    );
}

/// [C41] A query can match rows that are not commands — the CONTROL CLI
/// block is six of them. The title must not offer `↵ runs` there, and
/// `↵` must do what it did before the palette existed: close.
#[test]
fn enter_still_just_closes_when_the_query_names_no_command() {
    let Some(mut h) = open_keymap("no-command") else { return };
    h.write_bytes(b"/roost read");
    assert!(
        h.wait_for(Duration::from_secs(10), |s| s.contents().contains("/roost read")).is_some(),
        "the query never reached the title:\n{}",
        h.screen().contents(),
    );
    let armed = h.screen().contents();
    assert!(!armed.contains("↵ runs"), "nothing here is runnable, so nothing is offered:\n{armed}");
    assert!(!armed.contains('❯'), "and no row is marked:\n{armed}");

    h.write_bytes(b"\r");
    assert!(
        h.wait_for(Duration::from_secs(10), |s| s.contents().contains("NORMAL")).is_some(),
        "Enter must still close:\n{}",
        h.screen().contents(),
    );
    assert!(
        h.screen().contents().contains("Alt+n new"),
        "and the hint bar is untouched — closing is not running:\n{}",
        h.screen().contents(),
    );
}
