//! The 2026-09-01 map re-key, end to end through a real PTY.
//!
//! Unit tests in `src/ui/input.rs` prove the `KeyEvent` → `Action` table.
//! What they cannot prove is the half that broke people's muscle memory:
//! that the **bytes a terminal actually sends** for these chords reach that
//! table at all. Every chord the re-key touched is punctuation or a shifted
//! letter, and both are exactly where terminal delivery gets interesting —
//! `ESC` + `<` is one glyph on one terminal and `,`-with-the-shift-bit on
//! another, `Alt+Shift+s` arrives as `ESC S` rather than `ESC s`, and
//! `ESC` + anything is a byte away from an escape sequence roost must not
//! mistake it for. A table entry that no terminal can deliver is a chord
//! that does not exist.
//!
//! Three families, one file, because they share the re-key and its fixtures:
//! the `Alt+s`/`Alt+Shift+s` stack pair (C6–C8), the `Alt+- = < >` resize
//! punctuation, and `Alt+m`/`Alt+Shift+m` tab stepping (C2/U7).

// The shared harness is compiled per test binary; helpers other tenants use
// are dead code from this binary's view — not real rot.
#[allow(dead_code)]
mod harness;

use std::process::Command;
use std::time::Duration;

/// The meta-ESC shape a terminal sends for `Alt+<key>`: `ESC` then the
/// key's own byte, exactly what readline's M-b arrives in. The shifted
/// spellings are the *glyph* (`ESC S`, `ESC >`), not a modifier byte —
/// there is nowhere else to put the shift in this encoding, which is why
/// `default_chord_action` binds both the glyph and the base+SHIFT form.
const ALT_N: &[u8] = b"\x1bn";
const ALT_S: &[u8] = b"\x1bs";
const ALT_SHIFT_S: &[u8] = b"\x1bS";
const ALT_GROW_WIDTH: &[u8] = b"\x1b>";
const ALT_SHRINK_WIDTH: &[u8] = b"\x1b<";
const ALT_GROW_HEIGHT: &[u8] = b"\x1b=";
const ALT_SHRINK_HEIGHT: &[u8] = b"\x1b-";
const ALT_M: &[u8] = b"\x1bm";
const ALT_SHIFT_M: &[u8] = b"\x1bM";

/// Control-plane ground truth: the pane the workspace says is focused.
fn focused_pane(state_dir: &std::path::Path) -> Option<u64> {
    let out = Command::new(env!("CARGO_BIN_EXE_roost"))
        .args(["list"])
        .env("ROOST_STATE", state_dir)
        .env_remove("ROOST_SOCK")
        .env_remove("ROOST_TOKEN")
        .env_remove("ROOST_CONTROL_TOKEN")
        .output()
        .ok()?;
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
    let p = v.as_array()?.iter().find(|p| p["focused"] == true)?;
    p["pane"].as_u64()
}

/// Wait for the control CLI to answer at all — it comes up a moment after
/// the first frame, and every scenario here reads it.
fn wait_for_control(h: &mut harness::Harness) -> std::path::PathBuf {
    let sd = h.state_dir().to_path_buf();
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while focused_pane(&sd).is_none() {
        assert!(std::time::Instant::now() < deadline, "the control CLI never came up");
        std::thread::sleep(Duration::from_millis(100));
    }
    sd
}

/// How many panes the frame is drawing, counted by their top-left border
/// corner — enough to watch `Alt+n` land without reaching for the control
/// plane, and it works the same whether the panes are split or stacked.
fn pane_count(screen: &vt100::Screen) -> usize {
    let (rows, cols) = screen.size();
    (0..rows)
        .flat_map(|r| (0..cols).map(move |c| (r, c)))
        .filter(|&(r, c)| screen.cell(r, c).map(|cell| cell.contents()) == Some("┌".to_string()))
        .count()
}

/// The columns of the body's vertical pane borders on the widest body row —
/// the only geometry a resize is visible in from outside the process. Row 0
/// is the tab bar and the last row is the hint bar, so a row in the middle
/// crosses both panes of a side-by-side split.
fn border_columns(screen: &vt100::Screen) -> Vec<u16> {
    let (rows, cols) = screen.size();
    let mid = rows / 2;
    (0..cols)
        .filter(|&c| screen.cell(mid, c).map(|cell| cell.contents()) == Some("│".to_string()))
        .collect()
}

/// C6–C8, re-keyed: `Alt+s` collapses the split into a stack (the header
/// C6 contracts is the visible proof), and `Alt+Shift+s` — delivered as
/// `ESC S`, the uppercase spelling, since meta-ESC has no shift bit to
/// carry — explodes it back. Before the re-key one chord did both; the
/// pair is only a real pair if the second half is deliverable.
#[test]
fn alt_s_stacks_and_alt_shift_s_explodes_through_a_real_terminal() {
    let cwd = std::env::temp_dir();
    let cwd = cwd.to_str().expect("temp dir is valid utf8");
    let Some(mut h) = harness::spawn_or_skip("stack pair e2e", &harness::two_panes(cwd)) else {
        return;
    };
    assert!(h.settle(Duration::from_secs(15)), "roost never drew a first frame");
    assert!(
        h.wait_for(Duration::from_secs(15), |s| s.contents().contains("1 main")).is_some(),
        "roost never drew its tab bar",
    );
    assert!(
        !h.screen().contents().contains("STACK"),
        "the fixture starts as a split, not a stack:\n{}",
        h.screen().contents(),
    );

    h.write_bytes(ALT_S);
    assert!(
        h.wait_for(Duration::from_secs(5), |s| s.contents().contains("STACK · 2 PANES")).is_some(),
        "ESC s never reached the collapse half:\n{}",
        h.screen().contents(),
    );

    h.write_bytes(ALT_SHIFT_S);
    assert!(
        h.wait_for(Duration::from_secs(5), |s| !s.contents().contains("STACK")).is_some(),
        "ESC S never reached the explode half — the chord the re-key added:\n{}",
        h.screen().contents(),
    );

    // ...and the pair is not a toggle any more: a second `Alt+Shift+s` on a
    // tab with no stack left is a refusal, not a re-collapse (C38).
    h.write_bytes(ALT_SHIFT_S);
    h.settle(Duration::from_secs(2));
    assert!(
        !h.screen().contents().contains("STACK · 2 PANES"),
        "Alt+Shift+s re-stacked — it is the explode half only:\n{}",
        h.screen().contents(),
    );

    assert!(h.quit_and_wait(Duration::from_secs(5)).is_some(), "roost did not exit cleanly");
}

/// C42, the reported flow, at a real terminal: four panes built with
/// `Alt+n` the way a user builds them, then `Alt+s` pressed repeatedly from
/// the last one. Each press must absorb one more split — the stack header
/// counts up — and the run must end by naming its ceiling rather than
/// refusing on the second press, which is what it did before C42.
///
/// The header is the right observable precisely because it is what the user
/// sees: "STACK · N PANES" is the count going up.
#[test]
fn holding_alt_s_climbs_the_tab_pane_by_pane_through_a_real_terminal() {
    let cwd = std::env::temp_dir();
    let cwd = cwd.to_str().expect("temp dir is valid utf8");
    let Some(mut h) = harness::spawn_or_skip("stack ladder e2e", &harness::one_pane(cwd)) else {
        return;
    };
    assert!(h.settle(Duration::from_secs(15)), "roost never drew a first frame");
    assert!(
        h.wait_for(Duration::from_secs(15), |s| s.contents().contains("1 main")).is_some(),
        "roost never drew its tab bar",
    );

    // Four panes, built as `Alt+n` builds them — nested splits, not a flat
    // row, which is exactly the shape the one-rung collapse got stuck in.
    for n in 2..=4 {
        h.write_bytes(ALT_N);
        assert!(
            h.wait_for(Duration::from_secs(5), |s| pane_count(s) == n).is_some(),
            "Alt+n never produced pane {n}:\n{}",
            h.screen().contents(),
        );
    }

    // Rung 1 — the innermost split only.
    h.write_bytes(ALT_S);
    assert!(
        h.wait_for(Duration::from_secs(5), |s| s.contents().contains("STACK · 2 PANES")).is_some(),
        "the first press did not collapse the innermost split:\n{}",
        h.screen().contents(),
    );

    // Rungs 2 and 3 — this is the whole contract. Before C42 the header
    // stayed at 2 PANES and the second press flashed "already stacked".
    for want in [3, 4] {
        h.write_bytes(ALT_S);
        assert!(
            h.wait_for(Duration::from_secs(5), |s| s
                .contents()
                .contains(&format!("STACK · {want} PANES")))
                .is_some(),
            "the ladder stalled below {want} panes — this is the reported bug:\n{}",
            h.screen().contents(),
        );
    }

    // The ceiling: one more press changes nothing and says why.
    h.write_bytes(ALT_S);
    assert!(
        h.wait_for(Duration::from_secs(5), |s| s.contents().contains("the whole tab is one stack"))
            .is_some(),
        "the ceiling did not name itself:\n{}",
        h.screen().contents(),
    );
    assert!(
        h.screen().contents().contains("STACK · 4 PANES"),
        "a refusal must not disturb the stack it refused to grow:\n{}",
        h.screen().contents(),
    );

    assert!(h.quit_and_wait(Duration::from_secs(5)).is_some(), "roost did not exit cleanly");
}

/// The resize family's whole point is that it moved *off* the arrows onto
/// punctuation, and punctuation is where meta-ESC delivery is least
/// obvious: `ESC >` is a byte pair that a parser reading escape sequences
/// could plausibly swallow. This drives all four glyphs at a real terminal
/// and watches the split's divider move.
#[test]
fn the_resize_punctuation_moves_the_divider_through_a_real_terminal() {
    let cwd = std::env::temp_dir();
    let cwd = cwd.to_str().expect("temp dir is valid utf8");
    let Some(mut h) = harness::spawn_or_skip("resize e2e", &harness::two_panes(cwd)) else {
        return;
    };
    assert!(h.settle(Duration::from_secs(15)), "roost never drew a first frame");
    assert!(
        h.wait_for(Duration::from_secs(15), |s| !border_columns(s).is_empty()).is_some(),
        "roost never drew a bordered split:\n{}",
        h.screen().contents(),
    );
    let before = border_columns(h.screen());
    assert!(
        before.len() >= 3,
        "a side-by-side split draws at least three border columns: {before:?}"
    );

    // Pane 1 (the left one) is focused, so growing its width pushes every
    // border right of it further right.
    h.write_bytes(ALT_GROW_WIDTH);
    let widened = h
        .wait_for(Duration::from_secs(5), |s| border_columns(s) != before)
        .map(|_| border_columns(h.screen()))
        .unwrap_or_else(|| panic!("ESC > never reached the width-grow arm: {before:?}"));
    assert!(
        widened.last() >= before.last() && widened[1] > before[1],
        "growing the focused pane's width must push the divider right: {before:?} -> {widened:?}",
    );

    // ...and the shrink glyph walks it back. Not necessarily to the exact
    // starting columns — the resize step is a ratio delta, and rounding at
    // this width need not be its own inverse — so this asserts direction,
    // which is the contract, rather than an exact frame.
    h.write_bytes(ALT_SHRINK_WIDTH);
    let narrowed = h
        .wait_for(Duration::from_secs(5), |s| border_columns(s) != widened)
        .map(|_| border_columns(h.screen()))
        .unwrap_or_else(|| panic!("ESC < never reached the width-shrink arm: {widened:?}"));
    assert!(
        narrowed[1] < widened[1],
        "shrinking must walk the divider back left: {widened:?} -> {narrowed:?}",
    );

    // The height pair has no divider to move in a side-by-side split (C38
    // flashes "nothing to resize here"), so what this proves is the half a
    // unit test cannot: `ESC =` and `ESC -` arrive as chords rather than
    // being forwarded into the pane as text. A forwarded `=` or `-` would
    // land on the shell's command line and be echoed there.
    h.write_bytes(ALT_GROW_HEIGHT);
    h.write_bytes(ALT_SHRINK_HEIGHT);
    h.settle(Duration::from_secs(2));
    let frame = h.screen().contents();
    assert!(
        !frame.contains("=-") && !frame.contains("-="),
        "the height glyphs were forwarded into the pane instead of taken as chords:\n{frame}",
    );

    assert!(h.quit_and_wait(Duration::from_secs(5)).is_some(), "roost did not exit cleanly");
}

/// C2/U7 re-keyed: `Alt+m` steps the tab strip forward and `Alt+Shift+m`
/// (delivered `ESC M`) steps it back. `ESC M` is worth an e2e all by
/// itself — it is **RI, reverse index**, a real escape sequence — so this
/// pins that roost reads it as a chord on the way *in* rather than as
/// terminal output on the way out.
#[test]
fn alt_m_steps_the_tab_strip_both_ways_through_a_real_terminal() {
    let cwd = std::env::temp_dir();
    let cwd = cwd.to_str().expect("temp dir is valid utf8");
    let Some(mut h) = harness::spawn_or_skip("tab step e2e", &harness::two_tabs(cwd)) else {
        return;
    };
    assert!(
        h.wait_for(Duration::from_secs(15), |s| s.contents().contains("1 main")).is_some(),
        "roost never drew its tab bar",
    );
    let sd = wait_for_control(&mut h);
    assert_eq!(focused_pane(&sd), Some(1), "the fixture starts on tab 1's only pane");

    h.write_bytes(ALT_M);
    assert!(
        h.wait_for(Duration::from_secs(5), |_| focused_pane(&sd) == Some(2)).is_some(),
        "ESC m never stepped to the next tab (focus is {:?}):\n{}",
        focused_pane(&sd),
        h.screen().contents(),
    );

    h.write_bytes(ALT_SHIFT_M);
    assert!(
        h.wait_for(Duration::from_secs(5), |_| focused_pane(&sd) == Some(1)).is_some(),
        "ESC M never stepped back — the shifted half reverses, it does not carry the pane \
         (focus is {:?}):\n{}",
        focused_pane(&sd),
        h.screen().contents(),
    );

    // The re-key's actual regression risk, pinned: `Alt+Shift+m` used to
    // *carry the focused pane* to another tab. If it still did, pane 1
    // would have changed tabs rather than focus merely returning to it.
    let out = Command::new(env!("CARGO_BIN_EXE_roost"))
        .args(["list"])
        .env("ROOST_STATE", &sd)
        .env_remove("ROOST_SOCK")
        .env_remove("ROOST_TOKEN")
        .env_remove("ROOST_CONTROL_TOKEN")
        .output()
        .expect("run roost list");
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).expect("roost list is json");
    let panes = v.as_array().expect("roost list is an array");
    for (id, tab) in [(1u64, 0u64), (2, 1), (3, 1)] {
        let p = panes.iter().find(|p| p["pane"].as_u64() == Some(id)).expect("pane is listed");
        assert_eq!(
            p["tab"].as_u64(),
            Some(tab),
            "stepping the strip moved pane {id} between tabs — that is the carry verb (Alt+i)",
        );
    }

    assert!(h.quit_and_wait(Duration::from_secs(5)).is_some(), "roost did not exit cleanly");
}
