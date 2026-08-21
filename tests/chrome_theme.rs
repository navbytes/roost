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

/// `theme::SPINNER_FRAMES` (C5), verbatim — kept as a literal here since this
/// binary has no library target for an integration test to import the const
/// from. The Working glyph is always one of these ten, never `●`.
const SPINNER_FRAMES: &str = "⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏";

/// Alt+c: `Action::CopyMode`, ESC + `c` (not an escape introducer, so no
/// DCS-style ambiguity — same family as `ALT_SHIFT_A` below).
const ALT_C: &[u8] = b"\x1bc";
/// Alt+Shift+a: `Action::ToggleRoster`, as a terminal delivers it without
/// the kitty disambiguation (ESC + uppercase `A` — see `roster_overlay.rs`).
const ALT_SHIFT_A: &[u8] = b"\x1bA";

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
    let mut h = harness::spawn_or_skip(what, &fixture_workspace(cwd))?;
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
    cols.iter().enumerate().filter(|(_, s)| s.starts_with(glyph)).map(|(i, _)| i as u16).collect()
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

    // C4 (amended 2026-08-21): identity rides the **top border**, hard left
    // against the corner — it used to be a badge painted over the pane's own
    // first content row. It is **two-tone**: the text is the quiet rung —
    // readable as a watermark in any theme, because it is the user's own ink
    // dimmed — and the status glyph rightmost of it carries its own C5
    // style, which is only sometimes dim (`·` idle, `✕` exited; `○` waiting
    // is full-strength `ink()` per §2, and the Working spinner/`◆` are the
    // accent). So the scan has to step over the glyph by *position* before it
    // starts reading weight, or a waiting pane breaks the walk on its own
    // title.
    let cell = |c: u16| screen.cell(1, c).expect("cell inside the grid").contents();

    // The title runs from just past the corner to wherever the border's own
    // rule resumes.
    let start = corners[0] + 1;
    let title_end = (start..corners[1])
        .find(|c| cell(*c) == "─")
        .expect("the border's rule resumes after the title");
    let glyph_col = (start..title_end)
        .rev()
        .find(|c| !cell(*c).trim().is_empty())
        .expect("the identity title's status glyph");
    let glyph = cell(glyph_col);
    assert!(
        "◆○·✕".contains(&glyph) || SPINNER_FRAMES.contains(&glyph),
        "title col {glyph_col} should be a C5 status glyph, found {glyph:?}",
    );

    // Everything left of the glyph, back to the leading breathing-room
    // column, is the quiet run.
    let mut quiet_cells = 0;
    for c in (start..glyph_col).rev() {
        if cell(c).trim().is_empty() && quiet_cells == 0 {
            continue; // the space separating the glyph from the text
        }
        let (fg, dim, ..) = attrs(screen, 1, c);
        if !dim {
            break; // ran off the front of the title onto the border's own rule
        }
        assert_eq!(fg, vt100::Color::Default, "title col {c} is not the user's ink");
        quiet_cells += 1;
    }
    assert!(
        quiet_cells >= 3,
        "the identity title should be a run of quiet ink, found {quiet_cells}"
    );

    // ...and the row it used to occupy belongs to the pane again. The
    // fixture's panes are shells at a prompt, so their first inner row is
    // either their own output or blank — never roost's chrome.
    let first_inner: String = (corners[0] + 1..corners[1] - 1)
        .filter_map(|c| screen.cell(2, c).map(|x| x.contents()))
        .collect();
    assert!(
        !first_inner.contains('·') && !first_inner.chars().any(|g| SPINNER_FRAMES.contains(g)),
        "no chrome left on the pane's first content row: {first_inner:?}"
    );

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
        (0..COLS)
            .map(|c| s.cell(ROWS - 1, c).map(|x| x.contents()).unwrap_or_default())
            .collect::<String>()
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

/// The first cell on screen whose contents is one of `SPINNER_FRAMES`, if
/// any — `(row, col, glyph)`. Any single match is enough: C5 requires every
/// Working glyph on screen to show the same frame in the same draw (one
/// shared clock), and this scenario's fixture has exactly one Working pane.
fn find_spinner_cell(screen: &vt100::Screen) -> Option<(u16, u16, String)> {
    (0..ROWS).find_map(|r| {
        (0..COLS).find_map(|c| {
            let s = screen.cell(r, c)?.contents();
            // `str::contains("")` is vacuously true, and a blank cell's
            // `contents()` is empty — guard it explicitly, or this matches
            // the first blank cell on screen instead of a real glyph.
            (!s.is_empty() && SPINNER_FRAMES.contains(&s)).then_some((r, c, s))
        })
    })
}

/// C5, end to end (amended 2026-08-07 — the colour pulse is retired): the
/// Working glyph is one of pi's ten braille spinner frames, never the
/// retired `●`, and is always the one plain red — no second hue, and never
/// DIM, now that the animation lives in the glyph's *shape* rather than a
/// colour flip.
///
/// This also proves the property the redesign exists to guarantee: a
/// Working pane that is completely **output-silent** still visibly animates,
/// because roost's own render loop drives the clock, not the pane. The
/// status hook line below types a command into the pane's shell — so there
/// is a brief burst of real PTY bytes (the echo, the instant-silent
/// subcommand, the prompt reappearing) — but nothing further is ever sent to
/// the pane after that; the repeated sampling below spans roughly one full
/// rotation (800ms nominal) with the pane sitting completely quiet
/// throughout, so a spinner driven by the pane's own output would never
/// move, while one driven by roost's own render tick (`main.rs`'s ~33ms
/// crossterm poll) does.
#[test]
fn the_working_spinner_is_the_one_red_braille_set_and_animates_while_the_pane_is_silent() {
    let Some(mut h) = spawn("chrome spinner gate") else { return };

    // A real status report over the real control socket — the same call a
    // Claude Code hook or the pi extension makes (status_hook.rs) — flips
    // the focused pane to Working deterministically. No output heuristic,
    // no sleep-and-hope.
    let bin = env!("CARGO_BIN_EXE_roost");
    h.write_bytes(format!("{bin} __status working\r").as_bytes());
    assert!(
        h.wait_for(Duration::from_secs(5), |s| find_spinner_cell(s).is_some()).is_some(),
        "no spinner frame ever appeared after the status hook",
    );

    // Sample several times over roughly one full rotation with nothing sent
    // to the pane in between — every sample must be a valid frame in the one
    // red, and at least two distinct frames must appear, or the glyph never
    // actually animated.
    let mut seen = std::collections::HashSet::new();
    for _ in 0..10 {
        let screen = h.screen();
        let (r, c, glyph) = find_spinner_cell(screen).expect("the spinner is still on screen");
        let (fg, dim, ..) = attrs(screen, r, c);
        assert_eq!(
            fg, RED,
            "spinner glyph at ({r},{c}) is {fg:?} — must be the one red, not a second hue"
        );
        assert!(!dim, "the spinner must never lean on DIM ({r},{c})");
        seen.insert(glyph);
        std::thread::sleep(Duration::from_millis(90));
    }
    assert!(!seen.contains("●"), "the retired dot must never appear: {seen:?}");
    assert!(
        seen.len() > 1,
        "the spinner never advanced while the pane sat silent — saw only {seen:?}",
    );

    assert!(h.quit_and_wait(Duration::from_secs(5)).is_some(), "roost did not exit cleanly");
}

/// C16, end to end: a real child exit (not a poked internal) must draw the
/// dead-pane bar reversed in the one red, across its **whole** row — the
/// historical D1 bug was styling the span instead of the widget, which
/// reversed only the message's own columns and left the dead program's last
/// output showing through the rest of the row at normal video. The buffer-
/// level `render::tests::the_dead_pane_bar_reverses_its_whole_row` pins the
/// same shape in-process; this is the same regression class at the layer
/// that actually reaches a user's terminal.
#[test]
fn the_dead_pane_bar_is_the_one_red_reversed_across_its_whole_row() {
    let Some(mut h) = spawn("chrome dead-pane gate") else { return };

    // A real hangup: the shell exits, the PTY closes, roost's own
    // `on_pty_exit` fires — nothing poked, just what happens when an agent
    // dies.
    h.write_bytes(b"exit\r");
    assert!(
        h.wait_for(Duration::from_secs(5), |s| s.contents().contains("exited — Enter")).is_some(),
        "the dead-pane bar never appeared",
    );

    let screen = h.screen();
    let row = (0..ROWS)
        .find(|&r| row_text(&row_cols(screen, r)).contains("exited — Enter"))
        .expect("the bar's row");
    let cols = row_cols(screen, row);

    let reversed: Vec<u16> = (0..COLS).filter(|&c| attrs(screen, row, c).2).collect();
    assert!(!reversed.is_empty(), "the bar is not reversed at all:\n{}", row_text(&cols));
    let (first, last) = (reversed[0], *reversed.last().unwrap());
    assert_eq!(
        reversed.len() as u16,
        last - first + 1,
        "the reversal is not contiguous across the row: {:?}",
        row_text(&cols),
    );
    let text_cols = (first..=last).filter(|&c| cols[c as usize] != " ").count();
    assert!(
        (last - first + 1) as usize > text_cols,
        "the bar covers only its own glyphs, not the row — the C16/D1 regression shape",
    );
    for c in first..=last {
        let (fg, _, inverse, _) = attrs(screen, row, c);
        assert!(inverse, "col {c} of the dead-pane bar should be reversed");
        assert_eq!(
            fg, RED,
            "col {c}: the problem bar is the one red, reversed (attention_problem)"
        );
    }
    // Same §2 background policy as every other attention surface: the
    // reversal is a modifier, not a fill.
    assert_no_fill(screen, row, "dead-pane bar");

    assert!(h.quit_and_wait(Duration::from_secs(5)).is_some(), "roost did not exit cleanly");
}

/// C17/C24/C29, end to end: a copy-mode selection reverses whatever is
/// already there and paints no colour of its own — "sits on top of
/// arbitrary program colours" is the whole point of the contract, so this
/// asserts the selected cells stay the terminal's own default fg/bg with
/// only REVERSED added, never a forced hue and never a fill. `V` (line
/// select) is used deliberately over `v` + motions: it is absolute, so the
/// scenario needs no clamping arithmetic to reproduce.
#[test]
fn a_lit_selection_reverses_without_forcing_any_colour() {
    let Some(mut h) = spawn("chrome selection gate") else { return };

    h.write_bytes(ALT_C);
    assert!(
        h.wait_for(Duration::from_secs(5), |s| s.contents().contains("COPY")).is_some(),
        "Alt+c never entered copy mode",
    );
    h.write_bytes(b"V");
    assert!(h.settle(Duration::from_secs(3)), "the selection frame never settled");

    let screen = h.screen();
    // Find the selected row: the one with a long contiguous reversed run
    // (the whole pane's inner width) rather than assuming exact geometry.
    let selected_row =
        (1..ROWS - 1).find(|&r| (0..COLS).filter(|&c| attrs(screen, r, c).2).count() >= 10);
    let Some(selected_row) = selected_row else {
        panic!("no fully-reversed selection row found:\n{}", screen.contents());
    };
    let mut checked = 0;
    for c in 0..COLS {
        let (fg, _, inverse, bg) = attrs(screen, selected_row, c);
        if !inverse {
            continue;
        }
        assert_eq!(bg, vt100::Color::Default, "col {c}: selection may not paint a fill (C17)");
        assert_eq!(
            fg,
            vt100::Color::Default,
            "col {c}: selection may not force a palette colour (C17)"
        );
        checked += 1;
    }
    assert!(checked >= 10, "expected a wide reversed run, only checked {checked} cells");

    assert!(h.quit_and_wait(Duration::from_secs(5)).is_some(), "roost did not exit cleanly");
}

/// One pane per tab, the second never spawned (lazy spawn only starts the
/// active tab) — so a status-filtered-away group has no live corner badge
/// anywhere on screen to bleed through the modal's dimmed backdrop and
/// falsely satisfy a substring check. Named distinctly from the shared
/// `fixture_workspace`'s tabs so an absence assertion can't collide with
/// either.
fn roster_fixture_workspace(cwd: &str) -> String {
    serde_json::json!({
        "version": 1,
        "active_tab": 0,
        "tabs": [
            {
                "name": "main",
                "layout": { "pane": 1 },
                "panes": { "1": {"adapter": "shell", "cwd": cwd} }
            },
            {
                "name": "side",
                "layout": { "pane": 2 },
                "panes": { "2": {"adapter": "shell", "cwd": cwd} }
            }
        ]
    })
    .to_string()
}

/// C27 (ux P2-11, amended 2026-08-07 — the same day as this harness work):
/// the roster's status-filter tag carries the tier's own C5 colour on the
/// glyph alone, everything else in the title staying plain ink
/// (design-supervisor D4: a bare `ink()` glyph would read as no filter at
/// all). This is the newest colour-relevant surface in the contract set,
/// and until now had zero coverage above the in-process buffer gates.
#[test]
fn the_roster_status_filter_tags_its_title_in_the_tiers_own_colour() {
    let cwd = std::env::temp_dir();
    let cwd = cwd.to_str().expect("temp dir is valid utf8");
    let Some(mut h) =
        harness::spawn_or_skip("chrome roster-filter gate", &roster_fixture_workspace(cwd))
    else {
        return;
    };
    assert!(
        h.wait_for(Duration::from_secs(10), |s| s.contents().contains("1 main")).is_some(),
        "the tab bar never appeared",
    );
    assert!(h.settle(Duration::from_secs(5)), "the first frame never settled");

    // Deterministically mark the focused (tab 1) pane NeedsInput, same
    // real-socket technique as the pulse gate above.
    let bin = env!("CARGO_BIN_EXE_roost");
    h.write_bytes(format!("{bin} __status needs_input\r").as_bytes());
    assert!(
        h.wait_for(Duration::from_secs(5), |s| s.contents().contains("◆")).is_some(),
        "the needs-input glyph never appeared after the status hook",
    );

    h.write_bytes(ALT_SHIFT_A);
    assert!(
        h.wait_for(Duration::from_secs(5), |s| s.contents().contains("fleet")).is_some(),
        "Alt+Shift+a never opened the roster",
    );
    // One `Tab`: `ROSTER_STATUS_CYCLE`'s first stop past "every tier" is
    // NeedsInput (worst-first order, ux P2-11).
    h.write_bytes(b"\t");
    assert!(
        h.wait_for(Duration::from_secs(5), |s| s.contents().contains("only")).is_some(),
        "Tab never tagged the roster title with a status filter",
    );
    assert!(h.settle(Duration::from_secs(2)), "the filtered roster never settled");

    let screen = h.screen();
    let title_row = (0..ROWS)
        .find(|&r| row_text(&row_cols(screen, r)).contains("fleet"))
        .expect("the roster dialog's titled border row");
    let cols = row_cols(screen, title_row);

    let glyph_col = col_of(&cols, "◆").expect("the NeedsInput tag glyph on the title");
    let (fg, dim, ..) = attrs(screen, title_row, glyph_col);
    assert_eq!(fg, RED, "the status-filter tag glyph must be the one red");
    assert!(!dim, "the tag glyph is full-strength accent, not the quiet rung");

    let word_col = col_of(&cols, "fleet").expect("the title word");
    let (fg, dim, ..) = attrs(screen, title_row, word_col);
    assert_eq!(fg, vt100::Color::Default, "the title text is the user's own ink");
    assert!(!dim, "the title word is full-strength ink, not the tag's colour");

    // The filter actually took effect: the "side" tab's only pane never
    // spawned (so it has no live badge anywhere to bleed through the
    // dimmed backdrop) and fails the NeedsInput tier, dropping its whole
    // group — including the header.
    let frame = screen.contents();
    assert!(frame.contains("1 shell"), "the matching pane must still be listed:\n{frame}");
    assert!(!frame.contains("2 SIDE"), "the non-matching tab's group must vanish whole:\n{frame}");

    assert!(h.quit_and_wait(Duration::from_secs(5)).is_some(), "roost did not exit cleanly");
}

/// C30, end to end, at the exact geometry the render.rs unit fixture uses
/// (80×1) — `draw()` pre-empts every other chrome surface below the 2-row
/// floor, so this is the one scenario that needs its own PTY size rather
/// than the shared 120×40. Deliberately its own `Harness::try_spawn_sized`
/// call instead of the shared `spawn()` helper, which waits on the tab bar
/// — a surface this state never draws.
#[test]
fn below_the_two_row_floor_the_notice_is_plain_ink_with_no_fill() {
    let cwd = std::env::temp_dir();
    let cwd = cwd.to_str().expect("temp dir is valid utf8");
    let Some(mut h) = harness::spawn_or_skip_sized(
        "chrome sub-two-row gate",
        &fixture_workspace(cwd),
        &[],
        1,
        80,
    ) else {
        return;
    };
    assert!(
        h.wait_for(Duration::from_secs(10), |s| s.contents().contains("too small")).is_some(),
        "the sub-two-row notice never appeared",
    );
    assert!(h.settle(Duration::from_secs(5)), "the notice frame never settled");

    let screen = h.screen();
    let mut found_text = false;
    for c in 0..80u16 {
        let cell = screen.cell(0, c).expect("cell inside the 80x1 grid");
        let (fg, dim, inverse, bg) = (cell.fgcolor(), cell.dim(), cell.inverse(), cell.bgcolor());
        assert_eq!(bg, vt100::Color::Default, "col {c}: the notice paints no fill");
        if !cell.contents().trim().is_empty() {
            found_text = true;
            assert_eq!(fg, vt100::Color::Default, "col {c}: the notice is the user's own ink");
            assert!(!dim, "col {c}: the notice is full-strength ink, not the quiet rung");
            assert!(!inverse, "col {c}: no attention treatment on a plain notice");
        }
    }
    assert!(found_text, "no visible text on the 80x1 screen:\n{:?}", screen.contents());
    assert!(
        screen.contents().contains("too small — resize"),
        "exact message: {:?}",
        screen.contents(),
    );

    assert!(h.quit_and_wait(Duration::from_secs(5)).is_some(), "roost did not exit cleanly");
}
