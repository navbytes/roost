//! The control CLI's contract with a scripted/LLM caller (see cli.rs):
//! distinguishable exit codes, `--help`/`--version` on stdout, per-verb help
//! that never falls through to actually running the verb, and flags that
//! are rejected rather than silently dropped. Most of this never touches a
//! live roost instance — that's the point of items 2–5: a bad invocation
//! (or an explicit `--help`) is settled before the socket is ever opened.
//! `wait`'s timeout is the one exception that needs a real pane, so it's
//! driven through the PTY harness like `socket_status.rs`/`status_hook.rs`.
//! An oversized request is a second: the socket has to actually be open to
//! reject it.

// The shared harness is compiled per test binary; helpers other tenants use
// are dead code from this binary's view — not real rot.
#[allow(dead_code)]
mod harness;

use std::process::{Command, Output};
use std::time::Duration;

use harness::Harness;

/// A path nothing is listening on. Any test that points `ROOST_SOCK` here
/// and still gets a clean, non-network answer has proven that answer never
/// touched the socket — the whole claim behind "help never executes".
const DEAD_SOCK: &str = "/nonexistent-roost-cli-test.sock";

fn roost(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_roost"))
        .args(args)
        .env("ROOST_SOCK", DEAD_SOCK)
        .output()
        .expect("run roost")
}

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}
fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

/// Run `roost <args>` against a harness instance's state dir.
fn cli_in(state_dir: &std::path::Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_roost"))
        .args(args)
        .env("ROOST_STATE", state_dir)
        .env_remove("ROOST_SOCK")
        .env_remove("ROOST_TOKEN")
        .env_remove("ROOST_CONTROL_TOKEN")
        .output()
        .expect("run roost")
}

/// Block until a harness instance's control socket actually accepts
/// connections. `Harness::settle` only proves the terminal frame stopped
/// changing between two 40ms samples — an all-blank *pre-startup* frame
/// satisfies that just as well as a real one, so a single `roost list`
/// right after `settle` can race the socket's bind (same reasoning as
/// `socket_status.rs`/`status_hook.rs`'s own retry-loop `cli_status`).
fn wait_until_reachable(state_dir: &std::path::Path, timeout: Duration) {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        let o = cli_in(state_dir, &["list"]);
        if o.status.success() {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "roost's control socket never became reachable: {o:?}"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// QA repro: `roost --version` used to fall through to launching the TUI —
/// no version ever printed, and off-tty a raw panic.
#[test]
fn version_flag_prints_to_stdout_and_exits_0() {
    let o = roost(&["--version"]);
    assert!(o.status.success(), "{o:?}");
    assert!(out(&o).trim_start().starts_with("roost "), "stdout: {:?}", out(&o));
    assert!(err(&o).is_empty(), "stderr: {:?}", err(&o));
}

/// QA repro: `roost ls` (any unrecognized first argument) used to fall
/// through to the TUI — seizing the terminal, or off-tty panicking with a
/// raw backtrace. Now a hard, immediate usage error.
#[test]
fn unrecognized_verb_is_a_hard_error_not_the_tui() {
    let o = roost(&["ls"]);
    assert_eq!(o.status.code(), Some(2), "{o:?}");
    assert!(err(&o).contains("ls"), "stderr: {:?}", err(&o));
    assert!(out(&o).is_empty(), "must not print anything on stdout: {:?}", out(&o));
}

/// Fix 3: an explicit `--help` is a successful request, not an error —
/// `roost --help | grep spawn` must actually see output.
#[test]
fn help_flag_goes_to_stdout_and_exits_0() {
    let o = roost(&["--help"]);
    assert!(o.status.success(), "{o:?}");
    assert!(out(&o).contains("spawn"), "stdout: {:?}", out(&o));
    assert!(err(&o).is_empty(), "stderr: {:?}", err(&o));

    let o = roost(&["-h"]);
    assert!(o.status.success(), "{o:?}");
    assert!(out(&o).contains("spawn"));
}

/// Every verb's `--help` must print its own usage — never the same generic
/// text as another verb, and (for `spawn`) never byte-identical to the
/// bare-invocation usage error it used to reproduce exactly. Doesn't need a
/// live instance: an unreachable `ROOST_SOCK` proves none of these ever
/// touched the network either.
#[test]
fn every_verb_help_is_its_own_text() {
    for verb in ["list", "status", "fork", "spawn", "send", "read", "close", "wait"] {
        let o = roost(&[verb, "--help"]);
        assert!(o.status.success(), "{verb}: {o:?}");
        assert!(
            out(&o).starts_with(&format!("roost {verb}")),
            "{verb} stdout must lead with its own usage: {:?}",
            out(&o)
        );
        assert!(err(&o).is_empty(), "{verb}: stderr: {:?}", err(&o));
    }
    // spawn's specific regression: --help used to print the exact same
    // "spawn needs an ADAPTER" + usage text as a bare `spawn` — byte-identical.
    let help = out(&roost(&["spawn", "--help"]));
    let bare = err(&roost(&["spawn"]));
    assert_ne!(help, bare, "spawn --help must not be byte-identical to bare spawn's error");
}

/// Fix 4, the sharpest QA repro, reproduced exactly: with a REAL, reachable
/// instance (so there's something to execute), `list --help` / `status
/// --help` / `fork --help` used to silently run the real verb and return
/// live pane JSON, exit 0, instead of printing help.
#[test]
fn list_status_fork_help_never_execute_against_a_live_instance() {
    let cwd = std::env::temp_dir();
    let cwd = cwd.to_str().expect("temp dir is valid utf8");
    let workspace = serde_json::json!({
        "version": 1,
        "active_tab": 0,
        "tabs": [{
            "name": "main",
            "layout": { "pane": 1 },
            "panes": { "1": {"adapter": "shell", "cwd": cwd} }
        }]
    })
    .to_string();
    let mut h = match Harness::try_spawn(&workspace) {
        Ok(h) => h,
        Err(reason) => {
            eprintln!("SKIP list/status/fork --help gate: {reason}");
            return;
        }
    };
    assert!(h.settle(Duration::from_secs(5)), "initial frame never settled");
    wait_until_reachable(h.state_dir(), Duration::from_secs(5));

    // Proof the instance is genuinely live and reachable: a real `list`
    // (no --help) returns the pane array.
    let real_list = cli_in(h.state_dir(), &["list"]);
    assert!(real_list.status.success(), "{real_list:?}");
    assert!(out(&real_list).trim_start().starts_with('['), "stdout: {:?}", out(&real_list));

    for verb in ["list", "status", "fork"] {
        let o = cli_in(h.state_dir(), &[verb, "--help"]);
        assert!(o.status.success(), "{verb}: {o:?}");
        assert!(
            out(&o).starts_with(&format!("roost {verb}")),
            "{verb} --help must print its own usage, not execute: {:?}",
            out(&o)
        );
        assert!(
            !out(&o).trim_start().starts_with('['),
            "{verb} --help must not be pane JSON: {:?}",
            out(&o)
        );
    }

    let _ = h.quit_and_wait(Duration::from_secs(5));
}

/// Fix 5, first QA repro: an unknown/typo'd flag is rejected, not silently
/// dropped — `send 5 hi --entr` used to send without Enter and report ok.
#[test]
fn send_rejects_an_unknown_flag_instead_of_dropping_it() {
    let o = roost(&["send", "5", "hi", "--entr"]);
    assert_eq!(o.status.code(), Some(2), "{o:?}");
    assert!(err(&o).contains("--entr"), "stderr must name what was wrong: {:?}", err(&o));
    assert!(err(&o).contains("--enter"), "stderr must name the valid flag: {:?}", err(&o));
}

/// Off-tty misuse (piped/backgrounded/CI, no args) must fail cleanly —
/// message on stderr, nonzero exit — instead of ratatui/crossterm's raw
/// panic ("failed to initialize terminal", the most orchestrator-hostile
/// bug the QA pass found).
///
/// P3: exit code 2 specifically (a usage error, matching every other "you
/// invoked it wrong" path and `USAGE`'s own "2 usage error"), not 1 (a
/// runtime error) — which this used to return despite being exactly that
/// class of failure. A bare `assert!(!o.status.success())` can't catch that
/// drift: 1 is nonzero too.
#[test]
fn bare_invocation_off_tty_fails_cleanly_instead_of_panicking() {
    let o = Command::new(env!("CARGO_BIN_EXE_roost"))
        .stdin(std::process::Stdio::null())
        .output()
        .expect("run bare roost");
    assert_eq!(o.status.code(), Some(2), "{o:?}");
    assert_ne!(o.status.code(), Some(101), "must not be a Rust panic exit code: {o:?}");
    assert!(out(&o).is_empty(), "stdout: {:?}", out(&o));
    assert!(!err(&o).to_lowercase().contains("panic"), "must not be a panic: {:?}", err(&o));
    assert!(!err(&o).is_empty(), "must say something on stderr");
}

/// Fix 1, driven end to end: `wait`'s timeout reply must be a distinguishable
/// nonzero exit, not the same 0 as a resolved wait — `wait … && read` used
/// to proceed as though the pane had actually finished. The pane waits on
/// `needs_input`, which a bare shell (no extension/hook installed, no bell)
/// can never reach on its own — the only way this resolves is the timeout
/// branch.
#[test]
fn wait_timeout_exits_nonzero_and_distinct_from_runtime_and_usage_errors() {
    let cwd = std::env::temp_dir();
    let cwd = cwd.to_str().expect("temp dir is valid utf8");
    let workspace = serde_json::json!({
        "version": 1,
        "active_tab": 0,
        "tabs": [{
            "name": "main",
            "layout": { "pane": 1 },
            "panes": { "1": {"adapter": "shell", "cwd": cwd} }
        }]
    })
    .to_string();
    let mut h = match Harness::try_spawn(&workspace) {
        Ok(h) => h,
        Err(reason) => {
            eprintln!("SKIP wait-timeout gate: {reason}");
            return;
        }
    };
    assert!(h.settle(Duration::from_secs(5)), "initial frame never settled");
    wait_until_reachable(h.state_dir(), Duration::from_secs(5));

    let o = cli_in(h.state_dir(), &["wait", "1", "--until", "needs_input", "--timeout", "1"]);

    assert_eq!(o.status.code(), Some(3), "{o:?}");
    assert_ne!(o.status.code(), Some(1), "must differ from a runtime error");
    assert_ne!(o.status.code(), Some(2), "must differ from a usage error");
    assert!(out(&o).contains("timed_out"), "stdout: {:?}", out(&o));

    let _ = h.quit_and_wait(Duration::from_secs(5));
}

/// PR #67 follow-up, driven end to end: a request over the control plane's
/// 64 KiB line cap used to drop the connection with no reply — the client's
/// own write EPIPEd mid-payload, and `cli.rs` reported that as "cannot reach
/// a running roost", exactly the wrong diagnosis for an instance that was
/// alive the whole time. Well past both the cap and a local socket's default
/// kernel buffer (as little as 8 KiB each way on macOS), so this only passes
/// if the server actually drains the rest of the client's still-in-flight
/// write rather than merely replying to a payload that happened to fit in
/// one write() call.
#[test]
fn oversized_request_gets_a_true_diagnosis_and_leaves_the_instance_alive() {
    let cwd = std::env::temp_dir();
    let cwd = cwd.to_str().expect("temp dir is valid utf8");
    let workspace = serde_json::json!({
        "version": 1,
        "active_tab": 0,
        "tabs": [{
            "name": "main",
            "layout": { "pane": 1 },
            "panes": { "1": {"adapter": "shell", "cwd": cwd} }
        }]
    })
    .to_string();
    let mut h = match Harness::try_spawn(&workspace) {
        Ok(h) => h,
        Err(reason) => {
            eprintln!("SKIP oversized-request gate: {reason}");
            return;
        }
    };
    assert!(h.settle(Duration::from_secs(5)), "initial frame never settled");
    wait_until_reachable(h.state_dir(), Duration::from_secs(5));

    let payload = "A".repeat(200_000);
    let o = cli_in(h.state_dir(), &["send", "1", &payload]);

    assert_eq!(o.status.code(), Some(2), "must be a usage error, not a runtime one: {o:?}");
    let stderr = err(&o);
    assert!(stderr.contains("message too large"), "stderr must name the real problem: {stderr:?}");
    assert!(
        !stderr.contains("cannot reach a running roost"),
        "must not misdiagnose a live instance as unreachable: {stderr:?}"
    );

    // The actual regression risk: the instance must still be fully alive —
    // a subsequent `list`, on a brand new connection, must still succeed.
    let after = cli_in(h.state_dir(), &["list"]);
    assert!(after.status.success(), "control plane stopped answering after the oversized request: {after:?}");

    let _ = h.quit_and_wait(Duration::from_secs(5));
}
