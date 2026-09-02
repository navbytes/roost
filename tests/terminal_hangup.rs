//! Deterministic repro for the "closed tab" hangup: roost's main thread
//! blocks forever inside `crossterm::event::poll`'s stdin-EOF spin (strace
//! on this exact repro showed `read(0, "", 1024) = 0`, over and over) once
//! the terminal is gone, so it never reaches the `terminating()` check at
//! the bottom of its own loop — see `infra::signals::watch_for_hangup`'s
//! doc comment for the full story. `tests/signal_shutdown.rs` covers the
//! terminations that DO interrupt that poll (SIGHUP/SIGTERM/SIGINT sent
//! directly, which `SA_RESTART` merely keeps from surfacing as `EINTR`);
//! this file covers the one way a terminal actually says goodbye that does
//! not interrupt anything: the tab's own process exiting and taking every
//! fd onto the pty's master side with it.
//!
//! Reproducing that precisely needs roost to be the session leader of a PTY
//! whose *entire* master side then closes — which `harness::Harness`
//! deliberately does not model (it keeps the master open for the whole
//! test, so it can read the screen and type into it). So this file drives a
//! raw `openpty` pair directly, the way a real terminal emulator does, and
//! only borrows `harness::is_alive`/`harness::descendant_pids` — the
//! zombie-aware helpers every other gate in this crate already trusts.

#[allow(dead_code)]
mod harness;

use std::os::fd::FromRawFd;
use std::os::unix::process::CommandExt;
use std::time::{Duration, Instant};

/// Wait up to `budget` for every pid in `pids` to stop being alive
/// (`harness::is_alive`, zombie-aware — see its own doc comment for why a
/// bare `kill -0` is not enough). Returns whoever is still alive when the
/// budget runs out; empty means everyone died in time. Same shape as
/// `tests/signal_shutdown.rs`'s `survivors`, kept local rather than shared
/// since these are two different tests files' throwaway spin-wait, not a
/// piece of the harness proper.
fn survivors(pids: &[u32], budget: Duration) -> Vec<u32> {
    let deadline = Instant::now() + budget;
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
fn closing_the_tab_takes_roost_and_its_fleet_with_it() {
    let state = std::env::temp_dir().join(format!("roost-hangup-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&state);
    std::fs::create_dir_all(&state).expect("create ROOST_STATE dir");
    std::fs::write(
        state.join("workspace.json"),
        serde_json::json!({
            "version": 1,
            "active_tab": 0,
            "tabs": [{
                "name": "main",
                "layout": {"pane": 1},
                "panes": {"1": {"adapter": "shell", "cwd": "/tmp"}},
            }],
        })
        .to_string(),
    )
    .expect("seed workspace.json");

    let (mut master, mut slave) = (0i32, 0i32);
    // `openpty`'s last two parameters are `*const` on glibc and `*mut` on
    // the BSDs (macOS included), so a `*const` argument compiles on Linux and
    // breaks the macOS half of the CI matrix. `*mut` satisfies both: Rust
    // coerces `*mut T` to `*const T` at a call site, never the reverse.
    let mut ws = libc::winsize { ws_row: 30, ws_col: 100, ws_xpixel: 0, ws_ypixel: 0 };
    let ws_ptr: *mut libc::winsize = &mut ws;
    // SAFETY: `master`/`slave` are plain out-params on the stack and `ws` is
    // a fully-initialized, valid `winsize` — the documented `openpty(3)`
    // contract.
    let opened = unsafe {
        libc::openpty(&mut master, &mut slave, std::ptr::null_mut(), std::ptr::null_mut(), ws_ptr)
    };
    if opened != 0 {
        eprintln!("SKIP terminal-hangup gate: no pty available on this host");
        let _ = std::fs::remove_dir_all(&state);
        return;
    }

    // openpty(3) does NOT set CLOEXEC on the master: without this, the
    // roost we spawn below inherits it, ends up holding its own copy of
    // "the terminal" open, and closing *our* copy later never produces a
    // hangup at all — the gate would then pass vacuously no matter what
    // roost does. This is the load-bearing line the earlier repro this test
    // is adapted from called out for the same reason.
    // SAFETY: `master` is a valid fd this function owns exclusively so far.
    unsafe {
        libc::fcntl(master, libc::F_SETFD, libc::FD_CLOEXEC);
    }

    // Three independent dups of the same `slave` fd, one per stdio stream:
    // `Command` takes ownership of each and closes it in the parent once
    // spawned, so each stream needs its own.
    // SAFETY: `slave` is a valid fd owned by this function; `dup` and
    // `Stdio::from_raw_fd` together just hand one more owned copy of it to
    // `Command`.
    let (roost_stdin, roost_stdout, roost_stderr) = unsafe {
        (
            std::process::Stdio::from_raw_fd(libc::dup(slave)),
            std::process::Stdio::from_raw_fd(libc::dup(slave)),
            std::process::Stdio::from_raw_fd(libc::dup(slave)),
        )
    };
    let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_roost"));
    cmd.current_dir(env!("CARGO_MANIFEST_DIR"))
        .env("ROOST_STATE", &state)
        .env("SHELL", "/bin/sh")
        .env("TERM", "xterm-256color")
        .env("ROOST_NO_EXT_INSTALL", "1")
        .stdin(roost_stdin)
        .stdout(roost_stdout)
        .stderr(roost_stderr);
    // SAFETY: `pre_exec` runs in the forked child between `fork` and `exec`,
    // where only async-signal-safe calls are sound — `setsid` and `ioctl`
    // both qualify. This is what makes the spawned roost the pty's session
    // leader and gives it the pty as its controlling terminal, exactly as a
    // real terminal emulator does for whatever it launches.
    unsafe {
        cmd.pre_exec(move || {
            libc::setsid();
            // `TIOCSCTTY` is already `c_ulong` on Linux but `u32` on macOS,
            // where `ioctl` still wants `c_ulong`. `from` widens where it
            // must and is the identity where it need not, which a plain
            // `as` cast could not be without tripping `trivial_numeric_casts`
            // on the platform that needs no cast at all.
            if libc::ioctl(slave, libc::c_ulong::from(libc::TIOCSCTTY), 0) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let Ok(mut child) = cmd.spawn() else {
        eprintln!("SKIP terminal-hangup gate: could not spawn roost under the test pty");
        let _ = std::fs::remove_dir_all(&state);
        return;
    };
    let pid = child.id();
    // SAFETY: `slave` is a valid fd this function owns; only roost's own
    // dup'd copies (now inside the child) keep the slave side open from
    // here on, which is exactly the point — roost, and only roost, should
    // be left holding the pty.
    unsafe {
        libc::close(slave);
    }

    // Drain the master continuously so roost never blocks on a full pty
    // output buffer while it starts up and draws its first frames — that
    // would be a different hang than the one this test exists to catch.
    // SAFETY: `dup` of the valid `master` fd; CLOEXEC so this thread's own
    // copy can't leak into any process spawned after this point.
    let drain_fd = unsafe {
        let d = libc::dup(master);
        libc::fcntl(d, libc::F_SETFD, libc::FD_CLOEXEC);
        d
    };
    std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        loop {
            // SAFETY: `drain_fd` stays valid for this thread's whole life
            // (nothing else closes it before the `close` below ends the
            // read with EOF/EBADF); `buf` is sized exactly as passed.
            let n = unsafe { libc::read(drain_fd, buf.as_mut_ptr() as *mut _, buf.len()) };
            if n <= 0 {
                break;
            }
        }
    });

    std::thread::sleep(Duration::from_secs(3));
    assert!(harness::is_alive(pid), "roost should have started and still be up");
    let fleet = harness::descendant_pids(pid);
    assert!(!fleet.is_empty(), "roost should have spawned its one shell pane by now");

    // *** Close the tab: every fd onto the master side goes at once — the
    // same thing that happens when a real terminal emulator's own process
    // exits. Nothing else in this test process holds a copy of either. ***
    // SAFETY: `master` and `drain_fd` are both fds this function owns
    // exclusively.
    unsafe {
        libc::close(master);
        libc::close(drain_fd);
    }

    let roost_left = survivors(&[pid], Duration::from_secs(8));
    assert!(
        roost_left.is_empty(),
        "roost survived its terminal hanging up — it should have torn itself \
         down via infra::signals::watch_for_hangup instead of spinning forever \
         inside crossterm::event::poll's stdin-EOF loop"
    );
    let fleet_left = survivors(&fleet, Duration::from_secs(8));
    assert!(
        fleet_left.is_empty(),
        "roost died but left its fleet running: {fleet_left:?} — the hangup \
         watchdog's pane registry (infra::pty::live_pane_pgids) should have \
         reached them too"
    );

    // The verdict roost reports, not just that it reported one. Forcibly
    // shooting the fleet is not success: a supervisor, wrapper script or
    // shell loop has to be able to tell "the terminal vanished and roost
    // had to kill four agents" from a clean Alt+q. 128 + SIGHUP is the
    // encoding every shell already uses for "died because its terminal went
    // away", which is precisely what happened.
    //
    // `wait` here also reaps the child, so the temp state dir below is
    // removed after the process is genuinely gone rather than racing it.
    let status = child.wait().expect("roost was spawned, so it can be waited on");
    assert_eq!(
        status.code(),
        Some(128 + libc::SIGHUP),
        "the forced hangup teardown must not report success: {status:?}",
    );

    let _ = std::fs::remove_dir_all(&state);
}
