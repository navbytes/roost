//! Focus reporting (DEC private mode 1004), end to end — SPEC-parity P10.
//!
//! An application that runs `CSI ?1004h` is asking to be told when it gains
//! or loses focus: `CSI I` in, `CSI O` out. vim's `FocusGained` autoread, a
//! TUI that dims while unfocused, and increasingly the agent CLIs all lean
//! on it. Inside roost the focused *pane* is what the reports are about, so
//! moving roost's focus from one pane to another must produce them.
//!
//! Regression: roost tracked mode 1004 nowhere, never enabled host focus
//! events, and synthesized nothing on a pane switch — a subscribed pane saw
//! zero bytes across a focus round-trip.

// The shared harness is compiled per test binary; helpers other tenants use
// are dead code from this binary's view — not real rot.
#[allow(dead_code)]
mod harness;

use std::time::Duration;

// xterm modified arrows, mod 3 = Alt (same encoding the UX drive uses).
const ALT_RIGHT: &[u8] = b"\x1b[1;3C";
const ALT_LEFT: &[u8] = b"\x1b[1;3D";

#[test]
fn a_subscribed_pane_is_told_when_it_loses_and_regains_roost_focus() {
    let cwd = std::env::temp_dir();
    let cwd = cwd.to_str().expect("temp dir is valid utf8");
    let Some(mut h) = harness::spawn_or_skip("focus-reporting gate", &harness::two_panes(cwd))
    else {
        return;
    };
    assert!(h.settle(Duration::from_secs(5)), "initial frame never settled");

    // Subscribe from inside the focused (left) pane, print a sync marker,
    // then park `cat -v` on the tty so any bytes roost forwards show up as
    // visible `^[`-escaped text. The marker is spelled `REA''DY` in the
    // command so the tty's echo of the *typed line* can never satisfy the
    // gate — only printf's real output can, which also proves roost's pane
    // parser has consumed the `?1004h` that precedes it.
    h.write_bytes(b"printf '\\033[?1004hREA''DY'; cat -v\r");
    h.wait_for(Duration::from_secs(5), |s| s.contents().contains("READY"))
        .expect("pane never subscribed to focus reporting");
    assert!(
        !h.screen().contents().contains("^[[O"),
        "nothing has moved focus yet, so no report is owed:\n{}",
        h.screen().contents()
    );

    // Focus leaves for the right-hand pane: the subscriber is owed `CSI O`.
    h.write_bytes(ALT_RIGHT);
    if h.wait_for(Duration::from_secs(5), |s| s.contents().contains("^[[O")).is_none() {
        panic!(
            "pane running ?1004h never received CSI O when it lost focus:\n{}",
            h.screen().contents()
        );
    }

    // ...and `CSI I` when focus comes back.
    h.write_bytes(ALT_LEFT);
    if h.wait_for(Duration::from_secs(5), |s| s.contents().contains("^[[I")).is_none() {
        panic!(
            "pane running ?1004h never received CSI I when it regained focus:\n{}",
            h.screen().contents()
        );
    }

    assert!(h.quit_and_wait(Duration::from_secs(10)).is_some(), "roost did not exit cleanly");
}

#[test]
fn a_pane_that_never_asked_receives_no_focus_bytes() {
    let cwd = std::env::temp_dir();
    let cwd = cwd.to_str().expect("temp dir is valid utf8");
    let Some(mut h) = harness::spawn_or_skip("focus-reporting gate", &harness::two_panes(cwd))
    else {
        return;
    };
    assert!(h.settle(Duration::from_secs(5)), "initial frame never settled");

    // Same probe, minus the subscription: `CSI I`/`CSI O` would arrive as
    // keystrokes, and `\x1b[I` typed at a prompt is a live command edit.
    h.write_bytes(b"printf 'REA''DY'; cat -v\r");
    h.wait_for(Duration::from_secs(5), |s| s.contents().contains("READY"))
        .expect("pane never reached the probe");

    h.write_bytes(ALT_RIGHT);
    h.settle(Duration::from_secs(3));
    h.write_bytes(ALT_LEFT);
    h.settle(Duration::from_secs(3));

    let screen = h.screen().contents();
    assert!(
        !screen.contains("^[[O") && !screen.contains("^[[I"),
        "an unsubscribed pane was sent focus reports:\n{screen}"
    );

    assert!(h.quit_and_wait(Duration::from_secs(10)).is_some(), "roost did not exit cleanly");
}
