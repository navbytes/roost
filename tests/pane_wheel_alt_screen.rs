//! SPEC-parity P9 end to end: the wheel over an alternate-screen app.
//!
//! `less`/`man` never ask for mouse reporting, so roost routed the wheel to
//! its own scrollback for them — but an alternate-screen grid has scrollback
//! capacity 0, so the view could not move and the application was told
//! nothing. Measured byte-level: zero bytes reached the pane from six wheel
//! events, and `less`'s screen came back byte-identical. tmux #1333 → #4952
//! is this exact saga; the settled convention (DECSET 1007 / tmux's
//! `alternate-scroll`) is to translate each tick into arrow keys.
//!
//! The gate drives it the way a user does: `less` on a 400-line file, wheel
//! down over the pane, and the file has to move.

// The shared harness is compiled per test binary; helpers other tenants use
// are dead code from this binary's view — not real rot.
#[allow(dead_code)]
mod harness;

use std::time::Duration;

/// An SGR wheel-down over (col, row), 0-based — exactly what the host
/// terminal sends roost once it has enabled mouse capture.
fn sgr_wheel_down(col: u16, row: u16) -> Vec<u8> {
    format!("\x1b[<65;{};{}M", col + 1, row + 1).into_bytes()
}

#[test]
fn the_wheel_moves_an_alternate_screen_pager() {
    let path = std::env::temp_dir().join(format!("roost-p9-{}.txt", std::process::id()));
    let body: String = (1..=400).map(|i| format!("L{i:03}\n")).collect();
    std::fs::write(&path, body).expect("write the pager's file");

    let cwd = std::env::temp_dir();
    let cwd = cwd.to_str().expect("temp dir is valid utf8");
    let Some(mut h) = harness::spawn_or_skip_with_env(
        "alt-screen wheel gate",
        &harness::one_pane(cwd),
        &[("LESS", "")],
    ) else {
        let _ = std::fs::remove_file(&path);
        return;
    };
    assert!(h.settle(Duration::from_secs(5)), "initial frame never settled");

    h.write_bytes(format!("less {}\r", path.display()).as_bytes());
    let opened = h.wait_for(Duration::from_secs(10), |s| {
        let c = s.contents();
        c.contains("L001") && c.contains("L020")
    });
    if opened.is_none() {
        eprintln!("SKIP alt-screen wheel gate: `less` never opened the file");
        let _ = h.quit_and_wait(Duration::from_secs(5));
        let _ = std::fs::remove_file(&path);
        return;
    }
    assert!(
        !h.screen().contents().contains("L060"),
        "the pager must start at the top of the file:\n{}",
        h.screen().contents()
    );

    // Fifteen ticks: 45 lines at the three-lines-per-tick convention, so the
    // assertion holds with room to spare even if the terminal coalesces a
    // few of them.
    for _ in 0..15 {
        h.write_bytes(&sgr_wheel_down(40, 20));
        std::thread::sleep(Duration::from_millis(20));
    }

    let moved = h.wait_for(Duration::from_secs(5), |s| s.contents().contains("L060"));
    let screen = h.screen().contents();
    assert!(
        moved.is_some(),
        "P9: fifteen wheel-down events moved nothing. A pane on the alternate \
         screen with no mouse protocol has zero scrollback capacity, so the \
         wheel must reach the app as arrow keys instead of scrolling a buffer \
         that cannot move.\n{screen}"
    );
    assert!(!screen.contains("L001"), "the pager should have left the top:\n{screen}");

    h.write_bytes(b"q"); // leave less
    h.settle(Duration::from_secs(2));
    let _ = h.quit_and_wait(Duration::from_secs(5));
    let _ = std::fs::remove_file(&path);
}
