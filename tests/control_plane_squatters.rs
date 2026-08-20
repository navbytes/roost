//! An unauthenticated flood must not be able to keep the control plane shut.
//!
//! End to end, against the real binary and its real socket — the accounting
//! unit tests in `sock.rs` prove the pool arithmetic, this proves the thing
//! the user actually experiences: `roost list` answers while under attack.
//!
//! Before the pre-auth pool, measured here: 64 silent connections cost a
//! legitimate client 2.07s and 21 refused attempts, and a squatter that
//! retook each slot as it recycled stretched that to 12.1s and 118 refused
//! attempts. The 2s pre-auth deadline bounded how long any *one* squatter
//! was subsidised; it did not bound the denial, because reconnecting is free.

#[allow(dead_code)]
mod harness;

use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// The socket appears a moment after the first frame, so a settled screen is
/// not proof it is bound yet.
fn wait_for_socket(dir: &std::path::Path) -> std::path::PathBuf {
    let sock = dir.join("roost.sock");
    let deadline = Instant::now() + Duration::from_secs(10);
    while !sock.exists() {
        assert!(Instant::now() < deadline, "roost never bound its control socket");
        std::thread::sleep(Duration::from_millis(50));
    }
    sock
}

#[test]
fn a_sustained_silent_flood_does_not_lock_out_the_control_plane() {
    let cwd = std::env::temp_dir();
    let cwd = cwd.to_str().expect("temp dir is valid utf8");
    let Some(mut h) = harness::spawn_or_skip("squatter gate", &harness::one_pane(cwd)) else {
        return;
    };
    assert!(h.settle(Duration::from_secs(5)), "initial frame never settled");
    let state = h.state_dir().to_path_buf();
    let sock = wait_for_socket(&state);

    // Hold every slot and say nothing — and keep holding: a connection the
    // deadline recycles is immediately retaken, which is what turned a 2s
    // hiccup into an open-ended denial.
    let stop = Arc::new(AtomicBool::new(false));
    let ready = Arc::new(AtomicBool::new(false));
    let (s2, r2, sock2) = (stop.clone(), ready.clone(), sock.clone());
    let squatter = std::thread::spawn(move || {
        let mut held: Vec<UnixStream> = Vec::new();
        let mut announced = false;
        while !s2.load(Ordering::Relaxed) {
            // A displaced connection reads EOF; drop those and retake.
            held.retain(|s| {
                let _ = s.set_nonblocking(true);
                let mut b = [0u8; 1];
                !matches!(std::io::Read::read(&mut &*s, &mut b), Ok(0))
            });
            while held.len() < 64 {
                match UnixStream::connect(&sock2) {
                    Ok(s) => held.push(s),
                    Err(_) => break,
                }
            }
            if !announced && held.len() == 64 {
                announced = true;
                r2.store(true, Ordering::SeqCst);
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    });
    let deadline = Instant::now() + Duration::from_secs(10);
    while !ready.load(Ordering::SeqCst) {
        assert!(Instant::now() < deadline, "the flood never got going");
        std::thread::sleep(Duration::from_millis(5));
    }

    // The real client, invoked the way a user would.
    let started = Instant::now();
    let mut attempts = 0u32;
    let mut ok = false;
    while started.elapsed() < Duration::from_secs(15) {
        attempts += 1;
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_roost"))
            .arg("list")
            .env("ROOST_STATE", &state)
            .output();
        if out.is_ok_and(|o| o.status.success()) {
            ok = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let took = started.elapsed();
    stop.store(true, Ordering::Relaxed);
    let _ = squatter.join();

    assert!(ok, "a legitimate client never got in under a sustained flood");
    // Generous next to the 12.1s / 118 attempts this used to cost, and still
    // far tighter than anything the old first-come-first-served pool could
    // reach: an arriving client displaces the oldest squatter rather than
    // queueing behind all of them.
    assert!(
        took < Duration::from_secs(3) && attempts <= 5,
        "the flood still delayed a legitimate client: {took:?} over {attempts} attempts",
    );
}
