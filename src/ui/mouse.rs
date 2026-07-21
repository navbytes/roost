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

use crate::core::layout::PaneRect;
use crate::ports::MouseProto;

/// Lines per wheel notch for roost-side scrolling (tmux uses 3).
pub const WHEEL_LINES: i32 = 3;
/// The tab bar's fixed left brand mark. The 🪹 nest ("your agents come home to
/// roost") is a double-width glyph, so tab hit-testing must offset by display
/// columns via `TABBAR_PREFIX_WIDTH`, not a `.chars().count()` that would
/// undercount the emoji by one column and skew every click.
pub const TABBAR_PREFIX: &str = " 🪹 roost ";
/// Display width of `TABBAR_PREFIX` in terminal columns: leading space (1) +
/// nest (2) + space (1) + "roost" (5) + trailing space (1) = 10. Kept in sync
/// with `TABBAR_PREFIX` by hand; the unit test below guards the click offset.
pub const TABBAR_PREFIX_WIDTH: u16 = 10;

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

/// The tab bar label for a tab — shared with the renderer so click
/// hit-testing lines up exactly with what's drawn.
pub fn tab_label(index: usize, name: &str) -> String {
    format!("  {} {}", index + 1, name)
}

/// Which tab (if any) sits at column `x` on the tab bar row.
pub fn tab_at_x(names: &[String], x: u16) -> Option<usize> {
    let mut cur = TABBAR_PREFIX_WIDTH;
    for (i, name) in names.iter().enumerate() {
        let w = tab_label(i, name).chars().count() as u16;
        if x >= cur && x < cur + w {
            return Some(i);
        }
        cur += w;
    }
    None
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
    fn tab_hit_testing_matches_labels() {
        let names = vec!["main".to_string(), "api".to_string()];
        // prefix " 🪹 roost " is 10 cols → tab 0 "  1 main" starts at 10
        assert_eq!(tab_at_x(&names, 3), None); // in the prefix (over the nest)
        assert_eq!(tab_at_x(&names, 10), Some(0));
        assert_eq!(tab_at_x(&names, 15), Some(0)); // within "  1 main" (width 8: cols 10..18)
        assert_eq!(tab_at_x(&names, 18), Some(1)); // "  2 api" starts at 18
        assert_eq!(tab_at_x(&names, 200), None); // past the end
    }

    #[test]
    fn tabbar_prefix_width_accounts_for_the_wide_nest() {
        // The prefix has exactly one double-width glyph (🪹), so its display
        // width is the char count plus one. If someone edits TABBAR_PREFIX
        // without updating TABBAR_PREFIX_WIDTH, this fails and click offsets
        // won't silently drift.
        assert_eq!(TABBAR_PREFIX_WIDTH, TABBAR_PREFIX.chars().count() as u16 + 1);
    }
}
