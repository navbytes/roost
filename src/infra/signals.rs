//! Termination signals, turned into an ordinary loop exit.
//!
//! roost's whole job is owning other people's processes. Every pane is
//! `setsid`'d into its own session (`infra::pty`), so a signal aimed at
//! roost reaches roost alone — nothing propagates to the fleet on its own.
//! That makes the default disposition of SIGHUP/SIGTERM/SIGINT actively
//! wrong here: roost dies on the spot, `App::shutdown` never runs, and every
//! agent it was hosting keeps running detached until the machine reboots.
//! Closing the terminal window, an ssh connection dropping, `kill` from a
//! supervisor or an init system, and logout all arrive this way — for most
//! users far more often than Alt+q does.
//!
//! So the handler does the one thing a handler may safely do: set a flag.
//! Everything that actually matters (saving the workspace, hanging up each
//! pane, the SIGKILL backstop, restoring the terminal) happens on the normal
//! path, after the main loop notices and breaks — the identical teardown
//! Alt+q gets, which is the point. `SA_RESTART` keeps the interrupted
//! 33ms event poll from surfacing as a spurious `EINTR` error, and the loop
//! checks the flag every iteration, so the delay is bounded by one frame.
//!
//! SIGKILL and SIGSTOP are deliberately absent: they cannot be caught, and
//! nothing can be promised about them.

use std::sync::atomic::{AtomicBool, Ordering};

static TERMINATING: AtomicBool = AtomicBool::new(false);

/// The only thing the handler does. `AtomicBool::store` is a single
/// instruction on every platform roost builds for — async-signal-safe in the
/// way `write(2)` is, unlike anything that could allocate or take a lock.
extern "C" fn on_terminate(_sig: libc::c_int) {
    TERMINATING.store(true, Ordering::SeqCst);
}

/// Whether a termination signal has arrived. Polled by the main loop.
pub fn terminating() -> bool {
    TERMINATING.load(Ordering::SeqCst)
}

/// Install the handlers. Called once, before the main loop.
///
/// SIGINT is included even though roost's raw mode means Ctrl+C goes to the
/// focused pane rather than to roost: a SIGINT that does reach roost came
/// from something deliberate (`kill -INT`, a job-control front end), and
/// dropping the fleet on the floor is no better an answer there.
pub fn install() {
    for sig in [libc::SIGHUP, libc::SIGTERM, libc::SIGINT] {
        // SAFETY: `sigaction` with a plain function pointer and a zeroed
        // mask. `on_terminate` touches nothing but a static atomic.
        unsafe {
            let mut act: libc::sigaction = std::mem::zeroed();
            act.sa_sigaction = on_terminate as extern "C" fn(libc::c_int) as usize;
            act.sa_flags = libc::SA_RESTART;
            libc::sigemptyset(&mut act.sa_mask);
            libc::sigaction(sig, &act, std::ptr::null_mut());
        }
    }
}
