//! "A pane needs you" side-channel: terminal bell everywhere, plus a native
//! notification on macOS.

pub fn notify(msg: &str) {
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
