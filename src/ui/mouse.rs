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

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;
use unicode_width::UnicodeWidthStr;

use crate::core::layout::{PaneId, PaneRect};
use crate::ports::MouseProto;
use crate::ui::{input, theme};

/// Lines per wheel notch for roost-side scrolling (tmux uses 3).
pub const WHEEL_LINES: i32 = 3;

/// P9: arrow keys one wheel notch becomes over an alternate-screen app that
/// never asked for mouse reporting — the DECSET 1007 convention peers settled
/// on (tmux's `alternate-scroll`). Deliberately the same 3 as `WHEEL_LINES`:
/// the wheel should move a pager by exactly what it moves roost's own
/// scrollback by, so it feels the same either side of the alternate screen.
pub const ALT_SCROLL_KEYS: usize = 3;

/// C29: roost's own mouse-capture sequences, deliberately a **subset** of
/// crossterm's `EnableMouseCapture`/`DisableMouseCapture`, which bundle five
/// DEC private modes: `1000` (press/release), `1002` (button-drag motion),
/// `1003` (**any**-motion tracking — every pointer move, button held or
/// not), `1015` (RXVT extended coordinates) and `1006` (SGR extended
/// coordinates — "preferred over RXVT mode" per crossterm's own comment on
/// the command). `route_mouse` drops every `Moved` event unconditionally
/// (the match arm below has no case for it), so `1003` buys a continuous
/// stream of events roost pays to parse and immediately discards; `1006`
/// alone already gives unbounded coordinates in the one format
/// `encode_sgr` emits, so `1015` is a second protocol for a job `1006`
/// already does. `1000` and `1002` are the two modes every gesture in this
/// module (click-to-focus, seam-drag, native selection, SGR forwarding)
/// actually needs. Exact byte-for-byte subset of crossterm 0.29's own
/// `EnableMouseCapture::write_ansi` (verified against its source), so a
/// terminal sees nothing it wouldn't have seen from the blanket enable.
pub const MOUSE_CAPTURE_ENABLE: &str = "\x1b[?1000h\x1b[?1002h\x1b[?1006h";
/// The exact inverse, reverse order — same idiom as crossterm's own
/// `DisableMouseCapture`. Symmetric with `MOUSE_CAPTURE_ENABLE` on purpose:
/// clearing an already-clear DEC private mode is a harmless no-op on every
/// terminal, but there is no reason for enable and disable to name a
/// different set of modes.
pub const MOUSE_CAPTURE_DISABLE: &str = "\x1b[?1006l\x1b[?1002l\x1b[?1000l";

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

/// U21: the seam between two adjacent panes — the thing a drag resizes.
///
/// `a` is the pane on the low side of the split (left, or above) and `b` the
/// one on the high side, which is what lets the drag handler say "the border
/// is at `a`'s far edge" without re-deriving the orientation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Seam {
    pub a: PaneId,
    pub b: PaneId,
    /// A *vertical* split: the panes sit side by side and the seam is dragged
    /// horizontally. `false` = stacked, dragged vertically.
    pub vertical: bool,
}

/// U21: is (col, row) on the border between two panes?
///
/// Every pane draws its own border (C3), so two neighbours are separated by a
/// **two-cell seam** — `a`'s far edge and `b`'s near edge, which are adjacent
/// columns/rows. Both count: a user aiming at "the line between the panes"
/// cannot be expected to distinguish them, and one-cell targets are unkind
/// with a mouse.
///
/// Collapsed stack members are excluded. Their "borders" are the stack's own
/// internal rows (C6/C8 draw them as single bars, not framed panes), and a
/// stack's proportions are not a split ratio anything could resize.
pub fn seam_at(rects: &[PaneRect], col: u16, row: u16) -> Option<Seam> {
    let live: Vec<&PaneRect> = rects.iter().filter(|p| !p.collapsed).collect();
    for a in &live {
        for b in &live {
            if a.id == b.id {
                continue;
            }
            let (ar, br) = (a.rect, b.rect);
            // Side by side: b starts exactly where a ends.
            if br.x == ar.x + ar.width {
                let lo = ar.y.max(br.y);
                let hi = (ar.y + ar.height).min(br.y + br.height);
                let on_seam = col + 1 == br.x || col == br.x;
                if on_seam && row >= lo && row < hi {
                    return Some(Seam { a: a.id, b: b.id, vertical: true });
                }
            }
            // Stacked: b starts exactly where a ends.
            if br.y == ar.y + ar.height {
                let lo = ar.x.max(br.x);
                let hi = (ar.x + ar.width).min(br.x + br.width);
                let on_seam = row + 1 == br.y || row == br.y;
                if on_seam && col >= lo && col < hi {
                    return Some(Seam { a: a.id, b: b.id, vertical: false });
                }
            }
        }
    }
    None
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
/// the label body, a space, a 1-col status glyph, a 1-col **count cell**, a
/// space, the separator, and a trailing gutter space — eight fixed columns
/// plus the label. The gutter gives every divider symmetric 1-cell padding
/// (`│ ▎`).
///
/// The count cell (`◆3`) is **always reserved**, blank below 2, so a tab's
/// width never changes as its agents' statuses flip — stability is worth
/// more than the column, because a bar that reflows under a background
/// status change moves every hitbox on it (§4/§5 lockstep) for a signal the
/// user did not act on. `render::tab_count_cell` decides what goes in it;
/// this function only knows it is exactly one column, always.
pub fn tab_width(index: usize, name: &str) -> u16 {
    display_width(&tab_label(index, name)) + 8
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
///
/// **[Amended 2026-08-20 — a failed save outranks tab names.]** "Tabs win"
/// is right for context (a cwd, a mode word, `saved ✓`): those are things
/// you can get another way. `save failed ✕` is not context, it is the only
/// standing signal that the workspace on disk is going stale, and it was
/// being dropped *whole* by this rule — on an ordinary 80-column terminal
/// with five tabs, roost could fail every single write and show nothing at
/// all. The C10 flash added alongside this fires once, on the transition;
/// this is what stays on screen afterwards, and a signal that disappears
/// exactly when the terminal is busy is not a signal.
///
/// So on a failed save the strip yields instead: U7 already scrolls it and
/// marks each clipped end with `…`, and the *active* tab is always among the
/// ones kept — so what is lost is other tabs' names, temporarily and
/// visibly, not the tab you are on. The area is still dropped when even the
/// active tab could not be drawn beside it: a tab bar with no tabs is not a
/// trade worth making, and the flash already fired.
pub fn effective_status_width(
    names: &[String],
    bar_width: u16,
    status_width: u16,
    save_ok: bool,
    active: usize,
) -> u16 {
    if total_tabs_width(names).saturating_add(status_width) <= bar_width {
        return status_width;
    }
    if save_ok {
        return 0;
    }
    // Plus the column the left `…` costs — but only when the strip can
    // actually scroll. `tab_scroll_start` cannot move past tab 0, so an
    // active tab of 0 always draws at `x0 = 0` with no leading marker;
    // charging for one there dropped the indicator while the active tab
    // still fit exactly (`["main","api"]`, a 28-column bar: 28-14 = 14, tab
    // 0 needs 14). Found by the C2 design audit.
    let active_w = names.get(active).map(|n| tab_width(active, n)).unwrap_or(0);
    let marker = u16::from(active > 0);
    if bar_width.saturating_sub(status_width) >= active_w.saturating_add(marker) {
        status_width
    } else {
        0
    }
}

/// C2 (U7): the window of tabs actually drawn on the bar. The strip
/// scrolls so the ACTIVE tab is always visible — before this, tab 10 of 12
/// could be selected (by chord, by `Alt+a`'s jump) while the bar still
/// showed tabs 1–4 and nothing said where you were. One `…` marks each end
/// that hides tabs, keeping the overflow marker's meaning unchanged: it is
/// never a tab, and clicking it switches nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TabStrip {
    /// First tab drawn.
    pub start: usize,
    /// One past the last fully drawn tab.
    pub end: usize,
    /// Column the first drawn tab starts at — 1 when a left `…` is drawn.
    pub x0: u16,
    /// Columns through the last drawn tab (`x0` included); where the right
    /// `…`, the gap and the status area begin.
    pub width: u16,
    pub left_marker: bool,
    pub right_marker: bool,
}

/// The leftmost tab to draw so that `active` fits in `budget` columns: the
/// smallest such start, so the strip scrolls by the least it can and keeps
/// as much history on screen as possible (the active tab then rides the
/// right edge, like tmux's window list). 0 whenever everything fits.
fn tab_scroll_start(names: &[String], budget: u16, active: usize) -> usize {
    if total_tabs_width(names) <= budget {
        return 0;
    }
    let mut start = 0usize;
    while start < active.min(names.len()) {
        let x0 = u16::from(start > 0); // the left `…` costs a column
        let span: u16 = (start..=active).map(|i| tab_width(i, &names[i])).sum();
        if x0.saturating_add(span) <= budget {
            break;
        }
        start += 1;
    }
    start
}

/// C2: lay out the tab strip for this bar. `status_width` is the fitted
/// width of the right status area (`status_fit`), which tabs win against
/// exactly as before.
pub fn tab_strip(
    names: &[String],
    bar_width: u16,
    status_width: u16,
    save_ok: bool,
    active: usize,
) -> TabStrip {
    let budget =
        bar_width.saturating_sub(effective_status_width(names, bar_width, status_width, save_ok, active));
    let start = tab_scroll_start(names, budget, active);
    let x0 = u16::from(start > 0);
    let mut width = x0.min(budget);
    let mut end = start;
    for (i, name) in names.iter().enumerate().skip(start) {
        let w = tab_width(i, name);
        if width.saturating_add(w) > budget {
            break;
        }
        width += w;
        end = i + 1;
    }
    TabStrip {
        start,
        end,
        x0,
        width,
        left_marker: start > 0,
        // Opportunistic, exactly as before: the marker is drawn only when a
        // spare column is left for it (it is never allowed to displace a tab).
        right_marker: end < names.len() && width < budget,
    }
}

/// Which tab (if any) sits at column `x` on the tab bar row (C2: tabs start
/// at `x = 0` — the brand block is gone — or at `x = 1` when the strip has
/// scrolled and a left `…` leads it). Reads the same `tab_strip` the
/// renderer draws from, so a click on either `…` marker, the gap, or the
/// right-aligned status area correctly switches nothing.
pub fn tab_at_x(
    names: &[String],
    bar_width: u16,
    status_width: u16,
    save_ok: bool,
    active: usize,
    x: u16,
) -> Option<usize> {
    let strip = tab_strip(names, bar_width, status_width, save_ok, active);
    if x < strip.x0 || x >= strip.width {
        return None;
    }
    let mut cur = strip.x0;
    for (i, name) in names.iter().enumerate().take(strip.end).skip(strip.start) {
        let w = tab_width(i, name);
        if x < cur + w {
            return Some(i);
        }
        cur += w;
    }
    None
}

/// U8/C14: which picker row (if any) sits at (col, row), given the picker
/// dialog's drawn rect and its item count. The dialog is a 1-cell bordered
/// block, so item `i` occupies the single row `rect.y + 1 + i` inside the
/// border columns — a click on the border, the title, or past the last item
/// hits nothing. Lives here (not in the renderer) for the same reason
/// `tab_at_x` does: hit math is mouse's job, and `render::modal_rect` feeds
/// it the exact rect that was drawn.
pub fn picker_row_at(rect: Rect, items: usize, col: u16, row: u16) -> Option<usize> {
    if rect.width < 3 || rect.height < 3 {
        return None; // no inner area to click
    }
    if col <= rect.x || col >= rect.x + rect.width - 1 {
        return None; // left/right border (or outside)
    }
    let first = rect.y + 1;
    let i = row.checked_sub(first)? as usize;
    (i < items && i < (rect.height - 2) as usize).then_some(i)
}

/// The tab bar's right-aligned status text (C2): the mode word (U15 — only
/// when the hint bar isn't carrying it), the focused pane's cwd (already
/// `~`-abbreviated by the caller, `App::focused_cwd`) and the save
/// indicator, split into `(mode, cwd, save_word)` so the renderer can color
/// them independently. `mode`/`cwd` are `"{x} · "`, or empty when there's
/// nothing to show (that part is omitted, not blanked).
pub fn status_parts(mode: Option<&str>, cwd: Option<&str>, save_ok: bool) -> (String, String, String) {
    let save_word = if save_ok {
        format!("saved {}", theme::SAVED)
    } else {
        format!("save failed {}", theme::GLYPH_EXITED)
    };
    let sep = |s: &str| format!("{s} · ");
    (mode.map(sep).unwrap_or_default(), cwd.map(sep).unwrap_or_default(), save_word)
}

/// On-screen width of `status_parts`' output, including the C2 trailing
/// space — the column span `tab_at_x` treats as off-limits for tab clicks.
pub fn status_width(mode: Option<&str>, cwd: Option<&str>, save_ok: bool) -> u16 {
    let (m, c, save_word) = status_parts(mode, cwd, save_ok);
    display_width(&m) + display_width(&c) + display_width(&save_word) + 1
}

/// What the status area actually shows at this bar width, after C2's yield
/// ladder — the single computation the renderer and the click hit-test
/// share, so they can't disagree about which columns belong to tabs
/// (§4/§5 lockstep).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StatusFit<'a> {
    pub mode: Option<&'a str>,
    pub cwd: Option<&'a str>,
    /// Columns this content occupies — feed it to `tab_at_x` /
    /// `tab_at_x` as its `status_width` argument.
    pub width: u16,
}

/// C2 + U15: fit the status area alongside the tabs. Tabs still win
/// outright (a status area that can't fit is dropped whole, `None`), but
/// **within** the area the cwd yields before the mode word: the word is a
/// modal-safety affordance — with the hint bar hidden it is the only thing
/// telling you you're in ZOOM/RAW/COPY — while the cwd is context you can
/// get by looking at the pane. Returns the content plus its exact width.
pub fn status_fit<'a>(
    mode: Option<&'a str>,
    cwd: Option<&'a str>,
    save_ok: bool,
    names: &[String],
    bar_width: u16,
) -> Option<StatusFit<'a>> {
    let tabs = total_tabs_width(names);
    for (m, c) in [(mode, cwd), (mode, None)] {
        let width = status_width(m, c, save_ok);
        if tabs.saturating_add(width) <= bar_width {
            return Some(StatusFit { mode: m, cwd: c, width });
        }
    }
    // [Amended 2026-08-20] One more rung, for a failed save only: the bare
    // indicator, exempt from the fits-alongside-every-tab test above. The
    // mode word yields to it here — the reverse of U15's usual order, and
    // deliberately: ZOOM/RAW/COPY is an affordance you can also discover by
    // pressing a key, while `save failed ✕` is the only standing word for
    // "what you are doing is not reaching disk". `effective_status_width`
    // still has the last word on whether there is room to draw it at all.
    if !save_ok {
        let width = status_width(None, None, false);
        if width <= bar_width {
            return Some(StatusFit { mode: None, cwd: None, width });
        }
    }
    None
}

/// What roost knows about the pane an event landed on, as far as routing is
/// concerned. `proto` alone was enough while the only answers were "forward
/// it" and "scroll roost's buffer"; P9 needs a third (see `route_mouse`), and
/// two more facts to pick it.
/// A pane roost has no runtime for defaults to no protocol, no alternate
/// screen (`MouseProto::None` is itself the `#[default]` variant).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PaneMouseState {
    /// What the pane's inner application asked for, mouse-wise.
    pub proto: MouseProto,
    /// Is it drawing on the alternate screen (`?1049h`)? That grid has no
    /// scrollback, so roost-side scrolling there is a guaranteed no-op.
    pub alternate_screen: bool,
    /// Has it switched on DECCKM (`?1h`)? Decides `ESC O A` vs `ESC [ A` for
    /// the arrow keys P9 synthesizes — the same choice the keyboard path
    /// makes (`input::app_cursor_upgrade`).
    pub app_cursor_keys: bool,
}

/// Route a mouse event over a pane to the inner app or to roost's scrollback.
/// Focus (a roost concern) is decided by the caller.
///
/// P9: the wheel over an alternate-screen app with no mouse protocol becomes
/// arrow keys. `man`/`less` never ask for mouse reporting, and their grid has
/// scrollback capacity 0 — so routing the wheel to roost's own scrollback
/// moved nothing and told the application nothing (measured: zero bytes from
/// six wheel events). Every real terminal answers this the same way, via
/// DECSET 1007 / tmux's `alternate-scroll`.
pub fn route_mouse(state: PaneMouseState, pane: &PaneRect, me: &MouseEvent) -> MouseAction {
    if pane.collapsed {
        // A collapsed stack bar has no scrollable content and no inner app.
        return MouseAction::None;
    }
    match state.proto {
        MouseProto::Sgr => match encode_sgr(pane.rect, me) {
            Some(bytes) => MouseAction::Forward(bytes),
            None => MouseAction::None,
        },
        MouseProto::None => match me.kind {
            MouseEventKind::ScrollUp if state.alternate_screen => {
                MouseAction::Forward(alt_scroll_keys(KeyCode::Up, state.app_cursor_keys))
            }
            MouseEventKind::ScrollDown if state.alternate_screen => {
                MouseAction::Forward(alt_scroll_keys(KeyCode::Down, state.app_cursor_keys))
            }
            MouseEventKind::ScrollUp => MouseAction::Scroll(WHEEL_LINES),
            MouseEventKind::ScrollDown => MouseAction::Scroll(-WHEEL_LINES),
            _ => MouseAction::None, // plain app: clicks are roost's (focus)
        },
    }
}

/// P9: one wheel notch as `ALT_SCROLL_KEYS` presses of `code`. Encoded by the
/// keyboard path's own functions, so the application receives exactly the
/// bytes a real Up/Down would send it — SS3 when it asked for DECCKM, which
/// is what a pager driven through `smkx` is listening for.
fn alt_scroll_keys(code: KeyCode, app_cursor: bool) -> Vec<u8> {
    let key = KeyEvent::new(code, KeyModifiers::NONE);
    let once = input::app_cursor_upgrade(key, input::encode_raw(key), app_cursor);
    once.repeat(ALT_SCROLL_KEYS)
}

/// Translate screen coords to 1-based coords inside the pane's inner area
/// (borders excluded), clamped to the grid at **both** ends.
///
/// P20: the left/top clamp came free from `saturating_sub`; the right and
/// bottom didn't, so an event past the pane forwarded a column or row
/// outside the grid the inner app believes it has — a `CSI <0;140;40M` to a
/// program that asked for 80×24. Harmless-looking until gestures latch
/// (below), which makes "the pointer is outside this pane" the normal case
/// for a drag rather than an impossible one.
fn cell_in_pane(rect: Rect, col: u16, row: u16) -> (u16, u16) {
    let inner_w = rect.width.saturating_sub(2).max(1);
    let inner_h = rect.height.saturating_sub(2).max(1);
    let inner_x = rect.x.saturating_add(1);
    let inner_y = rect.y.saturating_add(1);
    (
        col.saturating_sub(inner_x).min(inner_w - 1).saturating_add(1),
        row.saturating_sub(inner_y).min(inner_h - 1).saturating_add(1),
    )
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
    let base = match me.kind {
        MouseEventKind::Down(b) => button_code(b),
        MouseEventKind::Up(b) => button_code(b), // SGR marks release via trailing 'm'
        MouseEventKind::Drag(b) => button_code(b) + 32,
        MouseEventKind::ScrollUp => 64,
        MouseEventKind::ScrollDown => 65,
        MouseEventKind::ScrollLeft => 66,
        MouseEventKind::ScrollRight => 67,
        MouseEventKind::Moved => return None,
    };
    let is_up = matches!(me.kind, MouseEventKind::Up(_));
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

    /// A pane whose app asked for nothing: no mouse protocol, primary screen.
    fn plain() -> PaneMouseState {
        PaneMouseState::default()
    }

    /// A pane whose app speaks SGR mouse reporting.
    fn sgr() -> PaneMouseState {
        PaneMouseState { proto: MouseProto::Sgr, ..PaneMouseState::default() }
    }

    /// P9: a pager — alternate screen, no mouse protocol.
    fn pager(app_cursor: bool) -> PaneMouseState {
        PaneMouseState { alternate_screen: true, app_cursor_keys: app_cursor, ..plain() }
    }

    /// C29: the capture sequences are the exact subset of crossterm 0.29's
    /// `EnableMouseCapture`/`DisableMouseCapture` (`?1000` `?1002` `?1003`
    /// `?1015` `?1006` on enable, reverse order on disable) that drops
    /// `?1003` (any-motion — pure cost, `route_mouse` never handles `Moved`)
    /// and `?1015` (RXVT coords — superseded by `?1006` SGR, which every
    /// gesture in this module already speaks). Enable and disable name the
    /// same three modes, reverse order, mirroring crossterm's own idiom.
    #[test]
    fn mouse_capture_sequences_are_the_1000_1002_1006_subset_symmetric_in_reverse() {
        assert_eq!(MOUSE_CAPTURE_ENABLE, "\x1b[?1000h\x1b[?1002h\x1b[?1006h");
        assert_eq!(MOUSE_CAPTURE_DISABLE, "\x1b[?1006l\x1b[?1002l\x1b[?1000l");
        fn modes(s: &str, suffix: char) -> Vec<&str> {
            s.split("\x1b[?").filter(|p| !p.is_empty()).map(|p| p.trim_end_matches(suffix)).collect()
        }
        let enabled = modes(MOUSE_CAPTURE_ENABLE, 'h');
        let mut disabled = modes(MOUSE_CAPTURE_DISABLE, 'l');
        disabled.reverse();
        assert_eq!(enabled, disabled, "disable must turn off exactly what enable turned on");
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
            route_mouse(plain(), &pane, &ev(MouseEventKind::ScrollUp, 5, 5)),
            MouseAction::Scroll(WHEEL_LINES)
        );
        assert_eq!(
            route_mouse(plain(), &pane, &ev(MouseEventKind::ScrollDown, 5, 5)),
            MouseAction::Scroll(-WHEEL_LINES)
        );
    }

    #[test]
    fn click_on_plain_app_is_not_forwarded() {
        let pane = pr(1, 0, 1, 50, 29, false);
        assert_eq!(
            route_mouse(plain(), &pane, &ev(MouseEventKind::Down(MouseButton::Left), 5, 5)),
            MouseAction::None
        );
    }

    #[test]
    fn wheel_over_mouse_aware_app_forwards_sgr() {
        let pane = pr(1, 10, 5, 40, 20, false);
        // screen (12, 7) → inner cell (2, 2), 1-based
        match route_mouse(sgr(), &pane, &ev(MouseEventKind::ScrollUp, 12, 7)) {
            MouseAction::Forward(b) => assert_eq!(b, b"\x1b[<64;2;2M"),
            other => panic!("expected forward, got {other:?}"),
        }
    }

    #[test]
    fn click_and_drag_forward_to_mouse_aware_app() {
        let pane = pr(1, 10, 5, 40, 20, false);
        // left press at inner (2,2)
        match route_mouse(sgr(), &pane, &ev(MouseEventKind::Down(MouseButton::Left), 12, 7))
        {
            MouseAction::Forward(b) => assert_eq!(b, b"\x1b[<0;2;2M"),
            other => panic!("{other:?}"),
        }
        // release → trailing 'm'
        match route_mouse(sgr(), &pane, &ev(MouseEventKind::Up(MouseButton::Left), 12, 7)) {
            MouseAction::Forward(b) => assert_eq!(b, b"\x1b[<0;2;2m"),
            other => panic!("{other:?}"),
        }
        // left drag → button + motion flag (0 + 32)
        match route_mouse(sgr(), &pane, &ev(MouseEventKind::Drag(MouseButton::Left), 13, 8))
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
        match route_mouse(sgr(), &pane, &e) {
            MouseAction::Forward(b) => assert_eq!(b, b"\x1b[<18;5;5M"),
            other => panic!("{other:?}"),
        }
    }

    /// P20: coordinates are clamped to the pane's inner grid at all four
    /// edges. A latched gesture (main.rs) routinely reports positions
    /// outside its own pane; the inner app must still only ever be told
    /// about cells it believes it has.
    #[test]
    fn forwarded_coords_clamp_to_the_panes_inner_grid_on_every_edge() {
        // Pane at (10, 5), 40×20 ⇒ inner origin (11, 6), inner 38×18, so
        // the SGR range is 1..=38 by 1..=18.
        let pane = pr(1, 10, 5, 40, 20, false);
        let fwd = |col, row| match route_mouse(
            sgr(),
            &pane,
            &ev(MouseEventKind::Drag(MouseButton::Left), col, row),
        ) {
            MouseAction::Forward(b) => String::from_utf8(b).unwrap(),
            other => panic!("{other:?}"),
        };
        assert_eq!(fwd(48, 23), "\x1b[<32;38;18M", "the last inner cell");
        // Past the right/bottom edge: clamped, not reported out of range
        // (this is what used to send `;41;25` to an app with 38×18).
        assert_eq!(fwd(60, 40), "\x1b[<32;38;18M");
        assert_eq!(fwd(u16::MAX, u16::MAX), "\x1b[<32;38;18M");
        // Past the left/top edge, and onto the borders: clamped to (1, 1).
        assert_eq!(fwd(0, 0), "\x1b[<32;1;1M");
        assert_eq!(fwd(10, 5), "\x1b[<32;1;1M");
    }

    /// The clamp must not divide by zero or wrap on a pane too small to
    /// have an inside — a 1×1 rect is all border.
    #[test]
    fn a_pane_with_no_inner_area_still_clamps_to_cell_one() {
        let pane = pr(1, 0, 0, 1, 1, false);
        match route_mouse(sgr(), &pane, &ev(MouseEventKind::ScrollUp, 9, 9)) {
            MouseAction::Forward(b) => assert_eq!(b, b"\x1b[<64;1;1M"),
            other => panic!("{other:?}"),
        }
    }

    /// P9: the wheel over `man`/`less` — alternate screen, no mouse protocol.
    /// Scrolling roost's own buffer there is a guaranteed no-op (that grid has
    /// scrollback capacity 0), so the tick reaches the app as arrow keys.
    #[test]
    fn the_wheel_over_an_alternate_screen_app_becomes_arrow_keys() {
        let pane = pr(1, 0, 1, 50, 29, false);
        assert_eq!(
            route_mouse(pager(false), &pane, &ev(MouseEventKind::ScrollDown, 5, 5)),
            MouseAction::Forward(b"\x1b[B\x1b[B\x1b[B".to_vec())
        );
        assert_eq!(
            route_mouse(pager(false), &pane, &ev(MouseEventKind::ScrollUp, 5, 5)),
            MouseAction::Forward(b"\x1b[A\x1b[A\x1b[A".to_vec())
        );
        // A pager driven through `smkx` (DECCKM) listens for the SS3 forms —
        // the same upgrade the keyboard path applies to a real arrow key.
        assert_eq!(
            route_mouse(pager(true), &pane, &ev(MouseEventKind::ScrollDown, 5, 5)),
            MouseAction::Forward(b"\x1bOB\x1bOB\x1bOB".to_vec())
        );
        // One notch is worth the same three lines either side of the
        // alternate screen.
        assert_eq!(ALT_SCROLL_KEYS, WHEEL_LINES as usize);
        // Clicks over such an app are still roost's (focus), as before.
        assert_eq!(
            route_mouse(pager(false), &pane, &ev(MouseEventKind::Down(MouseButton::Left), 5, 5)),
            MouseAction::None
        );
    }

    /// The other two branches are unchanged by P9: a primary-screen app still
    /// scrolls roost's scrollback, and an app that speaks SGR still gets the
    /// encoded event verbatim — alternate screen or not, since asking for
    /// mouse reporting means it wants the wheel itself.
    #[test]
    fn only_a_protocol_less_alternate_screen_pane_gets_arrow_keys() {
        let pane = pr(1, 0, 1, 50, 29, false);
        assert_eq!(
            route_mouse(plain(), &pane, &ev(MouseEventKind::ScrollUp, 5, 5)),
            MouseAction::Scroll(WHEEL_LINES),
            "primary screen: roost's own scrollback, as always"
        );
        let sgr_pager = PaneMouseState { proto: MouseProto::Sgr, ..pager(true) };
        assert_eq!(
            route_mouse(sgr_pager, &pane, &ev(MouseEventKind::ScrollUp, 5, 5)),
            // Pane at (0, 1) ⇒ inner origin (1, 2), so screen (5, 5) is the
            // inner cell (5, 4) — the encoding is untouched by P9.
            MouseAction::Forward(b"\x1b[<64;5;4M".to_vec()),
            "an app that asked for the wheel gets the wheel"
        );
    }

    #[test]
    fn collapsed_bars_forward_nothing() {
        let pane = pr(1, 0, 1, 50, 1, true);
        assert_eq!(
            route_mouse(sgr(), &pane, &ev(MouseEventKind::ScrollUp, 5, 1)),
            MouseAction::None
        );
    }

    #[test]
    fn tab_hit_testing_matches_the_c2_worked_example() {
        // C2 worked example (+8 with the count cell):
        // ["main", "api"] → tab 0 spans cols 0..14 ("1 main" is 6 chars + 8
        // fixed cols), tab 1 spans cols 14..27 ("2 api" is 5 chars + 8).
        // Generous bar width, no status area, so this pins the base hit-math
        // with nothing else in play.
        let names = vec!["main".to_string(), "api".to_string()];
        assert_eq!(tab_at_x(&names, 100, 0, true, 0, 0), Some(0)); // start of tab 0
        assert_eq!(tab_at_x(&names, 100, 0, true, 0, 13), Some(0)); // last col of tab 0 (its gutter)
        assert_eq!(tab_at_x(&names, 100, 0, true, 0, 14), Some(1)); // first col of tab 1
        assert_eq!(tab_at_x(&names, 100, 0, true, 0, 26), Some(1)); // last col of tab 1
        assert_eq!(tab_at_x(&names, 100, 0, true, 0, 27), None); // past the end
        assert_eq!(tab_at_x(&names, 100, 0, true, 0, 200), None);
    }

    /// C2's geometry rule: the count cell is **always** reserved, so a
    /// tab's width does not move when its count appears or disappears.
    /// Width is a pure function of the label — nothing about a tab's
    /// agents can shift the hitboxes beside it mid-session.
    #[test]
    fn tab_width_is_a_function_of_the_label_alone_so_a_count_cannot_move_it() {
        assert_eq!(tab_width(0, "main"), 14); // 6-col label + 8 fixed
        assert_eq!(tab_width(1, "api"), 13); // 5-col label + 8 fixed
        // The renderer's own cell is one column for every count it can show,
        // which is what makes the line above independent of status (the
        // boundaries themselves are pinned in `render`).
        for n in [0usize, 1, 2, 9, 10, 99] {
            assert_eq!(crate::ui::render::tab_count_cell_cols(n), 1, "count {n}");
        }
    }

    #[test]
    fn wide_glyph_tab_name_uses_display_width_not_char_count() {
        // "日本" is 2 chars but 4 display columns (CJK glyphs are
        // double-width in a terminal): label "1 日本" is 1 + 1 + 2 + 2 = 6
        // display columns, so tab_width is 6 + 8 = 14 — not the char-count
        // answer of 12. D1: a renamed tab with wide glyphs must not
        // misindex clicks past the glyph's real width.
        let names = vec!["日本".to_string()];
        assert_eq!(tab_width(0, "日本"), 14);
        assert_eq!(tab_at_x(&names, 100, 0, true, 0, 13), Some(0)); // last col of the tab
        assert_eq!(tab_at_x(&names, 100, 0, true, 0, 14), None); // just past it
    }

    #[test]
    fn tab_at_x_after_a_wide_glyph_tab_uses_its_real_width() {
        // tab 0's label "1 🦀x" is 1 + 1 + 2 + 1 = 5 display columns (the
        // crab emoji is double-width, 'x' is single) + 8 fixed cols = 13;
        // tab 1 must start at col 13, not the char-count answer of 12.
        let names = vec!["🦀x".to_string(), "b".to_string()];
        assert_eq!(tab_width(0, "🦀x"), 13);
        assert_eq!(tab_at_x(&names, 100, 0, true, 0, 12), Some(0)); // last col of tab 0
        assert_eq!(tab_at_x(&names, 100, 0, true, 0, 13), Some(1)); // first col of tab 1
    }

    #[test]
    fn status_area_click_switches_nothing() {
        // +8 with the count cell (2026-07-28): two tabs are 27 cols
        // (14 + 13); a 10-col status area then occupies cols 27..37.
        let names = vec!["main".to_string(), "api".to_string()];
        assert_eq!(tab_at_x(&names, 37, 10, true, 0, 26), Some(1)); // last tab col
        assert_eq!(tab_at_x(&names, 37, 10, true, 0, 27), None); // status area starts here
        assert_eq!(tab_at_x(&names, 37, 10, true, 0, 36), None); // status area, last col
    }

    #[test]
    fn status_area_is_dropped_before_tabs_clip() {
        // Tabs alone (27 cols) fit a 27-col bar, but not alongside a 10-col
        // status area (27+10=37 > 27) — C2 says the status area drops first,
        // so tab 1 (cols 14..27) stays fully clickable and nothing clips.
        let names = vec!["main".to_string(), "api".to_string()];
        assert_eq!(tab_at_x(&names, 27, 10, true, 0, 26), Some(1));
        assert_eq!(tab_at_x(&names, 27, 10, true, 0, 27), None); // past both tabs, no status shown
    }

    #[test]
    fn overflow_clips_and_the_clip_point_switches_nothing() {
        // Ten single-letter tabs: labels "1 a".."10 j", each 11 cols except
        // the last (12), after the +8 count cell. A 45-col bar with no status
        // area fits exactly four tabs (44 cols) before the fifth would
        // overflow, leaving one spare column for the `…` clip marker at 44.
        let names: Vec<String> = "abcdefghij".chars().map(|c| c.to_string()).collect();
        assert_eq!(tab_strip(&names, 45, 0, true, 0).width, 44);
        assert_eq!(tab_at_x(&names, 45, 0, true, 0, 43), Some(3)); // last col of tab 3 (0-based)
        assert_eq!(tab_at_x(&names, 45, 0, true, 0, 44), None); // the `…` clip marker
    }

    #[test]
    fn picker_rows_are_hit_inside_the_border_only() {
        // A 3-item picker as `render::dialog_rect` sizes it: 32 wide,
        // items + 2 tall, so rows y+1..y+4 are the items.
        let rect = Rect::new(34, 13, 32, 5);
        assert_eq!(picker_row_at(rect, 3, 35, 14), Some(0));
        assert_eq!(picker_row_at(rect, 3, 64, 15), Some(1));
        assert_eq!(picker_row_at(rect, 3, 50, 16), Some(2));
        // Borders: top, bottom, left, right — all miss.
        assert_eq!(picker_row_at(rect, 3, 50, 13), None);
        assert_eq!(picker_row_at(rect, 3, 50, 17), None);
        assert_eq!(picker_row_at(rect, 3, 34, 14), None);
        assert_eq!(picker_row_at(rect, 3, 65, 14), None);
        // Outside the dialog entirely, and past the last item.
        assert_eq!(picker_row_at(rect, 3, 5, 5), None);
        assert_eq!(picker_row_at(rect, 2, 50, 16), None);
    }

    #[test]
    fn a_picker_too_small_to_have_an_inside_hits_nothing() {
        assert_eq!(picker_row_at(Rect::new(0, 0, 2, 5), 3, 1, 1), None);
        assert_eq!(picker_row_at(Rect::new(0, 0, 32, 2), 3, 5, 1), None);
    }

    /// U7: ten tabs in a 45-col bar. The strip scrolls to whichever window
    /// contains the active tab, and it is the *least* it can scroll (the
    /// active tab rides the right edge, earlier tabs stay on screen).
    #[test]
    fn the_strip_scrolls_the_least_it_can_to_keep_the_active_tab_visible() {
        let names: Vec<String> = "abcdefghij".chars().map(|c| c.to_string()).collect();
        // Tab 0 active: nothing scrolls, four tabs fit (44 of 45 cols).
        let strip = tab_strip(&names, 45, 0, true, 0);
        assert_eq!((strip.start, strip.end, strip.x0), (0, 4, 0));
        assert!(!strip.left_marker && strip.right_marker);
        // Tab 3 is the last that fits unscrolled — still no scroll.
        assert_eq!(tab_strip(&names, 45, 0, true, 3).start, 0);
        // Tab 4 forces it: start=1 (the left `…` costs a column, so tabs
        // 1..4 = 44 + 1 = 45 <= 45). The window now fills the bar exactly, so
        // the *trailing* marker yields — it is opportunistic by contract and
        // may never displace a tab (C2, U7 amendment).
        let strip = tab_strip(&names, 45, 0, true, 4);
        assert_eq!((strip.start, strip.end, strip.x0), (1, 5, 1));
        assert!(strip.left_marker && !strip.right_marker);
        // The last tab (index 9, a 12-col label) is reachable and drawn.
        let strip = tab_strip(&names, 45, 0, true, 9);
        assert!(strip.end == names.len(), "the active tab must be inside the window");
        assert!(strip.start > 0 && strip.left_marker);
        assert!(!strip.right_marker, "nothing is hidden past the last tab");
    }

    /// Whatever the scroll, the active tab is always fully drawn — the
    /// property the whole feature exists for. Swept over every tab and a
    /// band of widths, including ones too narrow for even one tab.
    #[test]
    fn the_active_tab_is_always_inside_the_drawn_window() {
        let names: Vec<String> = (0..12).map(|i| format!("tab{i}")).collect();
        for active in 0..names.len() {
            for bar in 12..=140u16 {
                let strip = tab_strip(&names, bar, 0, true, active);
                let fits_one = tab_width(active, &names[active]) + strip.x0 <= bar;
                if fits_one {
                    assert!(
                        (strip.start..strip.end).contains(&active),
                        "bar={bar} active={active}: window {}..{} misses it",
                        strip.start,
                        strip.end
                    );
                }
                assert!(strip.width <= bar, "bar={bar} active={active}: strip overflows");
            }
        }
    }

    /// Hitboxes follow the scroll: on a scrolled strip, column 0 is the
    /// left `…` (no tab), the first drawn tab starts at column 1, and the
    /// indexes returned are the real tab numbers — not window offsets.
    #[test]
    fn clicks_land_on_the_scrolled_strips_real_tab_indexes() {
        let names: Vec<String> = "abcdefghij".chars().map(|c| c.to_string()).collect();
        let strip = tab_strip(&names, 45, 0, true, 4);
        assert_eq!((strip.start, strip.x0), (1, 1));
        assert_eq!(tab_at_x(&names, 45, 0, true, 4, 0), None, "the left `…` is not a tab");
        assert_eq!(tab_at_x(&names, 45, 0, true, 4, 1), Some(1), "first drawn tab, at its real index");
        assert_eq!(tab_at_x(&names, 45, 0, true, 4, 11), Some(1), "...through its last column");
        assert_eq!(tab_at_x(&names, 45, 0, true, 4, 12), Some(2));
        assert_eq!(tab_at_x(&names, 45, 0, true, 4, 34), Some(4), "the active tab is clickable");
        assert_eq!(tab_at_x(&names, 45, 0, true, 4, 44), Some(4), "...to its last column");
        // Exhaustive agreement with the drawn strip: every column either
        // maps to a tab inside the window, or to nothing.
        for x in 0..45u16 {
            match tab_at_x(&names, 45, 0, true, 4, x) {
                Some(i) => assert!((strip.start..strip.end).contains(&i), "x={x} hit tab {i}"),
                None => assert!(x < strip.x0 || x >= strip.width, "x={x} should have hit a tab"),
            }
        }
    }

    #[test]
    fn status_parts_formats_cwd_and_save_state() {
        let (mode, prefix, save) = status_parts(None, Some("~/work"), true);
        assert_eq!(mode, "");
        assert_eq!(prefix, "~/work · ");
        assert_eq!(save, format!("saved {}", theme::SAVED));
        assert_eq!(
            status_width(None, Some("~/work"), true),
            display_width(&prefix) + display_width(&save) + 1
        );

        let (_, prefix, save) = status_parts(None, None, false);
        assert_eq!(prefix, "");
        assert_eq!(save, format!("save failed {}", theme::GLYPH_EXITED));
    }

    /// U15: with the hint bar gone, the mode word leads the status area —
    /// `ZOOM · ~/work · saved ✓` — and it costs the width `tab_at_x` must
    /// then treat as off-limits.
    #[test]
    fn status_parts_lead_with_the_mode_word_when_the_hint_bar_is_gone() {
        let (mode, cwd, save) = status_parts(Some("ZOOM"), Some("~/work"), true);
        assert_eq!(mode, "ZOOM · ");
        assert_eq!(cwd, "~/work · ");
        assert_eq!(format!("{mode}{cwd}{save}"), format!("ZOOM · ~/work · saved {}", theme::SAVED));
        assert_eq!(
            status_width(Some("ZOOM"), Some("~/work"), true),
            status_width(None, Some("~/work"), true) + display_width("ZOOM · ")
        );
    }

    /// C2's ladder, U15 order: everything fits → drop the cwd (context)
    /// before the mode word (safety affordance) → drop the whole area
    /// (tabs still win outright).
    #[test]
    fn the_status_area_yields_its_cwd_before_its_mode_word() {
        let names = vec!["main".to_string(), "api".to_string()]; // 27 cols of tabs
        let tabs = total_tabs_width(&names);
        assert_eq!(tabs, 27);
        let full = status_width(Some("ZOOM"), Some("~/work"), true); // 24
        let no_cwd = status_width(Some("ZOOM"), None, true); // 15

        let fit = status_fit(Some("ZOOM"), Some("~/work"), true, &names, tabs + full).unwrap();
        assert_eq!((fit.mode, fit.cwd, fit.width), (Some("ZOOM"), Some("~/work"), full));

        // One column short of the full form: the cwd goes, the word stays.
        let fit = status_fit(Some("ZOOM"), Some("~/work"), true, &names, tabs + full - 1).unwrap();
        assert_eq!((fit.mode, fit.cwd, fit.width), (Some("ZOOM"), None, no_cwd));

        // Not even the word fits beside the tabs: no status area at all.
        assert_eq!(status_fit(Some("ZOOM"), Some("~/work"), true, &names, tabs + no_cwd - 1), None);
    }

    /// The fitted width is what bounds tab clicks — a wider status area
    /// (mode word present) takes its columns away from the tab hitboxes,
    /// exactly as it takes them away from the drawn tabs.
    #[test]
    fn a_mode_word_in_the_status_area_shrinks_the_clickable_tab_span() {
        let names = vec!["main".to_string(), "api".to_string()]; // tabs: 0..27
        let bar = total_tabs_width(&names) + status_width(Some("ZOOM"), None, true);
        let fit = status_fit(Some("ZOOM"), None, true, &names, bar).unwrap();
        assert_eq!(tab_at_x(&names, bar, fit.width, true, 0, 26), Some(1)); // last tab col
        assert_eq!(tab_at_x(&names, bar, fit.width, true, 0, 27), None); // status area
    }

    #[test]
    fn single_tab_bar_hit_testing() {
        // One tab, no separators to get confused by: "1 solo" is 6 chars + 8
        // fixed cols = 14, occupying the whole visible width.
        let names = vec!["solo".to_string()];
        assert_eq!(tab_width(0, "solo"), 14);
        assert_eq!(tab_strip(&names, 100, 0, true, 0).width, 14);
        assert_eq!(tab_at_x(&names, 100, 0, true, 0, 0), Some(0));
        assert_eq!(tab_at_x(&names, 100, 0, true, 0, 13), Some(0));
        assert_eq!(tab_at_x(&names, 100, 0, true, 0, 14), None); // just past the only tab
    }

    #[test]
    fn empty_or_zero_width_bar_has_no_clickable_tabs() {
        // No tabs, and a zero-width bar: nothing is clickable, and the width
        // math (which never divides) doesn't panic on the degenerate input.
        let none: Vec<String> = vec![];
        assert_eq!(total_tabs_width(&none), 0);
        assert_eq!(tab_at_x(&none, 40, 0, true, 0, 0), None);

        let one = vec!["solo".to_string()];
        assert_eq!(tab_strip(&one, 0, 0, true, 0).width, 0);
        assert_eq!(tab_at_x(&one, 0, 0, true, 0, 0), None);
    }

    /// C2 amendment (2026-08-20): "tabs win" holds for context, not for a
    /// failed save.
    ///
    /// The exact shape that made this a real loss: a bar too narrow for
    /// tabs + status, so the old rule dropped the status area whole and the
    /// only standing sign that writes were failing vanished — on an
    /// ordinary terminal with a handful of tabs, permanently.
    #[test]
    fn a_failed_save_is_not_dropped_to_make_room_for_tab_names() {
        let names: Vec<String> =
            ["main", "api", "web", "jobs", "docs"].iter().map(|s| s.to_string()).collect();
        // Narrow enough that everything cannot coexist...
        let bar = total_tabs_width(&names) - 4;
        let ok_w = status_width(None, Some("~/work"), true);
        let fail_w = status_width(None, None, false);

        assert_eq!(
            effective_status_width(&names, bar, ok_w, true, 0),
            0,
            "a healthy status area still yields to the tabs",
        );
        assert_eq!(
            effective_status_width(&names, bar, fail_w, false, 0),
            fail_w,
            "the failure indicator must survive the same squeeze",
        );

        // ...and what pays for it is other tabs' names, never the tab you
        // are on: the strip scrolls (U7) and keeps the active tab drawn.
        let active = 4;
        let strip = tab_strip(&names, bar, fail_w, false, active);
        assert!(
            (strip.start..strip.end).contains(&active),
            "the active tab was scrolled off to make room: {strip:?}",
        );
    }

    /// The floor is exactly "the active tab does not fit", not one column
    /// tighter. `tab_scroll_start` never scrolls past tab 0, so with tab 0
    /// active there is no leading `…` to pay for — charging for one anyway
    /// dropped the indicator while the tab still fit exactly. (C2 design
    /// audit, D1.)
    #[test]
    fn the_floor_does_not_charge_for_a_marker_that_is_never_drawn() {
        let names: Vec<String> = ["main", "api"].iter().map(|s| s.to_string()).collect();
        let fail_w = status_width(None, None, false);
        let tab0 = tab_width(0, "main");
        let bar = fail_w + tab0; // room for the indicator and tab 0, exactly

        assert_eq!(
            effective_status_width(&names, bar, fail_w, false, 0),
            fail_w,
            "tab 0 fits beside the indicator; nothing is scrolled off, so no `…` is drawn",
        );
        let strip = tab_strip(&names, bar, fail_w, false, 0);
        assert!(!strip.left_marker, "no leading marker with tab 0 active: {strip:?}");
        assert!((strip.start..strip.end).contains(&0), "tab 0 is drawn: {strip:?}");

        // With a later tab active the strip really does scroll, and the
        // marker really is drawn — so it must still be paid for.
        let tab1 = tab_width(1, "api");
        assert_eq!(
            effective_status_width(&names, fail_w + tab1, fail_w, false, 1),
            0,
            "tab 1 needs its leading `…` column too, and there is no room for it",
        );
    }

    /// The floor: a bar so narrow that keeping the indicator would leave no
    /// tab drawn at all. A tab bar with no tabs is not a trade worth making
    /// — and the C10 flash has already fired on the transition.
    #[test]
    fn the_failure_indicator_still_yields_rather_than_empty_the_bar() {
        let names = vec!["main".to_string()];
        let fail_w = status_width(None, None, false);
        let bar = fail_w + 2; // nowhere near enough for "1 main" beside it
        assert_eq!(effective_status_width(&names, bar, fail_w, false, 0), 0);
    }

    /// The status ladder's last rung, failed-save only: the mode word yields
    /// to the indicator — the reverse of U15's usual order, because
    /// ZOOM/RAW/COPY can be rediscovered by pressing a key and "not reaching
    /// disk" cannot.
    #[test]
    fn a_failed_save_outranks_even_the_mode_word() {
        let names: Vec<String> =
            ["main", "api", "web", "jobs"].iter().map(|s| s.to_string()).collect();
        let bar = total_tabs_width(&names) + status_width(None, None, false);

        let fit = status_fit(Some("ZOOM"), Some("~/work"), false, &names, bar)
            .expect("the indicator fits once everything else has yielded");
        assert_eq!(fit.mode, None, "the mode word yielded");
        assert_eq!(fit.cwd, None, "the cwd yielded first");
        assert_eq!(fit.width, status_width(None, None, false));

        // Same width, healthy save: the area is dropped whole instead —
        // nothing here is worth taking a tab's columns for.
        assert_eq!(status_fit(Some("ZOOM"), Some("~/work"), true, &names, bar - 4), None);
    }

}
