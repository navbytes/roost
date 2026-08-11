//! End to end: the `__status` subcommand the auto-installed Claude Code hooks
//! run (`infra::extension::ensure_claude_hooks`, see
//! `extensions/claude-code-hooks.md`) actually lands a status report through
//! a pane's real, inherited `ROOST_PANE`/`ROOST_TOKEN`/`ROOST_SOCK` env — no
//! `nc`/`socat` involved. Real binary, real PTY, real socket, exactly what a
//! Claude Code hook does from inside the pane it's running in.

// The shared harness is compiled per test binary; helpers other tenants use
// are dead code from this binary's view — not real rot.
#[allow(dead_code)]
mod harness;

use std::process::Command;
use std::time::{Duration, Instant};

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
    format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr))
}

#[test]
fn status_hook_subcommand_reports_over_the_real_socket() {
    let cwd = std::env::temp_dir();
    let cwd = cwd.to_str().expect("temp dir is valid utf8");
    let Some(mut h) = harness::spawn_or_skip("status hook gate", &harness::one_pane(cwd)) else {
        return;
    };
    assert!(h.settle(Duration::from_secs(5)), "initial frame never settled");

    // Exactly what the auto-installed Claude Code hook runs, from inside the
    // pane's own shell: the pane's ROOST_PANE/ROOST_TOKEN/ROOST_SOCK, set by
    // roost on spawn (src/core/app.rs, src/infra/pty.rs) and inherited by
    // every child process, with zero extra plumbing — the same env a real
    // Claude Code subprocess launched in this pane would see.
    let bin = env!("CARGO_BIN_EXE_roost");
    h.write_bytes(format!("{bin} __status needs_input\r").as_bytes());

    let deadline = Instant::now() + Duration::from_secs(5);
    let status = loop {
        let s = cli_status(h.state_dir());
        if s.contains("needs_input") || Instant::now() >= deadline {
            break s;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    assert!(status.contains("needs_input"), "status hook never landed: {status}");

    let _ = h.quit_and_wait(Duration::from_secs(5));
}

#[test]
fn status_hook_subcommand_is_a_silent_instant_noop_outside_roost() {
    // Run the binary directly (no pane, no ROOST_PANE) exactly as it would
    // be invoked if a Claude Code hook fired outside any roost pane.
    let start = Instant::now();
    let out = Command::new(env!("CARGO_BIN_EXE_roost"))
        .args(["__status", "working"])
        .env_remove("ROOST_PANE")
        .env_remove("ROOST_TOKEN")
        .env_remove("ROOST_SOCK")
        .output()
        .expect("run roost __status");
    assert!(start.elapsed() < Duration::from_secs(2), "not instant: {:?}", start.elapsed());
    assert!(out.status.success(), "must exit 0 outside roost: {out:?}");
    assert!(out.stdout.is_empty(), "must be silent on stdout: {out:?}");
    assert!(out.stderr.is_empty(), "must be silent on stderr: {out:?}");
}
