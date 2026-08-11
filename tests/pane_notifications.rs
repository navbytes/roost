//! P2 (SPEC-parity) end to end: a pane's OSC 9 / OSC 777 notification.
//!
//! Claude Code (and every agent CLI that wants your attention) publishes
//! "I need you" as `OSC 9 ; body BEL`. roost dropped it at `osc_dispatch`,
//! and because the OSC-terminating BEL is deliberately *not* counted as a
//! bell, the NeedsInput heuristic never fired either — measured: after
//! `OSC 9 ; NEEDS-YOU BEL` plus 3.3 s of quiet the pane still reported
//! `waiting`, and no `ESC ]9;` ever reached the host stream.
//!
//! Both halves are gated here: the pane must end up `needs_input` in the
//! control plane's own words, and roost must re-emit an OSC 9 to its host
//! terminal so the native desktop notification actually fires.

// The shared harness is compiled per test binary; helpers other tenants use
// are dead code from this binary's view — not real rot.
#[allow(dead_code)]
mod harness;

use std::process::Command;
use std::time::Duration;

use harness::Harness;

/// `roost status 1` against the harness instance, via the real client CLI
/// (ROOST_STATE routes it to the instance's socket + fleet control token).
fn cli_status(state_dir: &std::path::Path) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_roost"))
        .args(["status", "1"])
        .env("ROOST_STATE", state_dir)
        .env_remove("ROOST_SOCK")
        .env_remove("ROOST_TOKEN")
        .env_remove("ROOST_CONTROL_TOKEN")
        .output()
        .expect("run roost status");
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

#[test]
fn a_pane_osc9_notification_pulls_attention_and_reaches_the_host() {
    let cwd = std::env::temp_dir();
    let cwd = cwd.to_str().expect("temp dir is valid utf8");
    let Some(mut h) = harness::spawn_or_skip("notification gate", &harness::one_pane(cwd)) else {
        return;
    };
    assert!(h.settle(Duration::from_secs(5)), "initial frame never settled");

    // The pane notifies exactly the way an agent CLI does. The body is
    // spelled split (`NEEDS''-YOU`) so the shell's echo of the command line
    // can't satisfy the host-stream assertion — only roost's own
    // re-emission can.
    h.write_bytes(b"printf '\\033]9;NEEDS''-YOU\\007'\r");

    // Half one: roost forwards the notification to its host terminal, so the
    // desktop notification the app asked for actually happens. `ESC ]9;`
    // never appeared anywhere in the captured stream before this landed.
    let saw_osc9 = h.wait_for_host_bytes(Duration::from_secs(5), |b| {
        b.windows(4).any(|w| w == b"\x1b]9;")
    });
    assert!(
        saw_osc9,
        "roost never re-emitted an OSC 9 to the host; captured tail:\n{}",
        String::from_utf8_lossy(&tail(&h.host_bytes(), 400))
    );
    // ...carrying the pane's actual body, not an empty or truncated husk.
    let host = h.host_bytes();
    let osc9 = find_osc9(&host).expect("the re-emitted OSC 9 is well formed");
    assert_eq!(osc9, "NEEDS-YOU", "the pane's body must ride along verbatim");

    // Half two: the pane counts as needing the user. The heuristic waits for
    // the pane to go quiet (output within ~2 s still reads as Working), so
    // give it the same grace the original measurement did.
    std::thread::sleep(Duration::from_millis(2600));
    let status = cli_status(h.state_dir());
    assert!(
        status.contains("needs_input"),
        "pane status after an OSC 9 + quiet should be needs_input, got:\n{status}"
    );

    let _ = h.quit_and_wait(Duration::from_secs(5));
}

/// ux P1-6: the fallback case OSC 9 doesn't cover — a pane that only ever
/// rings its bare terminal bell, no extension, no OSC 9. Before this,
/// `Notifier::notify` fired only from `on_pty_output` (OSC 9) and
/// `on_status` (the socket) — the bell heuristic updated the pane's own
/// attention state but never reached the operator's real terminal, so
/// README's "rings the bell the moment one needs you" was false in exactly
/// the case the heuristic exists to serve.
#[test]
fn a_pane_bare_bell_relays_to_the_host_and_is_rate_limited() {
    let cwd = std::env::temp_dir();
    let cwd = cwd.to_str().expect("temp dir is valid utf8");
    let Some(mut h) = harness::spawn_or_skip("bare-bell gate", &harness::one_pane(cwd)) else {
        return;
    };
    assert!(h.settle(Duration::from_secs(5)), "initial frame never settled");
    let bell_count = |h: &mut Harness| h.host_bytes().iter().filter(|&&b| b == 0x07).count();
    // The OSC 2 host-title sequence is itself BEL-terminated and fires once
    // at startup, then goes quiet (unchanged label, `HOST_TITLE_INTERVAL`) —
    // `settle` already waited past it, so this baseline is stable for the
    // rest of the test.
    let baseline = bell_count(&mut h);

    // A bare bell — no OSC 9, nothing an extension could have reported.
    h.write_bytes(b"printf '\\007'\r");
    let relayed = h.wait_for_host_bytes(Duration::from_secs(5), |b| {
        b.iter().filter(|&&x| x == 0x07).count() > baseline
    });
    assert!(
        relayed,
        "roost never relayed the pane's bare bell to the host; captured tail:\n{}",
        String::from_utf8_lossy(&tail(&h.host_bytes(), 200))
    );
    let after_first = bell_count(&mut h);

    // A burst right behind it must not machine-gun the operator's terminal —
    // the same per-pane interval `queue_host_notify` already gates OSC 9
    // with (P2's `HOST_NOTIFY_INTERVAL`).
    h.write_bytes(b"for i in 1 2 3 4 5; do printf '\\007'; done\r");
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(bell_count(&mut h), after_first, "a bell burst must relay at most once per window");

    let _ = h.quit_and_wait(Duration::from_secs(5));
}

/// The body of the first well-formed `ESC ] 9 ; ... BEL` in the stream.
fn find_osc9(bytes: &[u8]) -> Option<String> {
    let start = bytes.windows(4).position(|w| w == b"\x1b]9;")? + 4;
    let end = start + bytes[start..].iter().position(|&b| b == 0x07)?;
    Some(String::from_utf8_lossy(&bytes[start..end]).to_string())
}

/// The last `n` bytes, for readable failure output.
fn tail(bytes: &[u8], n: usize) -> Vec<u8> {
    bytes[bytes.len().saturating_sub(n)..].to_vec()
}
