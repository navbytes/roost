//! P4 (SPEC-parity) end to end: the terminal-query black hole, both sides.
//!
//! Pane side: an app inside a pane probes its terminal (DA1, DSR 6) and must
//! get answers — pre-W2 roost swallowed every query, stalling crossterm /
//! yazi / atuin startups for seconds each. The probe pattern mirrors
//! tests/cursor_mode.rs: a `cat` parked on the tty makes the kernel's
//! ECHOCTL render whatever bytes roost writes back as visible `^[…` text.
//!
//! Client side: roost's own first paint must not block on crossterm's
//! 2-second keyboard-enhancement probe when the *host* terminal (this
//! harness — a bare vt100 parser) never answers. Measured pre-fix: first
//! frame at ~2014 ms; the gate allows a generous 1500 ms.

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

/// Does the echoed screen text contain a cursor-position report —
/// `^[[{row};{col}R` with 1-based numerics? The exact position depends on
/// prompt history and on how the kernel chunked the two queries (an echo of
/// the DA1 reply arriving between them legitimately moves the cursor), so
/// the gate checks the report's *shape*, which nothing else on this screen
/// can produce.
fn has_cpr(s: &str) -> bool {
    s.match_indices("^[[").any(|(i, _)| {
        let rest = &s[i + 3..];
        let row = rest.chars().take_while(char::is_ascii_digit).count();
        if row == 0 || !rest[row..].starts_with(';') {
            return false;
        }
        let rest = &rest[row + 1..];
        let col = rest.chars().take_while(char::is_ascii_digit).count();
        col > 0 && rest[col..].starts_with('R')
    })
}

#[test]
fn pane_da1_and_cursor_position_queries_are_answered() {
    let cwd = std::env::temp_dir();
    let cwd = cwd.to_str().expect("temp dir is valid utf8");
    let mut h = match Harness::try_spawn(&fixture_workspace(cwd)) {
        Ok(h) => h,
        Err(reason) => {
            eprintln!("SKIP pane query gate: {reason}");
            return;
        }
    };
    assert!(h.settle(Duration::from_secs(5)), "initial frame never settled");

    // Emit a sync marker, then DA1 (`CSI c`) + DSR 6 (`CSI 6n`) from inside
    // the pane, then park `cat` on the tty. The marker is spelled `QR''Y1`
    // in the command so the echo of the *typed line* can never satisfy the
    // gate — only printf's real output can, which also proves the pane
    // parser consumed the queries that ride the same chunk.
    h.write_bytes(b"printf 'QR''Y1'; printf '\\033[c\\033[6n'; cat\r");
    h.wait_for(Duration::from_secs(5), |s| s.contents().contains("QRY1"))
        .expect("pane never ran the query script");

    // roost's replies land on the pane's stdin; the parked cat's tty echoes
    // them as visible `^[` text. DA1 must be the VT100+AVO identity...
    if h.wait_for(Duration::from_secs(5), |s| s.contents().contains("^[[?1;2c")).is_none() {
        panic!(
            "pane never received the DA1 reply (CSI ?1;2c):\n{}",
            h.screen().contents()
        );
    }
    // ...and a well-formed CPR must arrive (typically `^[[2;5R` — the row
    // after the command echo, the column right after the QRY1 marker).
    if h.wait_for(Duration::from_secs(5), |s| has_cpr(&s.contents())).is_none() {
        panic!(
            "pane never received the DSR 6 cursor-position report:\n{}",
            h.screen().contents()
        );
    }

    h.write_bytes(b"\x03"); // Ctrl+C ends cat
    h.settle(Duration::from_secs(2));
    let _ = h.quit_and_wait(Duration::from_secs(5));
}

#[test]
fn first_frame_is_not_blocked_by_the_enhancement_probe() {
    let cwd = std::env::temp_dir();
    let cwd = cwd.to_str().expect("temp dir is valid utf8");
    // This harness never answers roost's own `CSI ?u`+`CSI c` startup probe
    // — exactly the environment where the pre-fix binary sat on crossterm's
    // internal 2 s timeout before drawing anything.
    let mut h = match Harness::try_spawn(&fixture_workspace(cwd)) {
        Ok(h) => h,
        Err(reason) => {
            eprintln!("SKIP first-frame gate: {reason}");
            return;
        }
    };
    let elapsed = h
        .wait_for(Duration::from_secs(5), |s| s.contents().contains("main"))
        .expect("roost never drew its first frame");
    eprintln!("first frame under a non-answering terminal: {elapsed:?} (bound 1500ms)");
    assert!(
        elapsed < Duration::from_millis(1500),
        "first frame took {elapsed:?} under a non-answering terminal \
         (the enhancement probe must not block paint)"
    );
    let _ = h.quit_and_wait(Duration::from_secs(5));
}
