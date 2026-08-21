//! C28, end to end: `Alt+Shift+m` carries the focused pane into the next tab
//! and takes focus with it — **without restarting the process running in it**.
//!
//! A unit test can prove the layout trees changed hands. Only a real PTY can
//! prove the chord survives terminal delivery (`ESC` + uppercase `M`, no kitty
//! disambiguation), that the shell in the moved pane is the *same* shell
//! afterwards, and that `roost list` — an independent process reading the
//! control socket — agrees about which tab the pane now lives in.

// The shared harness is compiled per test binary; helpers other tenants use
// are dead code from this binary's view — not real rot.
#[allow(dead_code)]
mod harness;

use std::process::Command;
use std::time::Duration;

/// `Alt+Shift+m` as a terminal delivers it: ESC + uppercase `M`. (Like
/// `Alt+Shift+a`, and unlike C23's `ESC`+`P`, this pair introduces nothing —
/// no DCS-style ambiguity. `ESC [` would have, which is why `Alt+]` was
/// rejected for this verb; see §8.)
const ALT_SHIFT_M: &[u8] = b"\x1bM";

/// Control-plane ground truth: `(tab index, focused)` for pane `id`.
fn pane_tab(state_dir: &std::path::Path, id: u64) -> Option<(u64, bool)> {
    let out = Command::new(env!("CARGO_BIN_EXE_roost"))
        .args(["list"])
        .env("ROOST_STATE", state_dir)
        .env_remove("ROOST_SOCK")
        .env_remove("ROOST_TOKEN")
        .env_remove("ROOST_CONTROL_TOKEN")
        .output()
        .expect("run roost list");
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
    let p = v.as_array()?.iter().find(|p| p["pane"].as_u64() == Some(id))?;
    Some((p["tab"].as_u64()?, p["focused"] == true))
}

#[test]
fn alt_shift_m_moves_the_focused_pane_into_the_next_tab_without_restarting_it() {
    let cwd = std::env::temp_dir();
    let cwd = cwd.to_str().expect("temp dir is valid utf8");
    let Some(mut h) = harness::spawn_or_skip("move-pane e2e", &harness::two_tabs(cwd)) else {
        return;
    };
    let sd = h.state_dir().to_path_buf();
    assert!(
        h.wait_for(Duration::from_secs(15), |s| s.contents().contains("1 main")).is_some(),
        "roost never drew its tab bar",
    );
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while pane_tab(&sd, 1).is_none() {
        assert!(std::time::Instant::now() < deadline, "the control CLI never came up");
        std::thread::sleep(Duration::from_millis(100));
    }
    assert_eq!(pane_tab(&sd, 1), Some((0, true)), "the fixture starts on tab 1's only pane");

    // Mark the running shell so "the same process" is provable after the
    // move rather than assumed: a shell variable survives a move and cannot
    // survive a respawn.
    h.write_bytes(b"MARK=keep-me; echo ready-ma''rk\r");
    assert!(
        h.wait_for(Duration::from_secs(10), |s| s.contents().contains("ready-mark")).is_some(),
        "pane input never went live",
    );

    // -- move it ------------------------------------------------------------
    h.write_bytes(ALT_SHIFT_M);
    assert!(
        h.wait_for(Duration::from_secs(5), |s| pane_tab(&sd, 1).is_some_and(|(t, _)| t == 0)
            && s.contents().contains("1 api"))
            .is_some(),
        "the tab bar never showed the move:\n{}",
        h.screen().contents(),
    );
    h.settle(Duration::from_secs(2));

    // `main` held only pane 1, so moving it away removed that tab — `api` is
    // the whole workspace now, and it is at index 0.
    let frame = h.screen().contents();
    assert!(
        frame.lines().next().is_some_and(|bar| bar.contains("1 api") && !bar.contains("main")),
        "the emptied source tab went with the pane:\n{frame}",
    );
    for id in [1, 2, 3] {
        assert_eq!(
            pane_tab(&sd, id).map(|(t, _)| t),
            Some(0),
            "pane {id} shares the one remaining tab",
        );
    }
    assert_eq!(pane_tab(&sd, 1), Some((0, true)), "focus followed the pane it moved");

    // -- and the process it was running is the one still running ------------
    h.write_bytes(b"echo \"mark=$MARK\"\r");
    assert!(
        h.wait_for(Duration::from_secs(10), |s| s.contents().contains("mark=keep-me")).is_some(),
        "the moved pane restarted its shell — the whole point is that it does not:\n{}",
        h.screen().contents(),
    );

    assert!(h.quit_and_wait(Duration::from_secs(5)).is_some(), "roost did not exit cleanly");
}
