//! A single oversized `roost send` must not wedge the process.
//!
//! 60 KB is under the control plane's 64 KiB line cap, so it is accepted — and
//! it is far more than a kernel tty input queue holds. Before the fix, roost's
//! event loop performed that write itself and parked in `write(2)`: the loop is
//! also the only drain for the PTY event channel, so the pane's reader thread
//! filled it and parked too, the pane's output backed up, its line discipline
//! stopped consuming input, and the write could never complete. One `send` took
//! down the control plane, the renderer and the keyboard, permanently.
//!
//! The pane is deliberately parked in a child that never reads stdin. A shell
//! sitting at its prompt reproduces it too, but only on a line discipline that
//! stops consuming (measured: /bin/sh and /bin/bash wedge, zsh's ZLE does not)
//! — and "the child may simply never drain" is the case the fix has to hold
//! for, so the gate asserts it directly instead of depending on whatever
//! `$SHELL` the host prefers.

#[allow(dead_code)]
mod harness;

use std::process::Command;
use std::time::{Duration, Instant};

/// Comfortably under `MAX_LINE` (64 KiB) with room for the JSON envelope and
/// token — the size QA froze roost with.
const PAYLOAD: usize = 60_000;

/// Run `roost <args>` against `state_dir`, killing it if it outlives
/// `timeout`. `None` means the call hung — the defect.
fn roost_cli(state_dir: &std::path::Path, args: &[&str], timeout: Duration) -> Option<Duration> {
    let mut child = Command::new(env!("CARGO_BIN_EXE_roost"))
        .args(args)
        .env("ROOST_STATE", state_dir)
        // The CLI must resolve this instance's socket and token from
        // ROOST_STATE alone — never from whatever the developer's shell
        // happens to export.
        .env_remove("ROOST_SOCK")
        .env_remove("ROOST_TOKEN")
        .env_remove("ROOST_CONTROL_TOKEN")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn roost cli");
    let start = Instant::now();
    loop {
        if matches!(child.try_wait(), Ok(Some(_))) {
            return Some(start.elapsed());
        }
        if start.elapsed() >= timeout {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn a_send_to_a_pane_that_never_reads_leaves_roost_fully_alive() {
    let cwd = std::env::temp_dir();
    let cwd = cwd.to_str().expect("temp dir is valid utf8");
    let Some(mut h) = harness::spawn_or_skip("send-backpressure gate", &harness::one_pane(cwd))
    else {
        return;
    };
    assert!(h.settle(Duration::from_secs(5)), "initial frame never settled");

    // `settle` can agree on two blank frames before roost has bound its
    // control socket — without this wait the CLI calls below fail to connect
    // and the gate passes without ever exercising the send path at all.
    let state = h.state_dir().to_path_buf();
    let sock = state.join("roost.sock");
    let start = Instant::now();
    while !sock.exists() && start.elapsed() < Duration::from_secs(10) {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(sock.exists(), "roost never bound its control socket");
    assert!(
        roost_cli(&state, &["list"], Duration::from_secs(10)).is_some(),
        "control plane was not answering before the oversized send"
    );

    // Park the pane in a child that reads nothing, ever. `exec` so the shell
    // is replaced rather than waiting on a job it would still read for.
    h.write_bytes(b"exec sleep 300\r");
    std::thread::sleep(Duration::from_millis(1500));

    let payload = "A".repeat(PAYLOAD);
    let sent = roost_cli(&state, &["send", "1", &payload], Duration::from_secs(20));
    assert!(sent.is_some(), "`roost send` of {PAYLOAD} bytes never returned (>20s) — wedged");

    // The damage was never the one slow call: it was that everything after it
    // was dead too. A second control request is the cheapest proof the event
    // loop is still turning.
    let after = roost_cli(&state, &["list"], Duration::from_secs(15));
    assert!(after.is_some(), "control plane stopped answering after the oversized send");

    // ...and so is the TUI's own input path — QA's Alt+q went unanswered for
    // the whole freeze, which is how a user actually discovers this.
    let exited = h.quit_and_wait(Duration::from_secs(10));
    assert!(exited.is_some(), "roost did not respond to Alt+q after the oversized send");
}
