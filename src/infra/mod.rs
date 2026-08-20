//! Infrastructure adapters — the production implementations of `ports`
//! traits plus the status socket listener. All real I/O lives here.

/// B2 round 2 (PR #46 review): a runtime escape hatch `clipboard::copy` and
/// `open::open_url` both honor, for the one thing `#[cfg(test)]` cannot
/// reach — the integration tests under `tests/` spawn the real, non-test
/// `roost` binary through a PTY (`CARGO_BIN_EXE_roost`), and that binary
/// carries the live clipboard/browser channels regardless of what flags
/// compiled *this* crate's own test binary. `tests/harness/mod.rs` sets
/// `ROOST_TEST_NO_HOST_IO=1` on every spawned roost by default (a scenario
/// that genuinely needs the real channel, e.g. `live_qa.rs`'s evidence
/// drive, overrides it back to `"0"`).
///
/// Trusted regardless of when/how often it's read (no caching, so a test can
/// flip it and re-check): a pane is a *child* process with its own
/// independent environment, with no OS mechanism to mutate its parent's
/// (roost's) env after roost starts, and roost never calls
/// `std::env::set_var` in response to pane or control-socket input — so
/// "set once at the real process's actual startup" and "read fresh on every
/// call" are equally unreachable by anything roost runs on a user's behalf.
/// A value of exactly `"0"` counts as "not set" so a scenario can override
/// the harness's own default back on (see `live_qa.rs`).
pub(crate) fn host_io_disabled() -> bool {
    std::env::var_os("ROOST_TEST_NO_HOST_IO").is_some_and(|v| v != "0")
}

/// Test hatch, same family and same reasoning as `ROOST_TEST_NO_HOST_IO`
/// above: panic the event loop this many milliseconds after it starts.
///
/// The property it gates cannot be reached any other way. "An unforeseen
/// panic must still tear the fleet down" is only observable by causing one
/// in the *real* binary, from the outside, with live panes and a
/// backgrounded job to count afterwards — and the whole point is that no
/// input reaches it deliberately (`tests/vt100_panics.rs` exists to keep it
/// that way). Read once, before the loop; unset — every real run — costs
/// one env lookup at startup and nothing else.
pub(crate) fn test_panic_after() -> Option<std::time::Duration> {
    let raw = std::env::var("ROOST_TEST_PANIC_AFTER_MS").ok()?;
    raw.parse().ok().map(std::time::Duration::from_millis)
}

pub mod clipboard;
pub mod config;
pub mod extension;
pub mod inspect;
pub mod notify;
pub mod open;
pub mod perf;
pub mod pty;
pub mod qos;
pub mod queries;
pub mod signals;
pub mod sock;
pub mod store;
