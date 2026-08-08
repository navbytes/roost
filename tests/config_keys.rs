//! config.json — the key-bindings escape hatch (`src/ui/input.rs`) — is
//! actually read from wherever `ROOST_STATE` points, end to end. The parsing
//! and translation logic itself is unit-tested directly in `src/ui/input.rs`
//! (no PTY needed there); this file proves the one thing that genuinely
//! needs a real process: that roost finds the file in its *own* isolated
//! state dir rather than, say, silently skipping it or reading the
//! developer's real one. (Not a unit test: `$ROOST_STATE` is a process-global
//! env var, racy across unit tests running in parallel in one process — see
//! `src/infra/sock.rs`'s own tests for the same reason.)

#[allow(dead_code)]
mod harness;

use std::time::Duration;

use harness::Harness;

/// One shell pane. Temp-dir cwd keeps the corner badge short (same
/// reasoning as the other PTY-harness fixtures).
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

/// Alt+f, meta-ESC encoding — the readline collision this whole feature
/// exists to fix (DESIGN doc). Same convention as `harness::ALT_Q`.
const ALT_F: &[u8] = b"\x1bf";

#[test]
fn config_json_disabling_alt_f_is_read_from_the_roost_state_dir() {
    let cwd = std::env::temp_dir();
    let cwd = cwd.to_str().expect("temp dir is valid utf8");
    let config = serde_json::json!({ "keys": { "alt+f": "disable" } }).to_string();
    let mut h = match Harness::try_spawn_with_config(&fixture_workspace(cwd), &config) {
        Ok(h) => h,
        Err(reason) => {
            eprintln!("SKIP config-keys gate: {reason}");
            return;
        }
    };
    assert!(
        h.settle(Duration::from_secs(5)),
        "initial frame never settled"
    );

    // Same ECHOCTL trick tests/cursor_mode.rs uses: park a dumb `cat` sink
    // so the bytes roost forwards echo back onto the screen verbatim,
    // independent of the shell's own readline (which would otherwise
    // swallow Alt+f as its own forward-word binding and leave nothing to
    // observe either way).
    h.write_bytes(b"printf READY; cat\r");
    h.wait_for(Duration::from_secs(5), |s| s.contents().contains("READY"))
        .expect("pane never reached the cat sink");

    h.write_bytes(ALT_F);
    // Without the disable entry, alt+f is roost's own ToggleFloat chord — it
    // would never reach the pane at all, so this can only pass if
    // config.json was actually read from the harness's own ROOST_STATE dir
    // (which starts empty but for what the harness itself wrote into it).
    if h.wait_for(Duration::from_secs(5), |s| s.contents().contains("^[f"))
        .is_none()
    {
        panic!(
            "alt+f was not forwarded to the pane — config.json's disable entry \
             was not picked up from ROOST_STATE:\n{}",
            h.screen().contents()
        );
    }

    let _ = h.quit_and_wait(Duration::from_secs(5));
}
