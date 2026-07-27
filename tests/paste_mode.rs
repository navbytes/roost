//! Bracketed-paste fidelity, end to end: a host paste (which roost now
//! captures via mode 2004 on the outer terminal) must reach a pane wrapped
//! in the `ESC[200~`/`ESC[201~` guards when — and only when — the pane's own
//! app switched bracketed paste on. Regression: roost neither enabled host
//! capture nor re-wrapped, so every newline in pasted text landed as a
//! pressed Enter — a multi-line prompt pasted into an agent pane submitted
//! line by line.

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

#[test]
fn pastes_are_guarded_only_for_panes_in_bracketed_paste_mode() {
    let cwd = std::env::temp_dir();
    let cwd = cwd.to_str().expect("temp dir is valid utf8");
    let mut h = match Harness::try_spawn(&fixture_workspace(cwd)) {
        Ok(h) => h,
        Err(reason) => {
            eprintln!("SKIP paste-mode gate: {reason}");
            return;
        }
    };
    assert!(h.settle(Duration::from_secs(5)), "initial frame never settled");

    // Switch mode 2004 on from inside the pane (what zsh/vim/agent TUIs do),
    // then park `cat` on the tty: the kernel's ECHOCTL echo renders whatever
    // bytes roost forwards as visible `^[` sequences — a probe independent
    // of which /bin/sh is installed. The marker is spelled `REA''DY1` in the
    // command so the echo of the *typed line* can't satisfy the gate — only
    // printf's real output can, which also proves roost's pane parser has
    // consumed the `?2004h` that precedes it.
    h.write_bytes(b"printf '\\033[?2004hREA''DY1'; cat\r");
    h.wait_for(Duration::from_secs(5), |s| s.contents().contains("READY1"))
        .expect("pane never entered bracketed paste mode");

    // A host paste, exactly as the outer terminal (with bracketed paste
    // capture on) delivers it to roost. The pane asked for the guards, so it
    // must receive them around the content.
    h.write_bytes(b"\x1b[200~hi there\x1b[201~");
    h.wait_for(Duration::from_secs(5), |s| s.contents().contains("^[[200~hi there^[[201~"))
        .expect("bracketed-paste pane did not receive the paste guards");

    // Mode off again: the same host paste must arrive verbatim, no guards.
    h.write_bytes(b"\x03");
    h.settle(Duration::from_secs(2));
    h.write_bytes(b"printf '\\033[?2004lREA''DY2'; cat\r");
    h.wait_for(Duration::from_secs(5), |s| s.contents().contains("READY2"))
        .expect("pane never left bracketed paste mode");
    h.write_bytes(b"\x1b[200~bye now\x1b[201~");
    h.wait_for(Duration::from_secs(5), |s| s.contents().contains("bye now"))
        .expect("plain pane never received the pasted text");
    assert!(
        !h.screen().contents().contains("200~bye"),
        "plain pane wrongly received paste guards"
    );

    let _ = h.quit_and_wait(Duration::from_secs(5));
}
