//! Mouse routing — pure decisions, no I/O, fully unit-tested.
//!
//! The scroll bug this module fixes: without mouse capture the terminal
//! emulator consumes wheel events itself and scrolls its *own* buffer —
//! visually "scrolling to content outside the TUI". With capture enabled,
//! wheel events arrive here and are routed per hovered pane:
//!
//! - inner app enabled SGR mouse reporting (pi/claude TUIs, vim, less…)
//!   → encode the event and forward it to the PTY; the app scrolls itself.
//! - plain apps (shells) → scroll roost's own scrollback for that pane.

use ratatui::layout::Rect;

use crate::core::layout::PaneRect;
use crate::ports::MouseProto;

/// Lines per wheel notch for roost-side scrolling (tmux uses 3).
pub const WHEEL_LINES: i32 = 3;

#[derive(Debug, PartialEq, Eq)]
pub enum WheelRoute {
    /// Forward these bytes to the pane's PTY (mouse-aware app).
    Forward(Vec<u8>),
    /// Scroll roost's scrollback by this delta (positive = into history).
    Scroll(i32),
    Ignore,
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

/// Route a wheel event over a pane.
pub fn route_wheel(proto: MouseProto, pane: &PaneRect, col: u16, row: u16, up: bool) -> WheelRoute {
    if pane.collapsed {
        // A collapsed stack bar has no scrollable content.
        return WheelRoute::Ignore;
    }
    match proto {
        MouseProto::Sgr => {
            let (cx, cy) = cell_in_pane(pane.rect, col, row);
            WheelRoute::Forward(sgr_wheel(up, cx, cy))
        }
        MouseProto::None => WheelRoute::Scroll(if up { WHEEL_LINES } else { -WHEEL_LINES }),
    }
}

/// Translate screen coords to 1-based coords inside the pane's inner area
/// (borders excluded), clamped to at least 1.
fn cell_in_pane(rect: Rect, col: u16, row: u16) -> (u16, u16) {
    let inner_x = rect.x.saturating_add(1);
    let inner_y = rect.y.saturating_add(1);
    (col.saturating_sub(inner_x).saturating_add(1), row.saturating_sub(inner_y).saturating_add(1))
}

/// SGR-encoded wheel event: `ESC [ < 64|65 ; x ; y M`.
fn sgr_wheel(up: bool, cx: u16, cy: u16) -> Vec<u8> {
    let btn = if up { 64 } else { 65 };
    format!("\x1b[<{btn};{cx};{cy}M").into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pr(id: crate::core::layout::PaneId, x: u16, y: u16, w: u16, h: u16, collapsed: bool) -> PaneRect {
        PaneRect { id, rect: Rect::new(x, y, w, h), collapsed }
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
            route_wheel(MouseProto::None, &pane, 5, 5, true),
            WheelRoute::Scroll(WHEEL_LINES)
        );
        assert_eq!(
            route_wheel(MouseProto::None, &pane, 5, 5, false),
            WheelRoute::Scroll(-WHEEL_LINES)
        );
    }

    #[test]
    fn wheel_over_mouse_aware_app_forwards_sgr() {
        let pane = pr(1, 10, 5, 40, 20, false);
        // screen (12, 7) → inner cell (2, 2), 1-based
        match route_wheel(MouseProto::Sgr, &pane, 12, 7, true) {
            WheelRoute::Forward(bytes) => assert_eq!(bytes, b"\x1b[<64;2;2M"),
            other => panic!("expected forward, got {other:?}"),
        }
        match route_wheel(MouseProto::Sgr, &pane, 12, 7, false) {
            WheelRoute::Forward(bytes) => assert_eq!(bytes, b"\x1b[<65;2;2M"),
            other => panic!("expected forward, got {other:?}"),
        }
    }

    #[test]
    fn collapsed_bars_ignore_wheel() {
        let pane = pr(1, 0, 1, 50, 1, true);
        assert_eq!(route_wheel(MouseProto::Sgr, &pane, 5, 1, true), WheelRoute::Ignore);
    }

    #[test]
    fn coords_clamp_on_borders() {
        let pane = pr(1, 0, 1, 50, 20, false);
        // click exactly on the border still yields cell (1,1)
        match route_wheel(MouseProto::Sgr, &pane, 0, 1, true) {
            WheelRoute::Forward(bytes) => assert_eq!(bytes, b"\x1b[<64;1;1M"),
            other => panic!("{other:?}"),
        }
    }
}
