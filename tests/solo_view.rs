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

/// Like `harness::spawn_or_skip`, but for `Harness::try_spawn_at` — the seam
/// this file's relaunch scenario needs (a caller-owned `ROOST_STATE` root
/// two spawns share), with the same "skip, don't fail" escape hatch for a
/// sandboxed runner with no usable PTY.
fn spawn_at_or_skip(root: &std::path::Path, what: &str) -> Option<harness::Harness> {
    match harness::Harness::try_spawn_at(root, &[], &[]) {
        Ok(h) => Some(h),
        Err(reason) => {
            eprintln!("SKIP {what}: {reason}");
            None
        }
    }
}

/// C43's headline claim — "a solo tab comes back solo, with its remembered
/// focus, on launch" (DESIGN-ui.md, C43 "Persistence and control plane") —
/// is a round trip through `workspace.json` on disk, at process boundaries
/// the single-process test above never crosses: it toggles, steps and
/// tiles back all inside one `roost` run, so it cannot tell "solo state
/// lives in memory" from "solo state round-trips through the saved file".
/// This spawns roost once, enters solo, steps the rail off the tab's first
/// pane, quits cleanly, then relaunches against the very same `ROOST_STATE`
/// and asserts the tab comes back solo, showing the same (non-first) pane.
#[test]
fn a_solo_tab_with_a_stepped_focus_survives_quit_and_relaunch_through_a_real_terminal() {
    let cwd = std::env::temp_dir();
    let cwd = cwd.to_str().expect("temp dir is valid utf8");
    let root = harness::shared_state_dir("solorelaunch");
    std::fs::write(root.join("workspace.json"), harness::two_panes(cwd))
        .expect("seed workspace.json");

    let Some(mut h1) = spawn_at_or_skip(&root, "solo relaunch e2e") else {
        let _ = std::fs::remove_dir_all(&root);
        return;
    };
    assert!(h1.settle(Duration::from_secs(15)), "roost never drew a first frame");
    assert!(
        h1.wait_for(Duration::from_secs(15), |s| s.contents().contains("1 main")).is_some(),
        "roost never drew its tab bar",
    );
    assert_eq!(pane_count(h1.screen()), 2, "the fixture starts as two tiled panes");

    // Enter solo, then step off the tab's first pane — the state a bare
    // toggle-on can't distinguish from "always shows the first pane".
    h1.write_bytes(ALT_SHIFT_T);
    assert!(
        h1.wait_for(Duration::from_secs(5), |s| s.contents().contains("SOLO · 2 PANES")).is_some(),
        "ESC T never reached ToggleSolo — the rail header never drew:\n{}",
        h1.screen().contents(),
    );
    let first_marker = marked_row(h1.screen());
    assert!(first_marker.is_some(), "the focused row must carry the ▎ marker on entry");

    h1.write_bytes(ALT_DOWN);
    let stepped_marker = h1.wait_for(Duration::from_secs(5), |s| {
        marked_row(s).is_some() && marked_row(s) != first_marker
    });
    assert!(
        stepped_marker.is_some(),
        "Alt+↓ never moved the ▎ marker off row {first_marker:?}:\n{}",
        h1.screen().contents(),
    );
    let stepped_marker = marked_row(h1.screen());

    assert!(h1.quit_and_wait(Duration::from_secs(5)).is_some(), "roost did not exit cleanly");

    // `h1` stays bound (not dropped) until the relaunch has spawned: its
    // `Drop` removes the shared `ROOST_STATE` root, and the second instance
    // needs that root's `workspace.json` still there to read (see
    // `Harness::try_spawn_at`'s own doc comment).
    let Some(mut h2) = spawn_at_or_skip(&root, "solo relaunch e2e (relaunch)") else {
        return;
    };
    assert!(h2.settle(Duration::from_secs(15)), "the relaunched roost never drew a first frame");
    assert!(
        h2.wait_for(Duration::from_secs(15), |s| s.contents().contains("SOLO · 2 PANES")).is_some(),
        "the relaunched tab did not come back solo:\n{}",
        h2.screen().contents(),
    );
    assert_eq!(
        marked_row(h2.screen()),
        stepped_marker,
        "the relaunched tab must remember the stepped-to pane, not fall back to the first:\n{}",
        h2.screen().contents(),
    );

    assert!(
        h2.quit_and_wait(Duration::from_secs(5)).is_some(),
        "relaunched roost did not exit cleanly"
    );
}
