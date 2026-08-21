//! "A pane needs you" side-channel: the host terminal's own notification.
//!
//! roost is a program inside a terminal, and the terminal is the thing macOS
//! and Linux already trust to raise a notification with a name and an icon.
//! So roost asks it to: a bell, and an `OSC 9` carrying the text — the same
//! sequence `infra::pty` already re-emits for a pane's own notification, and
//! the mechanism SPEC-parity P2 names as the contract ("re-emitted to the
//! host terminal so native desktop notifications fire"). Ghostty, iTerm2,
//! WezTerm and kitty all raise a real notification from it, attributed to
//! themselves, and it survives ssh because it is just bytes on a stream.
//!
//! **It used to fork `osascript` instead, and that was wrong twice.**
//!
//! Wrong once because of what the user sees: `display notification` posts
//! under the bundle running the script, and for `/usr/bin/osascript` that
//! bundle is **Script Editor**. `with title "roost"` sets the notification's
//! title line, not the app it is attributed to — so every roost notification
//! on macOS arrived from an app the user never opened. There is no flag for
//! this; short of shipping roost as an `.app`, a CLI cannot post under its
//! own name.
//!
//! Wrong twice because of double-counting. A pane's OSC 9 already took both
//! paths: `PtyPane::route_effect` relays it to the host *and* pushes it to
//! the composition root, which forked `osascript`. On any terminal that
//! honours OSC 9 — which is every terminal an agent user is likely to be in
//! — one `printf '\e]9;done\a'` produced **two** desktop notifications, one
//! correct and one from Script Editor.
//!
//! The cost of the change is honest and small: a host that ignores OSC 9
//! (Apple Terminal, Alacritty) no longer gets a desktop notification, only
//! the bell it already got. That is the same deal every other CLI in that
//! terminal offers, and it is the deal the spec already described.
//!
//! Deleting the fork also deletes what it cost: ~15 ms of CPU and ~10 MB per
//! notification, plus a thread to reap the child.

use std::sync::Mutex;
use std::time::Instant;

/// How many notifications may fire back to back before the rate below
/// applies. Sized for the legitimate burst: a broadcast lands on the whole
/// fleet at once and every pane that goes needy deserves its own ring.
const NOTIFY_BURST: f64 = 8.0;

/// Sustained ceiling, notifications per second, process-wide.
///
/// Kept after the `osascript` fork was deleted, and for the reason that
/// always mattered more than the fork's cost: this is a shared channel to
/// the *user's* terminal, and a notification storm is something they have
/// to dismiss by hand. `PtyPane::route_effect` caps the OSC 9 relay at one
/// per pane per second, but that is per pane and it is not the only door —
/// `App::on_status` fires on an extension's status transitions, whose only
/// bound is the control socket's own token bucket (20 lines/s per
/// principal). A pane is an untrusted principal by construction, which is
/// what its token exists to contain, so the shared channel keeps a bound of
/// its own rather than trusting every caller to have one.
const NOTIFY_PER_SEC: f64 = 2.0;

/// `(last refill, tokens)`. Lazily refilled, so no timer thread — the same
/// shape as the control socket's `Bucket`, kept here rather than shared
/// because `infra::sock`'s is private to a module with a very different
/// notion of a principal.
static BUDGET: Mutex<Option<(Instant, f64)>> = Mutex::new(None);

/// Spend one token if there is one. Split out from `notify` and taking its
/// clock as an argument so the policy is testable without sleeping.
fn take(state: &mut Option<(Instant, f64)>, now: Instant, capacity: f64, per_sec: f64) -> bool {
    let (last, tokens) = state.unwrap_or((now, capacity));
    let refilled = (tokens + now.duration_since(last).as_secs_f64() * per_sec).min(capacity);
    if refilled < 1.0 {
        *state = Some((now, refilled));
        return false;
    }
    *state = Some((now, refilled - 1.0));
    true
}

/// Whether a notification may fire now. A poisoned lock (a panic in another
/// thread while holding it) must not silence the channel, so it is cleared
/// and the notification allowed — failing open is right for an attention
/// signal, and the panic is the bigger problem either way.
fn allowed() -> bool {
    let mut guard = match BUDGET.lock() {
        Ok(g) => g,
        Err(poisoned) => {
            BUDGET.clear_poison();
            poisoned.into_inner()
        }
    };
    take(&mut guard, Instant::now(), NOTIFY_BURST, NOTIFY_PER_SEC)
}

/// The bytes for one roost notification, or `None` when the shared budget
/// says no (or there is nothing left after sanitizing).
///
/// Bytes rather than a write: these go on the host terminal's stream, and
/// W3's rule is that such bytes are *queued* by the core (`App::notify_host`)
/// and placed between frames by the composition root, never written from
/// wherever the event happened to be noticed.
pub fn host_bytes(msg: &str) -> Option<Vec<u8>> {
    if !allowed() {
        return None;
    }
    let bytes = host_notify(msg);
    // A body that sanitizes away leaves only the bell, which is still worth
    // sending — the attention signal is the point.
    Some(bytes)
}

/// The bytes `notify` puts on the host terminal: a bell, then an `OSC 9`
/// carrying the text.
///
/// Split out from the write so the shape is testable without a terminal, and
/// pure so the escaping can be proven rather than eyeballed.
///
/// The bell leads and is deliberately its own: a terminal that does not know
/// `OSC 9` parses the sequence as a string it ignores — **including its
/// terminating BEL** — so without this the audible fallback would vanish on
/// exactly the hosts that need it most.
///
/// `msg` is pane-derived (an agent's own notification text, or
/// `display_name`, which returns a `workspace.json` `title` verbatim) and
/// this is a raw write to the operator's real stdout, so it goes through the
/// same `sanitize_for_host` + `HOST_NOTIFY_CAP` pass `infra::pty`'s relay
/// uses: an ESC or BEL inside the body would close the sequence early and
/// leave its tail to be read as host commands.
fn host_notify(msg: &str) -> Vec<u8> {
    let body = super::pty::sanitize_for_host(msg, super::pty::HOST_NOTIFY_CAP);
    let mut out = Vec::with_capacity(body.len() + 6);
    out.push(0x07);
    if !body.is_empty() {
        out.extend_from_slice(b"\x1b]9;");
        out.extend_from_slice(body.as_bytes());
        out.push(0x07);
    }
    out
}

#[cfg(test)]
mod budget_tests {
    use super::{take, NOTIFY_BURST, NOTIFY_PER_SEC};
    use std::time::{Duration, Instant};

    /// The burst passes, the flood behind it does not, and the ceiling is
    /// the sustained rate rather than "one and then silence".
    #[test]
    fn a_flood_is_capped_at_the_sustained_rate_after_its_burst() {
        let t0 = Instant::now();
        let mut state = None;
        // Same instant throughout: no refill, so only the burst gets through.
        let burst =
            (0..1000).filter(|_| take(&mut state, t0, NOTIFY_BURST, NOTIFY_PER_SEC)).count();
        assert_eq!(burst, NOTIFY_BURST as usize, "the burst is exactly the capacity");

        // One second later the bucket has refilled by the sustained rate,
        // and not by more.
        let t1 = t0 + Duration::from_secs(1);
        let after =
            (0..1000).filter(|_| take(&mut state, t1, NOTIFY_BURST, NOTIFY_PER_SEC)).count();
        assert_eq!(after, NOTIFY_PER_SEC as usize, "a second buys exactly the sustained rate");

        // A long quiet period refills to the cap, never past it.
        let t2 = t1 + Duration::from_secs(3600);
        let later =
            (0..1000).filter(|_| take(&mut state, t2, NOTIFY_BURST, NOTIFY_PER_SEC)).count();
        assert_eq!(later, NOTIFY_BURST as usize, "the bucket never exceeds its capacity");
    }

    /// The common case — a notification now and then — is never throttled.
    #[test]
    fn an_occasional_notification_always_fires() {
        let mut state = None;
        let mut t = Instant::now();
        for i in 0..50 {
            assert!(
                take(&mut state, t, NOTIFY_BURST, NOTIFY_PER_SEC),
                "notification {i}, one every 5s, must not be throttled"
            );
            t += Duration::from_secs(5);
        }
    }
}

#[cfg(test)]
mod host_bytes_tests {
    use super::*;

    /// The whole point of the change: what goes on the wire is a bell plus a
    /// well-formed `OSC 9`, and the terminal — not Script Editor — is what
    /// raises the notification.
    #[test]
    fn a_notification_is_a_bell_then_an_osc_9() {
        assert_eq!(host_notify("pi needs you"), b"\x07\x1b]9;pi needs you\x07".to_vec());
    }

    /// The bell is separate from the sequence's own terminator on purpose: a
    /// terminal that does not implement OSC 9 swallows the whole string,
    /// terminator included, so a body-terminating BEL alone would leave the
    /// audible fallback silent on exactly the hosts that only have it.
    #[test]
    fn the_audible_bell_survives_a_host_that_ignores_osc_9() {
        let bytes = host_notify("anything");
        assert_eq!(bytes[0], 0x07, "the bell leads, outside the sequence");
        assert_eq!(&bytes[1..5], b"\x1b]9;", "and the sequence starts after it");
    }

    /// Replaces the AppleScript-injection pair this module used to carry.
    /// The body is still attacker-controlled — an agent's own notification
    /// text, or a `workspace.json` `title` — and this is still a raw write
    /// to the operator's real stdout. The escape it must not manage is now
    /// the OSC string's, not AppleScript's: an ESC or BEL inside the body
    /// would close the sequence early and leave its tail to be read as host
    /// commands.
    #[test]
    fn a_hostile_body_cannot_break_out_of_the_osc_sequence() {
        let hostile = "safe\x07\x1b]0;pwned\x07\x1b[2Jtail";
        let bytes = host_notify(hostile);
        // Exactly two BELs: the leading bell and the sequence terminator.
        assert_eq!(bytes.iter().filter(|b| **b == 0x07).count(), 2);
        // Exactly one ESC: the sequence's own introducer.
        assert_eq!(bytes.iter().filter(|b| **b == 0x1b).count(), 1);
        // The leftover `]0;` and `[2J` are *printable* text and survive, on
        // purpose: without an ESC to introduce them they are inert, and the
        // sanitizer's contract is "no control bytes", which is precisely
        // what makes the sequence unbreakable. Stripping more would be
        // guessing at what a terminal might do with ordinary characters.
        assert_eq!(bytes, b"\x07\x1b]9;safe]0;pwned[2Jtail\x07".to_vec());
    }

    /// And bounded, for the same reason `infra::pty`'s relay is: a pane must
    /// not push a megabyte through roost's own stdout.
    #[test]
    fn the_body_is_capped() {
        let bytes = host_notify(&"A".repeat(10_000));
        // bell + `ESC ] 9 ;` + cap chars + terminating bell.
        assert_eq!(bytes.len(), 1 + 4 + super::super::pty::HOST_NOTIFY_CAP + 1);
    }

    /// A body that sanitizes away to nothing emits the bell alone rather
    /// than an empty `OSC 9;` the host would have to parse for no reason.
    #[test]
    fn a_body_that_sanitizes_away_leaves_just_the_bell() {
        assert_eq!(host_notify("\x07\x1b\x00"), vec![0x07]);
    }
}
