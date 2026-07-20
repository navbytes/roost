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
            let script = format!(
                "display notification \"{}\" with title \"roost\"",
                msg.replace('\\', "").replace('"', "'")
            );
            let _ = std::process::Command::new("osascript").arg("-e").arg(script).spawn();
        }
        #[cfg(not(target_os = "macos"))]
        let _ = msg;
    }
}
