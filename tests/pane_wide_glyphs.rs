//! SPEC-ux U24 / SPEC-parity P15+P17 end to end: the CJK/emoji golden frame.
//!
//! U24's contract asks for exactly this — "add a CJK/emoji golden frame to the
//! harness" — and it is the only gate that exercises the whole chain at once:
//! a pane prints wide text, roost's parser measures it, `blit_screen` lays it
//! into the ratatui buffer, and the backend flushes it to the host terminal.
//! Every link has to agree about which columns a glyph owns, and they used to
//! disagree in two ways:
//!
//!   * `blit_screen` stamped `" "` over the cell after a two-column glyph
//!     (self-documented as approximate), and
//!   * the parser measured widths with unicode-width 0.1.14 while the backend
//!     measured the very same symbol with 0.2 (P17). For a VS16 emoji-
//!     presentation sequence the two differ, so the parser put the *next*
//!     glyph in the column the backend was already treating as the emoji's
//!     right half — and ratatui's diff, which skips the cell after a
//!     two-column symbol, dropped that glyph from the host stream entirely.
//!
//! Asserting on the host's parsed grid catches both: a dropped, doubled, or
//! space-split glyph all fail the same verbatim comparison.

// The shared harness is compiled per test binary; helpers other tenants use
// are dead code from this binary's view — not real rot.
#[allow(dead_code)]
mod harness;

use std::time::Duration;

/// The payload: CJK (natively wide), a VS16 emoji-presentation sequence (the
/// pair the two width tables disagreed about), and a natively-wide emoji,
/// interleaved with narrow text so a column slip shows up as corrupted ASCII.
const CJK: &str = "\u{65e5}\u{672c}\u{8a9e}";
const VS16: &str = "\u{2764}\u{fe0f}";
const EMOJI: &str = "\u{1f600}";

#[test]
fn wide_glyphs_reach_the_host_intact_and_own_two_columns_each() {
    let expected = format!("W1[{CJK}]W2[{VS16}]W3[{EMOJI}]END");

    let cwd = std::env::temp_dir();
    let cwd = cwd.to_str().expect("temp dir is valid utf8");
    let Some(mut h) = harness::spawn_or_skip("wide-glyph gate", &harness::one_pane(cwd)) else {
        return;
    };
    assert!(h.settle(Duration::from_secs(5)), "initial frame never settled");

    // `W''1[` splits the marker across two shell string literals, so the
    // echoed command line cannot satisfy the assertion — only the printed
    // output, which is what actually round-trips through the blit, can.
    let cmd = format!("printf 'W''1[{CJK}]W2[{VS16}]W3[{EMOJI}]END\\n'\r");
    h.write_bytes(cmd.as_bytes());

    let found = h.wait_for(Duration::from_secs(5), |s| s.contents().contains(&expected));
    assert!(
        found.is_some(),
        "the pane's wide text never reached the host intact.\nexpected: {expected:?}\ngot:\n{}",
        h.screen().contents()
    );

    // Golden frame: the row's *cells*, not just its reconstructed text. A
    // two-column glyph must occupy one wide cell followed by an empty
    // continuation — never two cells each carrying something.
    let screen = h.screen();
    let (rows, cols) = screen.size();
    let row = (0..rows)
        .find(|&r| {
            let mut line = String::new();
            for c in 0..cols {
                if let Some(cell) = screen.cell(r, c) {
                    line.push_str(&cell.contents());
                }
            }
            line.contains(&expected)
        })
        .expect("the printed row is on screen");

    let mut checked = 0;
    for col in 0..cols {
        let Some(cell) = screen.cell(row, col) else { continue };
        if ![CJK[..3].to_string(), VS16.to_string(), EMOJI.to_string()].contains(&cell.contents()) {
            continue;
        }
        assert!(
            cell.is_wide(),
            "{:?} at row {row} col {col} should own two columns",
            cell.contents()
        );
        let next = screen.cell(row, col + 1).expect("a wide glyph has a right half");
        assert!(
            next.is_wide_continuation() && next.contents().is_empty(),
            "the cell after {:?} should be its untouched continuation, got {:?}",
            cell.contents(),
            next.contents()
        );
        checked += 1;
    }
    assert!(checked >= 3, "expected the wide glyphs on the row, checked {checked}");

    let _ = h.quit_and_wait(Duration::from_secs(5));
}
