//! P6 (SPEC-parity) end to end: live pane titles, both directions.
//!
//! Claude Code continuously publishes `spinner + task` through OSC 0/2 —
//! the cheapest live fleet-status text there is. vt100 has been parsing and
//! storing it all along, with **zero** call sites reading `screen().title()`;
//! and nothing re-emitted a title to the host, so the outer tab went stale
//! the moment roost started.
//!
//! Inbound: a pane's OSC 2 becomes its display name, visible on the corner
//! badge. Outbound: roost publishes `roost · <focused pane>` to its own
//! terminal.

// The shared harness is compiled per test binary; helpers other tenants use
// are dead code from this binary's view — not real rot.
#[allow(dead_code)]
mod harness;

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

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

#[test]
fn a_pane_osc_title_names_it_on_the_badge_and_in_the_host_title() {
    let cwd = std::env::temp_dir();
    let cwd = cwd.to_str().expect("temp dir is valid utf8");
    let mut h = match Harness::try_spawn(&fixture_workspace(cwd)) {
        Ok(h) => h,
        Err(reason) => {
            eprintln!("SKIP pane-title gate: {reason}");
            return;
        }
    };
    // Before: the untitled pane's badge carries the `adapter · cwd-tag`
    // fallback, so "1 shell" is on screen and no task name is.
    if h.wait_for(Duration::from_secs(5), |s| s.contents().contains("1 shell")).is_none() {
        panic!(
            "expected the adapter/cwd fallback badge before any OSC title:\n{}",
            h.screen().contents()
        );
    }
    assert!(h.settle(Duration::from_secs(5)), "initial frame never settled");

    // The pane publishes a title the way an agent CLI does. The marker is
    // spelled split (`TAS''K-X`) so the shell's echo of the command line
    // can never satisfy the assertion — only the badge can.
    h.write_bytes(b"printf '\\033]2;TAS''K-X\\007'\r");

    // Inbound: the corner badge adopts it (C4's badge leads with the pane
    // id, so `1 TASK-X` is the exact rendered token).
    if h.wait_for(Duration::from_secs(5), |s| s.contents().contains("1 TASK-X")).is_none() {
        panic!(
            "the pane's OSC 2 title never reached its badge:\n{}",
            h.screen().contents()
        );
    }

    // Outbound: roost publishes the focused pane's name as the host
    // terminal's own title. `wait_for_host_bytes` rather than a one-shot
    // read: the update is throttled, so it may lag the badge by a tick.
    let published = h.wait_for_host_bytes(Duration::from_secs(5), |b| {
        contains(b, "\x1b]2;roost · TASK-X\x07".as_bytes())
    });
    assert!(
        published,
        "roost never published `roost · TASK-X` as the host title; tail:\n{}",
        String::from_utf8_lossy(&tail(&h.host_bytes(), 400))
    );

    // On the way out the title goes back to a plain `roost` — leaving the
    // user's tab named after a pane that no longer exists would be worse
    // than the stale title P6 started from.
    let _ = h.quit_and_wait(Duration::from_secs(5));
    assert!(
        contains(&h.host_bytes(), b"\x1b]2;roost\x07"),
        "roost must reset the host title on exit; tail:\n{}",
        String::from_utf8_lossy(&tail(&h.host_bytes(), 400))
    );
}

/// The last `n` bytes, for readable failure output.
fn tail(bytes: &[u8], n: usize) -> Vec<u8> {
    bytes[bytes.len().saturating_sub(n)..].to_vec()
}
