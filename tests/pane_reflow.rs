//! SPEC-parity P5 end to end: a zoom round-trip must not destroy the grid.
//!
//! Zoom deliberately resizes the focused pane's PTY to the full body and back
//! (C21), so a pane that prints at the zoomed width is later asked to live at
//! the tiled one. The vendored grid used to resize its rows *in place* —
//! hard-truncating every column past the new width and clearing the wrap
//! flags — so the tail of anything printed while zoomed was gone, not hidden:
//! still gone after re-zooming, absent from `roost read`, unrecoverable.
//!
//! The gate drives exactly that shape at exactly P5's measured geometry: two
//! panes side by side in the 120-col harness (58 inner columns each), zoom to
//! 118, print a 110-column line, unzoom. The line has to wrap — and keep every
//! column.
//!
//! Both markers are spelled split (`Q7''TAIL`) so the shell's echo of the
//! command line can never satisfy the assertion; only printf's real output,
//! which is the thing that round-trips through the resize, can.

// The shared harness is compiled per test binary; helpers other tenants use
// are dead code from this binary's view — not real rot.
#[allow(dead_code)]
mod harness;

use std::process::Command;
use std::time::Duration;

use harness::Harness;

/// Two shell panes side by side: `vertical` splits along a vertical line, so
/// each pane is half the harness's 120 columns (58 inner) and zooming the
/// focused one takes it to 118 — P5's measured pair of widths.
fn fixture_workspace(cwd: &str) -> String {
    serde_json::json!({
        "version": 1,
        "active_tab": 0,
        "tabs": [{
            "name": "main",
            "layout": {
                "split": {
                    "dir": "vertical",
                    "ratios": [0.5, 0.5],
                    "children": [{ "pane": 1 }, { "pane": 2 }]
                }
            },
            "panes": {
                "1": {"adapter": "shell", "cwd": cwd},
                "2": {"adapter": "shell", "cwd": cwd}
            }
        }]
    })
    .to_string()
}

/// `roost read 1 --tail N` against the harness instance, via the real client
/// CLI (ROOST_STATE routes it to the instance's socket + fleet control token)
/// — the pane's whole recorded output, history included, which is where a
/// destroyed column stays destroyed.
fn cli_read_tail(state_dir: &std::path::Path) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_roost"))
        .args(["read", "1", "--tail", "80"])
        .env("ROOST_STATE", state_dir)
        .env_remove("ROOST_SOCK")
        .env_remove("ROOST_TOKEN")
        .env_remove("ROOST_CONTROL_TOKEN")
        .output()
        .expect("run roost read");
    String::from_utf8_lossy(&out.stdout).to_string()
}

const ALT_Z: &[u8] = b"\x1bz";

#[test]
fn a_zoom_round_trip_keeps_every_column_of_a_line_printed_while_zoomed() {
    let cwd = std::env::temp_dir();
    let cwd = cwd.to_str().expect("temp dir is valid utf8");
    let mut h = match Harness::try_spawn(&fixture_workspace(cwd)) {
        Ok(h) => h,
        Err(reason) => {
            eprintln!("SKIP reflow gate: {reason}");
            return;
        }
    };
    assert!(h.settle(Duration::from_secs(5)), "initial frame never settled");

    // Zoom the focused (left) pane: its PTY goes from 58 to 118 columns.
    h.write_bytes(ALT_Z);
    assert!(
        h.wait_for(Duration::from_secs(5), |s| s.contents().contains("ZOOM")).is_some(),
        "Alt+z never zoomed:\n{}",
        h.screen().contents()
    );

    // 110 columns: 6 + 98 + 6. Fits the zoomed width, must wrap at the tiled
    // one — and the tail is what P5 measured being destroyed.
    h.write_bytes(b"printf 'Q7''HEAD%098dQ7''TAIL\\n' 0\r");
    let mut zoomed = String::new();
    let printed = (0..100).any(|_| {
        zoomed = cli_read_tail(h.state_dir());
        if zoomed.contains("Q7TAIL") {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
        false
    });
    assert!(printed, "the pane never printed the marker line while zoomed:\n{zoomed}");
    assert!(zoomed.contains("Q7HEAD"), "the zoomed read is missing the line's head:\n{zoomed}");

    // Unzoom: back to 58 columns. The line has to rewrap, not truncate.
    h.write_bytes(ALT_Z);
    assert!(
        h.wait_for(Duration::from_secs(5), |s| !s.contents().contains("ZOOM")).is_some(),
        "Alt+z never unzoomed:\n{}",
        h.screen().contents()
    );
    assert!(h.settle(Duration::from_secs(5)), "the unzoomed frame never settled");

    let tiled = cli_read_tail(h.state_dir());
    assert!(
        tiled.contains("Q7HEAD"),
        "the head of the marker line vanished on unzoom:\n{tiled}"
    );
    assert!(
        tiled.contains("Q7TAIL"),
        "P5: unzooming truncated the pane's grid — the marker's tail (printed \
         at column 105 of 118 while zoomed) is gone from `roost read --tail` \
         at 58 columns, not merely off screen. The live grid must rewrap its \
         logical lines on a width change.\n{tiled}"
    );

    // Re-zoom: widening rejoins the pieces, so the whole line is one row
    // again — the other direction of the same rewrap.
    h.write_bytes(ALT_Z);
    assert!(
        h.wait_for(Duration::from_secs(5), |s| s.contents().contains("ZOOM")).is_some(),
        "Alt+z never re-zoomed:\n{}",
        h.screen().contents()
    );
    assert!(h.settle(Duration::from_secs(5)), "the re-zoomed frame never settled");
    let rezoomed = cli_read_tail(h.state_dir());
    assert!(
        rezoomed.lines().any(|l| l.contains("Q7HEAD") && l.contains("Q7TAIL")),
        "widening must rejoin the wrapped halves onto one logical line:\n{rezoomed}"
    );

    let _ = h.quit_and_wait(Duration::from_secs(5));
}
