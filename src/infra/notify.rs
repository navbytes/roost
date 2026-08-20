//! "A pane needs you" side-channel: terminal bell everywhere, plus a native
//! notification on macOS.

use std::sync::Mutex;
use std::time::Instant;

/// How many notifications may fire back to back before the rate below
/// applies. Sized for the legitimate burst: a broadcast lands on the whole
/// fleet at once and every pane that goes needy deserves its own ring.
const NOTIFY_BURST: f64 = 8.0;

/// Sustained ceiling, notifications per second, process-wide.
///
/// Every producer routes through `notify`, and on macOS each call forks an
/// `osascript` (~15 ms of CPU and ~10 MB, plus a thread to reap it) and
/// rings the host bell. `PtyPane::route_effect` caps the OSC 9 channel at
/// one per pane per second, but that is per pane and it is not the only
/// door: `App::on_status` fires on an extension's status transitions, whose
/// only bound is the control socket's own token bucket (20 lines/s per
/// principal). A pane is an untrusted principal by construction — that is
/// what its token exists to contain — so the shared, expensive channel gets
/// a bound of its own rather than trusting every caller to have one.
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

pub fn notify(msg: &str) {
    if !allowed() {
        return;
    }
    use std::io::Write;
    let mut out = std::io::stdout();
    let _ = out.write_all(b"\x07");
    let _ = out.flush();
    #[cfg(target_os = "macos")]
    {
        // Info-b: `msg` is pane-derived (an agent's own notification
        // text, or `display_name`, which returns a workspace.json
        // `title` verbatim) and must never become part of the
        // AppleScript source — escaping only `"` and `\` was
        // quoting-by-luck, safe today only because vte happens to drop
        // C0 bytes from OSC payloads upstream. Passed as an `on run`
        // argument instead, `msg` is never parsed as script text at
        // all, so there's nothing to escape and nothing to break out
        // of. `--` is load-bearing, not decorative: `osascript`'s own
        // flag parsing does not stop at the first positional arg, so
        // without it a `msg` starting with `-e` is read as a *second*
        // `-e` script instead of `on run`'s argv (confirmed on this
        // box) — the same quoting-by-luck the interpolation fix set out
        // to delete, just moved from AppleScript's own quoting to
        // `osascript`'s argv parsing. No shell is involved either way —
        // `Command` execs `osascript` directly — so this isn't shell
        // quoting, just `osascript` reusing `-e` for both roles.
        let script = "on run {b}\n  display notification b with title \"roost\"\nend run";
        if let Ok(child) = std::process::Command::new("osascript")
            .arg("-e")
            .arg(script)
            .arg("--")
            .arg(msg)
            .spawn()
        {
            // Reap it so rapid notifications don't accumulate zombies.
            std::thread::spawn(move || {
                let mut child = child;
                let _ = child.wait();
            });
        }
    }
    #[cfg(not(target_os = "macos"))]
    let _ = msg;
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
        let burst = (0..1000).filter(|_| take(&mut state, t0, NOTIFY_BURST, NOTIFY_PER_SEC)).count();
        assert_eq!(burst, NOTIFY_BURST as usize, "the burst is exactly the capacity");

        // One second later the bucket has refilled by the sustained rate,
        // and not by more.
        let t1 = t0 + Duration::from_secs(1);
        let after = (0..1000).filter(|_| take(&mut state, t1, NOTIFY_BURST, NOTIFY_PER_SEC)).count();
        assert_eq!(after, NOTIFY_PER_SEC as usize, "a second buys exactly the sustained rate");

        // A long quiet period refills to the cap, never past it.
        let t2 = t1 + Duration::from_secs(3600);
        let later = (0..1000).filter(|_| take(&mut state, t2, NOTIFY_BURST, NOTIFY_PER_SEC)).count();
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

#[cfg(all(test, target_os = "macos"))]
mod tests {
    /// Builds the same invocation shape `notify` uses (`-e SCRIPT -- MSG`),
    /// swapping `display notification b with title "roost"` for `return b`
    /// so the proof is on stdout, not on whether a notification actually
    /// posted (permission-dependent, irrelevant here).
    fn run_with_hostile_body(hostile: &str) -> std::process::Output {
        std::process::Command::new("osascript")
            .arg("-e")
            .arg("on run {b}\n  return b\nend run")
            .arg("--")
            .arg(hostile)
            .output()
            .expect("osascript ships with macOS")
    }

    /// Info-b: a payload built to break out of a double-quoted AppleScript
    /// string literal (`" & (do shell script ...) & "`) must come back
    /// byte-for-byte instead of being interpreted, when passed as an
    /// `on run` argument the same way `notify` passes it.
    #[test]
    fn hostile_body_cannot_break_out_of_the_applescript_source() {
        let hostile = "safe\" & (do shell script \"echo pwned\") & \"tail";
        let out = run_with_hostile_body(hostile);
        assert!(out.status.success());
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim_end(), hostile);
    }

    /// Info-b (review round 2): `osascript`'s own flag parsing does not stop
    /// at the first positional arg — without the `--` separator, a body
    /// starting with `-e` is read as a *second* `-e` script instead of
    /// `on run`'s argv (`display_name` returns a workspace.json `title`
    /// verbatim, so an attacker fully controls this prefix). With `--`, it
    /// comes back as inert text, same as any other payload.
    #[test]
    fn hostile_body_leading_with_a_flag_is_never_reparsed_as_a_second_dash_e() {
        let hostile = "-e return \"INJECTED\"";
        let out = run_with_hostile_body(hostile);
        assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim_end(), hostile);
    }
}
