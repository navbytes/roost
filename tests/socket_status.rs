//! A socket-reported "exited" must never kill a live pane — driven end to
//! end: real binary, real PTY, real unix socket.
//!
//! Regression scenario: the pane's ROOST_* env is inherited by every
//! descendant process, so a *nested* pi (a subagent, a one-shot `pi -p` tool
//! call, a pi run by hand inside a shell pane) loads the same global
//! `roost.ts` extension and, the moment it finishes its work and exits,
//! reports its `session_shutdown` over the socket *as the pane* — with the
//! pane's own (inherited, therefore valid) token. The pane's real child is
//! alive and well; roost used to mark the pane exited anyway, stickily:
//! dead-pane error bar, keys intercepted, Enter killing the live agent to
//! "relaunch" it. Process death has exactly one ground truth — the PTY EOF.

// The shared harness is compiled per test binary; helpers other tenants use
// (pid, is_alive, ...) are dead code from this binary's view — not real rot.
#[allow(dead_code)]
mod harness;

use std::io::Write;
use std::os::unix::net::UnixStream;
use std::process::Command;
use std::time::{Duration, Instant};

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

/// Poll for a file to exist non-empty, returning its trimmed contents.
fn wait_for_file(path: &std::path::Path, timeout: Duration) -> Option<String> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Ok(s) = std::fs::read_to_string(path) {
            let s = s.trim().to_string();
            if !s.is_empty() {
                return Some(s);
            }
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

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
fn socket_exited_on_a_live_pane_is_advisory_not_death() {
    let cwd = std::env::temp_dir();
    let cwd = cwd.to_str().expect("temp dir is valid utf8");
    let mut h = match Harness::try_spawn(&fixture_workspace(cwd)) {
        Ok(h) => h,
        Err(reason) => {
            eprintln!("SKIP socket status gate: {reason}");
            return;
        }
    };
    assert!(h.settle(Duration::from_secs(5)), "initial frame never settled");

    // The pane's shell reveals the secret only its own env holds — exactly
    // what a nested pi inherits. Writing it to the state dir stands in for
    // "a descendant process knows the pane's token".
    let tok_path = h.state_dir().join("tok");
    h.write_bytes(format!("printf '%s' \"$ROOST_TOKEN\" > {}\r", tok_path.display()).as_bytes());
    let token = wait_for_file(&tok_path, Duration::from_secs(5))
        .expect("pane shell never wrote its ROOST_TOKEN");

    // The nested pi finishes its work: session_shutdown → "exited", sent
    // over the pane's socket with the pane's token (verbatim what a stale
    // roost.ts emits).
    let sock_path = h.state_dir().join("roost.sock");
    let mut sock = UnixStream::connect(&sock_path).expect("connect to roost.sock");
    sock.write_all(
        format!(r#"{{"pane":"1","token":"{token}","event":"status","status":"exited"}}{}"#, '\n')
            .as_bytes(),
    )
    .expect("send exited status");
    sock.flush().expect("flush");

    // The control plane must keep answering for the pane with a live status
    // (recent shell output ⇒ working, else waiting) — never "exited". This
    // round-trips through the same event loop that consumed the status line,
    // so it also serves as the "message was processed" barrier.
    let deadline = Instant::now() + Duration::from_secs(5);
    let status = loop {
        let s = cli_status(h.state_dir());
        if s.contains("\"status\"") || Instant::now() >= deadline {
            break s;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    assert!(!status.contains("exited"), "socket 'exited' killed a live pane: {status}");

    // No dead-pane error bar on screen...
    h.settle(Duration::from_secs(2));
    let screen = h.screen().contents();
    assert!(
        !screen.contains("exited — Enter: relaunch"),
        "dead-pane bar shown for a live pane:\n{screen}"
    );

    // ...and keys still reach the live shell (a falsely-dead pane swallows
    // them in the relaunch key handler instead of forwarding).
    h.write_bytes(b"echo pane_sti11_a1ive\r");
    h.wait_for(Duration::from_secs(5), |s| s.contents().contains("pane_sti11_a1ive"))
        .expect("typed command produced no output — pane keys were intercepted as dead");

    let _ = h.quit_and_wait(Duration::from_secs(5));
}

/// D1/D2 end to end, real binary + real PTY + real socket: while an
/// extension's status-socket connection stays open, PTY byte noise must not
/// repaint a resting report to "working" (the phantom-pulse bug); once that
/// connection closes, the fallback promotion is restored. Cheap on purpose —
/// no 45s `STUCK_WORKING` wait needed, just `ACTIVE_WINDOW` (2s) either side
/// of the connection closing.
#[test]
fn output_does_not_promote_a_resting_report_while_the_link_is_live_but_does_once_it_drops() {
    let cwd = std::env::temp_dir();
    let cwd = cwd.to_str().expect("temp dir is valid utf8");
    let mut h = match Harness::try_spawn(&fixture_workspace(cwd)) {
        Ok(h) => h,
        Err(reason) => {
            eprintln!("SKIP socket status gate: {reason}");
            return;
        }
    };
    assert!(
        h.settle(Duration::from_secs(5)),
        "initial frame never settled"
    );

    let tok_path = h.state_dir().join("tok");
    h.write_bytes(format!("printf '%s' \"$ROOST_TOKEN\" > {}\r", tok_path.display()).as_bytes());
    let token = wait_for_file(&tok_path, Duration::from_secs(5))
        .expect("pane shell never wrote its ROOST_TOKEN");

    // A "live" extension link for pane 1: one connection, held open, that
    // already reported a resting status — exactly the pi extension's shape.
    let sock_path = h.state_dir().join("roost.sock");
    let mut sock = UnixStream::connect(&sock_path).expect("connect to roost.sock");
    sock.write_all(
        format!(
            r#"{{"pane":"1","token":"{token}","event":"status","status":"waiting"}}{}"#,
            '\n'
        )
        .as_bytes(),
    )
    .expect("send waiting status");
    sock.flush().expect("flush");

    // Round-trip through the event loop (same barrier as the test above) so
    // the link-up + status have definitely both landed before we look.
    let deadline = Instant::now() + Duration::from_secs(5);
    let status = loop {
        let s = cli_status(h.state_dir());
        if s.contains("\"waiting\"") || Instant::now() >= deadline {
            break s;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    assert!(
        status.contains("\"waiting\""),
        "setup: extension report never took: {status}"
    );

    // Byte noise while the link is live: composer echo from typing. Must
    // stay "waiting" throughout — never a phantom "working".
    h.write_bytes(b"echo phantom_pulse_check\r");
    let watch_until = Instant::now() + Duration::from_millis(1500);
    while Instant::now() < watch_until {
        let s = cli_status(h.state_dir());
        assert!(
            !s.contains("\"working\""),
            "byte noise repainted a phantom ● while the link is live: {s}"
        );
        std::thread::sleep(Duration::from_millis(100));
    }

    // Drop the connection: link-down for pane 1.
    drop(sock);

    // Give the close a moment to reach the event loop, then produce fresh
    // output — with the link down, this is the fallback D1 preserves.
    std::thread::sleep(Duration::from_millis(500));
    h.write_bytes(b"echo fallback_restored\r");
    let deadline = Instant::now() + Duration::from_secs(5);
    let status = loop {
        let s = cli_status(h.state_dir());
        if s.contains("\"working\"") || Instant::now() >= deadline {
            break s;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    assert!(
        status.contains("\"working\""),
        "output must promote a resting report once the link is down: {status}"
    );

    let _ = h.quit_and_wait(Duration::from_secs(5));
}
