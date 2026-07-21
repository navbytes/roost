//! Mouse routing — pure decisions, no I/O, fully unit-tested.
//!
//! Two jobs:
//! 1. Wheel over a pane → forward to the inner app when it speaks SGR mouse
//!    reporting (pi/claude TUIs, vim, less…), else scroll roost's own
//!    scrollback for that pane. Without mouse capture the hosting terminal
//!    would scroll its *own* buffer — content outside the TUI.
//! 2. Clicks/drags over a mouse-aware pane are forwarded too, so you can
//!    actually interact with an agent's TUI (menus, buttons, selection).
//!    Over a plain app only the wheel does anything; roost keeps the click
//!    for focus.
//!
//! The tab bar row is handled separately (click to switch tabs).

use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;
use unicode_width::UnicodeWidthStr;

use crate::core::layout::PaneRect;
use crate::ports::MouseProto;
use crate::ui::theme;

/// Lines per wheel notch for roost-side scrolling (tmux uses 3).
pub const WHEEL_LINES: i32 = 3;

#[derive(Debug, PartialEq, Eq)]
pub enum MouseAction {
    /// Forward these bytes to the pane's PTY (mouse-aware app).
    Forward(Vec<u8>),
    /// Scroll roost's scrollback by this delta (positive = into history).
    Scroll(i32),
    /// Nothing to send to the pane (focus is handled by the caller).
    None,
}

/// Which pane is under (col, row)? Collapsed stack bars count too.
pub fn hit_test(rects: &[PaneRect], col: u16, row: u16) -> Option<PaneRect> {
    rects
        .iter()
        .find(|pr| {
            col >= pr.rect.x
                && col < pr.rect.x + pr.rect.width
                && row >= pr.rect.y
                && row < pr.rect.y + pr.rect.height
        })
        .copied()
}

/// The body of a tab's label: `N name` (no separator, no status glyph).
/// `tab_width` accounts for the marker/spacing/glyph/separator columns the
/// renderer draws around this so click hit-testing lines up with what's
/// drawn (C2).
pub fn tab_label(index: usize, name: &str) -> String {
    format!("{} {}", index + 1, name)
}

/// Terminal display width of `s` — the one measure every hitbox/clip
/// computation in this module, and `render.rs`'s `clip_spans`/`corner_badge`/
/// `collapsed_row_spans`, shares (D1). A renamed tab or pane can hold wide
/// glyphs (CJK, emoji — two terminal columns each); `.chars().count()`
/// undercounts those and desyncs mouse math from what's actually drawn.
pub fn display_width(s: &str) -> u16 {
    s.width() as u16
}

/// Total columns one tab occupies in the bar (C2): a 1-col marker, a space,
/// the label body, a space, a 1-col status glyph, a space, and the trailing
/// separator — six fixed columns plus the label.
pub fn tab_width(index: usize, name: &str) -> u16 {
    display_width(&tab_label(index, name)) + 6
}

/// Sum of `tab_width` over every tab.
pub fn total_tabs_width(names: &[String]) -> u16 {
    names.iter().enumerate().map(|(i, n)| tab_width(i, n)).sum()
}

/// The status area's width when shown, or 0 when it's dropped. C2 overflow
/// rule: tabs win — if not every tab fits alongside the status area, the
/// status area goes first, freeing its width back to the tabs.
/// `draw_tab_bar` and `tab_at_x` both derive their layout from this number
/// so they can't disagree about whether the status area is on screen.
pub fn effective_status_width(names: &[String], bar_width: u16, status_width: u16) -> u16 {
    if total_tabs_width(names).saturating_add(status_width) <= bar_width {
        status_width
    } else {
        0
    }
}

/// How many of `bar_width`'s columns, starting at 0, are occupied by fully
/// drawn tabs — i.e. where the renderer stops and (if anything didn't fit)
/// draws the single `…` clip marker. Everything from here to the right edge
/// (the ellipsis, the gap, the status area) belongs to no tab.
pub fn tabs_visible_width(names: &[String], bar_width: u16, status_width: u16) -> u16 {
    let budget = bar_width.saturating_sub(effective_status_width(names, bar_width, status_width));
    let mut used = 0u16;
    for (i, name) in names.iter().enumerate() {
        let w = tab_width(i, name);
        if used.saturating_add(w) > budget {
            break;
        }
        used += w;
    }
    used
}

/// Which tab (if any) sits at column `x` on the tab bar row (C2: tabs start
/// at `x = 0` — the brand block is gone). `bar_width`/`status_width` bound
/// this the same way the renderer clips overflow (`tabs_visible_width`), so
/// a click on the `…` marker, the gap, or the right-aligned status area
/// correctly switches nothing.
pub fn tab_at_x(names: &[String], bar_width: u16, status_width: u16, x: u16) -> Option<usize> {
    if x >= tabs_visible_width(names, bar_width, status_width) {
        return None;
    }
    let mut cur = 0u16;
    for (i, name) in names.iter().enumerate() {
        let w = tab_width(i, name);
        if x < cur + w {
            return Some(i);
        }
        cur += w;
    }
    None
}

/// The tab bar's right-aligned status text (C2): the focused pane's cwd
/// (already `~`-abbreviated by the caller, `App::focused_cwd`) and the save
/// indicator, split into `(prefix, save_word)` so the renderer can color
/// them independently. `prefix` is `"{cwd} · "`, or empty when there's no
/// cwd to show (the segment is omitted, not blanked).
pub fn status_parts(cwd: Option<&str>, save_ok: bool) -> (String, String) {
    let save_word = if save_ok {
        format!("saved {}", theme::SAVED)
    } else {
        format!("save failed {}", theme::GLYPH_EXITED)
    };
    let prefix = cwd.map(|c| format!("{c} · ")).unwrap_or_default();
    (prefix, save_word)
}

/// On-screen width of `status_parts`' output, including the C2 trailing
/// space — the column span `tab_at_x` treats as off-limits for tab clicks.
pub fn status_width(cwd: Option<&str>, save_ok: bool) -> u16 {
    let (prefix, save_word) = status_parts(cwd, save_ok);
    display_width(&prefix) + display_width(&save_word) + 1
}

/// Route a mouse event over a pane to either the inner app or roost's
/// scrollback. Focus (a roost concern) is decided by the caller.
pub fn route_mouse(proto: MouseProto, pane: &PaneRect, me: &MouseEvent) -> MouseAction {
    if pane.collapsed {
        // A collapsed stack bar has no scrollable content and no inner app.
        return MouseAction::None;
    }
    match proto {
        MouseProto::Sgr => match encode_sgr(pane.rect, me) {
            Some(bytes) => MouseAction::Forward(bytes),
            None => MouseAction::None,
        },
        MouseProto::None => match me.kind {
            MouseEventKind::ScrollUp => MouseAction::Scroll(WHEEL_LINES),
            MouseEventKind::ScrollDown => MouseAction::Scroll(-WHEEL_LINES),
            _ => MouseAction::None, // plain app: clicks are roost's (focus)
        },
    }
}

/// Translate screen coords to 1-based coords inside the pane's inner area
/// (borders excluded), clamped to at least 1.
fn cell_in_pane(rect: Rect, col: u16, row: u16) -> (u16, u16) {
    let inner_x = rect.x.saturating_add(1);
    let inner_y = rect.y.saturating_add(1);
    (col.saturating_sub(inner_x).saturating_add(1), row.saturating_sub(inner_y).saturating_add(1))
}

fn button_code(b: MouseButton) -> u16 {
    match b {
        MouseButton::Left => 0,
        MouseButton::Middle => 1,
        MouseButton::Right => 2,
    }
}

fn modifier_bits(m: KeyModifiers) -> u16 {
    let mut b = 0;
    if m.contains(KeyModifiers::SHIFT) {
        b += 4;
    }
    if m.contains(KeyModifiers::ALT) {
        b += 8;
    }
    if m.contains(KeyModifiers::CONTROL) {
        b += 16;
    }
    b
}

/// Encode a mouse event in SGR form: `ESC [ < Cb ; x ; y (M|m)`.
/// Bare motion (no button) is dropped — crossterm's capture doesn't request
/// it, and forwarding it would just spam apps that didn't ask.
fn encode_sgr(rect: Rect, me: &MouseEvent) -> Option<Vec<u8>> {
    let (base, release) = match me.kind {
        MouseEventKind::Down(b) => (button_code(b), false),
        MouseEventKind::Up(b) => (button_code(b), false), // SGR marks release via trailing 'm'
        MouseEventKind::Drag(b) => (button_code(b) + 32, false),
        MouseEventKind::ScrollUp => (64, false),
        MouseEventKind::ScrollDown => (65, false),
        MouseEventKind::ScrollLeft => (66, false),
        MouseEventKind::ScrollRight => (67, false),
        MouseEventKind::Moved => return None,
    };
    let is_up = matches!(me.kind, MouseEventKind::Up(_));
    let _ = release;
    let cb = base + modifier_bits(me.modifiers);
    let (cx, cy) = cell_in_pane(rect, me.column, me.row);
    let terminator = if is_up { 'm' } else { 'M' };
    Some(format!("\x1b[<{cb};{cx};{cy}{terminator}").into_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pr(id: crate::core::layout::PaneId, x: u16, y: u16, w: u16, h: u16, collapsed: bool) -> PaneRect {
        PaneRect { id, rect: Rect::new(x, y, w, h), collapsed }
    }

    fn ev(kind: MouseEventKind, col: u16, row: u16) -> MouseEvent {
        MouseEvent { kind, column: col, row, modifiers: KeyModifiers::NONE }
    }

    #[test]
    fn hit_test_picks_correct_pane() {
        let rects = vec![pr(1, 0, 1, 50, 29, false), pr(2, 50, 1, 50, 29, false)];
        assert_eq!(hit_test(&rects, 10, 5).unwrap().id, 1);
        assert_eq!(hit_test(&rects, 60, 5).unwrap().id, 2);
        assert!(hit_test(&rects, 10, 0).is_none()); // tab bar row
    }

    #[test]
    fn wheel_over_plain_app_scrolls_roost_side() {
        let pane = pr(1, 0, 1, 50, 29, false);
        assert_eq!(
            route_mouse(MouseProto::None, &pane, &ev(MouseEventKind::ScrollUp, 5, 5)),
            MouseAction::Scroll(WHEEL_LINES)
        );
        assert_eq!(
            route_mouse(MouseProto::None, &pane, &ev(MouseEventKind::ScrollDown, 5, 5)),
            MouseAction::Scroll(-WHEEL_LINES)
        );
    }

    #[test]
    fn click_on_plain_app_is_not_forwarded() {
        let pane = pr(1, 0, 1, 50, 29, false);
        assert_eq!(
            route_mouse(MouseProto::None, &pane, &ev(MouseEventKind::Down(MouseButton::Left), 5, 5)),
            MouseAction::None
        );
    }

    #[test]
    fn wheel_over_mouse_aware_app_forwards_sgr() {
        let pane = pr(1, 10, 5, 40, 20, false);
        // screen (12, 7) → inner cell (2, 2), 1-based
        match route_mouse(MouseProto::Sgr, &pane, &ev(MouseEventKind::ScrollUp, 12, 7)) {
            MouseAction::Forward(b) => assert_eq!(b, b"\x1b[<64;2;2M"),
            other => panic!("expected forward, got {other:?}"),
        }
    }

    #[test]
    fn click_and_drag_forward_to_mouse_aware_app() {
        let pane = pr(1, 10, 5, 40, 20, false);
        // left press at inner (2,2)
        match route_mouse(MouseProto::Sgr, &pane, &ev(MouseEventKind::Down(MouseButton::Left), 12, 7))
        {
            MouseAction::Forward(b) => assert_eq!(b, b"\x1b[<0;2;2M"),
            other => panic!("{other:?}"),
        }
        // release → trailing 'm'
        match route_mouse(MouseProto::Sgr, &pane, &ev(MouseEventKind::Up(MouseButton::Left), 12, 7)) {
            MouseAction::Forward(b) => assert_eq!(b, b"\x1b[<0;2;2m"),
            other => panic!("{other:?}"),
        }
        // left drag → button + motion flag (0 + 32)
        match route_mouse(MouseProto::Sgr, &pane, &ev(MouseEventKind::Drag(MouseButton::Left), 13, 8))
        {
            MouseAction::Forward(b) => assert_eq!(b, b"\x1b[<32;3;3M"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn right_click_with_modifiers_encodes_button_and_mods() {
        let pane = pr(1, 0, 0, 40, 20, false);
        let mut e = ev(MouseEventKind::Down(MouseButton::Right), 5, 5);
        e.modifiers = KeyModifiers::CONTROL; // +16, right button = 2 → 18
        // pane at (0,0): inner origin (1,1), so screen (5,5) → inner cell (5,5)
        match route_mouse(MouseProto::Sgr, &pane, &e) {
            MouseAction::Forward(b) => assert_eq!(b, b"\x1b[<18;5;5M"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn collapsed_bars_forward_nothing() {
        let pane = pr(1, 0, 1, 50, 1, true);
        assert_eq!(
            route_mouse(MouseProto::Sgr, &pane, &ev(MouseEventKind::ScrollUp, 5, 1)),
            MouseAction::None
        );
    }

    #[test]
    fn tab_hit_testing_matches_the_c2_worked_example() {
        // C2 worked example: ["main", "api"] → tab 0 spans cols 0..12
        // ("1 main" is 6 chars + 6 fixed cols), tab 1 spans cols 12..23
        // ("2 api" is 5 chars + 6). Generous bar width, no status area, so
        // this pins the base hit-math with nothing else in play.
        let names = vec!["main".to_string(), "api".to_string()];
        assert_eq!(tab_at_x(&names, 100, 0, 0), Some(0)); // start of tab 0
        assert_eq!(tab_at_x(&names, 100, 0, 11), Some(0)); // last col of tab 0
        assert_eq!(tab_at_x(&names, 100, 0, 12), Some(1)); // first col of tab 1
        assert_eq!(tab_at_x(&names, 100, 0, 22), Some(1)); // last col of tab 1
        assert_eq!(tab_at_x(&names, 100, 0, 23), None); // past the end
        assert_eq!(tab_at_x(&names, 100, 0, 200), None);
    }

    #[test]
    fn wide_glyph_tab_name_uses_display_width_not_char_count() {
        // "日本" is 2 chars but 4 display columns (CJK glyphs are
        // double-width in a terminal): label "1 日本" is 1 + 1 + 2 + 2 = 6
        // display columns, so tab_width is 6 + 6 = 12 — not the char-count
        // answer of 10. D1: a renamed tab with wide glyphs must not
        // misindex clicks past the glyph's real width.
        let names = vec!["日本".to_string()];
        assert_eq!(tab_width(0, "日本"), 12);
        assert_eq!(tab_at_x(&names, 100, 0, 11), Some(0)); // last col of the tab
        assert_eq!(tab_at_x(&names, 100, 0, 12), None); // just past it
    }

    #[test]
    fn tab_at_x_after_a_wide_glyph_tab_uses_its_real_width() {
        // tab 0's label "1 🦀x" is 1 + 1 + 2 + 1 = 5 display columns (the
        // crab emoji is double-width, 'x' is single) + 6 fixed cols = 11;
        // tab 1 must start at col 11, not the char-count answer of 10.
        let names = vec!["🦀x".to_string(), "b".to_string()];
        assert_eq!(tab_width(0, "🦀x"), 11);
        assert_eq!(tab_at_x(&names, 100, 0, 10), Some(0)); // last col of tab 0
        assert_eq!(tab_at_x(&names, 100, 0, 11), Some(1)); // first col of tab 1
    }

    #[test]
    fn status_area_click_switches_nothing() {
        let names = vec!["main".to_string(), "api".to_string()];
        // Bar exactly wide enough for both tabs (23 cols) plus a 10-col
        // status area: the status area occupies cols 23..33.
        assert_eq!(tab_at_x(&names, 33, 10, 22), Some(1)); // last tab col
        assert_eq!(tab_at_x(&names, 33, 10, 23), None); // status area starts here
        assert_eq!(tab_at_x(&names, 33, 10, 32), None); // status area, last col
    }

    #[test]
    fn status_area_is_dropped_before_tabs_clip() {
        // Tabs alone (23 cols) fit a 25-col bar, but not alongside a 10-col
        // status area (23+10=33 > 25) — C2 says the status area drops first,
        // so tab 1 (cols 12..23) stays fully clickable and nothing clips.
        let names = vec!["main".to_string(), "api".to_string()];
        assert_eq!(tab_at_x(&names, 25, 10, 22), Some(1));
        assert_eq!(tab_at_x(&names, 25, 10, 23), None); // past both tabs, no status shown
    }

    #[test]
    fn overflow_clips_and_the_clip_point_switches_nothing() {
        // Ten single-letter tabs: labels "1 a".."10 j", each 9 cols except
        // the last (10). A 40-col bar with no status area fits exactly four
        // tabs (36 cols) before the fifth would overflow.
        let names: Vec<String> = "abcdefghij".chars().map(|c| c.to_string()).collect();
        assert_eq!(tabs_visible_width(&names, 40, 0), 36);
        assert_eq!(tab_at_x(&names, 40, 0, 35), Some(3)); // last col of tab 3 (0-based)
        assert_eq!(tab_at_x(&names, 40, 0, 36), None); // the `…` clip marker
        assert_eq!(tab_at_x(&names, 40, 0, 39), None); // past the bar too
    }

    #[test]
    fn status_parts_formats_cwd_and_save_state() {
        let (prefix, save) = status_parts(Some("~/work"), true);
        assert_eq!(prefix, "~/work · ");
        assert_eq!(save, format!("saved {}", theme::SAVED));
        assert_eq!(status_width(Some("~/work"), true), display_width(&prefix) + display_width(&save) + 1);

        let (prefix, save) = status_parts(None, false);
        assert_eq!(prefix, "");
        assert_eq!(save, format!("save failed {}", theme::GLYPH_EXITED));
    }

    #[test]
    fn single_tab_bar_hit_testing() {
        // One tab, no separators to get confused by: "1 solo" is 6 chars + 6
        // fixed cols = 12, occupying the whole visible width.
        let names = vec!["solo".to_string()];
        assert_eq!(tab_width(0, "solo"), 12);
        assert_eq!(tabs_visible_width(&names, 100, 0), 12);
        assert_eq!(tab_at_x(&names, 100, 0, 0), Some(0));
        assert_eq!(tab_at_x(&names, 100, 0, 11), Some(0));
        assert_eq!(tab_at_x(&names, 100, 0, 12), None); // just past the only tab
    }

    #[test]
    fn empty_or_zero_width_bar_has_no_clickable_tabs() {
        // No tabs, and a zero-width bar: nothing is clickable, and the width
        // math (which never divides) doesn't panic on the degenerate input.
        let none: Vec<String> = vec![];
        assert_eq!(total_tabs_width(&none), 0);
        assert_eq!(tab_at_x(&none, 40, 0, 0), None);

        let one = vec!["solo".to_string()];
        assert_eq!(tabs_visible_width(&one, 0, 0), 0);
        assert_eq!(tab_at_x(&one, 0, 0, 0), None);
    }
}
