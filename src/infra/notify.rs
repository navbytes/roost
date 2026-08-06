//! Production `Notifier`: terminal bell everywhere, plus a native
//! notification on macOS.

use crate::ports::Notifier;

#[derive(Default)]
pub struct TermNotifier;

impl Notifier for TermNotifier {
    fn notify(&mut self, msg: &str) {
        use std::io::Write;
        let mut out = std::io::stdout();
        let _ = out.write_all(b"\x07");
        let _ = out.flush();
        #[cfg(target_os = "macos")]
        {
            // Info-b: `msg` is pane-derived (an agent's own notification
            // text) and must never become part of the AppleScript source —
            // escaping only `"` and `\` was quoting-by-luck, safe today only
            // because vte happens to drop C0 bytes from OSC payloads
            // upstream. Passed as an `on run` argument instead, `msg` is
            // never parsed as script text at all, so there's nothing to
            // escape and nothing to break out of.
            let script = "on run {b}\n  display notification b with title \"roost\"\nend run";
            if let Ok(child) =
                std::process::Command::new("osascript").arg("-e").arg(script).arg(msg).spawn()
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
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    /// Info-b: proves the property the fix relies on — a payload built to
    /// break out of a double-quoted AppleScript string literal (`" & (do
    /// shell script ...) & "`) must come back byte-for-byte instead of
    /// being interpreted, when passed as an `on run` argument the same way
    /// `notify` passes it. `return b` stands in for `display notification b
    /// with title "roost"` so the proof is on stdout, not on whether a
    /// notification actually posted (permission-dependent, irrelevant here).
    #[test]
    fn hostile_body_cannot_break_out_of_the_applescript_source() {
        let hostile = "safe\" & (do shell script \"echo pwned\") & \"tail";
        let out = std::process::Command::new("osascript")
            .arg("-e")
            .arg("on run {b}\n  return b\nend run")
            .arg(hostile)
            .output()
            .expect("osascript ships with macOS");
        assert!(out.status.success());
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim_end(), hostile);
    }
}
