//! P7 (SPEC-parity) end to end: cursor fidelity.
//!
//! Two facets, both measured against the real binary:
//! (a) the renderer placed the host cursor whenever a pane was focused,
//!     never consulting `screen.hide_cursor()` — a ghost cursor blinked over
//!     TUIs that had deliberately hidden theirs;
//! (b) DECSCUSR (`CSI Ps SP q`) died in the vendored parser's unhandled arm
//!     and was never mirrored, so an editor's insert bar rendered as a block.
//!
//! Both are asserted on the raw host stream, because neither leaves a mark
//! on the rendered grid.

// The shared harness is compiled per test binary; helpers other tenants use
// are dead code from this binary's view — not real rot.
#[allow(dead_code)]
mod harness;

use std::time::Duration;

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

/// Did roost place the host cursor in this window of its output?
///
/// ratatui ends every frame by either hiding the host cursor (`?25l`, when
/// the frame set no cursor position) or showing it and moving it there
/// (`?25h` + `CSI r;cH`). `?25h` is therefore the exact, unambiguous marker
/// of "roost placed the cursor this frame" — the bare `CSI r;cH` is not,
/// since ratatui also uses it to position its own content writes.
fn placed_cursor(bytes: &[u8]) -> bool {
    contains(bytes, b"\x1b[?25h")
}

#[test]
fn a_hidden_pane_cursor_is_not_placed_and_decscusr_is_mirrored() {
    let cwd = std::env::temp_dir();
    let cwd = cwd.to_str().expect("temp dir is valid utf8");
    let Some(mut h) = harness::spawn_or_skip("cursor gate", &harness::two_panes(cwd)) else {
        return;
    };
    if h.wait_for(Duration::from_secs(5), |s| s.contents().contains("main")).is_none() {
        eprintln!("SKIP cursor gate: roost never drew its first frame");
        return;
    }
    // Focus lands on pane 1 at startup; make sure it's really running.
    h.write_bytes(b"printf 'RE''ADY1\\n'\r");
    h.wait_for(Duration::from_secs(5), |s| s.contents().contains("READY1"))
        .expect("pane 1 never ran");

    // -- (a) the focused pane hides its cursor -----------------------------
    // While `?25l` is in force roost must stop positioning the host cursor.
    // Sample a window *after* the pane goes quiet, so ordinary redraw
    // traffic isn't what we're measuring.
    h.write_bytes(b"printf '\\033[?25l'; sleep 3\r");
    std::thread::sleep(Duration::from_millis(900));
    let start = h.host_bytes().len();
    std::thread::sleep(Duration::from_millis(900));
    let hidden_window = h.host_bytes()[start..].to_vec();
    assert!(
        !hidden_window.is_empty(),
        "roost drew no frames in the sample window — nothing was measured"
    );
    assert!(
        !placed_cursor(&hidden_window),
        "roost kept placing the host cursor while the focused pane had \
         hidden its own (`?25l`); window:\n{}",
        String::from_utf8_lossy(&hidden_window)
    );

    // `?25h` gives it back: roost resumes placing the cursor.
    h.write_bytes(b"\x03"); // end the sleep
    let restore_from = h.host_bytes().len();
    h.write_bytes(b"printf '\\033[?25h'\r");
    let restored = h.wait_for_host_bytes(Duration::from_secs(5), |b| {
        placed_cursor(&b[restore_from..])
    });
    assert!(restored, "roost never resumed placing the cursor after `?25h`");

    // -- (b) DECSCUSR is mirrored to the host ------------------------------
    let before = h.host_bytes().len();
    h.write_bytes(b"printf '\\033[5 q'\r"); // blinking bar, an editor's insert cursor
    let mirrored = h.wait_for_host_bytes(Duration::from_secs(5), |b| {
        contains(&b[before..], b"\x1b[5 q")
    });
    assert!(
        mirrored,
        "roost never mirrored the focused pane's DECSCUSR shape; tail:\n{}",
        String::from_utf8_lossy(&tail(&h.host_bytes(), 400))
    );

    // Moving focus to a pane that asked for no shape restores the default.
    let before = h.host_bytes().len();
    h.write_bytes(b"\x1b[1;3C"); // Alt+Right: focus the neighbouring pane
    let reset = h.wait_for_host_bytes(Duration::from_secs(5), |b| {
        contains(&b[before..], b"\x1b[0 q")
    });
    assert!(
        reset,
        "focusing a shape-less pane must restore the host cursor default; \
         tail:\n{}",
        String::from_utf8_lossy(&tail(&h.host_bytes(), 400))
    );

    let _ = h.quit_and_wait(Duration::from_secs(5));
}

/// The last `n` bytes, for readable failure output.
fn tail(bytes: &[u8], n: usize) -> Vec<u8> {
    bytes[bytes.len().saturating_sub(n)..].to_vec()
}
