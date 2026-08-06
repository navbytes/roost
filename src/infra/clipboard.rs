//! Copy text to the system clipboard, robustly across environments:
//! a native helper (pbcopy on macOS, wl-copy / xclip / xsel on Linux) *and*
//! an OSC 52 escape to the terminal. Whichever the environment supports
//! lands the text; OSC 52 also covers SSH / tmux where no local helper runs.

#[cfg(not(test))]
use std::io::Write;
#[cfg(not(test))]
use std::process::{Command, Stdio};

use crate::ports::ClipboardOutcome;

/// Copy `text` to the clipboard via every available channel, and report
/// which one actually took it (U14). Both channels still fire on every
/// copy — the emission is unchanged; what changed is that the results are
/// no longer thrown away, so the hint bar can stop claiming a copy that
/// never happened.
///
/// B2 (PR #46 code review): under `cargo test` this is the `#[cfg(test)]`
/// twin below instead — `handle_mouse`/`handle_copy_mouse`'s test suites
/// drive this function for real (that's the point: the gesture-matrix
/// tests exercise the actual composition-root path), and doing so for
/// real left a stray `pbcopy` write sitting in the *operator's own*
/// system clipboard after every `cargo test` run (confirmed: `pbpaste`
/// afterwards) and OSC 52 bytes leaking into captured stdout, which
/// `--show-output` (CI's `ci.yml`) prints straight into the run's log.
///
/// **`#[cfg(test)]` only ever reaches this crate's own test binary.**
/// `tests/*.rs` spawns the real `roost` binary (built *without*
/// `cfg(test)`) through a PTY, and that process still carries this exact
/// code path — a pre-existing gap, not something this contract's own new
/// copy-on-release gestures introduced (`live_qa.rs` already documented
/// "writes the macOS clipboard" as a known, opt-in-only cost; this is the
/// same class of leak reaching a run that never opted in). `host_io_
/// disabled` (`infra/mod.rs`) is the runtime half of the fix: the harness
/// sets it on every spawned roost, so the real binary refuses both
/// channels the same way the test-build stub below always has.
#[cfg(not(test))]
pub fn copy(text: &str) -> ClipboardOutcome {
    if super::host_io_disabled() {
        return ClipboardOutcome::Failed;
    }
    let native = native_copy(text);
    let osc52 = emit_osc52(text);
    match (native, osc52) {
        (true, _) => ClipboardOutcome::Native,
        (false, true) => ClipboardOutcome::Osc52,
        (false, false) => ClipboardOutcome::Failed,
    }
}

/// The test build's `copy`: touches neither the real system clipboard nor
/// real stdout, and reports `Native` (so a test can pin the exact
/// `copied N chars` flash text instead of whichever real channel happened
/// to win on the machine running it) — unless the same runtime hatch the
/// real `copy` above honors is set, in which case it reports `Failed` too.
/// Sharing the check here as well is what makes it possible to unit-test
/// at all (the real, `#[cfg(not(test))]` copy is by definition never
/// compiled into a `cargo test` run): the two bodies differ, but both gate
/// on the identical `host_io_disabled()` call.
#[cfg(test)]
pub fn copy(_text: &str) -> ClipboardOutcome {
    if super::host_io_disabled() {
        return ClipboardOutcome::Failed;
    }
    ClipboardOutcome::Native
}

/// Pipe `text` into the first clipboard helper that succeeds. Returns
/// whether one actually took the text: spawning is not success — a helper
/// can exist and still fail (no `$DISPLAY` for xclip, no compositor for
/// wl-copy, a broken pipe), and the old "we spawned something" answer is
/// exactly how an empty clipboard got reported as `copied N chars`. A
/// helper that fails is skipped in favour of the next candidate.
#[cfg(not(test))]
fn native_copy(text: &str) -> bool {
    // (program, args) candidates in preference order.
    let candidates: &[(&str, &[&str])] = &[
        ("pbcopy", &[]),
        ("wl-copy", &[]),
        ("xclip", &["-selection", "clipboard"]),
        ("xsel", &["--clipboard", "--input"]),
    ];
    for (prog, args) in candidates {
        let Ok(mut child) = Command::new(prog).args(*args).stdin(Stdio::piped()).spawn() else {
            continue; // not installed — try the next
        };
        // The pipe must be dropped (closed) before waiting, or the helper
        // sits on a stdin that never reaches EOF and `wait` deadlocks.
        let wrote = match child.stdin.take() {
            Some(mut stdin) => stdin.write_all(text.as_bytes()).is_ok(),
            None => false,
        };
        // Reap either way so we don't leak a zombie.
        let exited_ok = matches!(child.wait(), Ok(status) if status.success());
        if wrote && exited_ok {
            return true;
        }
    }
    false
}

/// Write an OSC 52 clipboard-set sequence to stdout. Modern terminals
/// (iTerm2 w/ setting, kitty, wezterm, alacritty, tmux) copy it to the system
/// clipboard; terminals that don't support it ignore the sequence. Returns
/// whether the bytes made it out of this process — that is the *most* this
/// channel can ever know (there is no acknowledgement to wait for), which is
/// why its flash says "(OSC 52)" rather than an unqualified "copied".
#[cfg(not(test))]
fn emit_osc52(text: &str) -> bool {
    let seq = format!("\x1b]52;c;{}\x07", base64(text.as_bytes()));
    let mut out = std::io::stdout();
    out.write_all(seq.as_bytes()).and_then(|()| out.flush()).is_ok()
}

/// Minimal standard base64 (no external dep).
pub fn base64(input: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (b[0] as u32) << 16 | (b[1] as u32) << 8 | b[2] as u32;
        out.push(T[(n >> 18 & 63) as usize] as char);
        out.push(T[(n >> 12 & 63) as usize] as char);
        out.push(if chunk.len() > 1 { T[(n >> 6 & 63) as usize] as char } else { '=' });
        out.push(if chunk.len() > 2 { T[(n & 63) as usize] as char } else { '=' });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{base64, copy};
    use crate::ports::ClipboardOutcome;

    #[test]
    fn base64_matches_known_vectors() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"hello"), "aGVsbG8=");
    }

    /// B2 round 2 (PR #46 review): the runtime hatch the *real* binary
    /// needs (this crate's own test build can't stand in for it — that's
    /// the whole finding) is proven here instead, since `copy`'s
    /// `#[cfg(test)]` twin shares the identical `host_io_disabled()` guard
    /// as the `#[cfg(not(test))]` one. `remove_var` first/last: this is
    /// process-global state, so the window it's set is kept as short as
    /// the assertion needs.
    #[test]
    fn copy_no_ops_when_the_runtime_hatch_is_set() {
        assert_eq!(copy("unaffected"), ClipboardOutcome::Native, "off by default");
        std::env::set_var("ROOST_TEST_NO_HOST_IO", "1");
        assert_eq!(copy("must not land anywhere"), ClipboardOutcome::Failed);
        std::env::set_var("ROOST_TEST_NO_HOST_IO", "0"); // the live_qa.rs override form
        assert_eq!(copy("0 means back on"), ClipboardOutcome::Native);
        std::env::remove_var("ROOST_TEST_NO_HOST_IO");
    }
}
