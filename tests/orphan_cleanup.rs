//! Quit must not leave live processes behind — including the ones an
//! interactive shell put in *their own* process groups.
//!
//! roost SIGKILLs each pane's process group, and each pane is its own
//! session leader. That covers the pane's shell and anything sharing its
//! group, and closing the PTY sends SIGHUP to the session's *foreground*
//! group — but a shell with job control puts every job in a new process
//! group, and a **backgrounded** job is by definition not the foreground
//! one. Nothing in that chain reaches it. ROADMAP carried this as
//! "[perf] Orphan-child cleanup … worth revisiting if leaks appear"; this
//! is the test that decides whether they do.

// The shared harness is compiled per test binary; helpers other tenants use
// are dead code from this binary's view — not real rot.
#[allow(dead_code)]
mod harness;

use std::time::{Duration, Instant};

/// Wait for every pid in `pids` to be gone, up to `grace`. Returns whatever
/// is still live at the end.
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

#[test]
fn a_backgrounded_job_in_a_pane_does_not_survive_quit() {
    let cwd = std::env::temp_dir();
    let cwd = cwd.to_str().expect("temp dir is valid utf8");
    let Some(mut h) = harness::spawn_or_skip("orphan gate", &harness::one_pane(cwd)) else {
        return;
    };
    assert!(h.settle(Duration::from_secs(5)), "initial frame never settled");

    // A long sleep, backgrounded, then printed so the test knows its pid and
    // that the shell really detached it. `&` is what puts it in a process
    // group of its own — the exact case group-kill and SIGHUP both miss.
    h.write_bytes(b"sleep 600 & echo bg''pid=$!\r");
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

    let roost_pid = h.pid();
    let before = harness::descendant_pids(roost_pid);
    assert!(before.contains(&bg), "the job is a descendant of roost: {before:?} vs {bg}");

    assert!(h.quit_and_wait(Duration::from_secs(5)).is_some(), "roost did not exit cleanly");

    let left = survivors(&before, Duration::from_millis(1500));
    assert!(left.is_empty(), "these survived the quit: {left:?} (the backgrounded job was {bg})",);
}
