//! A panic in the event loop must still tear the fleet down.
//!
//! Every pane is `setsid`'d into its own session, so nothing roost spawned
//! dies with roost. `main.rs`'s teardown — SIGHUP, a grace window, then
//! SIGKILL over the whole session — is the only thing that stops an agent,
//! and an unwinding panic goes straight past it. That leaves every agent
//! running, detached, with no terminal attached to it: the identical
//! outcome `tests/signal_shutdown.rs` gates for SIGHUP/SIGTERM, reached by
//! a bug rather than a signal. `tests/vt100_panics.rs` records two real
//! ways a pane could cause one just by printing an emoji.
//!
//! Provoked through `ROOST_TEST_PANIC_AFTER_MS` (`infra::test_panic_after`),
//! because by construction no *input* still reaches a panic — keeping it
//! that way is what the vt100 gate is for.

#[allow(dead_code)]
mod harness;

use std::time::{Duration, Instant};

#[test]
fn a_panic_in_the_event_loop_does_not_orphan_the_fleet() {
    let cwd = std::env::temp_dir();
    let cwd = cwd.to_str().expect("temp dir is valid utf8");
    // Generous: the setup below (settle, background a job, read its pid back
    // off the screen) has to finish inside this window.
    let h = harness::Harness::try_spawn_with_env(
        &harness::two_panes(cwd),
        &[("ROOST_TEST_PANIC_AFTER_MS", "9000")],
    );
    let mut h = match h {
        Ok(h) => h,
        Err(reason) => {
            eprintln!("SKIP panic-teardown gate: {reason}");
            return;
        }
    };
    assert!(h.settle(Duration::from_secs(5)), "initial frame never settled");

    // A job in its own right, with its fds off the pty — neither the pane's
    // process group nor the terminal's foreground group, so only the session
    // sweep in the teardown reaches it. Exactly what an agent's daemonized
    // helper looks like.
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

    let roost = h.pid();
    let before = harness::descendant_pids(roost);
    assert!(before.len() >= 2, "expected roost's two shell panes as descendants, got {before:?}");

    // Now wait out the hatch. roost panics, the hook restores the terminal,
    // and the teardown runs on the way out.
    let died = h.wait_for_exit(Duration::from_secs(20));
    assert!(died, "roost never exited after the deliberate panic");

    // A just-killed process can linger briefly as a zombie before its new
    // parent reaps it; the same short grace the other orphan gates use.
    let mut lingering: Vec<u32> = before.iter().copied().chain([bg]).collect();
    lingering.sort_unstable();
    lingering.dedup();
    let deadline = Instant::now() + Duration::from_millis(1500);
    loop {
        lingering.retain(|&p| harness::is_alive(p));
        if lingering.is_empty() || Instant::now() >= deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(
        lingering.is_empty(),
        "a panic orphaned {lingering:?} (the backgrounded job was {bg}, panes {before:?})"
    );
}
