//! Open a URL in the user's default browser (Alt+click on a link).

use std::process::{Command, Stdio};

pub fn open_url(url: &str) {
    // `open` on macOS, `xdg-open` on Linux. Detach stdio so it can't disturb
    // the TUI, and reap in a thread so we don't leak a zombie.
    let prog = if cfg!(target_os = "macos") { "open" } else { "xdg-open" };
    if let Ok(child) = Command::new(prog)
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        std::thread::spawn(move || {
            let mut child = child;
            let _ = child.wait();
        });
    }
}
