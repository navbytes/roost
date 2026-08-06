//! Open a URL in the user's default browser (Alt+click on a link).

#[cfg(not(test))]
use std::process::{Command, Stdio};

/// Same B2-class fix as `infra::clipboard::copy` (PR #46 code review): a
/// test driving `handle_mouse`'s Alt+click-URL path for real (see
/// `main.rs`'s `alt_click_opening_a_url_clears_a_different_panes_selection`)
/// would otherwise spawn the operator's actual browser on every
/// `cargo test` run. `#[cfg(test)]` swaps in a no-op.
#[cfg(not(test))]
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

#[cfg(test)]
pub fn open_url(_url: &str) {}
