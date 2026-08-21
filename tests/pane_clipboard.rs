//! P3 (SPEC-parity) end to end: an inner app's OSC 52 clipboard write.
//!
//! An app inside a pane (Claude Code's copy action, nvim's osc52 provider,
//! anything running over SSH) sets the system clipboard with
//! `OSC 52 ; c ; <base64>`. roost ate it: measured, the bytes `ESC ]52`
//! never appeared anywhere in a whole session's captured host stream, so the
//! app reported "copied" over an unchanged clipboard — the same lie SPEC-ux
//! U14 documents for roost's own copy path, now for inner apps.
//!
//! The gate asserts both directions: a *write* reaches the host verbatim,
//! and a *read* request (`52;c;?` — the paste-theft vector) never does.

// The shared harness is compiled per test binary; helpers other tenants use
// are dead code from this binary's view — not real rot.
#[allow(dead_code)]
mod harness;

use std::time::Duration;

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

#[test]
fn a_pane_clipboard_write_reaches_the_host_and_a_read_never_does() {
    let cwd = std::env::temp_dir();
    let cwd = cwd.to_str().expect("temp dir is valid utf8");
    let Some(mut h) = harness::spawn_or_skip("clipboard gate", &harness::one_pane(cwd)) else {
        return;
    };
    assert!(h.settle(Duration::from_secs(5)), "initial frame never settled");

    // `cm9vc3Q=` is base64 for "roost". The payload is spelled split
    // (`cm9v''c3Q=`) so the shell's echo of the command line can't satisfy
    // the host-stream assertion — only roost's own relay can.
    h.write_bytes(b"printf '\\033]52;c;cm9v''c3Q=\\007'\r");

    let relayed =
        h.wait_for_host_bytes(Duration::from_secs(5), |b| contains(b, b"\x1b]52;c;cm9vc3Q=\x07"));
    assert!(
        relayed,
        "roost never relayed the pane's OSC 52 write to the host; tail:\n{}",
        String::from_utf8_lossy(&tail(&h.host_bytes(), 400))
    );

    // A read request must be refused. Send it, let roost settle, then
    // confirm nothing shaped like a read (or an answer to one) ever left.
    let before = h.host_bytes().len();
    h.write_bytes(b"printf '\\033]52;c;?\\007'\r");
    h.settle(Duration::from_secs(3));
    let after = h.host_bytes();
    let since = &after[before..];
    assert!(
        !contains(since, b"\x1b]52;c;?"),
        "a pane's OSC 52 *read* must never be forwarded to the host \
         (it would hand the app the user's clipboard); tail:\n{}",
        String::from_utf8_lossy(&tail(since, 400))
    );

    let _ = h.quit_and_wait(Duration::from_secs(5));
}

/// The last `n` bytes, for readable failure output.
fn tail(bytes: &[u8], n: usize) -> Vec<u8> {
    bytes[bytes.len().saturating_sub(n)..].to_vec()
}
