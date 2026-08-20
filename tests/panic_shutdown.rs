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

    // The complement of the background-thread gate below: on the UI thread
    // the terminal *must* be handed back, or the user is left in raw mode
    // inside an alternate screen no process is drawing to any more.
    let raw = h.host_bytes();
    assert!(
        raw.windows(8).any(|w| w == b"\x1b[?1049l"),
        "a panic on the UI thread must still leave the alternate screen"
    );

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

/// ...and a panic on a **background** thread must leave both the terminal
/// and the fleet alone.
///
/// roost runs real work off the event loop — a reader and a writer thread
/// per pane, the socket's accept loop and a thread per connection (parsing
/// untrusted input from panes), the notification reapers. None of those
/// panicking ends the process, so the hook restoring the terminal there
/// would leave the alternate screen and raw mode off while the event loop
/// kept drawing into the user's shell scrollback, keystrokes line-buffered
/// and Alt+q no longer reaching roost.
#[test]
fn a_panic_on_a_background_thread_leaves_the_terminal_alone() {
    let cwd = std::env::temp_dir();
    let cwd = cwd.to_str().expect("temp dir is valid utf8");
    let h = harness::Harness::try_spawn_with_env(
        &harness::two_panes(cwd),
        &[("ROOST_TEST_PANIC_THREAD_AFTER_MS", "2000")],
    );
    let mut h = match h {
        Ok(h) => h,
        Err(reason) => {
            eprintln!("SKIP background-panic gate: {reason}");
            return;
        }
    };
    // Wait for a painted frame, not just for `settle` — two agreeing reads
    // of a screen nothing has been written to yet also "settle".
    assert!(
        h.wait_for(Duration::from_secs(10), |s| s.alternate_screen()).is_some(),
        "setup: roost never entered the alternate screen"
    );
    assert!(h.settle(Duration::from_secs(5)), "initial frame never settled");

    // Wait out the hatch, plus room for the hook to have done damage.
    std::thread::sleep(Duration::from_millis(4000));

    // 1. The terminal is still roost's: no leave-alternate-screen went out.
    // Asserted on the raw byte stream, not the parsed screen: leaving the
    // alternate screen is precisely a sequence that leaves no mark on the
    // grid, which is what `host_bytes` exists for.
    let raw = h.host_bytes();
    let leaves = raw.windows(8).filter(|w| *w == b"\x1b[?1049l").count();
    assert_eq!(
        leaves, 0,
        "the hook handed the terminal back on a background-thread panic \
         (ESC[?1049l went out while roost was still running)"
    );
    assert!(h.screen().alternate_screen(), "roost is still drawing in the alternate screen");

    // 2. roost is still running, and still driving the fleet: a keystroke
    //    reaches the focused pane and comes back.
    h.write_bytes(b"echo ali''ve\r");
    assert!(
        h.wait_for(Duration::from_secs(10), |s| s.contents().contains("alive")).is_some(),
        "roost stopped serving its panes after a background-thread panic:\n{}",
        h.screen().contents()
    );

    // 3. And it still quits the way it always did, with nothing left behind.
    let roost = h.pid();
    let before = harness::descendant_pids(roost);
    assert!(h.quit_and_wait(Duration::from_secs(10)).is_some(), "roost did not exit cleanly");
    let mut lingering = before;
    let deadline = Instant::now() + Duration::from_millis(1500);
    loop {
        lingering.retain(|&p| harness::is_alive(p));
        if lingering.is_empty() || Instant::now() >= deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(lingering.is_empty(), "panes survived the quit: {lingering:?}");
}
