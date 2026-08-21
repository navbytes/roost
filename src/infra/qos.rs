//! Thread QoS promotion (macOS): keep typing crisp when agents saturate the
//! machine.
//!
//! The problem this solves is scheduling, not throughput. Roost's own work
//! per keystroke is microseconds, but when a pane's agent (claude crunching,
//! a build it spawned) pins every core, roost waits in the same scheduler
//! queue as everything else — measured on the firehose gate: echo p50 stays
//! ~19ms under full load, while the worst keystroke stretches to ~350ms
//! (past DESIGN-ui.md §6's 250ms budget), purely from contention.
//!
//! The deliberate shape: promote ROOST's keystroke path, never demote the
//! agents. `nice`-ing spawned children was rejected because niceness is
//! global — a roost-spawned claude would yield not just to roost but to
//! every process on the machine, making the fleet's real work second-class
//! against anything launched outside roost. QoS promotion of our own
//! threads has no such side effect: agents keep exactly the priority any
//! hand-launched process gets, and only the moments of direct contention
//! between roost's input handling and *anything* else tip toward roost.
//!
//! What gets promoted, and to what:
//! - The main event-loop thread → `USER_INTERACTIVE`: it polls the
//!   keyboard, parses pane output, and draws — the whole visible product.
//!   This is the exact class Apple reserves for "the user is interacting
//!   with this right now".
//! - Each pane's PTY *writer* thread → `USER_INITIATED`: it carries the
//!   typed bytes to the child. One rung below interactive — it finishes a
//!   user-initiated hand-off but doesn't paint the screen.
//! - PTY *reader* threads stay at default, deliberately: they front the
//!   agents' output firehose, and promoting them would spend the scheduler
//!   boost on exactly the flood the event loop already paces via the
//!   bounded channel (`EVENT_CHANNEL_BOUND`).
//!
//! Off macOS both calls are no-ops: raising priority on Linux needs
//! CAP_SYS_NICE (demoting is unprivileged, but demoting is the rejected
//! design), and macOS is where the fleet runs today. The Linux story can
//! revisit sched deadlines if it ever matters.
//!
//! Honest evidence note: the firehose gate could NOT demonstrate an
//! end-to-end win from this (A/B under 12 saturating processes: p50 ~23ms
//! and a 300–500ms max tail on both sides). That gate measures a full echo
//! round trip, and most of its hops are outside roost's control — the
//! pane's own shell *generates* the echo while contended, and the test
//! harness observes through an unpromoted process of its own. What this
//! module buys is scheduling priority for the roost-owned hops (keystroke
//! pickup, delivery to the PTY, drawing) — cheap, OS-sanctioned insurance
//! for the interactive path, verified to take effect (the unit test reads
//! the class back from the OS), but not a measured end-to-end latency
//! reduction. If a future measurement isolates the roost-owned hops and
//! still shows nothing, deleting this module is the right move.

/// Is QoS promotion active for this run? macOS with no opt-out. The
/// `ROOST_NO_QOS=1` env var is both the kill switch and the A/B lever for
/// the telemetry-driven keep-or-delete decision (`infra::perf`): run some
/// days with it, some without, compare stall distributions at similar load.
pub fn enabled() -> bool {
    cfg!(target_os = "macos") && std::env::var_os("ROOST_NO_QOS").is_none()
}

/// Mark the calling thread as user-interactive (macOS). Call once, from the
/// main event-loop thread, before the loop starts. Best-effort: a refusal
/// (unsupported class, sandbox) changes nothing about correctness, so the
/// return code is deliberately ignored beyond debug builds.
pub fn promote_input_loop_thread() {
    if !enabled() {
        return;
    }
    #[cfg(target_os = "macos")]
    // SAFETY: plain FFI with no pointers; affects only the calling thread's
    // scheduler class.
    unsafe {
        let rc =
            libc::pthread_set_qos_class_self_np(libc::qos_class_t::QOS_CLASS_USER_INTERACTIVE, 0);
        debug_assert_eq!(rc, 0, "QOS_CLASS_USER_INTERACTIVE refused for the event loop");
    }
}

/// Mark the calling thread as user-initiated (macOS). Call once at the top
/// of each pane's PTY writer thread — the thread that delivers keystrokes
/// to the child. Same best-effort contract as `promote_input_loop_thread`.
pub fn promote_input_delivery_thread() {
    if !enabled() {
        return;
    }
    #[cfg(target_os = "macos")]
    // SAFETY: as above — no pointers, calling thread only.
    unsafe {
        let rc =
            libc::pthread_set_qos_class_self_np(libc::qos_class_t::QOS_CLASS_USER_INITIATED, 0);
        debug_assert_eq!(rc, 0, "QOS_CLASS_USER_INITIATED refused for a writer thread");
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;

    /// Read the calling thread's QoS class back from the OS, as its raw
    /// value (`qos_class_t` carries no `PartialEq` in libc).
    fn my_qos() -> u32 {
        let mut class = libc::qos_class_t::QOS_CLASS_UNSPECIFIED;
        let mut prio = 0i32;
        // SAFETY: out-pointers to locals; pthread_self is the calling thread.
        let rc =
            unsafe { libc::pthread_get_qos_class_np(libc::pthread_self(), &mut class, &mut prio) };
        assert_eq!(rc, 0);
        class as u32
    }

    /// The promotions must actually land — asserted via the OS's own
    /// read-back, on scratch threads so the test runner's threads are
    /// never mutated.
    #[test]
    fn promotions_set_the_class_the_os_reports() {
        std::thread::spawn(|| {
            promote_input_loop_thread();
            assert_eq!(my_qos(), libc::qos_class_t::QOS_CLASS_USER_INTERACTIVE as u32);
        })
        .join()
        .unwrap();

        std::thread::spawn(|| {
            promote_input_delivery_thread();
            assert_eq!(my_qos(), libc::qos_class_t::QOS_CLASS_USER_INITIATED as u32);
        })
        .join()
        .unwrap();
    }
}
