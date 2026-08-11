//! P6 (SPEC-parity) end to end: live pane titles, both directions.
//!
//! Claude Code continuously publishes `spinner + task` through OSC 0/2 —
//! the cheapest live fleet-status text there is. vt100 has been parsing and
//! storing it all along, with **zero** call sites reading `screen().title()`;
//! and nothing re-emitted a title to the host, so the outer tab went stale
//! the moment roost started.
//!
//! Inbound: an *agent* pane's OSC 2 becomes its display name, visible on the
//! corner badge. Outbound: roost publishes `roost · <focused pane>` to its
//! own terminal.
//!
//! A plain `shell` pane deliberately skips the live rung — its title is
//! `PS1` chrome (`user@host: /path`) that restates the cwd tag already on
//! the badge. This drive covers both sides of that line, and the crossing
//! between them: the pane starts as a shell (title ignored), then runs a
//! process argv-named `pi`, which `observe_panes` promotes to the pi
//! adapter — and the same title it had been ignoring is adopted.

// The shared harness is compiled per test binary; helpers other tenants use
// are dead code from this binary's view — not real rot.
#[allow(dead_code)]
mod harness;

use std::time::Duration;

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

#[test]
fn a_pane_osc_title_names_it_on_the_badge_and_in_the_host_title() {
    let cwd = std::env::temp_dir();
    let cwd = cwd.to_str().expect("temp dir is valid utf8");
    let Some(mut h) = harness::spawn_or_skip("pane-title gate", &harness::one_pane(cwd)) else {
        return;
    };
    // Before: the untitled pane's badge carries the `adapter · cwd-tag`
    // fallback, so "1 shell" is on screen and no task name is.
    if h.wait_for(Duration::from_secs(5), |s| s.contents().contains("1 shell")).is_none() {
        panic!(
            "expected the adapter/cwd fallback badge before any OSC title:\n{}",
            h.screen().contents()
        );
    }
    assert!(h.settle(Duration::from_secs(5)), "initial frame never settled");

    // The pane publishes a title the way an agent CLI does. The marker is
    // spelled split (`TAS''K-X`) so the shell's echo of the command line
    // can never satisfy the assertion — only the badge can.
    h.write_bytes(b"printf '\\033]2;TAS''K-X\\007'\r");

    // While it is still a plain shell, that title is ignored: a shell's
    // title is `PS1` chrome, not fleet status. Two detect ticks (2 s each)
    // is well past any adoption that was going to happen.
    std::thread::sleep(Duration::from_secs(5));
    h.settle(Duration::from_secs(3));
    let shell_frame = h.screen().contents();
    assert!(
        shell_frame.contains("1 shell") && !shell_frame.contains("1 TASK-X"),
        "a shell pane must keep its adapter/cwd badge despite an OSC title:\n{shell_frame}"
    );

    // Now the pane runs an agent: `sh -c '…' pi` puts `pi` in the process's
    // argv, which is exactly what `observe_panes` matches on, so the pane is
    // promoted to the pi adapter — the real path for an agent launched by
    // hand inside a shell pane.
    //
    // The trailing `; :` is load-bearing. A shell handed a `-c` string
    // holding exactly one command **exec-replaces itself** with it, and the
    // `pi` that was going to be argv[0] dies with the shell: on macOS
    // `ps -o command=` then reports a bare `sleep 300` and the promotion
    // never fires. A second command forces the shell to stay alive and fork,
    // so `pi` survives in its argv on every platform. (macOS's /bin/sh
    // optimizes here where Linux's does not, which is why this test passed
    // on one and failed on the other for as long as it existed.)
    h.write_bytes(b"sh -c 'sleep 300; :' pi\r");

    // Inbound: once promoted, the corner badge adopts the title it had been
    // ignoring (C4's badge leads with the pane id, so `1 TASK-X` is the
    // exact rendered token). Generous: promotion waits on a 2 s detect tick.
    if h.wait_for(Duration::from_secs(15), |s| s.contents().contains("1 TASK-X")).is_none() {
        panic!(
            "the promoted pane's OSC 2 title never reached its badge:\n{}",
            h.screen().contents()
        );
    }

    // Outbound: roost publishes the focused pane's name as the host
    // terminal's own title, led by the pane id exactly like the badge — a
    // window title reading only `shell · cwd` cannot say which of a
    // project's identically-named panes is focused. `wait_for_host_bytes`
    // rather than a one-shot read: the update is throttled, so it may lag
    // the badge by a tick.
    let published = h.wait_for_host_bytes(Duration::from_secs(5), |b| {
        contains(b, "\x1b]2;roost · 1 TASK-X\x07".as_bytes())
    });
    assert!(
        published,
        "roost never published `roost · 1 TASK-X` as the host title; tail:\n{}",
        String::from_utf8_lossy(&tail(&h.host_bytes(), 400))
    );

    // On the way out the title goes back to a plain `roost` — leaving the
    // user's tab named after a pane that no longer exists would be worse
    // than the stale title P6 started from.
    let _ = h.quit_and_wait(Duration::from_secs(5));
    assert!(
        contains(&h.host_bytes(), b"\x1b]2;roost\x07"),
        "roost must reset the host title on exit; tail:\n{}",
        String::from_utf8_lossy(&tail(&h.host_bytes(), 400))
    );
}

/// The last `n` bytes, for readable failure output.
fn tail(bytes: &[u8], n: usize) -> Vec<u8> {
    bytes[bytes.len().saturating_sub(n)..].to_vec()
}
