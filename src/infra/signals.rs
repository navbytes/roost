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
use std::time::Duration;

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

/// How long the watchdog gives the main thread to notice `TERMINATING` and
/// exit through the normal path (`App::shutdown`, which also re-saves the
/// workspace) before assuming it never will and tearing the fleet down by
/// hand instead.
const HANGUP_GRACE_FOR_MAIN_LOOP: Duration = Duration::from_millis(500);

/// How much longer the watchdog waits once the ordinary teardown has
/// actually *started*.
///
/// The 500 ms above is sized for "did the main thread notice at all?", not
/// for "has it finished?". `App::kill_fleet` spends `pty::HANGUP_GRACE`
/// (200 ms) plus a reap budget plus a workspace save — comfortably inside
/// 500 ms on an idle machine, and not reliably so on a loaded one or a slow
/// disk. Forcing at a flat deadline would then SIGKILL agents *mid-flush*,
/// destroying the final turn that `hangup` exists to let them write. So once
/// teardown is visibly under way the watchdog stands back and gives it room.
///
/// Waiting costs nothing when teardown succeeds: the process exits the
/// moment `main` returns, taking this thread with it. The cap only bounds
/// the pathological case where teardown itself wedges.
const TEARDOWN_COMPLETION_CAP: Duration = Duration::from_secs(5);

/// Set by `App::kill_fleet` the moment the ordinary teardown begins, so the
/// hangup watchdog can tell "the main thread is wedged" from "the main
/// thread is doing the job right now".
static TEARDOWN_STARTED: AtomicBool = AtomicBool::new(false);

/// Called by `App::kill_fleet`. Idempotent; the teardown path may be reached
/// from Alt+q, a signal, the panic hook or the watchdog itself.
pub fn note_teardown_started() {
    TEARDOWN_STARTED.store(true, Ordering::SeqCst);
}

/// Whether the ordinary teardown has begun. Read by the watchdog, and by the
/// test that pins `kill_fleet` announcing itself.
pub fn teardown_started() -> bool {
    TEARDOWN_STARTED.load(Ordering::SeqCst)
}

/// How long the watchdog gives a SIGHUP'd pane to exit on its own — an agent
/// flushing its last turn to its session file, the same courtesy
/// `App::kill_fleet`/`PtyPane::hangup` extend on every other exit path —
/// before escalating that pane to SIGKILL.
const HANGUP_GRACE_FOR_PANES: Duration = Duration::from_millis(200);

/// How often the watchdog thread checks fd 0 for a hangup. Small enough that
/// a real hangup is noticed promptly; large enough that the thread spends
/// its life asleep rather than spinning — the opposite of the bug it exists
/// to catch.
const POLL_INTERVAL: Duration = Duration::from_millis(200);

/// Rescue roost from the one termination path `on_terminate` above cannot
/// cover: the controlling terminal hanging up *while the main thread is
/// blocked inside `crossterm::event::poll`, reading stdin*.
///
/// `on_terminate`'s whole design rests on the main loop getting back around
/// to check `terminating()` — true for `kill -HUP`, `kill -TERM`, and
/// Ctrl+C-as-SIGINT, all of which interrupt whatever syscall the main
/// thread is blocked in (`SA_RESTART` just keeps that interruption from
/// surfacing as `EINTR`). A closed terminal tab is different: it does not
/// interrupt anything. strace on a faithful repro shows the main thread
/// spinning `read(0, "", 1024) = 0` — stdin at EOF — forever, because
/// `crossterm::event::poll` treats each EOF read as "nothing ready yet" and
/// loops *inside the library*, never returning control to roost's own loop
/// at all. A flag no one ever checks again fixes nothing; roost sits at
/// 100% CPU, every pane stays orphaned, and the advisory `flock` on
/// `roost.lock` — correctly held by a process that, from the kernel's
/// perspective, is still very much alive — refuses every later `roost`.
///
/// So this watches fd 0 directly with `poll(2)`. Unlike a `read` loop,
/// `poll` reports `POLLHUP`/`POLLERR` on a hung-up terminal instead of
/// spinning through it — that is the one signal `crossterm` never asks for.
/// On seeing it: set the same `TERMINATING` flag `on_terminate` sets (a main
/// thread that happens to be between `crossterm` calls right now takes the
/// normal, graceful exit — `App::shutdown`, one more workspace save, panes
/// hung up through the ordinary code path); give that a grace period; and
/// only if the process is *still around* afterward — proof the main thread
/// really is wedged inside the poll that started this — reach into
/// `infra::pty`'s pane registry and finish the job by hand.
///
/// Losing the extra workspace save on that forced branch is deliberate, not
/// an oversight: roost writes the workspace atomically (temp file + rename)
/// on every mutating action, so the on-disk copy is at most one action
/// stale already — `App::kill_fleet`'s own doc comment accepts exactly this
/// trade for the panic path, for the identical reason.
pub fn watch_for_hangup() {
    std::thread::spawn(|| {
        loop {
            let mut fds = [libc::pollfd { fd: 0, events: libc::POLLIN, revents: 0 }];
            // SAFETY: one stack-local pollfd, a plain millisecond timeout.
            // No pointers escape this call.
            let ready = unsafe {
                libc::poll(fds.as_mut_ptr(), 1, POLL_INTERVAL.as_millis() as libc::c_int)
            };
            if ready < 0 {
                // EINTR (one of the real signals above landed) or some other
                // transient poll failure. Neither means the terminal hung
                // up — an interrupted syscall is not a hangup — so keep
                // watching rather than acting on it.
                continue;
            }
            if fds[0].revents & (libc::POLLHUP | libc::POLLERR) != 0 {
                break;
            }
        }

        // The terminal is gone. Try the graceful path first — the main
        // thread may be between `crossterm` calls rather than stuck inside
        // one, and if so `App::shutdown` is strictly better than anything
        // this thread can do by hand.
        TERMINATING.store(true, Ordering::SeqCst);
        std::thread::sleep(HANGUP_GRACE_FOR_MAIN_LOOP);
        // Never truncate a teardown that is already running: it does
        // everything below and does it better, hanging agents up gently so
        // they can flush. Only a main thread that never got started — the
        // stdin-EOF spin this watchdog exists for — reaches the forced path.
        if teardown_started() {
            std::thread::sleep(TEARDOWN_COMPLETION_CAP);
        }

        // Still executing after that sleep means the process is still
        // alive (an exited process takes every thread with it, this one
        // included) — i.e. the main thread really is wedged inside
        // `event::poll`'s stdin-EOF spin and never reached the `break` its
        // own loop needs. Do by hand what `App::kill_fleet` would have:
        // SIGHUP every pane, wait, then SIGKILL. `infra::pty::live_pane_pgids`
        // exists for exactly this — the `App` and its `runtimes` map live on
        // the thread that is stuck, unreachable from here.
        for pgid in crate::infra::pty::live_pane_pgids() {
            // SAFETY: `kill(2)` on a process group id, targeting only ids
            // this registry was ever populated with — panes roost itself
            // spawned and `setsid`'d (`infra::pty::register_pane_pgid`).
            unsafe {
                libc::kill(-pgid, libc::SIGHUP);
            }
        }
        std::thread::sleep(HANGUP_GRACE_FOR_PANES);
        for pgid in crate::infra::pty::live_pane_pgids() {
            // SAFETY: as above.
            unsafe {
                libc::kill(-pgid, libc::SIGKILL);
            }
        }

        // `_exit`, not `std::process::exit`: the latter still runs Rust's
        // `atexit`-style cleanups, and whatever left the main thread wedged
        // in the first place is exactly the code we do not want to risk
        // re-entering right now. The advisory `flock` on `roost.lock` is
        // released by the kernel the instant this process is gone either
        // way — that release, not any cleanup here, is what lets the next
        // `roost` start.
        //
        // What this path skips, and why none of it is missed: the socket
        // file and `control.token` are normally unlinked on the way out
        // (`main.rs`), but `sock`'s bind already unlinks a stale socket
        // before listening, the token is rewritten at every startup and
        // authorizes nothing while no process is listening, and the
        // workspace is saved atomically on every mutation, so the copy on
        // disk is already current.
        //
        // **Not `_exit(0)`.** Forcibly killing the fleet is not success, and
        // reporting it as success makes "the terminal vanished and I had to
        // shoot four agents" indistinguishable from Alt+q to a supervisor,
        // a wrapper script or a shell loop. 128 + SIGHUP is the encoding
        // every shell already uses for "died because its terminal went
        // away", which is exactly what happened — roost simply had to
        // deliver the verdict itself because the signal alone could not
        // reach a wedged main thread.
        //
        // SAFETY: `_exit(2)` takes only an exit status; nothing here can be
        // unsound, only abrupt (which is the point).
        unsafe {
            libc::_exit(128 + libc::SIGHUP);
        }
    });
}
