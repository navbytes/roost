//! C27, end to end: the fleet roster lists panes that live in a tab you are
//! **not** looking at, and Enter goes to one of them across the tab boundary.
//!
//! That cross-tab case is the whole reason the contract exists — `Alt+a`
//! reaches other tabs' panes without listing them, `Alt+e` is a time-ordered
//! log rather than current state, and a tab with three needy agents renders
//! as one `◆`. A unit test can pin the row model; only a real PTY can prove
//! the chord arrives, the overlay draws those rows, and the jump lands —
//! with `roost list` as independent ground truth for where focus ended up.

// The shared harness is compiled per test binary; helpers other tenants use
// are dead code from this binary's view — not real rot.
#[allow(dead_code)]
mod harness;

use std::process::Command;
use std::time::Duration;

use harness::Harness;

/// Two tabs: `main` holds one pane, `api` holds two. The roster must show
/// all three under two headers whichever tab is active.
fn fixture_workspace(cwd: &str) -> String {
    serde_json::json!({
        "version": 1,
        "active_tab": 0,
        "tabs": [
            {
                "name": "main",
                "layout": { "pane": 1 },
                "panes": { "1": {"adapter": "shell", "cwd": cwd} }
            },
            {
                "name": "api",
                "layout": {
                    "split": {
                        "dir": "vertical",
                        "ratios": [0.5, 0.5],
                        "children": [{ "pane": 2 }, { "pane": 3 }]
                    }
                },
                "panes": {
                    "2": {"adapter": "shell", "cwd": cwd},
                    "3": {"adapter": "shell", "cwd": cwd}
                }
            }
        ],
        "next_pane_id": 4
    })
    .to_string()
}

/// `Alt+Shift+a` as a terminal delivers it without the kitty disambiguation:
/// ESC + uppercase `A`. (Unlike C23's ESC+`P`, this pair is not an escape
/// introducer, so there is no DCS-style ambiguity — SPEC-ux N3.)
const ALT_SHIFT_A: &[u8] = b"\x1bA";

/// Control-plane ground truth: which pane `roost list` says is focused.
fn focused_pane(state_dir: &std::path::Path) -> u64 {
    let out = Command::new(env!("CARGO_BIN_EXE_roost"))
        .args(["list"])
        .env("ROOST_STATE", state_dir)
        .env_remove("ROOST_SOCK")
        .env_remove("ROOST_TOKEN")
        .env_remove("ROOST_CONTROL_TOKEN")
        .output()
        .expect("run roost list");
    serde_json::from_slice::<serde_json::Value>(&out.stdout)
        .ok()
        .and_then(|v| {
            v.as_array()?
                .iter()
                .find(|p| p["focused"] == true)
                .and_then(|p| p["pane"].as_u64())
        })
        .unwrap_or(0)
}

#[test]
fn the_roster_lists_another_tabs_panes_and_jumps_across_to_one() {
    let cwd = std::env::temp_dir();
    let cwd = cwd.to_str().expect("temp dir is valid utf8");
    let mut h = match Harness::try_spawn(&fixture_workspace(cwd)) {
        Ok(h) => h,
        Err(reason) => {
            eprintln!("SKIP roster e2e: {reason}");
            return;
        }
    };
    let sd = h.state_dir().to_path_buf();
    assert!(
        h.wait_for(Duration::from_secs(15), |s| s.contents().contains("1 main")).is_some(),
        "roost never drew its tab bar",
    );
    // ...and until the control socket answers, so the ground-truth probe is
    // trustworthy rather than merely early.
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while focused_pane(&sd) == 0 {
        assert!(std::time::Instant::now() < deadline, "the control CLI never came up");
        std::thread::sleep(Duration::from_millis(100));
    }
    assert_eq!(focused_pane(&sd), 1, "the fixture starts on tab 1's only pane");

    // The chord has to actually reach roost before anything is measured: a
    // key sent while crossterm's keyboard-enhancement probe still owns the
    // reader arrives late and desyncs the rest of the script.
    h.write_bytes(b"echo ro''ster-up\r");
    assert!(
        h.wait_for(Duration::from_secs(10), |s| s.contents().contains("roster-up")).is_some(),
        "pane input never went live",
    );

    // -- open the roster ---------------------------------------------------
    h.write_bytes(ALT_SHIFT_A);
    assert!(
        h.wait_for(Duration::from_secs(5), |s| s.contents().contains("fleet")).is_some(),
        "Alt+Shift+a never opened the roster",
    );
    h.settle(Duration::from_secs(2));
    let frame = h.screen().contents();

    // Both tabs are grouped, headers included — including the tab that is
    // NOT active, whose panes have no other resting surface in roost.
    assert!(frame.contains("1 MAIN · 1 PANE"), "tab 1's group header:\n{frame}");
    assert!(frame.contains("2 API · 2 PANES"), "the INACTIVE tab's group header:\n{frame}");
    // C8's row format for each of the inactive tab's panes: the id leads.
    for id in [2, 3] {
        assert!(
            frame.lines().any(|l| l.contains(&format!("{id} shell"))),
            "pane {id} (inactive tab) is listed:\n{frame}",
        );
    }
    assert!(frame.contains("ROSTER"), "the C9 mode word is on the bar:\n{frame}");

    // -- walk to a pane in the other tab and go there -----------------------
    h.write_bytes(b"\x1b[B"); // ↓ — skips tab 2's header, lands on pane 2
    h.settle(Duration::from_secs(2));
    h.write_bytes(b"\r"); // Enter: jump
    assert!(
        h.wait_for(Duration::from_secs(5), |s| !s.contents().contains("ROSTER")).is_some(),
        "the roster never closed behind the jump",
    );
    h.settle(Duration::from_secs(2));

    let landed = focused_pane(&sd);
    assert_eq!(landed, 2, "Enter jumped across tabs to the second tab's first pane");
    let frame = h.screen().contents();
    assert!(
        frame.lines().next().is_some_and(|bar| bar.contains("2 api")),
        "the tab bar followed the jump:\n{frame}",
    );

    // -- the entry chord closes it (U18) -----------------------------------
    h.write_bytes(ALT_SHIFT_A);
    assert!(
        h.wait_for(Duration::from_secs(5), |s| s.contents().contains("fleet")).is_some(),
        "the roster did not reopen",
    );
    h.write_bytes(ALT_SHIFT_A);
    assert!(
        h.wait_for(Duration::from_secs(5), |s| !s.contents().contains("ROSTER")).is_some(),
        "Alt+Shift+a a second time must close the roster (U18)",
    );
    assert_eq!(focused_pane(&sd), landed, "toggling closed moves nothing");

    assert!(h.quit_and_wait(Duration::from_secs(5)).is_some(), "roost did not exit cleanly");
}
