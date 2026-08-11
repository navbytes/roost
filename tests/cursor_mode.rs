//! DECCKM application-cursor fidelity, end to end: a pane app that switches
//! on `CSI ?1h` must receive SS3 cursor keys (`ESC O A` …), exactly what a
//! real terminal transmits in that mode — and what terminfo-bound shell
//! widgets listen for (zsh `smkx` + `$terminfo[kcuu1]`, e.g. atuin's
//! up-arrow search). Regression: roost used to send the normal-mode CSI
//! forms unconditionally, so those bindings never fired inside roost.

// The shared harness is compiled per test binary; helpers other tenants use
// are dead code from this binary's view — not real rot.
#[allow(dead_code)]
mod harness;

use std::time::Duration;

#[test]
fn arrows_switch_to_ss3_while_the_pane_is_in_application_cursor_mode() {
    let cwd = std::env::temp_dir();
    let cwd = cwd.to_str().expect("temp dir is valid utf8");
    let Some(mut h) = harness::spawn_or_skip("cursor-mode gate", &harness::one_pane(cwd)) else {
        return;
    };
    assert!(h.settle(Duration::from_secs(5)), "initial frame never settled");

    // Switch DECCKM on from inside the pane (what `smkx`/a TUI would emit),
    // print a sync marker, then park `cat` on the tty: the kernel's ECHOCTL
    // echo renders whatever bytes roost forwards for the arrow as visible
    // `^[` sequences — a probe independent of which /bin/sh is installed.
    // The marker is spelled `REA''DY1` in the command so the tty's echo of
    // the *typed line* can never satisfy the gate — only printf's real
    // output can, which also proves roost's pane parser has consumed the
    // `?1h` that precedes it (same Output event, processed in order).
    h.write_bytes(b"printf '\\033[?1hREA''DY1'; cat\r");
    h.wait_for(Duration::from_secs(5), |s| s.contents().contains("READY1"))
        .expect("pane never reached application-cursor mode");

    // The host terminal sends a normal-mode Up to roost; the pane must
    // receive the SS3 form.
    h.write_bytes(b"\x1b[A");
    if h.wait_for(Duration::from_secs(5), |s| s.contents().contains("^[OA")).is_none() {
        panic!(
            "pane did not receive SS3 Up (ESC O A) while in application cursor mode:\n{}",
            h.screen().contents()
        );
    }

    // Back to normal mode: kill cat, reset DECCKM, re-park cat. Down must
    // arrive as CSI again (Down, not Up, so the earlier echo can't satisfy
    // the wait).
    h.write_bytes(b"\x03");
    h.settle(Duration::from_secs(2));
    h.write_bytes(b"printf '\\033[?1lREA''DY2'; cat\r");
    h.wait_for(Duration::from_secs(5), |s| s.contents().contains("READY2"))
        .expect("pane never left application-cursor mode");
    h.write_bytes(b"\x1b[B");
    h.wait_for(Duration::from_secs(5), |s| s.contents().contains("^[[B"))
        .expect("pane did not receive CSI Down (ESC [ B) after leaving application cursor mode");
    assert!(
        !h.screen().contents().contains("^[OB"),
        "normal-mode pane wrongly received an SS3 arrow"
    );

    let _ = h.quit_and_wait(Duration::from_secs(5));
}
