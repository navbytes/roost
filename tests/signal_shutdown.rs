//! roost must tear its fleet down when the host kills it, not only when the
//! user presses Alt+q.
//!
//! Every pane is `setsid`'d into its own session (pty.rs), so the SIGHUP a
//! terminal sends when its window closes reaches roost and nothing else.
//! With no handler installed roost took the default action — die on the spot
//! — and `shutdown()` never ran: every agent kept running, detached, for as
//! long as the machine stayed up. Closing the window, an ssh connection
//! dropping, a `kill` from a supervisor and logout all land here.

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

fn fleet_dies_on(sig: libc::c_int, what: &str) {
    let cwd = std::env::temp_dir();
    let cwd = cwd.to_str().expect("temp dir is valid utf8");
    let Some(mut h) = harness::spawn_or_skip(what, &harness::one_pane(cwd)) else {
        return;
    };
    assert!(h.settle(Duration::from_secs(5)), "initial frame never settled");

    // A backgrounded job in its own process group — the case a bare
    // `kill(-pgid)` misses and only the session sweep catches. Also proves
    // the pane's shell is really up before we count descendants.
    h.write_bytes(b"sleep 600 & echo bg''pid=$!\r");
    assert!(
        h.wait_for(Duration::from_secs(10), |s| s.contents().contains("bgpid=")).is_some(),
        "{what}: the pane never backgrounded the job:\n{}",
        h.screen().contents()
    );

    let roost = h.pid();
    let fleet = harness::descendant_pids(roost);
    assert!(!fleet.is_empty(), "{what}: no pane processes to orphan");

    // SAFETY: `kill(2)` on a pid the harness owns, with a signal number
    // from this test's own table. No pointers involved.
    unsafe { libc::kill(roost as libc::pid_t, sig) };

    assert!(
        survivors(&[roost], Duration::from_secs(10)).is_empty(),
        "{what}: roost itself is still running",
    );
    let left = survivors(&fleet, Duration::from_secs(10));
    assert!(left.is_empty(), "{what}: roost died and left its fleet running: {left:?}");
}

#[test]
fn closing_the_window_takes_the_fleet_with_it() {
    fleet_dies_on(libc::SIGHUP, "SIGHUP gate");
}

#[test]
fn a_terminating_signal_takes_the_fleet_with_it() {
    fleet_dies_on(libc::SIGTERM, "SIGTERM gate");
}
