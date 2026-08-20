//! Companion gates to `orphan_cleanup.rs`, for the cases it does not cover:
//! the pane's own shell has already EXITED by the time roost tears the pane
//! down, so `PtyPane::pid` has been cleared by `on_exit` and every
//! pid-gated cleanup (`kill(-pgid)`, the session sweep) is skipped.

#[allow(dead_code)]
mod harness;

use std::time::{Duration, Instant};

fn survivors(pids: &[u32], grace: Duration) -> Vec<u32> {
    let deadline = Instant::now() + grace;
    let mut left: Vec<u32> = pids.to_vec();
    loop {
        left.retain(|&p| harness::is_alive(p));
        if left.is_empty() || Instant::now() >= deadline {
            return left;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Start a detached `sleep` in the focused pane (fds off the pty, so the
/// shell's own exit really does EOF the master) and return its pid.
fn background_sleep(h: &mut harness::Harness) -> u32 {
    h.write_bytes(b"sleep 600 </dev/null >/dev/null 2>&1 & echo bg''pid=$!\r");
    assert!(
        h.wait_for(Duration::from_secs(10), |s| s.contents().contains("bgpid=")).is_some(),
        "the pane never backgrounded the job:\n{}",
        h.screen().contents()
    );
    let frame = h.screen().contents();
    let bg: u32 = frame
        .lines()
        .find_map(|l| l.split("bgpid=").nth(1))
        .and_then(|t| t.split_whitespace().next())
        .and_then(|t| t.parse().ok())
        .unwrap_or_else(|| panic!("no bgpid in:\n{frame}"));
    assert!(harness::is_alive(bg), "the backgrounded sleep is running (pid {bg})");
    bg
}

#[test]
fn a_backgrounded_job_does_not_survive_its_own_shell_exiting() {
    let cwd = std::env::temp_dir();
    let cwd = cwd.to_str().expect("temp dir is valid utf8");
    let Some(mut h) = harness::spawn_or_skip("orphan-after-exit gate", &harness::two_panes(cwd))
    else {
        return;
    };
    assert!(h.settle(Duration::from_secs(5)), "initial frame never settled");

    let bg = background_sleep(&mut h);

    // The pane's shell exits on its own. roost sees EOF, marks the pane
    // dead, and `PtyPane::on_exit` reaps + clears `pid`. Two panes so this
    // does not quit roost outright.
    h.write_bytes(b"exit\r");
    assert!(
        h.wait_for(Duration::from_secs(10), |s| s.contents().contains("exited")).is_some(),
        "the pane never reported its exit:\n{}",
        h.screen().contents()
    );

    // The job must NOT outlive the shell that started it. `kill()` gates
    // every cleanup path on `self.pid`, which `on_exit` nulls the moment the
    // child is reaped — so a pane's shell exiting on its own (the normal way
    // a pane dies) used to turn all later cleanup into a no-op, and the job
    // survived close, respawn and quit alike. The sweep now runs at the
    // instant of exit, while the session id is still unambiguously ours.
    let left = survivors(&[bg], Duration::from_secs(3));
    assert!(left.is_empty(), "the detached sleep outlived its own shell: {left:?} (bg {bg})");

    assert!(h.quit_and_wait(Duration::from_secs(5)).is_some(), "roost did not exit cleanly");

    let left = survivors(&[bg], Duration::from_millis(1500));
    assert!(left.is_empty(), "these survived the quit: {left:?} (the backgrounded job was {bg})");
}

/// Control: the SAME close, with the pane's shell still alive. This is the
/// path `close_pane_id` → `PtyPane::kill` covers today, and it must keep
/// passing — it is what makes the two tests below a real contrast rather
/// than "roost never sweeps on close".
#[test]
fn closing_a_live_pane_sweeps_its_backgrounded_job() {
    let cwd = std::env::temp_dir();
    let cwd = cwd.to_str().expect("temp dir is valid utf8");
    let Some(mut h) = harness::spawn_or_skip("close-live gate", &harness::two_panes(cwd)) else {
        return;
    };
    assert!(h.settle(Duration::from_secs(5)), "initial frame never settled");
    let bg = background_sleep(&mut h);

    h.write_bytes(b"\x1bw"); // Alt+w
    let left = survivors(&[bg], Duration::from_secs(3));
    assert!(left.is_empty(), "closing a live pane left {left:?} running (bg was {bg})");
}

#[test]
fn closing_an_already_exited_pane_sweeps_its_backgrounded_job() {
    let cwd = std::env::temp_dir();
    let cwd = cwd.to_str().expect("temp dir is valid utf8");
    let Some(mut h) = harness::spawn_or_skip("close-exited gate", &harness::two_panes(cwd)) else {
        return;
    };
    assert!(h.settle(Duration::from_secs(5)), "initial frame never settled");
    let bg = background_sleep(&mut h);

    h.write_bytes(b"exit\r");
    assert!(
        h.wait_for(Duration::from_secs(10), |s| s.contents().contains("exited")).is_some(),
        "the pane never reported its exit:\n{}",
        h.screen().contents()
    );
    // The job must NOT outlive the shell that started it. `kill()` gates
    // every cleanup path on `self.pid`, which `on_exit` nulls the moment the
    // child is reaped — so a pane's shell exiting on its own (the normal way
    // a pane dies) used to turn all later cleanup into a no-op, and the job
    // survived close, respawn and quit alike. The sweep now runs at the
    // instant of exit, while the session id is still unambiguously ours.
    let left = survivors(&[bg], Duration::from_secs(3));
    assert!(left.is_empty(), "the detached sleep outlived its own shell: {left:?} (bg {bg})");

    // Alt+w on a dead pane: no confirm guard (nothing is mid-turn), so one
    // press closes it.
    h.write_bytes(b"\x1bw");
    let left = survivors(&[bg], Duration::from_secs(3));
    assert!(left.is_empty(), "closing an exited pane left {left:?} running (bg was {bg})");
}

#[test]
fn respawning_over_an_exited_pane_sweeps_its_backgrounded_job() {
    let cwd = std::env::temp_dir();
    let cwd = cwd.to_str().expect("temp dir is valid utf8");
    let Some(mut h) = harness::spawn_or_skip("respawn-exited gate", &harness::two_panes(cwd))
    else {
        return;
    };
    assert!(h.settle(Duration::from_secs(5)), "initial frame never settled");
    let bg = background_sleep(&mut h);

    h.write_bytes(b"exit\r");
    assert!(
        h.wait_for(Duration::from_secs(10), |s| s.contents().contains("exited")).is_some(),
        "the pane never reported its exit:\n{}",
        h.screen().contents()
    );
    // The job must NOT outlive the shell that started it. `kill()` gates
    // every cleanup path on `self.pid`, which `on_exit` nulls the moment the
    // child is reaped — so a pane's shell exiting on its own (the normal way
    // a pane dies) used to turn all later cleanup into a no-op, and the job
    // survived close, respawn and quit alike. The sweep now runs at the
    // instant of exit, while the session id is still unambiguously ours.
    let left = survivors(&[bg], Duration::from_secs(3));
    assert!(left.is_empty(), "the detached sleep outlived its own shell: {left:?} (bg {bg})");

    // Enter on a dead focused pane = relaunch (main.rs's dead-pane keys).
    h.write_bytes(b"\r");
    let left = survivors(&[bg], Duration::from_secs(3));
    assert!(left.is_empty(), "respawning over an exited pane left {left:?} running (bg was {bg})");
}
