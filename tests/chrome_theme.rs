//! The chrome inherits the terminal's theme, end to end (DESIGN-ui.md §2,
//! amended 2026-07-27).
//!
//! The unit gates in `src/ui/theme.rs` and `src/ui/render.rs` pin what roost
//! *builds*; this pins what roost actually *emits*. The harness parses the
//! real SGR stream with vt100, so every assertion below is on the attributes
//! a terminal would receive — which is where the reported bug lived: the
//! active tab's label was drawn near-white on `Color::Reset`, i.e. invisible
//! the moment `Color::Reset` was a light background.
//!
//! The three rules being checked, in emitted-SGR terms:
//! * text is **SGR 39** (default fg) — plus **SGR 2** for the quiet rung —
//!   so it is the same ink the user's shell prompt uses;
//! * structure is **SGR 90** (ANSI 8) and appears only on rules/borders;
//! * attention surfaces are **SGR 7** (reverse), never a colour fill, and no
//!   chrome row sets a background at all.

// The shared harness is compiled per test binary; helpers other tenants use
// are dead code from this binary's view — not real rot.
#[allow(dead_code)]
mod harness;

use std::time::Duration;

use harness::{Harness, COLS, ROWS};

/// ANSI 1 — the user's red, whatever hue their theme gives it.
const RED: vt100::Color = vt100::Color::Idx(1);
/// ANSI 8 — the structure slot: borders, separators, rules. Never text.
const STRUCTURE: vt100::Color = vt100::Color::Idx(8);

/// Two tabs (so the bar has an active label, an inactive one and separators)
/// and two panes side by side (so one border is focused and one is not).
fn fixture_workspace(cwd: &str) -> String {
    serde_json::json!({
        "version": 1,
        "active_tab": 0,
        "tabs": [
            {
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
            },
            {
                "name": "api",
                "layout": { "pane": 3 },
                "panes": { "3": {"adapter": "shell", "cwd": cwd} }
            }
        ]
    })
    .to_string()
}

/// Spawn and wait for a real first frame. An empty screen "settles"
/// instantly, so waiting on a token roost itself draws is what actually
/// proves the chrome is up before anything gets measured.
fn spawn(what: &str) -> Option<Harness> {
    let cwd = std::env::temp_dir();
    let cwd = cwd.to_str().expect("temp dir is valid utf8");
    let mut h = match Harness::try_spawn(&fixture_workspace(cwd)) {
        Ok(h) => h,
        Err(reason) => {
            eprintln!("SKIP {what}: {reason}");
            return None;
        }
    };
    assert!(
        h.wait_for(Duration::from_secs(10), |s| s.contents().contains("1 main")).is_some(),
        "{what}: the tab bar never appeared",
    );
    assert!(h.settle(Duration::from_secs(5)), "{what}: the first frame never settled");
    Some(h)
}

/// One screen row, one `String` per **column**. Deliberately not a plain
/// `String`: chrome is full of multi-byte single-width glyphs (`▎ │ · ✕ …`),
/// so a byte offset from `str::find` is not a column and using one lands the
/// attribute probes a few cells to the left of what they name.
fn row_cols(screen: &vt100::Screen, row: u16) -> Vec<String> {
    (0..COLS)
        .map(|c| screen.cell(row, c).map(|x| x.contents()).unwrap_or_default())
        .map(|s| if s.is_empty() { " ".to_string() } else { s })
        .collect()
}

/// The column `needle` starts at on a `row_cols` row.
fn col_of(cols: &[String], needle: &str) -> Option<u16> {
    let want: Vec<String> = needle.chars().map(|c| c.to_string()).collect();
    cols.windows(want.len()).position(|w| w == want.as_slice()).map(|i| i as u16)
}

/// Every column holding `glyph` on a `row_cols` row.
fn cols_of(cols: &[String], glyph: char) -> Vec<u16> {
    cols.iter()
        .enumerate()
        .filter(|(_, s)| s.starts_with(glyph))
        .map(|(i, _)| i as u16)
        .collect()
}

/// The row rendered as text, for failure messages only.
fn row_text(cols: &[String]) -> String {
    cols.concat()
}

/// `(fg, dim, inverse, bg)` for one cell — the whole vocabulary chrome is
/// allowed to speak in.
fn attrs(screen: &vt100::Screen, row: u16, col: u16) -> (vt100::Color, bool, bool, vt100::Color) {
    let cell = screen.cell(row, col).expect("cell inside the grid");
    (cell.fgcolor(), cell.dim(), cell.inverse(), cell.bgcolor())
}

/// Assert every cell of `row` leaves the terminal's own background showing:
/// §2's background policy, which is what deleted `TAB_STRIP` and `BAR`.
fn assert_no_fill(screen: &vt100::Screen, row: u16, what: &str) {
    for c in 0..COLS {
        let (_, _, inverse, bg) = attrs(screen, row, c);
        assert_eq!(
            bg,
            vt100::Color::Default,
            "{what}: col {c} paints a background fill (inverse={inverse})",
        );
    }
}

#[test]
fn the_tab_bar_and_hint_bar_are_ink_on_the_users_own_paper() {
    let Some(mut h) = spawn("chrome theme gate") else { return };
    let screen = h.screen();

    // -- the two bars carry no fill (the reported bug's other half: on a
    // light theme those bands rendered as near-black stripes) ------------
    assert_no_fill(screen, 0, "tab bar");
    assert_no_fill(screen, ROWS - 1, "hint bar");

    // -- the active tab's label is the terminal's own foreground ---------
    // This is the reported defect, exactly: it used to be a near-white
    // truecolor ink on `Color::Reset`, so a light background swallowed it.
    let bar = row_cols(screen, 0);
    let active = col_of(&bar, "1 main").expect("the active tab's label is on the bar");
    for i in 0.."1 main".chars().count() as u16 {
        let (fg, dim, ..) = attrs(screen, 0, active + i);
        assert_eq!(fg, vt100::Color::Default, "active tab label col {i} is not the user's ink");
        assert!(!dim, "the active tab's label is the full-strength rung");
    }

    // ...and the inactive tab is the same ink, one rung quieter — the
    // distinction is weight plus the `▎` marker, never a highlight fill.
    let inactive = col_of(&bar, "2 api").expect("the inactive tab's label is on the bar");
    for i in 0.."2 api".chars().count() as u16 {
        let (fg, dim, ..) = attrs(screen, 0, inactive + i);
        assert_eq!(fg, vt100::Color::Default, "inactive tab label col {i} is not the user's ink");
        assert!(dim, "the inactive tab's label is the quiet rung");
    }

    // -- the marker is the user's red, the separators are structure ------
    let markers = cols_of(&bar, '▎');
    assert_eq!(markers.len(), 1, "exactly one tab is active: {:?}", row_text(&bar));
    assert_eq!(attrs(screen, 0, markers[0]).0, RED, "the active-tab marker is the one red");

    let separators = cols_of(&bar, '│');
    assert_eq!(separators.len(), 2, "one separator per tab: {:?}", row_text(&bar));
    for c in separators {
        assert_eq!(attrs(screen, 0, c).0, STRUCTURE, "tab separator at col {c}");
    }

    // -- the hint bar: accent keys, quiet labels -------------------------
    let hints = row_cols(screen, ROWS - 1);
    let key = col_of(&hints, "Alt+?").expect("the help pair is on the bar");
    for i in 0.."Alt+?".chars().count() as u16 {
        let (fg, ..) = attrs(screen, ROWS - 1, key + i);
        assert_eq!(fg, RED, "hint key col {i} is not the one red");
    }
    let label = col_of(&hints, "keys").expect("the help pair's label");
    for i in 0.."keys".len() as u16 {
        let (fg, dim, ..) = attrs(screen, ROWS - 1, label + i);
        assert_eq!(fg, vt100::Color::Default, "hint label col {i} is not the user's ink");
        assert!(dim, "hint labels are the quiet rung");
    }

    assert!(h.quit_and_wait(Duration::from_secs(5)).is_some(), "roost did not exit cleanly");
}

#[test]
fn borders_are_structure_and_the_badge_is_quiet_ink() {
    let Some(mut h) = spawn("chrome border/badge gate") else { return };
    let screen = h.screen();

    // Row 1 is the top border row of both tiled panes: the focused pane's
    // frame is the one red (C3), the unfocused one is the structure slot —
    // the single place a theme is allowed to make roost faint, and losing it
    // costs a hairline rather than a word.
    let border_row = row_cols(screen, 1);
    let corners = cols_of(&border_row, '┌');
    assert_eq!(corners.len(), 2, "two tiled panes ⇒ two corners: {:?}", row_text(&border_row));
    assert_eq!(attrs(screen, 1, corners[0]).0, RED, "the focused pane's border");
    assert_eq!(attrs(screen, 1, corners[1]).0, STRUCTURE, "the unfocused pane's border");

    // C4's corner badge rides the focused pane's first inner row, hard right
    // against its inner edge. It is **two-tone**: the text is the quiet rung
    // — readable as a watermark in any theme, because it is the user's own
    // ink dimmed — and the status glyph rightmost of it carries its own C5
    // style, which is only sometimes dim (`·` idle, `✕` exited; `○` waiting
    // is full-strength `ink()` per §2, and `●`/`◆` are the accent). So the
    // scan has to step over the glyph by *position* before it starts reading
    // weight, or a waiting pane breaks the walk on its own badge.
    let badge_row = 2;
    let inner_right = corners[1] - 2; // last inner column of the focused pane
    let cell = |c: u16| screen.cell(badge_row, c).expect("cell inside the grid").contents();

    // Rightmost non-blank cell on the row: the breathing-room column comes
    // after the glyph, so this is the glyph itself.
    let glyph_col = (0..=inner_right)
        .rev()
        .find(|c| !cell(*c).trim().is_empty())
        .expect("the badge's status glyph");
    assert!(
        "●◆○·✕".contains(&cell(glyph_col)),
        "badge col {glyph_col} should be a C5 status glyph, found {:?}",
        cell(glyph_col),
    );

    // Everything left of the glyph is the quiet run, up to the point the walk
    // leaves the badge and lands in the pane's own content.
    let mut quiet_cells = 0;
    for c in (0..glyph_col).rev() {
        let (fg, dim, ..) = attrs(screen, badge_row, c);
        if cell(c).trim().is_empty() && quiet_cells == 0 {
            continue; // the space separating the glyph from the text
        }
        if !dim {
            break; // ran off the front of the badge into pane content
        }
        assert_eq!(fg, vt100::Color::Default, "badge col {c} is not the user's ink");
        quiet_cells += 1;
    }
    assert!(quiet_cells >= 3, "the corner badge should be a run of quiet ink, found {quiet_cells}");

    assert!(h.quit_and_wait(Duration::from_secs(5)).is_some(), "roost did not exit cleanly");
}

#[test]
fn the_flash_reverses_the_terminals_own_pair_instead_of_filling() {
    let Some(mut h) = spawn("chrome flash gate") else { return };

    // Alt+u with an empty undo stack is the cheapest honest flash there is.
    // It also carries Alt, which retires the C11 alt-trap bar — the one
    // surface that outranks a flash on this row.
    h.write_bytes(b"\x1bu");
    let found = h.wait_for(Duration::from_secs(5), |s| {
        (0..COLS).map(|c| s.cell(ROWS - 1, c).map(|x| x.contents()).unwrap_or_default()).collect::<String>()
            .contains("nothing to reopen")
    });
    assert!(found.is_some(), "the flash never reached the bar");

    let screen = h.screen();
    // No fill: the attention surface reverses the user's own fg/bg, which is
    // guaranteed contrasty in a light, dark or tinted theme alike.
    assert_no_fill(screen, ROWS - 1, "flash row");
    let bar = row_cols(screen, ROWS - 1);
    let at = col_of(&bar, "nothing to reopen").expect("the flash text");
    for i in 0.."nothing to reopen".len() as u16 {
        let (fg, _, inverse, _) = attrs(screen, ROWS - 1, at + i);
        assert!(inverse, "flash col {i} must be REVERSED, not filled");
        assert_eq!(fg, vt100::Color::Default, "flash col {i} must reverse the user's own ink");
    }

    assert!(h.quit_and_wait(Duration::from_secs(5)).is_some(), "roost did not exit cleanly");
}
