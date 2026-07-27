//! P1 (SPEC-parity) end to end: synchronized output, DEC private mode 2026.
//!
//! An application wraps a redraw in `CSI ?2026h … CSI ?2026l` to say "do not
//! present anything between these" — the mechanism behind a year of "Claude
//! Code flickers/tears in tmux" reports. roost used to have no 2026 arm at
//! all: the renderer blitted, and `roost read` reported, whatever the parser
//! happened to hold — measured at 31 of 50 server-side samples caught
//! mid-bracket (cleared or half-drawn).
//!
//! The gate drives the defect's own shape: a pane loops
//! `?2026h` + clear + head + *sleep inside the bracket* + tail + `?2026l`,
//! and the test samples `roost read` through the real control CLI while it
//! runs. Every sample must be a complete frame — never a head without its
//! tail. Pre-fix this fails within a handful of samples; the sleep sits
//! inside the bracket precisely so a naive sampler lands there ~half the
//! time.

// The shared harness is compiled per test binary; helpers other tenants use
// are dead code from this binary's view — not real rot.
#[allow(dead_code)]
mod harness;

use std::process::Command;
use std::time::Duration;

use harness::Harness;

/// One shell pane. Temp-dir cwd keeps C4's corner badge short (same
/// reasoning as the firehose gate's fixture).
fn fixture_workspace(cwd: &str) -> String {
    serde_json::json!({
        "version": 1,
        "active_tab": 0,
        "tabs": [{
            "name": "main",
            "layout": { "pane": 1 },
            "panes": { "1": {"adapter": "shell", "cwd": cwd} }
        }]
    })
    .to_string()
}

/// `roost read 1` against the harness instance, via the real client CLI
/// (ROOST_STATE routes it to the instance's socket + fleet control token) —
/// the exact server-side surface P1 measured tearing on.
fn cli_read(state_dir: &std::path::Path) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_roost"))
        .args(["read", "1"])
        .env("ROOST_STATE", state_dir)
        .env_remove("ROOST_SOCK")
        .env_remove("ROOST_TOKEN")
        .env_remove("ROOST_CONTROL_TOKEN")
        .output()
        .expect("run roost read");
    String::from_utf8_lossy(&out.stdout).to_string()
}

#[test]
fn a_pane_inside_a_2026_bracket_is_never_read_mid_redraw() {
    let cwd = std::env::temp_dir();
    let cwd = cwd.to_str().expect("temp dir is valid utf8");
    let mut h = match Harness::try_spawn(&fixture_workspace(cwd)) {
        Ok(h) => h,
        Err(reason) => {
            eprintln!("SKIP sync-output gate: {reason}");
            return;
        }
    };
    assert!(h.settle(Duration::from_secs(5)), "initial frame never settled");

    // The frame's two halves are written 60 ms apart *inside* the bracket,
    // so a reader that ignores 2026 sees HEAD-without-TAIL (or a bare
    // cleared grid) about half the time. Both markers are spelled split
    // (`SYNC''HEAD`) so the shell's echo of the command line can never
    // satisfy — or falsify — an assertion; only printf's real output can.
    // 60 ms sits comfortably under the 150 ms staleness cap, so the honest
    // path is "present the previous complete frame", not "cap expired".
    h.write_bytes(
        b"while :; do printf '\\033[?2026h\\033[2J\\033[HSYNC''HEAD'; sleep 0.06; \
          printf 'SYNC''TAIL\\033[?2026l'; sleep 0.06; done\r",
    );

    // Wait until the loop has produced at least one complete frame, so the
    // samples below are taken against a running animation rather than the
    // shell prompt.
    let mut seen = false;
    for _ in 0..100 {
        if cli_read(h.state_dir()).contains("SYNCTAIL") {
            seen = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(seen, "pane never ran the synchronized-output loop");

    // Sample the server-side read repeatedly. Each CLI round trip is a few
    // milliseconds, so 60 samples straddle many bracket cycles.
    let mut torn = Vec::new();
    for i in 0..60 {
        let text = cli_read(h.state_dir());
        if text.contains("SYNCHEAD") && !text.contains("SYNCTAIL") {
            torn.push(i);
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        torn.is_empty(),
        "{}/60 `roost read` samples caught the pane mid-bracket \
         (SYNCHEAD without SYNCTAIL) at indices {torn:?} — mode 2026 must \
         present the last complete frame while a bracket is open",
        torn.len()
    );

    h.write_bytes(b"\x03"); // Ctrl+C ends the loop
    h.settle(Duration::from_secs(2));
    let _ = h.quit_and_wait(Duration::from_secs(5));
}
