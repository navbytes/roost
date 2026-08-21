//! C3/C4/C32 (amended 2026-08-21): a pane's own first content row is the
//! pane's.
//!
//! roost's identity label used to be a corner badge painted **over** that
//! row — the pane's most valuable one: a prompt, the first line of a diff,
//! the answer you just asked for. It took as much of it as it needed, and at
//! four panes across a 120-column terminal that was all of it, on every
//! pane. Identity now rides the top border and a parking note the bottom,
//! both of which were empty.
//!
//! Driven through the real binary because the claim is about what a *user*
//! sees: roost's chrome and the pane's program both write into the same
//! grid, and only the composed frame can say which won.

#[allow(dead_code)]
mod harness;

use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Four panes across, each one narrow enough that the old badge covered its
/// whole first row. Two carry parking notes; one is titled and one is not,
/// so both C4 name forms are on screen at once.
fn four_panes(cwd: &str, now: u64) -> String {
    let plain = |title: &str| serde_json::json!({"adapter": "shell", "cwd": cwd, "title": title});
    let noted = |title: &str, note: &str| {
        serde_json::json!({"adapter": "shell", "cwd": cwd, "title": title,
                           "note": note, "noted_at": now - 7200})
    };
    serde_json::json!({
        "version": 1, "active_tab": 0,
        "tabs": [{ "name": "main",
            "layout": {"split": {"dir": "vertical", "ratios": [0.25, 0.25, 0.25, 0.25],
                       "children": [{"pane": 1}, {"pane": 2}, {"pane": 3}, {"pane": 4}]}},
            "panes": {
                "1": noted("api-refactor", "waiting on the schema review"),
                "2": plain("flaky-test-hunt"),
                "3": noted("docs", "rewrite the intro\nand the examples"),
                "4": plain("scratch"),
            }}]
    })
    .to_string()
}

/// The marker roost draws for a parked note (C32) and the id-first join key
/// (U2) are the two things that used to land on the content row.
#[test]
fn roost_draws_no_chrome_on_a_panes_first_content_row() {
    let cwd = std::env::temp_dir();
    let cwd = cwd.to_str().expect("temp dir is valid utf8");
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
    let Some(mut h) = harness::spawn_or_skip("pane chrome placement", &four_panes(cwd, now)) else {
        return;
    };
    assert!(h.settle(Duration::from_secs(5)), "initial frame never settled");

    // Fill every pane completely: one unbroken run of `X` with no newline,
    // so every content row is solid from border to border. Anything else on
    // one is roost's, and a clipped row is roost overwriting the program.
    // Broadcast (C36) so all four get it without focus games.
    h.write_bytes(b"\x1b'"); // the broadcast composer
    assert!(
        h.wait_for(Duration::from_secs(5), |s| s.contents().contains("BROADCAST")).is_some(),
        "the broadcast composer never opened:\n{}",
        h.screen().contents()
    );
    h.write_bytes(b"printf 'X%.0s' $(seq 1 3000)");
    h.write_bytes(b"\r");
    // Wait for the fill to reach the row under the top border — the row this
    // gate is about — rather than for the output to merely start.
    assert!(
        h.wait_for(Duration::from_secs(20), |s| {
            let frame = s.contents();
            let lines: Vec<&str> = frame.lines().collect();
            lines
                .iter()
                .position(|l| l.contains('┌'))
                .and_then(|i| lines.get(i + 1))
                .is_some_and(|row| row.chars().all(|c| c == 'X' || c == '│'))
        })
        .is_some(),
        "the panes never filled their first content row:\n{}",
        h.screen().contents()
    );

    let screen = h.screen();
    let (rows, cols) = screen.size();
    let row_text = |r: u16| -> String {
        (0..cols).filter_map(|c| screen.cell(r, c).map(|x| x.contents())).collect()
    };

    // Find the frame rather than assume it: the panes' top border is the row
    // carrying `┌`, their bottom the row carrying `└`, and the first content
    // row the one between the top border and everything else.
    let find = |ch: char| {
        (0..rows).find(|r| row_text(*r).contains(ch)).unwrap_or_else(|| panic!("no {ch} row"))
    };
    let top = find('┌');
    let top_border = row_text(top);
    let first_content = row_text(top + 1);
    let bottom_border = row_text(find('└'));

    // Identity is on the border, and all four panes have one.
    for name in ["api-refactor", "flaky-test-hunt", "docs", "scratch"] {
        assert!(top_border.contains(name), "{name} is missing from the top border: {top_border:?}");
    }
    // The notes are on the *other* border, and only the two panes that have
    // one are marked (C32's reveal-on-visit: presence everywhere, content
    // only where focus is).
    assert_eq!(
        bottom_border.matches('¶').count(),
        2,
        "exactly the two noted panes are marked: {bottom_border:?}"
    );

    // ...and the row that used to be roost's is the panes'. Every cell of it
    // is either a pane border or the program's own `X` — no pane id, no
    // name, no `¶`, and nothing clipped away to make room for them.
    let stray: String = first_content.chars().filter(|c| *c != 'X' && *c != '│').collect();
    assert!(
        stray.is_empty(),
        "roost left {stray:?} on a pane's first content row: {first_content:?}"
    );

    assert!(h.quit_and_wait(Duration::from_secs(10)).is_some(), "roost did not exit cleanly");
}
