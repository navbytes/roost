//! Rendering: tab bar + pane borders + vt100 grid blit (design doc §8).

use std::collections::{HashSet, VecDeque};

use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Paragraph};
use ratatui::Frame;
use unicode_width::UnicodeWidthChar;

use crate::core::app::{
    feed_overlay_size, roster_overlay_size, App, FeedEntry, Mode, RenameTarget,
    RosterRow, Search, Selection, TabSummary,
};
use crate::core::status::AgentStatus;
use crate::core::layout::{self, PaneRect};
use crate::ports::PaneBackend;
use crate::ui::mouse;
use crate::ui::theme;

pub fn draw<B: PaneBackend>(f: &mut Frame, app: &mut App<B>) {
    let area = f.area();
    if area.height < 2 {
        // ux P3-16: below the tab-bar-plus-one-body-row floor there is no
        // room for real chrome — this used to `return` here and leave the
        // screen blank, with nothing telling the user why.
        draw_too_small(f, area);
        return;
    }
    let tab_bar = Rect::new(area.x, area.y, area.width, 1);
    // Body comes from the app so pane rects, PTY sizing, and rendering all
    // agree on where the hint bar's reserved row is.
    let body = app.body_area();
    // C5/D3: one shared clock read for the whole frame. The pulse contract
    // requires every Working glyph to flip in unison; sampling `app.elapsed()`
    // separately per glyph left a real (if tiny) window for the clock to tick
    // past the 550ms edge mid-draw and split a frame across both phases.
    let pulse: Style = theme::pulse_phase(app.elapsed());

    draw_tab_bar(f, app, tab_bar, pulse);

    // C6: header row above each stack tall enough to spare one — a separate
    // walk over the same tree `app.rects()` reads, since the header isn't a
    // `PaneRect` (it belongs to no pane; §5). C21: stack headers are
    // real-tree chrome, suppressed entirely while zoomed.
    if !app.zoomed() {
        for header in layout::stack_headers(&app.ws.active_tab().layout, body) {
            draw_stack_header(f, header);
        }
    }
    // C7: which pane (if any) is the currently-expanded member of a stack —
    // computed once per frame, independent of whether that stack's header
    // row is shown.
    let mut stack_expanded = HashSet::new();
    layout::stack_expanded_ids(&app.ws.active_tab().layout, &mut stack_expanded);

    // C21/C22/§5: the zoom-and-float-aware display list — every
    // render/PTY-resize/mouse-hit path shares this one accessor so none of
    // them can disagree with what's actually on screen. It orders the float
    // *first* (topmost priority for `hit_test`'s first-match rule), but the
    // float must paint *last* (topmost visually, C22 stacking order: tiled
    // panes → zoomed view → float → modals) — so painting walks it in
    // reverse. Reversing has no effect on the non-float cases (a zoomed
    // singleton, or disjoint tiled rects that never overlap each other).
    let rects = app.display_rects();
    for pr in rects.iter().rev() {
        draw_pane(f, app, *pr, stack_expanded.contains(&pr.id), pulse);
    }

    if app.hints_shown() {
        let hint_bar = Rect::new(area.x, area.y + area.height - 1, area.width, 1);
        draw_hint_bar(f, app, hint_bar);
    }

    // Anchor floating dialogs near the focused pane rather than dead-center
    // of the whole screen, so it's visually obvious which pane they affect.
    let anchor = rects.iter().find(|pr| pr.id == app.focused).map(|pr| pr.rect).unwrap_or(body);
    draw_mode_overlay(f, app, body, anchor, pulse);
}

/// ux P3-16: what `draw` shows instead of nothing when the terminal is
/// shorter than the tab-bar-plus-one-body-row floor (`area.height < 2`) —
/// there's no room left for real chrome, but a blank screen with no
/// explanation is worse than one plain line. At zero rows there is no cell
/// to draw into, so the only safe move is to draw nothing; at exactly one
/// row, that row is spent on the notice. `Paragraph` clips without
/// wrapping — the same idiom `draw_hint_bar` leans on — so the message
/// degrades character by character as the terminal narrows, down to 1×1,
/// with no manual width arithmetic of its own to get wrong.
fn draw_too_small(f: &mut Frame, area: Rect) {
    if area.height == 0 || area.width == 0 {
        return;
    }
    f.render_widget(Paragraph::new("too small — resize").style(theme::ink()), area);
}

/// C9: Normal-mode hint pairs — exactly these seven; bindings the old
/// ten-pair list dropped (tab/undo/hide/quit) stay discoverable via
/// `Alt+?`. Every other mode's pairs are unchanged in content (restyled
/// only). Pure — no `Frame` — so the exact Normal-mode list pins down.
/// [Amended, C23] a focused-raw Normal pane shows exactly one pair instead
/// — every other hint would be a lie, since nothing else is intercepted;
/// checked ahead of `focused_dead` would be, but a dead pane can't be raw-
/// routed either way (`raw_routing_active` requires it alive), so the dead
/// branch stays first and wins when both happen to be true.
///
/// [Amended 2026-07-28, C15] `help_scrolled` says whether the keymap is
/// taller than its overlay right now. Only Help reads it, and it is the
/// difference between two *true* hint rows: on a terminal showing the whole
/// table every key really does close it; on a shorter one the arrows read on
/// instead, and a bar still promising "any key close" would be advertising a
/// dismissal the arrows no longer perform.
fn hint_pairs(
    mode: &Mode,
    focused_dead: bool,
    focused_raw: bool,
    help_scrolled: bool,
) -> Vec<(&'static str, &'static str)> {
    match mode {
        // C24: keyboard cursor + mouse drag, replacing the old two-pair list.
        // [Amended, U17] The mode's whole vocabulary is now on the bar: the
        // word motions and `V` are new, and `0`/`$` had existed since C24
        // while appearing in no hint and no help row — a binding nothing
        // advertises may as well not exist.
        Mode::Copy { .. } => vec![
            ("hjkl", "move"),
            ("w/b/e", "word"),
            ("0/$", "ends"),
            ("v/V", "mark"),
            ("y/↵", "yank"),
            // [Amended, U19] `o` opens the URL under the cursor — the
            // keyboard half of Alt+click.
            ("o", "open"),
            ("drag", "select"),
            ("Esc", "exit"),
        ],
        Mode::Help { .. } if help_scrolled => {
            vec![("↑↓ PgUp/Dn", "read on"), ("any other key", "close")]
        }
        Mode::Help { .. } => vec![("Alt+?", "all keys"), ("any key", "close")],
        // [Amended, U16] `←→` joins the list once there is a cursor to move:
        // a text field whose caret is stuck at the end is one nobody tries
        // to move, and the motion was the half of U16 left unimplemented.
        // 45 columns.
        Mode::Rename { target, .. } => {
            let what = match target {
                RenameTarget::Pane => "pane name",
                RenameTarget::Tab => "tab name",
            };
            vec![("type", what), ("←→", "move"), ("↵", "save"), ("Esc", "cancel")]
        }
        // [Amended, U20] `1..9` joins the list: the accelerator is the
        // fastest way through the picker and the only one that wasn't
        // advertised anywhere. [Re-amended, U20 second half] and so do the
        // type-ahead and the cwd column — 71 columns, inside the floor.
        // `j/k` are gone from the picker (they are filter text now) and
        // were never on this bar, so nothing advertised was lost.
        Mode::Picker { .. } => vec![
            ("↑↓", "choose"),
            ("↵", "open"),
            ("1..9", "launch"),
            ("type", "filter"),
            ("←→", "dir"),
            ("Esc", "cancel"),
        ],
        // [Amended, P21] `/ search` and `n/N next` join the list: scroll
        // mode is where a search starts and where its hits are walked, and
        // an unadvertised key is an absent one. 59 columns — comfortably
        // inside the 100-col floor alongside the right segment.
        Mode::Scroll => vec![
            ("↑↓", "scroll"),
            ("PgUp/Dn", "page"),
            ("/", "search"),
            ("n/N", "next"),
            ("Esc", "exit"),
        ],
        // [Added, P21] The search prompt's own list. `↵ keep` and
        // `Esc cancel` are the two exits and lead the yield order; the
        // hits-walking pair trails because it is the one that keeps
        // working after the prompt closes (and is advertised again on the
        // Scroll list once it does).
        Mode::Search { .. } => {
            vec![("type", "filter"), ("↵", "keep"), ("Esc", "cancel"), ("n/N", "next")]
        }
        // [Amended, U25] The feed's own working keys were missing from its
        // own hint: PgUp/PgDn paged and `q` closed, both unadvertised, and
        // Enter is new. A mode whose hint omits its keys has no keys.
        Mode::Feed { .. } => vec![
            ("↑↓", "select"),
            ("PgUp/Dn", "page"),
            ("↵", "go to pane"),
            ("q/Esc", "close"),
        ],
        // [Added, C27] The roster's own key set — 68 columns, inside the
        // 100-col floor beside the right segment. `q` is deliberately absent
        // where the feed has it: this list filters as you type, so a letter
        // is filter text (U20's rule), and `Esc` is the way out. [Amended,
        // ux P2-11] `Tab status` joins it (81 columns, still inside the
        // floor) — the one narrowing key that isn't filter text.
        Mode::Roster { .. } => vec![
            ("↑↓", "select"),
            ("PgUp/Dn", "page"),
            ("↵", "go to pane"),
            ("type", "filter"),
            ("Tab", "status"),
            ("Esc", "close"),
        ],
        Mode::Normal if focused_dead => {
            vec![("↵", "relaunch"), ("f", "fresh — drops resume"), ("Alt+w", "close"), ("Alt+q", "quit")]
        }
        Mode::Normal if focused_raw => vec![("Alt+Shift+p", "exit raw")],
        // [Amended, U6] Same seven pairs, reordered: pairs drop whole from
        // the right, so `Alt+? keys` leads (it is the last thing to yield —
        // it's the way to everything that already dropped) and `Alt+r
        // rename` trails (first to go — the one pair whose absence costs a
        // user nothing they can't find under Alt+?).
        Mode::Normal => vec![
            ("Alt+?", "keys"),
            ("Alt+n", "new"),
            ("Alt+↵", "launch"),
            ("Alt+s", "stack"),
            ("Alt+←↓↑→", "focus"),
            ("Alt+w", "close"),
            ("Alt+r", "rename"),
        ],
    }
}

/// C9's right-segment uppercase mode word. A real non-Normal mode always
/// wins; in Normal, `RAW` (C23) beats `ZOOM` (C21) beats `NORMAL` — input
/// safety (knowing you're raw) trumps view state.
fn mode_word(mode: &Mode, zoomed: bool, raw: bool) -> &'static str {
    match mode {
        Mode::Normal if raw => "RAW",
        Mode::Normal if zoomed => "ZOOM",
        Mode::Normal => "NORMAL",
        Mode::Rename { .. } => "RENAME",
        Mode::Picker { .. } => "PICKER",
        Mode::Scroll => "SCROLL",
        Mode::Copy { .. } => "COPY",
        Mode::Help { .. } => "HELP",
        Mode::Feed { .. } => "FEED",
        Mode::Roster { .. } => "ROSTER",
        Mode::Search { .. } => "SEARCH",
    }
}

/// C9's right-aligned segment: the aggregate "◆ N needs you · Alt+a" —
/// omitted at `n == 0` rather than shown as a hollow "0 needs you" — then
/// (P21) the search prompt, then (Scroll/Search, U3) the dim position, then
/// the uppercase mode word, then one trailing space. Everything rides inside
/// the segment so C9's fit/yield machinery covers it for free: pairs drop
/// whole before any of it clips. Pure so the omission rules are
/// unit-testable without a `Frame`.
///
/// [P21] `query` is the live search prompt (`/foo`) and is the one token on
/// this bar drawn in `ink`: it is text the user is typing right now, and
/// quiet input is input you cannot proofread. `position` carries `↑N/M` in
/// Scroll mode and the `i/n` hit counter while searching — both `quiet`, both
/// the same "where am I" role.
fn hint_bar_right_spans(
    n: usize,
    query: Option<String>,
    position: Option<String>,
    word: &str,
) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    if n > 0 {
        spans.push(Span::styled(
            format!("◆ {n} needs you · Alt+a"),
            theme::accent(),
        ));
        spans.push(Span::raw("  "));
    }
    if let Some(q) = query {
        spans.push(Span::styled(format!("{q} "), theme::ink()));
    }
    if let Some(pos) = position {
        spans.push(Span::styled(format!("{pos} "), theme::quiet()));
    }
    spans.push(Span::styled(word.to_string(), theme::quiet()));
    spans.push(Span::raw(" "));
    spans
}

/// P21: the search prompt and hit counter for C9's right segment — `/query`
/// (with the `▏` insertion caret the rename dialog already uses, so a typed
/// prompt looks like a text field everywhere in roost) and `i/n`, or `0/0`
/// when nothing matches. `None` for both outside a search. Pure.
fn search_segment(search: Option<&Search>) -> (Option<String>, Option<String>) {
    let Some(s) = search else { return (None, None) };
    let counter = if s.matches.is_empty() {
        "0/0".to_string()
    } else {
        format!("{}/{}", s.current + 1, s.matches.len())
    };
    (Some(format!("/{}{}", s.query, theme::RENAME_CURSOR)), Some(counter))
}

/// Zellij-style shortcut bar. Mode-aware: the keys shown match what you can
/// actually press right now. Precedence (C9): alt-warning, then flash, then
/// the hint pairs — each takes over the whole bar from the next.
/// Rendered column width of one hint pair. THE single source both the fit
/// calculation and the actual draw use, so they can never drift (a +3/+4
/// mismatch once dropped the whole right segment at widths 111–116).
/// Matches the span strings in `draw_hint_bar`: `" {key} "` (key + 2) plus
/// `"{label}  "` (label + 2).
fn hint_pair_cols(key: &str, label: &str) -> u16 {
    (key.chars().count() + label.chars().count() + 4) as u16
}

/// How many leading hint pairs fit alongside the right segment (C9 yield
/// order: pairs drop whole, from the right, before the segment ever yields).
fn fit_hint_pairs(hints: &[(&'static str, &'static str)], right_w: u16, width: u16) -> usize {
    let budget = width.saturating_sub(right_w);
    let mut used = 0u16;
    for (i, (key, label)) in hints.iter().enumerate() {
        let w = hint_pair_cols(key, label);
        if used + w > budget {
            return i;
        }
        used += w;
    }
    hints.len()
}

fn draw_hint_bar<B: PaneBackend>(f: &mut Frame, app: &App<B>, area: Rect) {
    if app.show_alt_hint() {
        // C11/U4: same bar, per-terminal wording — the app knows the host's
        // TERM_PROGRAM and picks the real menu path where there is one. A
        // problem bar, so the red-tinted reversal, not the neutral one.
        f.render_widget(
            Paragraph::new(app.alt_hint_line()).style(theme::attention_problem()),
            area,
        );
        return;
    }

    // A transient action result (e.g. "copied") takes over the bar briefly.
    if let Some(msg) = app.flash() {
        f.render_widget(
            Paragraph::new(format!(" {msg} ")).style(theme::attention()),
            area,
        );
        return;
    }

    // (key, what it does) pairs for the current context: key `accent`, label
    // `quiet`, no chip bg. The right segment (aggregate + mode word) WINS over
    // the pairs (C9 yield order): the mode word is a modal-safety affordance
    // and "◆ N needs you" the fleet's primary signal — trailing pairs drop
    // whole until the segment fits.
    let focused_raw = app.is_raw(app.focused);
    let (help_visible, help_total) = help_scroll_extent(app.body_area());
    let hints =
        hint_pairs(&app.mode, app.focused_dead(), focused_raw, help_visible < help_total);
    // U3: Scroll mode's right segment shows where in history the view sits
    // — `↑N/M` from the backend's grid-clamped (offset, banked) pair, so it
    // can never report a phantom row the grid refused (U9's overshoot).
    // P21: while the prompt is up the segment carries the query and the hit
    // counter instead — the search *is* the position report, and stacking
    // `↑N/M` beside `3/17` would be two answers to one question.
    let (query, position) = match &app.mode {
        Mode::Search { .. } => search_segment(app.search.as_ref()),
        Mode::Scroll => {
            let (off, total) = app.scroll_position();
            (None, Some(format!("{}{off}/{total}", theme::SCROLLED)))
        }
        _ => (None, None),
    };
    let right = hint_bar_right_spans(
        app.needs_input_count(),
        query,
        position,
        mode_word(&app.mode, app.zoomed(), focused_raw),
    );
    let right_w: u16 = right.iter().map(|s| s.content.chars().count() as u16).sum();

    let shown = fit_hint_pairs(&hints, right_w, area.width);
    let mut spans: Vec<Span> = Vec::with_capacity(shown * 2 + 4);
    let mut used = 0u16;
    for (key, label) in &hints[..shown] {
        used += hint_pair_cols(key, label); // same source as fit_hint_pairs
        spans.push(Span::styled(format!(" {key} "), theme::accent()));
        spans.push(Span::styled(format!("{label}  "), theme::quiet()));
    }
    if used.saturating_add(right_w) <= area.width {
        let pad = area.width.saturating_sub(used).saturating_sub(right_w);
        spans.push(Span::raw(" ".repeat(pad as usize)));
        spans.extend(right);
    }

    // Paragraph truncates (no wrap) so a narrow terminal just clips the
    // tail. No row fill: the bar is ink on the terminal's own paper (§2
    // background policy), so every cell no span touches keeps the user's
    // background instead of a band roost painted.
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// Floating rect of the given size, centered on `anchor` (the focused pane)
/// but clamped to fully fit inside `bounds` — so a dialog near the screen
/// edge still lands on-screen instead of centering blindly on the whole
/// terminal.
fn centered_near(anchor: Rect, bounds: Rect, width: u16, height: u16) -> Rect {
    let w = width.min(bounds.width);
    let h = height.min(bounds.height);
    let cx = anchor.x + anchor.width / 2;
    let cy = anchor.y + anchor.height / 2;
    let x = cx.saturating_sub(w / 2).clamp(bounds.x, bounds.x + bounds.width - w);
    let y = cy.saturating_sub(h / 2).clamp(bounds.y, bounds.y + bounds.height - h);
    Rect::new(x, y, w, h)
}

/// Dim every cell in `body` outside `dialog` so a floating overlay reads as a
/// distinct modal layer sitting on top of the panes, not more pane chrome.
fn dim_backdrop(f: &mut Frame, body: Rect, dialog: Rect) {
    let buf = f.buffer_mut();
    for y in body.y..body.y + body.height {
        for x in body.x..body.x + body.width {
            let inside_dialog =
                x >= dialog.x && x < dialog.x + dialog.width && y >= dialog.y && y < dialog.y + dialog.height;
            if inside_dialog {
                continue;
            }
            if let Some(cell) = buf.cell_mut((x, y)) {
                let style = cell.style().add_modifier(Modifier::DIM);
                cell.set_style(style);
            }
        }
    }
}

/// Border style for floating dialogs (C12): the one look all three modals
/// share — a modal is the focused interaction surface, so it takes the
/// focus color; the dimmed backdrop (below) keeps it from being confused
/// with the focused pane's own accent border. No BOLD (§2 bold policy).
fn dialog_border_style() -> Style {
    theme::accent()
}

/// C12's title style: regular weight, primary ink — shared by all three
/// modals so there's exactly one place that sets it.
fn dialog_title(text: &'static str) -> Line<'static> {
    Line::from(text).style(theme::ink())
}

/// C14 (U20): a picker row's text after its 1-column marker — the `1..9`
/// accelerator, then the adapter id. Items past the ninth get no digit
/// (there is no `Alt+10`), but keep the columns so the ids stay aligned.
/// Pure, and shared by the selected and unselected arms so the two rows can
/// never differ by anything but their style.
fn picker_row_body(i: usize, item: &str) -> String {
    match i {
        0..=8 => format!(" {} {item}", i + 1),
        _ => format!("   {item}"),
    }
}

/// C13 (U16): the rename field's rendered text — the buffer with the `▏`
/// caret sitting *at* the insertion point rather than always at the end.
/// `cursor` is a char index and is clamped, so a stale value (a resize
/// between keystrokes, a paste that shortened the buffer) renders the caret
/// at the end instead of panicking on a bad slice. Pure so the caret's
/// placement has a unit-test seam.
fn rename_field(buffer: &str, cursor: usize) -> String {
    let at = cursor.min(buffer.chars().count());
    let byte = buffer.char_indices().nth(at).map_or(buffer.len(), |(b, _)| b);
    format!("{}{}{}", &buffer[..byte], theme::RENAME_CURSOR, &buffer[byte..])
}

/// C14 (U20): one picker row's leading marker and text style, given whether
/// it is its column's selection and whether that column has the keyboard.
/// Three states, no fourth: the focused column's selection wears `❯` in
/// `accent` with `ink` text (C14's idiom, unchanged); the *other* column's
/// selection keeps `ink` text without the marker — what a launch would use
/// has to stay readable from either side — and everything else is `quiet`.
/// Pure so the three states are pinned without a `Frame`.
fn row_marks(selected: bool, column_focused: bool) -> (Span<'static>, Style) {
    match (selected, column_focused) {
        (true, true) => {
            (Span::styled(theme::PICKER_SELECTED.to_string(), theme::accent()), theme::ink())
        }
        (true, false) => (Span::raw(" "), theme::ink()),
        _ => (Span::raw(" "), theme::quiet()),
    }
}

/// C14 (U20): the picker's cwd column entry for `path` — the last two path
/// components, so `~/src/roost/vendor` reads as `roost/vendor` and a column
/// of sibling checkouts stays distinguishable without spending the width a
/// full path would. The root and single-component paths render whole.
fn picker_cwd_label(path: &std::path::Path) -> String {
    let parts: Vec<String> =
        path.components().map(|c| c.as_os_str().to_string_lossy().to_string()).collect();
    match parts.len() {
        0 => "/".to_string(),
        1 => parts[0].clone(),
        // A top-level directory's parent IS the root, so joining blindly
        // would spell `/tmp` as `//tmp`.
        n if parts[n - 2] == "/" => format!("/{}", parts[n - 1]),
        n => format!("{}/{}", parts[n - 2], parts[n - 1]),
    }
}

/// C14 (U20): the picker dialog's width — the adapter column (a fixed 16,
/// as before the cwd column existed) plus the widest cwd label, plus the
/// gap and the two border columns. `centered_near` still clamps it to the
/// screen. Pure so the sizing and the drawing can't drift.
fn picker_dialog_width(cwds: &[std::path::PathBuf]) -> u16 {
    const ADAPTER_COL: u16 = 16;
    let widest = cwds
        .iter()
        .map(|p| mouse::display_width(&picker_cwd_label(p)))
        .max()
        .unwrap_or(0);
    // Never narrower than the pre-U20 dialog: a column of short labels must
    // not make the picker *shrink* relative to the one it replaced.
    const MIN: u16 = 32;
    if widest == 0 {
        return MIN;
    }
    (ADAPTER_COL + 2 + widest + 2).max(MIN)
}

/// The help overlay's key-column prefix: the key label left-padded to a
/// fixed column so every description lines up underneath it. Shared by the
/// width computation and the row-rendering loop below so they can't drift.
fn help_key_prefix(key: &str) -> String {
    format!(" {key:<18}")
}

/// C15 (amended 2026-07-28): the keymap's content width — the widest row
/// (key column + description). One *column* of it; the overlay may draw two
/// side by side, which `help_layout` decides. Pure so the sizing has a
/// unit-test seam.
fn help_content_width() -> u16 {
    HELP_GROUPS
        .iter()
        .flat_map(|g| g.rows.iter())
        .map(|(k, d)| mouse::display_width(&help_key_prefix(k)) + mouse::display_width(d))
        .max()
        .unwrap_or(0)
}

/// C15: one line of the drawn keymap — a group heading or a chord row.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum HelpLine {
    Head(&'static str),
    Row(&'static str, &'static str),
}

/// C15: the keymap flattened into drawable lines, in group order — each
/// heading followed by its rows, with no blank between groups. The
/// underlined heading is the separator (C6's idiom, exactly as C27's roster
/// stacks its groups), and a blank row per group would cost six rows of a
/// table that already has to scroll on a short terminal.
fn help_lines() -> Vec<HelpLine> {
    let mut out = Vec::new();
    for g in HELP_GROUPS {
        out.push(HelpLine::Head(g.title));
        out.extend(g.rows.iter().map(|(k, d)| HelpLine::Row(k, d)));
    }
    out
}

/// C15: how the keymap lays out in `body` — how many columns, how the lines
/// divide between them, and the dialog those choices need.
struct HelpLayout {
    /// One `Vec<HelpLine>` per column, left to right.
    columns: Vec<Vec<HelpLine>>,
    /// Rows of content each column shows at once (the shorter of "what it
    /// holds" and "what fits"); the scroll step and the `top` clamp use it.
    height: u16,
    /// The dialog rect's size, borders included.
    size: (u16, u16),
}

/// C15 (amended 2026-07-28): lay the keymap out for `body`, taking **the
/// fewest columns that fit**.
///
/// One column is the calm form and stays the answer whenever the list fits
/// in the body's height. A second column is taken only when one column
/// would overflow *and* the terminal is wide enough to hold two — never to
/// fill space for its own sake. When neither fits, the overlay scrolls
/// (C15's amended key rules); nothing is dropped and nothing is merged away
/// to fit a cap, which is the whole point of this shape.
///
/// The split point is a group boundary, chosen to balance the two columns,
/// so a group is never sawn in half across the gutter.
fn help_layout(body: Rect) -> HelpLayout {
    let lines = help_lines();
    let content = help_content_width();
    let avail_h = body.height.saturating_sub(2); // the dialog's borders
    let one_fits = lines.len() as u16 <= avail_h;
    let two_wide_enough = content * 2 + HELP_GUTTER + 2 <= body.width;
    let columns = if one_fits || !two_wide_enough {
        vec![lines]
    } else {
        let at = help_split_point(&lines);
        let (a, b) = lines.split_at(at);
        vec![a.to_vec(), b.to_vec()]
    };
    let tallest = columns.iter().map(|c| c.len()).max().unwrap_or(0) as u16;
    let height = tallest.min(avail_h);
    // +1 so a row that spends the full content width still has a column of
    // air before the right border (the key column already opens with one).
    let w = content * columns.len() as u16 + HELP_GUTTER * (columns.len() as u16 - 1) + 3;
    HelpLayout { columns, height, size: (w.min(body.width), height + 2) }
}

/// Columns are separated by this many blank cells.
const HELP_GUTTER: u16 = 2;

/// C15: draw the laid-out keymap into the dialog's inner area, every column
/// scrolled to the same `top` (they scroll as one sheet — two columns
/// sliding independently would be two lists, not one table).
///
/// A heading borrows C6's idiom, the same one the roster's group rows use:
/// uppercase, `quiet()`, underlined across its own column so it reads as a
/// rule rather than as another chord.
fn draw_help_columns(f: &mut Frame, layout: &HelpLayout, top: usize, inner: Rect) {
    let content = help_content_width();
    for (i, column) in layout.columns.iter().enumerate() {
        let x = inner.x + i as u16 * (content + HELP_GUTTER);
        if x >= inner.x + inner.width {
            break;
        }
        let width = content.min(inner.x + inner.width - x);
        let lines: Vec<Line> = column
            .iter()
            .skip(top)
            .take(layout.height as usize)
            .map(|line| match line {
                HelpLine::Head(title) => {
                    let pad = (width as usize).saturating_sub(mouse::display_width(title) as usize + 1);
                    Line::from(Span::styled(
                        format!(" {title}{}", " ".repeat(pad)),
                        theme::quiet().add_modifier(Modifier::UNDERLINED),
                    ))
                }
                HelpLine::Row(k, d) => Line::from(vec![
                    Span::styled(help_key_prefix(k), theme::accent()),
                    Span::styled(d.to_string(), theme::quiet()),
                ]),
            })
            .collect();
        f.render_widget(
            Paragraph::new(lines),
            Rect::new(x, inner.y, width, inner.height),
        );
    }
}

/// C15: `(rows shown at once, rows the tallest column holds)` for `body` —
/// the one place `App`'s scroll clamp and this renderer agree about how far
/// the keymap can move. Equal values mean the whole table is on screen and
/// the scroll keys have nothing to do (`Mode::Help`'s "any key closes it"
/// then holds unamended).
pub fn help_scroll_extent(body: Rect) -> (usize, usize) {
    let l = help_layout(body);
    (l.height as usize, l.columns.iter().map(|c| c.len()).max().unwrap_or(0))
}

/// C15: the group boundary that divides `lines` most evenly between two
/// columns — i.e. the heading nearest the halfway mark. Headings only:
/// splitting inside a group would put a heading in one column and half its
/// chords in the other, which reads as two unrelated tables.
fn help_split_point(lines: &[HelpLine]) -> usize {
    let half = lines.len() / 2;
    lines
        .iter()
        .enumerate()
        .filter(|(i, l)| *i > 0 && matches!(l, HelpLine::Head(_)))
        .min_by_key(|(i, _)| i.abs_diff(half))
        .map(|(i, _)| i)
        .unwrap_or(lines.len())
}

/// C15: one titled block of the keymap.
struct HelpGroup {
    title: &'static str,
    rows: &'static [(&'static str, &'static str)],
}

/// C15/§8 (amended 2026-07-28): the canonical key table, **grouped by what
/// the chord acts on**, plus U23's reference block. The single source
/// `draw_mode_overlay` reads for `Mode::Help`.
///
/// History, because the shape is the point. U23 (2026-07-27) added the
/// glyph legend and the mouse rows and paid for them by *merging three
/// natural chord pairs* — the only currency available under a ≤20-row hard
/// cap. C28 then wanted two more chords, and merging again would have put
/// four chords on one row. The cap is gone instead: the overlay takes a
/// second column when the terminal is wide enough and scrolls when it is
/// not (`help_layout`), so the keymap can grow with roost rather than
/// rationing itself. Unmerged rows are also plainly easier to read —
/// "Alt+z / Alt+f → zoom pane (view only) / floating scratch shell" made
/// the reader pair the halves up themselves.
///
/// Group order is "how often you reach for it": panes, then their layout,
/// then tabs, then the fleet surfaces, then reading, then the session.
///
/// [Amended, ux P2-15] `CONTROL CLI` closes the table. Every group above it
/// teaches keys pressed *inside* roost; this one is the odd surface out —
/// the reference block for `roost send`/`read`/`status`/`spawn`/`wait`, the
/// control CLI an outside actor (an LLM, a script, another pane) drives the
/// fleet with. Before this the whole surface was invisible from inside the
/// product, even though U2 put the pane id on every badge *specifically* as
/// the join key between what's on screen and what a caller types — a join
/// key nothing on screen ever named. Rows are the verbs a caller reaches
/// for, not a man page: the CLI's own `--help` covers `list`/`fork`/`close`
/// and every flag. It sorts last for the same reason `READING THE SCREEN`
/// does — it teaches the product rather than a binding — and follows it
/// rather than leading, since a user opens this overlay to remember a
/// chord first and learns the fleet is scriptable second.
const HELP_GROUPS: &[HelpGroup] = &[
    HelpGroup {
        title: "PANES",
        rows: &[
            ("Alt+n", "new shell pane (auto split)"),
            ("Alt+Enter", "picker: 1..9 launch · type filters · ←→ recent cwd"),
            ("Alt+←↓↑→ / hjkl", "move focus"),
            ("Alt+r", "rename this pane"),
            ("Alt+w", "close pane (confirm if busy)"),
            ("Alt+u", "reopen the last pane or tab you closed"),
            ("Alt+Shift+p", "raw pass-through for this pane (same chord exits)"),
        ],
    },
    HelpGroup {
        title: "LAYOUT",
        rows: &[
            ("Alt+Shift+←↓↑→", "resize along that axis"),
            ("Alt+s", "toggle split ⇄ stack"),
            ("Alt+o", "flip this split's orientation"),
            ("Alt+g", "cycle layout: grid / main+stack / all-stack"),
            ("Alt+z", "zoom the focused pane (view only)"),
            ("Alt+f", "floating scratch shell"),
        ],
    },
    HelpGroup {
        title: "TABS",
        rows: &[
            ("Alt+t", "new tab"),
            ("Alt+1..9 / Alt+0", "go to that tab / the last one"),
            ("Alt+i / Alt+m", "previous / next tab (wraps)"),
            // C28: the shifted siblings sit directly under the chords they
            // are the shifted form of — the pairing *is* the explanation.
            ("Alt+Shift+i / +m", "move this pane to the previous / next tab"),
            ("Alt+Shift+r", "rename this tab"),
        ],
    },
    HelpGroup {
        title: "FLEET",
        rows: &[
            ("Alt+a", "jump to the next pane that needs you"),
            ("Alt+Shift+a", "roster: every pane, grouped by tab · Tab filters by status"),
            ("Alt+e", "activity feed (status / spawns / exits / control)"),
        ],
    },
    HelpGroup {
        title: "READING",
        rows: &[
            ("Alt+PgUp", "scroll back — `/` searches, n/N step the hits"),
            ("Alt+c", "copy mode: hjkl wbe 0$ · v/V select · y yank · o URL"),
        ],
    },
    HelpGroup {
        title: "SESSION",
        rows: &[
            ("Alt+/", "toggle the hint bar"),
            ("Alt+?", "this keymap"),
            ("Alt+q", "quit (workspace saved; sessions live)"),
        ],
    },
    HelpGroup {
        title: "READING THE SCREEN",
        rows: &[
            ("status", "● working ◆ needs you ○ waiting · idle ✕ exited"),
            // D2 (PR #46 design audit, C29 amendment): widened for native
            // selection — a chord (Shift+click) gets a row here by C15's
            // own stated rule ("Alt+click gets its own row because it is a
            // chord"), applied to the mouse verb that is now also one.
            ("mouse", "wheel scrolls · click focuses · drag/2x/3x/shift selects"),
            ("Alt+click / o", "open the URL under the pointer / copy cursor"),
        ],
    },
    HelpGroup {
        title: "CONTROL CLI",
        rows: &[
            ("<id>", "same id shown on each pane's badge"),
            ("roost send <id> \"text\"", "type into that pane (--enter submits)"),
            ("roost read <id>", "print its current screen"),
            ("roost status", "list every pane and its status"),
            ("roost spawn ADAPTER", "launch a new pane"),
            ("roost wait <id>", "block until its turn ends"),
        ],
    },
];

/// U23: the legend row's text, rebuilt from the `theme` glyph constants —
/// the same table C5 renders everywhere else. `HELP_KEYS` must spell a
/// `const` literal, so this is the drift *check* rather than the source
/// (`help_legend_row_matches_the_theme_glyph_table`), which is why it is
/// test-only: retheming a glyph has to break a test, not the build.
#[cfg(test)]
fn status_legend_text() -> String {
    format!(
        "{} working {} needs you {} waiting {} idle {} exited",
        theme::GLYPH_WORKING,
        theme::GLYPH_NEEDS_INPUT,
        theme::GLYPH_WAITING,
        theme::GLYPH_IDLE,
        theme::GLYPH_EXITED,
    )
}

/// U8: the rect the current mode's modal dialog occupies on screen, or None
/// when the mode draws none — the SAME geometry `draw_mode_overlay` paints
/// (it reads this function too), exposed so the mouse path can hit-test
/// against exactly what's drawn. Renderer/hitbox lockstep, §4/§5.
pub fn modal_rect<B: PaneBackend>(app: &App<B>) -> Option<Rect> {
    let body = app.body_area();
    let anchor = app
        .display_rects()
        .iter()
        .find(|pr| pr.id == app.focused)
        .map(|pr| pr.rect)
        .unwrap_or(body);
    dialog_rect(&app.mode, body, anchor, app.picker_filtered().len(), app.picker_cwds())
}

/// The pure half of `modal_rect`: mode + geometry in, dialog rect out.
/// [U20] The picker's size now depends on app state (the type-ahead filter's
/// row count and the recent-cwd column), so it takes those as `rows`/`cwds`
/// rather than re-deriving them — `modal_rect` and `draw_mode_overlay` pass
/// the same values, keeping the drawn dialog and the mouse hitbox in
/// lockstep (§4/§5).
fn dialog_rect(
    mode: &Mode,
    body: Rect,
    anchor: Rect,
    rows: usize,
    cwds: &[std::path::PathBuf],
) -> Option<Rect> {
    match mode {
        // Copy mode has no centered overlay — the cursor/selection are
        // drawn in-pane (C17/C24). [P21] Nor does search: its prompt lives
        // in the hint bar's right segment, so the pane it is searching
        // stays fully visible while the query narrows.
        Mode::Normal | Mode::Scroll | Mode::Copy { .. } | Mode::Search { .. } => None,
        Mode::Rename { .. } => Some(centered_near(anchor, body, 44, 3)),
        Mode::Picker { .. } => {
            // U20: as tall as the longer of the two columns (a filter can
            // shrink the adapter side below the cwd side), never shorter
            // than one row — an empty result still needs a frame to say so.
            let h = rows.max(cwds.len()).max(1) as u16 + 2;
            Some(centered_near(anchor, body, picker_dialog_width(cwds), h))
        }
        Mode::Help { .. } => {
            let (w, h) = help_layout(body).size;
            Some(centered_near(anchor, body, w, h))
        }
        Mode::Feed { .. } => {
            let (w, h) = feed_overlay_size(body);
            Some(centered_near(anchor, body, w, h))
        }
        // C27: deliberately the feed's own geometry — the two overlays
        // answer the fleet's two questions and should not resize under a
        // user toggling between them.
        Mode::Roster { .. } => {
            let (w, h) = roster_overlay_size(body);
            Some(centered_near(anchor, body, w, h))
        }
    }
}

/// `pulse` is the frame's one shared C5 phase read (see `draw`) — the roster
/// draws status glyphs, and a second `app.elapsed()` sample here could split a
/// frame across both phases.
fn draw_mode_overlay<B: PaneBackend>(
    f: &mut Frame,
    app: &App<B>,
    body: Rect,
    anchor: Rect,
    pulse: Style,
) {
    let Some(rect) =
        dialog_rect(&app.mode, body, anchor, app.picker_filtered().len(), app.picker_cwds())
    else {
        return;
    };
    match &app.mode {
        Mode::Normal | Mode::Scroll | Mode::Copy { .. } | Mode::Search { .. } => {}
        Mode::Rename { buffer, cursor, target } => {
            dim_backdrop(f, body, rect);
            f.render_widget(Clear, rect);
            let heading = match target {
                RenameTarget::Pane => " rename pane ",
                RenameTarget::Tab => " rename tab ",
            };
            let block = Block::bordered()
                .title(dialog_title(heading))
                .border_type(BorderType::Plain)
                .border_style(dialog_border_style());
            let inner = block.inner(rect);
            f.render_widget(block, rect);
            f.render_widget(
                Paragraph::new(rename_field(buffer, *cursor)).style(theme::ink()),
                inner,
            );
        }
        Mode::Picker { selection, filter, cwd, on_cwd } => {
            let items = app.picker_filtered();
            let cwds = app.picker_cwds();
            dim_backdrop(f, body, rect);
            f.render_widget(Clear, rect);
            // [Amended, U20] The title carries the live type-ahead query, so
            // a narrowed list always says *why* it is narrow — a filtered
            // picker showing one row and no query would read as a picker
            // that lost its adapters.
            let heading = if filter.is_empty() {
                " new pane — pick agent ".to_string()
            } else {
                format!(" new pane — {filter}{} ", theme::RENAME_CURSOR)
            };
            let block = Block::bordered()
                .title(Line::from(Span::styled(heading, theme::ink())))
                .border_type(BorderType::Plain)
                .border_style(dialog_border_style());
            let inner = block.inner(rect);
            f.render_widget(block, rect);
            // C14: selected row is a `❯`-prefix + `ink` item text, no bg
            // highlight; unselected rows are plain `quiet` text. [Amended,
            // U20] Each row leads with its `1..9` accelerator — an
            // accelerator nothing shows is one nobody presses — and a
            // second column lists the recent working directories. The
            // column with focus marks its selection with `❯`; the other
            // shows its selection in `ink` without the marker, so what will
            // actually be launched is readable from either side.
            const ADAPTER_COL: usize = 16;
            let rows = items.len().max(cwds.len());
            let lines: Vec<Line> = (0..rows)
                .map(|i| {
                    let mut spans: Vec<Span> = Vec::with_capacity(3);
                    match items.get(i) {
                        Some(item) => {
                            let text = picker_row_body(i, item);
                            let pad = ADAPTER_COL.saturating_sub(mouse::display_width(&text) as usize + 1);
                            let (marker, style) = row_marks(i == *selection, !*on_cwd);
                            spans.push(marker);
                            spans.push(Span::styled(format!("{text}{}", " ".repeat(pad)), style));
                        }
                        None => spans.push(Span::raw(" ".repeat(ADAPTER_COL))),
                    }
                    if let Some(path) = cwds.get(i) {
                        let (marker, style) = row_marks(i == *cwd, *on_cwd);
                        spans.push(marker);
                        spans.push(Span::styled(format!(" {}", picker_cwd_label(path)), style));
                    }
                    Line::from(spans)
                })
                .collect();
            f.render_widget(Paragraph::new(lines), inner);
        }
        Mode::Help { top } => {
            // C15 (amended): the §8 key table, grouped, in as few columns as
            // fit and scrolled when even those don't. `HELP_GROUPS` is the
            // single source and `help_layout` the single geometry — the same
            // call `dialog_rect` above made for this rect.
            let layout = help_layout(body);
            let (visible, total) = (layout.height as usize, layout.columns.iter().map(|c| c.len()).max().unwrap_or(0));
            let top = (*top).min(total.saturating_sub(visible));
            dim_backdrop(f, body, rect);
            f.render_widget(Clear, rect);
            // The title says how to leave — and, only when the table doesn't
            // fit, that there is more of it and which keys reach it. A
            // terminal showing everything says nothing about scrolling,
            // because there is nothing to scroll.
            let heading = if total > visible {
                format!(" keys — {}/{} · ↑↓ more · any key closes ", (top + visible).min(total), total)
            } else {
                " keys — any key to close ".to_string()
            };
            let block = Block::bordered()
                .title(Line::from(heading).style(theme::ink()))
                .border_type(BorderType::Plain)
                .border_style(dialog_border_style());
            let inner = block.inner(rect);
            f.render_widget(block, rect);
            draw_help_columns(f, &layout, top, inner);
        }
        Mode::Feed { offset } => {
            dim_backdrop(f, body, rect);
            f.render_widget(Clear, rect);
            let block = Block::bordered()
                .title(dialog_title(" activity "))
                .border_type(BorderType::Plain)
                .border_style(dialog_border_style());
            let inner = block.inner(rect);
            f.render_widget(block, rect);
            draw_feed_entries(f, app.feed(), *offset, inner);
        }
        Mode::Roster { cursor, filter, status_filter, .. } => {
            dim_backdrop(f, body, rect);
            f.render_widget(Clear, rect);
            // The live query rides in the title, exactly as the picker's
            // does (U20): a list narrowed to two rows with an ordinary title
            // reads as a fleet that lost its panes. [Amended, ux P2-11] The
            // status filter joins it as a glyph tag, in that status's own C5
            // glyph *and* color (design-supervisor D4: a bare `ink()` glyph
            // read as no tier at all) — the same pairing the narrowed rows
            // themselves draw (`theme::status_style`) — so a narrowed list
            // always says *why* it is narrow, same as the type-ahead already
            // did. `ink()` everywhere else: the tag is the one thing this
            // title borrows color for.
            let glyph_span = status_filter.map(|s| {
                let (glyph, style, _) = theme::status_style(s);
                Span::styled(glyph.to_string(), style)
            });
            let mut spans: Vec<Span<'static>> = Vec::new();
            match (glyph_span, filter.is_empty()) {
                (None, true) => spans.push(Span::styled(" fleet ".to_string(), theme::ink())),
                (None, false) => {
                    spans.push(Span::styled(format!(" fleet — {filter}{} ", theme::RENAME_CURSOR), theme::ink()));
                }
                (Some(glyph), true) => {
                    spans.push(Span::styled(" fleet — ".to_string(), theme::ink()));
                    spans.push(glyph);
                    spans.push(Span::styled(" only ".to_string(), theme::ink()));
                }
                (Some(glyph), false) => {
                    spans.push(Span::styled(" fleet — ".to_string(), theme::ink()));
                    spans.push(glyph);
                    spans.push(Span::styled(format!(" {filter}{} ", theme::RENAME_CURSOR), theme::ink()));
                }
            }
            let block = Block::bordered()
                .title(Line::from(spans))
                .border_type(BorderType::Plain)
                .border_style(dialog_border_style());
            let inner = block.inner(rect);
            f.render_widget(block, rect);
            draw_roster_rows(f, app, *cursor, inner, pulse);
        }
    }
}

/// C27: the roster's visible rows inside the modal's inner area — tab group
/// headers in C6's underlined-label idiom, pane rows in C8's collapsed-row
/// format verbatim (through the very same `collapsed_row_spans`), each behind
/// the one leading column that carries the cursor marker.
fn draw_roster_rows<B: PaneBackend>(
    f: &mut Frame,
    app: &App<B>,
    cursor: layout::PaneId,
    inner: Rect,
    pulse: Style,
) {
    if inner.height == 0 || inner.width == 0 {
        return;
    }
    let (rows, top) = app.roster_view();
    if rows.is_empty() {
        // Only reachable through the type-ahead: the workspace always has at
        // least one pane. Same shape as the feed's empty state.
        let text = "no pane matches";
        let pad = inner.width.saturating_sub(text.chars().count() as u16) / 2;
        let y = inner.y + inner.height / 2;
        f.render_widget(
            Paragraph::new(format!("{}{text}", " ".repeat(pad as usize))).style(theme::quiet()),
            Rect::new(inner.x, y, inner.width, 1),
        );
        return;
    }
    let lines: Vec<Line> = rows
        .iter()
        .skip(top)
        .take(inner.height as usize)
        .map(|row| Line::from(roster_row_spans(app, row, inner.width, cursor, pulse)))
        .collect();
    f.render_widget(Paragraph::new(lines), inner);
}

/// C27: one roster row's spans.
///
/// A group header is C6's idiom — an uppercase label, `quiet()`, every cell
/// of the row underlined (the text is padded to the full width so the rule
/// runs edge to edge, exactly as `stack_header_text` does it).
///
/// A pane row is `marker + C8's collapsed row`. The leading column is the
/// cursor (`❯`, the picker/feed idiom) and the C8 row keeps its own `▎` for
/// the *focused* pane, so the roster answers "which row will Enter act on"
/// and "where am I right now" with two different marks instead of
/// overloading one.
fn roster_row_spans<B: PaneBackend>(
    app: &App<B>,
    row: &RosterRow,
    width: u16,
    cursor: layout::PaneId,
    pulse: Style,
) -> Vec<Span<'static>> {
    match row {
        RosterRow::Group { label } => {
            let pad = (width as usize).saturating_sub(mouse::display_width(label) as usize);
            vec![Span::styled(
                format!("{label}{}", " ".repeat(pad)),
                theme::quiet().add_modifier(Modifier::UNDERLINED),
            )]
        }
        RosterRow::Pane { id } => {
            let id = *id;
            let spec = app.find_spec(id);
            // `display_status`, not `runtimes` directly: the roster is the
            // one surface that lists panes in tabs the lazy spawn has never
            // started, and `None` is what the tab bar reads as `·` for the
            // very same tab in the very same frame. [P2-10] Also the one
            // place a quiet shell's `Waiting` reads `Idle`, same as every
            // other chrome surface.
            let status = app.display_status(id);
            let has_title = spec.and_then(|s| s.title.as_ref()).is_some();
            let adapter = spec.map(|s| s.adapter.clone()).unwrap_or_else(|| "?".into());
            let name = if spec.is_some() { app.display_name(id) } else { "?".into() };
            let (_, base, pulses) = row_status_style(status);
            let glyph_style = if pulses { pulse } else { base };
            let marker = if id == cursor {
                Span::styled(theme::PICKER_SELECTED.to_string(), theme::accent())
            } else {
                Span::raw(" ")
            };
            let mut spans = vec![marker];
            spans.extend(collapsed_row_spans(
                width.saturating_sub(1),
                app.focused == id,
                status,
                id,
                &name,
                &adapter,
                has_title,
                app.is_raw(id),
                glyph_style,
            ));
            spans
        }
    }
}

/// C20: the feed's visible entry rows inside the modal's inner area — newest
/// at the bottom, scrolled back by `offset` entries from the tail; a single
/// centered line when the ring is empty.
fn draw_feed_entries(f: &mut Frame, feed: &VecDeque<FeedEntry>, offset: usize, inner: Rect) {
    if inner.height == 0 {
        return;
    }
    if feed.is_empty() {
        let text = "no activity yet";
        let pad = inner.width.saturating_sub(text.chars().count() as u16) / 2;
        let y = inner.y + inner.height / 2;
        f.render_widget(
            Paragraph::new(format!("{}{text}", " ".repeat(pad as usize))).style(theme::quiet()),
            Rect::new(inner.x, y, inner.width, 1),
        );
        return;
    }
    let range = feed_window(feed.len(), offset, inner.height as usize);
    // U25: the window's last row IS the selected entry (`feed_window` ends
    // at `len - 1 - offset`), so the marker can't drift from what Enter acts
    // on — both read the same number.
    let selected = range.end.saturating_sub(1);
    let lines: Vec<Line> = feed
        .iter()
        .enumerate()
        .skip(range.start)
        .take(range.len())
        .map(|(i, e)| {
            Line::from(feed_entry_spans(
                &local_hh_mm_ss(e.at),
                &e.text,
                e.needs_input,
                i == selected,
            ))
        })
        .collect();
    f.render_widget(Paragraph::new(lines), inner);
}

/// C20: which of the feed's `len` entries (0 = oldest .. `len` = newest+1)
/// fall inside a `rows`-tall window, given a scroll `offset` counting
/// entries back from the newest (0 = the live tail). Pure so the clamping is
/// unit-tested without a `Frame` or a real ring buffer.
fn feed_window(len: usize, offset: usize, rows: usize) -> std::ops::Range<usize> {
    if len == 0 || rows == 0 {
        return 0..0;
    }
    let offset = offset.min(len - 1);
    let last = len - 1 - offset;
    let first = last.saturating_sub(rows - 1);
    first..last + 1
}

/// C20's per-row rule: `" HH:MM:SS  {text}"`, timestamp and text both
/// `quiet` — except a status line landing on NeedsInput, which gets the `◆ `
/// `accent` prefix and `ink` text (the one red in the feed, same meaning as
/// everywhere, C5). Pure so the exception is unit-tested without a `Frame`.
/// [Amended, U25] The row's leading column is now a selection marker: `❯`
/// `accent` on the entry Enter would act on, a space on every other row. Same
/// `❯` idiom as the picker (C14), and it costs no columns — the leading
/// space was already there.
fn feed_entry_spans(
    hhmmss: &str,
    text: &str,
    needs_input: bool,
    selected: bool,
) -> Vec<Span<'static>> {
    let marker = if selected { theme::PICKER_SELECTED } else { ' ' };
    let mut spans = vec![
        Span::styled(marker.to_string(), theme::accent()),
        Span::styled(format!("{hhmmss}  "), theme::quiet()),
    ];
    if needs_input {
        spans.push(Span::styled(format!("{} ", theme::GLYPH_NEEDS_INPUT), theme::accent()));
        spans.push(Span::styled(text.to_string(), theme::ink()));
    } else {
        spans.push(Span::styled(text.to_string(), theme::quiet()));
    }
    spans
}

/// Local wall-clock `HH:MM:SS` for a feed entry's timestamp (C20). Uses libc
/// (already a dependency, see `Cargo.toml`) for the local-timezone
/// breakdown — the stdlib has no calendar conversion at all, and pulling in
/// a chrono/time crate would be a lot of dependency for three integers.
fn local_hh_mm_ss(t: std::time::SystemTime) -> String {
    let secs = t
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as libc::time_t)
        .unwrap_or(0);
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    unsafe { libc::localtime_r(&secs, &mut tm) };
    format!("{:02}:{:02}:{:02}", tm.tm_hour, tm.tm_min, tm.tm_sec)
}

/// C2: numbered tabs (marker + label + status glyph + separator) filling
/// the row edge-to-edge on the terminal's own background, plus a right-aligned
/// "{cwd} · {save}" status area. Column bookkeeping here is the renderer
/// half of the mouse-hitbox lockstep rule (DESIGN-ui.md §4/§5) —
/// `mouse::tab_width`/`tab_at_x` mirror this exactly and change together.
/// U15: the mode word the TAB BAR carries, if any. With the hint bar drawn
/// it carries the word itself and this is None; with the bar hidden (Alt+/)
/// or squeezed out by a short terminal, any word other than `NORMAL` — a
/// real mode, or the `ZOOM`/`RAW` pseudo-states — moves here. Otherwise a
/// zoomed pane with hints off is indistinguishable from a one-pane tab, and
/// RAW/COPY become unescapable-looking (live QA: `ZOOM` appeared nowhere).
/// Pure enough to hit-test against: the click path reads this too, so the
/// status area's width can't differ between the two.
pub fn tab_status_word<B: PaneBackend>(app: &App<B>) -> Option<&'static str> {
    if app.hints_shown() {
        return None; // C9's right segment already says it
    }
    let word = mode_word(&app.mode, app.zoomed(), app.is_raw(app.focused));
    (word != "NORMAL").then_some(word)
}

fn draw_tab_bar<B: PaneBackend>(f: &mut Frame, app: &App<B>, area: Rect, pulse: Style) {
    let cwd = app.focused_cwd();
    let saved = app.last_save_ok();
    let names: Vec<String> = app.ws.tabs.iter().map(|t| t.name.clone()).collect();
    let fit = mouse::status_fit(tab_status_word(app), cwd.as_deref(), saved, &names, area.width);
    let status_w = fit.map(|f| f.width).unwrap_or(0);
    let show_status = mouse::effective_status_width(&names, area.width, status_w) > 0;
    // U7: the drawn window — scrolled so the active tab is always visible.
    // `tab_at_x` reads the same layout, so hitboxes follow the scroll.
    let strip = mouse::tab_strip(&names, area.width, status_w, app.ws.active_tab);

    let mut spans: Vec<Span> = Vec::with_capacity(names.len() * 7 + 4);
    let mut used = 0u16;
    // A leading `…` when the strip has scrolled past earlier tabs (C2's
    // marker, now at whichever end is hiding tabs). It occupies column 0,
    // which is why `TabStrip::x0` exists.
    if strip.left_marker {
        spans.push(Span::styled(theme::TAB_OVERFLOW.to_string(), theme::quiet()));
        used += strip.x0;
    }
    // Left to right, one 9-part span group per tab (marker/label/glyph/count/
    // separator/gutter), stopping exactly where `tab_strip` says to.
    for (i, tab) in app.ws.tabs.iter().enumerate().take(strip.end).skip(strip.start) {
        let active = i == app.ws.active_tab;
        // [Amended 2026-07-28] ...and how many panes are in that state, so a
        // tab of three needy agents stops reading like a tab of one.
        let (summary, count) = app.tab_summary(i);
        let (glyph, base_style) = tab_summary_badge(summary);
        let glyph_style = if summary == TabSummary::Working { pulse } else { base_style };
        push_tab_spans(&mut spans, i, &tab.name, active, glyph, glyph_style, count);
        used += mouse::tab_width(i, &tab.name);
    }

    // ...and a trailing `…` when tabs remain past the right edge and a
    // spare column is left to show it in (overflow, C2).
    if strip.right_marker {
        spans.push(Span::styled(theme::TAB_OVERFLOW.to_string(), theme::quiet()));
        used += 1;
    }

    if let Some(fit) = fit.filter(|_| show_status) {
        let (mode_word, prefix, save_word) = mouse::status_parts(fit.mode, fit.cwd, saved);
        let pad = area.width.saturating_sub(used).saturating_sub(status_w);
        if pad > 0 {
            spans.push(Span::raw(" ".repeat(pad as usize)));
        }
        // U15: the mode word reads a step brighter than the cwd beside it —
        // it's state, not context — while staying inside the ink ramp (never
        // the accent: it isn't an alarm). [Amended 2026-07-27, theme
        // inheritance] The ramp is two rungs deep now, so "a step brighter"
        // is spelled `ink` over `quiet` rather than the old MUTED over DIM —
        // the *relationship* is what the rule was ever about.
        if !mode_word.is_empty() {
            spans.push(Span::styled(mode_word, theme::ink()));
        }
        if !prefix.is_empty() {
            spans.push(Span::styled(prefix, theme::quiet()));
        }
        let save_style = if saved { theme::quiet() } else { theme::accent() };
        spans.push(Span::styled(format!("{save_word} "), save_style));
    }

    // No row fill: the tab bar carries no background of its own (§2), so the
    // gap before the status area and the row past the last tab show the
    // user's terminal background rather than a band roost painted over it.
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// One tab's 9-part span sequence (C2): marker, label, status glyph, the
/// **count cell**, the separator, and a trailing gutter space — column count
/// matches `mouse::tab_width` (`display_width(label) + 8`) exactly.
///
/// [Amended 2026-07-28] `count` is how many of the tab's panes are in the
/// summarized state (the second half of `App::tab_summary`). It renders in the glyph's
/// own style — pulse included, so `●3` flips as one token rather than as a
/// dot with a number stuck to it: the count is part of the signal, not a
/// separate remark about it.
fn push_tab_spans(
    spans: &mut Vec<Span<'static>>,
    index: usize,
    name: &str,
    active: bool,
    glyph: char,
    glyph_style: Style,
    count: usize,
) {
    if active {
        spans.push(Span::styled(theme::MARKER_ACTIVE.to_string(), theme::accent()));
    } else {
        spans.push(Span::raw(" "));
    }
    spans.push(Span::raw(" "));

    // Active vs inactive is ink weight plus the `▎` marker — no highlight
    // fill to go invisible against a light theme (§2, 2026-07-27).
    let label_style = if active { theme::active_tab_label() } else { theme::quiet() };
    spans.push(Span::styled(mouse::tab_label(index, name), label_style));
    spans.push(Span::raw(" "));

    spans.push(Span::styled(glyph.to_string(), glyph_style));
    // C2 (amended 2026-07-28): the count cell, glyph-adjacent and in the
    // glyph's own style. Always exactly one column — blank below 2 — so tab
    // widths never jitter as statuses flip.
    spans.push(Span::styled(tab_count_cell(count).to_string(), glyph_style));
    spans.push(Span::raw(" "));

    spans.push(Span::styled(theme::TAB_SEPARATOR.to_string(), theme::rule()));
    // C2 (amended 2026-07-23): trailing gutter — one space after the separator
    // gives every divider symmetric 1-cell padding, so adjacent tabs read
    // `│ ▎` not `│▎`. Counts as this tab's own column (mouse::tab_width +8).
    spans.push(Span::raw(" "));
}

/// C2 (amended 2026-07-28): what goes in a tab's count cell for `n` panes in
/// the summarized state. **Exactly one column, always** — that invariant is
/// the contract's geometry rule, and `mouse::tab_width` depends on it:
/// - `0`/`1` → a space. One is what a glyph already means; drawing `◆1`
///   would spend a column to say nothing, and *omitting* the cell instead
///   would make every tab's width follow its agents' statuses.
/// - `2..=9` → the digit.
/// - `10+` → `+`. Past nine the exact number stops being actionable ("more
///   than you can eyeball") and two digits would break the one-column rule,
///   which the whole strip's hit math is built on.
///
/// Pure so the boundaries are unit-tested; ASCII text in the glyph's own
/// style, so §2's glyph inventory is unchanged (no new symbol).
fn tab_count_cell(n: usize) -> char {
    match n {
        0 | 1 => ' ',
        2..=9 => char::from_digit(n as u32, 10).unwrap_or('+'),
        _ => '+',
    }
}

/// The count cell's rendered width, for whatever `n` — **always 1**. This is
/// the number `mouse::tab_width`'s `+8` is built on, so it lives here, beside
/// the cell it measures, and the mouse side asserts against it rather than
/// against a hard-coded 1 (§4/§5 lockstep: the width formula and the drawn
/// cells have to move together or not at all).
#[cfg(test)]
pub fn tab_count_cell_cols(n: usize) -> u16 {
    mouse::display_width(&tab_count_cell(n).to_string())
}

/// Map a tab's aggregate summary to a tab-bar glyph + style (theme::C5).
/// `Quiet` renders as a blank (no clutter for tabs with nothing to report);
/// `Unknown` is a quiet dot so a not-yet-spawned background tab reads as
/// "unknown", not idle.
fn tab_summary_badge(s: crate::core::app::TabSummary) -> (char, Style) {
    theme::tab_summary_style(s)
}

/// C8's state-word table: the collapsed row's right-segment word for each
/// status — also reused by the C20 feed's status-transition lines
/// (`app.rs`'s `diff_statuses`), so there's exactly one word table. `Exited`
/// is contracted bare — no exit code (SPEC-GAP-1: no exit-code plumbing
/// exists; `status.rs` tracks only a bool).
pub fn state_word(status: AgentStatus) -> &'static str {
    match status {
        AgentStatus::Working => "working",
        AgentStatus::NeedsInput => "needs you",
        AgentStatus::Waiting => "your turn",
        AgentStatus::Idle => "idle",
        AgentStatus::Exited => "exited",
    }
}

/// C8's state word for a row whose pane may have no runtime yet
/// (`App::row_status` → `None`): a tab this session has not visited, whose
/// panes the lazy spawn has not started. "not started" is the row-length
/// spelling of what `tab_summary` reports as `TabSummary::Unknown` for the
/// same tab — never "exited", which claims a life that has not happened.
fn row_word(status: Option<AgentStatus>) -> &'static str {
    match status {
        Some(s) => state_word(s),
        None => "not started",
    }
}

/// C8's glyph + style for a row whose pane may have no runtime yet — the
/// counterpart to `row_word`. `None` routes through the *tab bar's* own
/// `Unknown` styling, so the roster's glyph for an unspawned pane is by
/// construction the glyph its tab is wearing in the same frame.
fn row_status_style(status: Option<AgentStatus>) -> (char, Style, bool) {
    match status {
        Some(s) => theme::status_style(s),
        None => {
            let (glyph, style) = theme::tab_summary_style(crate::core::app::TabSummary::Unknown);
            (glyph, style, false)
        }
    }
}

/// C8's collapsed-row name style, by status and focus.
///
/// [Amended 2026-07-27, theme inheritance] The focused row used to be a
/// `RULE` fill across its whole width. Chrome paints no fills now (§2), so
/// focus is carried the way the tab bar carries it: full-strength ink plus
/// the `▎` marker. The status ramp still speaks on unfocused rows — and it
/// is two rungs deep, not three, so Waiting/Idle/Exited share the quiet one
/// (the `✕` glyph and the `exited` state word already say which is dead).
fn collapsed_name_style(status: Option<AgentStatus>, focused: bool) -> Style {
    if focused {
        return theme::ink();
    }
    match status {
        Some(AgentStatus::Working | AgentStatus::NeedsInput) => theme::ink(),
        Some(AgentStatus::Waiting | AgentStatus::Idle | AgentStatus::Exited) | None => {
            theme::quiet()
        }
    }
}

/// C4 (amended, U2): the badge leads with the pane id — the join key for
/// `roost send <id>`, which previously appeared nowhere in the TUI. No-dup
/// rule: an untitled pane's display `name` (`display_name_of`) already
/// embeds the adapter — so appending `· {adapter}` again would duplicate it
/// ("pi · repo · pi"). Only a custom title needs the adapter spelled out
/// separately.
fn badge_text(id: layout::PaneId, name: &str, adapter: &str, has_title: bool) -> String {
    if has_title {
        format!("{id} {name} · {adapter}")
    } else {
        format!("{id} {name}")
    }
}

/// Render `parts` in order, stopping once `budget` columns are spent — the
/// part that doesn't fully fit is cut off mid-string rather than dropped
/// whole, so a badge/row degrades by trimming its tail instead of losing a
/// whole segment early. Shared by `corner_badge` (C4) and the collapsed-row
/// left side (C8) so both clip identically. Measured in display columns, not
/// chars (D1): a wide glyph (CJK, emoji) in a renamed pane/tab counts as the
/// two columns it actually draws, and a clip point never splits one in half.
fn clip_spans(parts: &[(String, Style)], budget: u16) -> Vec<Span<'static>> {
    let mut spans = Vec::with_capacity(parts.len());
    let mut left = budget;
    for (text, style) in parts {
        if left == 0 {
            break;
        }
        let w = mouse::display_width(text);
        if w <= left {
            spans.push(Span::styled(text.clone(), *style));
            left -= w;
        } else {
            spans.push(Span::styled(take_width(text, left), *style));
            left = 0;
        }
    }
    spans
}

/// The longest prefix of `s` whose display width fits within `budget`
/// columns — the wide-glyph-aware sibling of `.chars().take(n)`. Stops
/// before a character that would only partially fit rather than splitting
/// it, so a clip point never lands mid-glyph.
fn take_width(s: &str, budget: u16) -> String {
    let mut used = 0u16;
    let mut out = String::new();
    for ch in s.chars() {
        let w = ch.width().unwrap_or(0) as u16;
        if used + w > budget {
            break;
        }
        out.push(ch);
        used += w;
    }
    out
}

fn draw_pane<B: PaneBackend>(
    f: &mut Frame,
    app: &mut App<B>,
    pr: PaneRect,
    stack_expanded: bool,
    pulse: Style,
) {
    let focused = app.focused == pr.id;
    let raw = app.is_raw(pr.id);
    let (status, name, has_title, adapter) = {
        // find_spec, not the active tab's map directly: C22 learns the
        // float here too (its spec lives on the `Float`, not `Tab::panes`).
        let spec = app.find_spec(pr.id);
        // A drawn pane lives in the active tab or is the float, so it has
        // been spawned and `display_status` answers `Some`; the fallback
        // only covers a pane whose spec vanished mid-frame, which is dead.
        // [P2-10] `display_status`, not `row_status`: a quiet shell's
        // `Waiting` reads `Idle` on the badge/collapsed row, same as
        // everywhere else chrome shows status.
        let status = app.display_status(pr.id).unwrap_or(AgentStatus::Exited);
        let has_title = spec.and_then(|s| s.title.as_ref()).is_some();
        let adapter = spec.map(|s| s.adapter.clone()).unwrap_or_else(|| "?".into());
        // U2 (amended, P6): the shared display name — explicit title, else
        // the pane's live OSC 0/2 title, else `adapter · cwd-tag`. One
        // helper for every fleet surface, `App::display_name`, so the badge
        // can never drift from what the feed/notifications/host title call
        // the same pane. Untitled panes on the same adapter are otherwise
        // indistinguishable at a glance; a pane publishing a live task line
        // now says what it's doing.
        let name = if spec.is_some() { app.display_name(pr.id) } else { "?".into() };
        (status, name, has_title, adapter)
    };

    if pr.collapsed {
        // C8: collapsed stack member — a single-row fleet-view bar.
        draw_collapsed_row(f, pr.rect, focused, status, pr.id, &name, &adapter, has_title, raw, pulse);
        return;
    }

    // C3: focus is the only signal a border carries now — status lives in
    // the glyph system (corner badge / collapsed row), not the border color,
    // and the border no longer carries a title (identity moved to the
    // corner badge, C4). No BOLD.
    let border_style = if focused { theme::accent() } else { theme::rule() };
    let block = Block::bordered().border_style(border_style);
    let inner = block.inner(pr.rect);
    f.render_widget(block, pr.rect);

    // C7: an unfocused expanded stack member gets its left border column
    // overpainted with the accent-dim edge marker. Suppressed when focused —
    // the full accent border is already the stronger signal, and stacking
    // a quiet red inside a red frame would smear the one-red discipline.
    if stack_expanded && !focused {
        paint_stack_edge(f, pr.rect);
    }

    // U3/N1 + P7c: the cursor's honesty gate is computed before the screen
    // borrow, since it needs `app` immutably for the scroll offset.
    let scrolled = app.scroll_offset(pr.id);
    if let Some(screen) = app.runtimes.get(&pr.id).and_then(|rt| rt.screen()) {
        blit_screen(f, screen, inner);
        // P7: the host cursor is placed only when the pane is actually
        // showing one. `should_place_cursor` holds the whole rule.
        if should_place_cursor(focused, status, screen.hide_cursor(), scrolled) {
            let (cr, cc) = screen.cursor_position();
            let x = inner.x.saturating_add(cc);
            let y = inner.y.saturating_add(cr);
            if x < inner.x + inner.width && y < inner.y + inner.height {
                f.set_cursor_position(Position::new(x, y));
            }
        }
    }

    // P21: search hits, painted before the selection so a selection over a
    // hit still reads as selected. C17's rule holds — modifiers only, no
    // color tokens: hits sit on top of arbitrary program colors.
    if let Some(search) = app.search.as_ref().filter(|s| s.pane == pr.id) {
        let (offset, total) = app.scroll_position();
        highlight_matches(f, inner, search, total.saturating_sub(offset));
    }

    // Copy-mode selection: reverse-highlight the selected cells in this pane.
    if let Some(sel) = app.selection.filter(|s| s.pane == pr.id) {
        highlight_selection(f, inner, sel.anchor, sel.cursor);
    }

    // C24: the keyboard copy cursor, always on the focused pane while in
    // Mode::Copy — painted after the selection pass so it stays visible
    // inside it (REVERSED, +UNDERLINED when inside an active selection).
    if focused {
        if let Mode::Copy { cursor } = app.mode {
            paint_copy_cursor(f, inner, cursor, app.selection);
        }
    }

    // C4: corner badge — the pane label, top-right. Drawn after the content
    // so it stays visible (a cell TUI can't do true translucency; quiet text
    // reads as a watermark rather than content). Drawn on every pane,
    // focused included: occlusion of the inner app's own top-right cells is
    // accepted by design now that identity lives here, not a border title.
    // U3: a scrolled pane's badge gains the dim ↑N token; N1: its Working
    // glyph stops pulsing while the view is frozen (badge_glyph_color).
    let (glyph, glyph_base, pulses) = theme::status_style(status);
    let scrolled = app.scroll_offset(pr.id);
    let glyph_style = badge_glyph_style(pulses, scrolled, pulse, glyph_base);
    let text = badge_text(pr.id, &name, &adapter, has_title);
    if let Some((rect, spans)) = corner_badge(inner, &text, raw, scrolled, glyph, glyph_style) {
        f.render_widget(Paragraph::new(Line::from(spans)), rect);
    }

    // C16: dead pane — overlay the relaunch hint (and spawn error, if any)
    // on the bottom rows. The last screen contents stay visible above.
    if status == AgentStatus::Exited && inner.height > 0 {
        let mut lines: Vec<Line> = Vec::new();
        if let Some(err) = app.dead.get(&pr.id) {
            lines.push(Line::from(Span::styled(format!(" spawn failed: {err} "), theme::accent())));
        }
        lines.push(Line::from(Span::raw(format!(
            " {} exited — Enter: relaunch/resume · f: fresh (drops resume) · Alt+w: close ",
            theme::GLYPH_EXITED
        ))));
        let n = lines.len() as u16;
        let y = inner.y + inner.height.saturating_sub(n);
        let overlay = Rect::new(inner.x, y, inner.width, n.min(inner.height));
        // C16: the style rides on the *widget*, not the span, so the whole
        // inner width reverses. Styling the span alone would reverse only the
        // ~76 text columns and leave the dead program's last output showing
        // through the rest of the row at normal video — the bar has to read as
        // one bar, exactly as C10/C11's rows do.
        f.render_widget(Paragraph::new(lines).style(theme::attention_problem()), overlay);
    }
}

/// C7: overpaint an expanded stack member's left border column with the
/// accent-dim half-block edge — the cell translation of the mockup's 2px
/// `--tui-red-dim` left edge (a half-block reads "thicker than a 1px line").
fn paint_stack_edge(f: &mut Frame, rect: Rect) {
    let buf = f.buffer_mut();
    for y in rect.y..rect.y + rect.height {
        if let Some(cell) = buf.cell_mut((rect.x, y)) {
            cell.set_symbol(&theme::MARKER_EXPANDED_EDGE.to_string());
            cell.set_style(theme::accent_quiet());
        }
    }
}

/// C8: one collapsed stack row's spans for the given width — marker, status
/// glyph, pane id, and name on the left; the right-aligned dim
/// "adapter · word" segment when there's room. The right segment drops first
/// when narrow; if even the left side overflows, the id+name (last in
/// `left`) is what visibly clips. Pure so the width-shedding order is
/// unit-testable.
///
/// [Amended, U2] the left side carries the pane id ahead of the name — the
/// `roost send <id>` join key, same placement as the corner badge (C4).
///
/// No-dup rule (C8, mirrors C4's `badge_text`): an untitled pane's `name` is
/// the shared `display_name_of` fallback, which already embeds the adapter —
/// so the right segment drops the `{adapter} · ` prefix and shows just the
/// state word (`has_title` false). A custom title doesn't embed the adapter,
/// so titled panes keep the full `"{adapter} · {word}"`.
/// [Amended, C23] a raw pane's right segment gains a `raw · ` prefix ahead
/// of whichever of the above it would otherwise be.
/// [Amended, C27] `status` is optional because the roster reuses this row
/// for panes in tabs the lazy spawn has not started: `None` is "not started"
/// (`row_word`/`row_status_style`), not a corpse.
#[allow(clippy::too_many_arguments)]
fn collapsed_row_spans(
    width: u16,
    focused: bool,
    status: Option<AgentStatus>,
    id: layout::PaneId,
    name: &str,
    adapter: &str,
    has_title: bool,
    raw: bool,
    glyph_style: Style,
) -> Vec<Span<'static>> {
    let (glyph, ..) = row_status_style(status);
    let marker = if focused {
        (theme::MARKER_ACTIVE.to_string(), theme::accent())
    } else {
        (" ".to_string(), Style::default())
    };
    let left: Vec<(String, Style)> = vec![
        marker,
        (glyph.to_string(), glyph_style),
        (format!(" {id} {name}"), collapsed_name_style(status, focused)),
    ];
    let left_w: u16 = left.iter().map(|(t, _)| mouse::display_width(t)).sum();
    let right = if has_title {
        format!("{adapter} · {} ", row_word(status))
    } else {
        format!("{} ", row_word(status))
    };
    let right = if raw { format!("raw · {right}") } else { right };
    let right_w = mouse::display_width(&right);

    if width >= left_w + right_w {
        let pad = width - left_w - right_w;
        let mut spans: Vec<Span> = left.into_iter().map(|(t, s)| Span::styled(t, s)).collect();
        spans.push(Span::raw(" ".repeat(pad as usize)));
        spans.push(Span::styled(right, theme::quiet()));
        spans
    } else {
        clip_spans(&left, width)
    }
}

/// C8: render one collapsed stack member's row. No row fill in either state
/// (background policy, §2): focus is the `▎` marker plus full-strength ink
/// (`collapsed_name_style`), so the row reads the same way the active tab
/// does and can't go invisible on a light theme.
#[allow(clippy::too_many_arguments)]
fn draw_collapsed_row(
    f: &mut Frame,
    rect: Rect,
    focused: bool,
    status: AgentStatus,
    id: layout::PaneId,
    name: &str,
    adapter: &str,
    has_title: bool,
    raw: bool,
    pulse: Style,
) {
    let (_, base, pulses) = theme::status_style(status);
    let glyph_style = if pulses { pulse } else { base };
    // `Some`, always: a drawn row belongs to the active tab (or the float),
    // and those are spawned by construction — only C27's roster reaches
    // across into tabs that aren't.
    let spans = collapsed_row_spans(
        rect.width,
        focused,
        Some(status),
        id,
        name,
        adapter,
        has_title,
        raw,
        glyph_style,
    );
    f.render_widget(Paragraph::new(Line::from(spans)), rect);
}

/// C6's header text for the given row width: uppercase " STACK · N PANES"
/// left, "ALT+↑↓ " right-aligned, filled with spaces between. Pure so the
/// content and right-alignment are unit-testable without a `Frame`.
fn stack_header_text(width: u16, n: usize) -> String {
    let left = format!(" STACK · {n} PANES");
    let right = "ALT+↑↓ ";
    let pad = width
        .saturating_sub(left.chars().count() as u16)
        .saturating_sub(right.chars().count() as u16);
    format!("{left}{}{right}", " ".repeat(pad as usize))
}

/// C6: a stack's header row. Every cell (text and fill alike) carries
/// `Modifier::UNDERLINED` — the cell translation of the mockup's 1px bottom
/// rule — via the paragraph-level style, the same edge-to-edge-fill trick
/// `draw_tab_bar` uses. No bg (background policy, §2).
fn draw_stack_header(f: &mut Frame, header: layout::StackHeader) {
    f.render_widget(
        Paragraph::new(stack_header_text(header.rect.width, header.n))
            .style(theme::quiet().add_modifier(Modifier::UNDERLINED)),
        header.rect,
    );
}

/// Top-right corner badge (C4): pane name (+ adapter, when titled) and the
/// status glyph, right-aligned with one column of breathing room. Two-tone:
/// the text is `quiet`, the glyph carries its own C5 status style. [Amended,
/// C23] a raw pane's badge gains a `raw` token between the text and the
/// glyph, in its own `accent_quiet` span (never folded into the quiet text —
/// it needs its own colour). [Amended, U3] a scrolled pane's badge carries a
/// quiet `↑N` token — its grid-clamped view offset, 0 = live tail = no token
/// — glyph-adjacent (after `raw`), same `accent_quiet` family. Returns the
/// 1-row rect and the clipped spans — or `None` if the pane is too small to
/// be worth badging. Pure so it can be unit-tested.
fn corner_badge(
    inner: Rect,
    text: &str,
    raw: bool,
    scrolled: usize,
    glyph: char,
    glyph_style: Style,
) -> Option<(Rect, Vec<Span<'static>>)> {
    if text.trim().is_empty() || inner.width < 3 || inner.height == 0 {
        return None;
    }
    let max = inner.width.saturating_sub(1);
    // One space of breathing room on the right edge (the trailing space in
    // the glyph part).
    let mut parts: Vec<(String, Style)> = Vec::with_capacity(4);
    if raw || scrolled > 0 {
        parts.push((format!(" {text} · "), theme::quiet()));
    } else {
        parts.push((format!(" {text} "), theme::quiet()));
    }
    if raw {
        let token = if scrolled > 0 { "raw · " } else { "raw " };
        parts.push((token.to_string(), theme::accent_quiet()));
    }
    if scrolled > 0 {
        parts.push((format!("{}{scrolled} ", theme::SCROLLED), theme::accent_quiet()));
    }
    parts.push((format!("{glyph} "), glyph_style));
    let total: u16 = parts.iter().map(|(t, _)| mouse::display_width(t)).sum();
    let w = total.min(max);
    let spans = clip_spans(&parts, w);
    let x = inner.x + inner.width - w;
    Some((Rect::new(x, inner.y, w, 1), spans))
}

/// N1: the badge glyph's style — the C5 pulse only while the pane's view is
/// at the live tail. A pulsing `●` asserts "alive right now", which a frozen
/// (scrolled) view must not do; the glyph keeps its steady base style (the
/// status itself stays truthful — the agent IS working) while the C4 `↑N`
/// token carries the frozen-view signal. Any path that resets the offset
/// resumes the pulse. Collapsed rows and the tab bar keep pulsing: they
/// show no grid, so there is no frozen view to lie about.
fn badge_glyph_style(pulses: bool, scrolled: usize, pulse: Style, base: Style) -> Style {
    if pulses && scrolled == 0 {
        pulse
    } else {
        base
    }
}

/// Reverse-video the cells between `a` and `b` (inclusive, pane-inner coords)
/// to show a copy-mode selection. Reading-order/linewise, clipped to `inner`.
fn highlight_selection(f: &mut Frame, inner: Rect, a: (u16, u16), b: (u16, u16)) {
    let (start, end) = if (a.0, a.1) <= (b.0, b.1) { (a, b) } else { (b, a) };
    let (w, h) = (inner.width, inner.height);
    let buf = f.buffer_mut();
    let mut row = start.0;
    while row <= end.0 && row < h {
        let first = if row == start.0 { start.1 } else { 0 };
        let last = if row == end.0 { end.1 } else { w.saturating_sub(1) };
        let mut col = first;
        while col <= last && col < w {
            if let Some(cell) = buf.cell_mut((inner.x + col, inner.y + row)) {
                let s = cell.style().add_modifier(Modifier::REVERSED);
                cell.set_style(s);
            }
            col += 1;
        }
        row += 1;
    }
}

/// P21: paint the search hits visible in this pane. `first_line` is the
/// haystack line index currently on the view's top row — `banked − offset`,
/// both read from the grid — so a hit's screen row is just `line −
/// first_line`. Every hit is `REVERSED`; the *current* one additionally
/// `UNDERLINED`, so "which of these am I parked on" is answerable at a
/// glance. Modifier-only by C17: hits land on arbitrary program colors, and
/// a palette token here would be a DEVIATED.
fn highlight_matches(f: &mut Frame, inner: Rect, search: &Search, first_line: usize) {
    let width = search.width();
    if width == 0 {
        return;
    }
    let current = search.current_match();
    let buf = f.buffer_mut();
    for (i, &(line, col)) in search.matches.iter().enumerate() {
        let Some(row) = line.checked_sub(first_line) else { continue };
        if row >= inner.height as usize {
            continue;
        }
        let is_current = current == Some(search.matches[i]) && i == search.current;
        for c in col..col + width {
            if c >= inner.width as usize {
                break;
            }
            let Some(cell) = buf.cell_mut((inner.x + c as u16, inner.y + row as u16)) else {
                continue;
            };
            let mut style = cell.style().add_modifier(Modifier::REVERSED);
            if is_current {
                style = style.add_modifier(Modifier::UNDERLINED);
            }
            cell.set_style(style);
        }
    }
}

/// C24: whether inner-cell `pos` (row, col) lies within the inclusive,
/// reading-order selection spanning `a`..=`b` — the same ordering
/// `highlight_selection` paints. `(u16, u16)`'s derived `Ord` compares row
/// first then column, which *is* reading order, so a plain range check
/// suffices. Pure so the cursor-in-selection rule is unit-tested without a
/// `Frame`.
fn cell_in_selection(pos: (u16, u16), a: (u16, u16), b: (u16, u16)) -> bool {
    let (start, end) = if a <= b { (a, b) } else { (b, a) };
    pos >= start && pos <= end
}

/// C24: paint the keyboard-copy cursor cell — always `REVERSED`;
/// additionally `UNDERLINED` when it sits inside an active selection, so it
/// stays distinguishable within the reversed region. Painted after the
/// selection pass (C17). Modifier-only, no color tokens — any styling here
/// beyond modifiers is a DEVIATED (C24).
fn paint_copy_cursor(f: &mut Frame, inner: Rect, cursor: (u16, u16), selection: Option<Selection>) {
    let (row, col) = cursor;
    if row >= inner.height || col >= inner.width {
        return;
    }
    let in_selection = selection.is_some_and(|s| cell_in_selection(cursor, s.anchor, s.cursor));
    let buf = f.buffer_mut();
    if let Some(cell) = buf.cell_mut((inner.x + col, inner.y + row)) {
        let mut style = cell.style().add_modifier(Modifier::REVERSED);
        if in_selection {
            style = style.add_modifier(Modifier::UNDERLINED);
        }
        cell.set_style(style);
    }
}

/// P7: may roost place the host's single real cursor inside this pane?
///
/// Four conditions, each a way the old unconditional "focused ⇒ place it"
/// rule lied:
/// * only the focused pane may hold it — there is one cursor;
/// * an exited pane isn't running anything to own it (pre-existing rule);
/// * `hide_cursor` (DECTCEM `?25l`) means the app deliberately hid its
///   cursor — roost blinking a ghost one over a TUI that hid its own is
///   P7(a) verbatim;
/// * a scrolled-back view (`scroll_offset > 0`) is frozen on history, so the
///   live grid's cursor position has nothing to do with what's on screen —
///   it would float over old output (P7(c), same frozen-view surface as
///   SPEC-ux U3/N1: while the `↑N` token shows, roost stops asserting
///   liveness, and a blinking cursor is the loudest such assertion).
///
/// Pure, so all four are unit-testable without a `Frame`.
fn should_place_cursor(
    focused: bool,
    status: AgentStatus,
    hidden: bool,
    scroll_offset: usize,
) -> bool {
    focused && status != AgentStatus::Exited && !hidden && scroll_offset == 0
}

/// Copy the vt100 grid into the ratatui buffer.
///
/// U24: wide-char (CJK/emoji) cells are blitted the way ratatui itself lays
/// out a wide grapheme — the glyph goes in its own cell and the continuation
/// cell to its right is `reset()`, never written to. Previously the
/// continuation was stamped with `" "`, which drew a space *over* the right
/// half of every CJK/emoji glyph the pane emitted. Since P17 the grid measures
/// widths with the same unicode-width the terminal backend uses, so a cell
/// marked wide here is a cell ratatui's diff will skip when flushing — the two
/// halves can no longer disagree about which columns the glyph owns.
fn blit_screen(f: &mut Frame, screen: &vt100::Screen, inner: Rect) {
    let (rows, cols) = screen.size();
    let visible_cols = inner.width.min(cols);
    let buf = f.buffer_mut();
    for row in 0..inner.height.min(rows) {
        for col in 0..visible_cols {
            let Some(cell) = screen.cell(row, col) else { continue };
            let x = inner.x + col;
            let y = inner.y + row;
            let Some(out) = buf.cell_mut((x, y)) else { continue };
            if cell.is_wide_continuation() {
                // Owned by the glyph on its left. Left at the buffer's reset
                // default — exactly what ratatui's own wide-grapheme layout
                // does — so nothing in roost ever writes a symbol into a cell
                // another glyph already spans.
                out.reset();
                continue;
            }
            let contents = cell.contents();
            // A wide glyph whose second half falls outside the drawn area
            // would overflow into the pane's border, so it degrades to a
            // space rather than corrupting the chrome.
            if contents.is_empty() || (cell.is_wide() && col + 1 >= visible_cols) {
                out.set_symbol(" ");
            } else {
                out.set_symbol(&contents);
            }
            out.set_style(cell_style(cell));
        }
    }
}

/// Program output, not chrome (C18): a pane's own colours pass through
/// byte-faithfully and keep the **full** palette — indexed and truecolor
/// alike. §2's inherit-the-terminal rule governs what roost draws *around*
/// panes, never what a program draws inside one, so these three lines carry
/// the `chrome-gate-exempt` marker the theme module's source scan honours.
fn conv_color(c: vt100::Color) -> Option<Color> {
    match c {
        vt100::Color::Default => None,
        vt100::Color::Idx(i) => Some(Color::Indexed(i)), // chrome-gate-exempt: program output
        vt100::Color::Rgb(r, g, b) => Some(Color::Rgb(r, g, b)), // chrome-gate-exempt: program output
    }
}

fn cell_style(cell: &vt100::Cell) -> Style {
    let mut style = Style::default();
    if let Some(fg) = conv_color(cell.fgcolor()) {
        style = style.fg(fg);
    }
    if let Some(bg) = conv_color(cell.bgcolor()) {
        style = style.bg(bg); // chrome-gate-exempt: program output
    }
    if cell.bold() {
        style = style.add_modifier(Modifier::BOLD);
    }
    if cell.italic() {
        style = style.add_modifier(Modifier::ITALIC);
    }
    if cell.underline() {
        style = style.add_modifier(Modifier::UNDERLINED);
    }
    if cell.inverse() {
        style = style.add_modifier(Modifier::REVERSED);
    }
    // P16: dim (SGR 2) and strikethrough (SGR 9) were dropped end to end, so
    // a pane's secondary text rendered at the same weight as its primary —
    // roost asserting emphasis the program never asked for. Claude Code leans
    // on dim heavily; without it a pane is one flat wall of text.
    if cell.dim() {
        style = style.add_modifier(Modifier::DIM);
    }
    if cell.strikethrough() {
        style = style.add_modifier(Modifier::CROSSED_OUT);
    }
    style
}

#[cfg(test)]
mod tests {
    use crate::App;
    use super::{
        badge_text, blit_screen, cell_in_selection, centered_near, collapsed_name_style,
        collapsed_row_spans, corner_badge, dialog_border_style, feed_entry_spans, feed_window,
        help_content_width, help_layout, help_lines, hint_bar_right_spans, hint_pairs, mode_word,
        push_tab_spans, should_place_cursor, stack_header_text, state_word, HelpLine, HELP_GROUPS,
    };
    use crate::core::app::{Mode, RenameTarget, RosterRow};
    use crate::core::status::AgentStatus;
    use crate::ui::mouse;
    use crate::ui::theme;
    use ratatui::layout::Rect;
    use ratatui::style::{Color, Modifier};

    #[test]
    fn dialog_centers_on_focused_pane_not_whole_screen() {
        // Body is a wide 3-pane screen; anchor is the rightmost pane only.
        let body = Rect::new(0, 1, 120, 30);
        let anchor = Rect::new(80, 1, 40, 30);
        let rect = centered_near(anchor, body, 32, 5);
        // Centered within the anchor pane, not the full 120-wide body.
        assert_eq!(rect.x, anchor.x + (anchor.width - rect.width) / 2);
        assert_eq!(rect.width, 32);
        assert_eq!(rect.height, 5);
    }

    #[test]
    fn dialog_stays_on_screen_when_anchor_is_near_the_edge() {
        // Anchor pane hugs the right edge; a dialog centered on it alone
        // would spill off-screen — must clamp back inside `body`.
        let body = Rect::new(0, 1, 60, 20);
        let anchor = Rect::new(50, 1, 10, 20);
        let rect = centered_near(anchor, body, 32, 5);
        assert!(rect.x + rect.width <= body.x + body.width);
        assert!(rect.x >= body.x);
    }

    #[test]
    fn badge_no_dup_rule_pins_c4() {
        // U2: every badge leads with the pane id (the `roost send <id>` key).
        // Untitled fallback name already embeds the adapter — don't repeat it.
        assert_eq!(badge_text(3, "pi · myrepo", "pi", false), "3 pi · myrepo");
        // A custom title doesn't embed the adapter — spell it out.
        assert_eq!(badge_text(7, "worker1", "claude", true), "7 worker1 · claude");
    }

    #[test]
    fn badge_is_two_toned_and_right_aligned_on_top_row() {
        // inner content area at (1,1) sized 40x20 (borders excluded)
        let inner = Rect::new(1, 1, 40, 20);
        let (rect, spans) = corner_badge(inner, "claude", false, 0, theme::GLYPH_WORKING, theme::accent()).unwrap();
        assert_eq!(rect.y, inner.y); // top row of the content
        assert_eq!(rect.height, 1);
        // right edge: badge ends one col shy of the inner right edge is fine;
        // here it butts to the edge because the text fits.
        assert_eq!(rect.x + rect.width, inner.x + inner.width);
        assert_eq!(spans.len(), 2);
        assert_eq!(spans[0].content.as_ref(), " claude ");
        assert_eq!(spans[0].style, theme::quiet());
        assert_eq!(spans[1].content.as_ref(), format!("{} ", theme::GLYPH_WORKING));
        assert_eq!(spans[1].style, theme::accent());
    }

    #[test]
    fn badge_clips_and_drops_the_glyph_first_when_pane_too_small() {
        let inner = Rect::new(0, 0, 6, 5);
        let (rect, spans) =
            corner_badge(inner, "a-very-long-name", false, 0, theme::GLYPH_WORKING, theme::accent()).unwrap();
        let total: usize = spans.iter().map(|s| s.content.chars().count()).sum();
        assert!(total <= 5); // width-1 breathing room
        assert!(rect.x >= inner.x && rect.x + rect.width <= inner.x + inner.width);
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(!text.contains(theme::GLYPH_WORKING)); // too narrow even for the text alone
    }

    #[test]
    fn badge_clips_wide_glyphs_without_overflowing_the_display_width_budget() {
        // "日本語" is 3 chars but 6 display columns — the old .chars().count()
        // measure would treat a 4-column budget as fitting all 3 chars and
        // overflow the pane by several columns (D1). The fix must stop
        // clipping on a display-width boundary and never split a glyph.
        let inner = Rect::new(0, 0, 5, 5); // budget = inner.width - 1 = 4
        let (rect, spans) = corner_badge(inner, "日本語", false, 0, theme::GLYPH_IDLE, theme::quiet()).unwrap();
        let rendered_width: u16 = spans.iter().map(|s| mouse::display_width(&s.content)).sum();
        assert!(rendered_width <= 4, "clipped badge must fit its column budget, got {rendered_width}");
        assert!(rect.width <= 4);
    }

    /// P7: every way the pane can be "not showing a cursor" suppresses
    /// roost's placement of the host's one real cursor.
    #[test]
    fn cursor_is_placed_only_when_the_pane_is_really_showing_one() {
        use AgentStatus::{Exited, Working};
        // The baseline: focused, alive, cursor visible, view at the tail.
        assert!(should_place_cursor(true, Working, false, 0));

        // Unfocused — there is one cursor and it belongs to the focused pane.
        assert!(!should_place_cursor(false, Working, false, 0));
        // Exited — nothing is running to own it (pre-existing rule, kept).
        assert!(!should_place_cursor(true, Exited, false, 0));
        // P7(a): the app hid its cursor with `?25l`; roost blinking a ghost
        // one over it is exactly the defect.
        assert!(!should_place_cursor(true, Working, true, 0));
        // P7(c)/U3: the view is frozen on history, so the live grid's cursor
        // position describes something that isn't on screen.
        assert!(!should_place_cursor(true, Working, false, 1));

        // The gates are independent — any one of them suppresses.
        assert!(!should_place_cursor(true, Working, true, 7));
        assert!(!should_place_cursor(false, Exited, true, 3));
    }

    #[test]
    fn no_badge_for_tiny_or_empty() {
        assert!(corner_badge(Rect::new(0, 0, 2, 5), "x", false, 0, theme::GLYPH_WORKING, theme::accent()).is_none());
        assert!(corner_badge(Rect::new(0, 0, 40, 0), "x", false, 0, theme::GLYPH_WORKING, theme::accent()).is_none());
        assert!(corner_badge(Rect::new(0, 0, 40, 5), "   ", false, 0, theme::GLYPH_WORKING, theme::accent()).is_none());
    }

    // -- C23 raw indication ---------------------------------------------------

    #[test]
    fn badge_gains_a_raw_token_in_its_own_quiet_red() {
        let inner = Rect::new(0, 0, 40, 20);
        let (_, spans) =
            corner_badge(inner, "scratch · shell", true, 0, theme::GLYPH_IDLE, theme::quiet()).unwrap();
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, format!(" scratch · shell · raw {} ", theme::GLYPH_IDLE));
        let raw_span = spans.iter().find(|s| s.content.as_ref() == "raw ").expect("raw token span");
        assert_eq!(raw_span.style, theme::accent_quiet());
        // The raw token must be its own span, not folded into the muted text.
        assert!(spans.iter().any(|s| s.style == theme::quiet()));
    }

    #[test]
    fn badge_without_raw_has_no_raw_token() {
        let inner = Rect::new(0, 0, 40, 20);
        let (_, spans) = corner_badge(inner, "pi", false, 0, theme::GLYPH_IDLE, theme::quiet()).unwrap();
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(!text.contains("raw"));
    }

    // -- U3 scrollback indication ---------------------------------------------

    #[test]
    fn badge_gains_a_scrolled_token_in_quiet_red() {
        // U3: a frozen view must say so — `↑N` (grid-clamped offset), same
        // quiet-red family as the raw token, glyph-adjacent.
        let inner = Rect::new(0, 0, 40, 20);
        let (_, spans) =
            corner_badge(inner, "3 pi", false, 42, theme::GLYPH_WORKING, theme::accent()).unwrap();
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, format!(" 3 pi · ↑42 {} ", theme::GLYPH_WORKING));
        let token = spans.iter().find(|s| s.content.as_ref() == "↑42 ").expect("↑N token span");
        assert_eq!(token.style, theme::accent_quiet());
    }

    #[test]
    fn badge_scrolled_and_raw_tokens_compose_in_order() {
        // raw · ↑N — input state first, then view state, then the glyph.
        let inner = Rect::new(0, 0, 40, 20);
        let (_, spans) =
            corner_badge(inner, "3 pi", true, 7, theme::GLYPH_IDLE, theme::quiet()).unwrap();
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, format!(" 3 pi · raw · ↑7 {} ", theme::GLYPH_IDLE));
    }

    #[test]
    fn badge_at_live_tail_has_no_scrolled_token() {
        let inner = Rect::new(0, 0, 40, 20);
        let (_, spans) =
            corner_badge(inner, "3 pi", false, 0, theme::GLYPH_WORKING, theme::accent()).unwrap();
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(!text.contains('↑'), "{text}");
    }

    #[test]
    fn badge_glyph_pulse_yields_while_the_view_is_frozen() {
        // N1: pulsing red means "alive right now" — a scrolled pane's glyph
        // holds the steady base color instead; the tail resumes the pulse.
        let phase = theme::pulse_bright(); // pretend mid-pulse phase A
        assert_eq!(super::badge_glyph_style(true, 0, phase, theme::accent()), phase);
        assert_eq!(super::badge_glyph_style(true, 12, phase, theme::accent()), theme::accent());
        // Non-pulsing statuses are steady either way.
        assert_eq!(super::badge_glyph_style(false, 0, phase, theme::ink()), theme::ink());
        assert_eq!(super::badge_glyph_style(false, 12, phase, theme::ink()), theme::ink());
    }

    #[test]
    fn hint_bar_right_carries_the_scroll_position_ahead_of_the_mode_word() {
        // U3: `↑N/M` rides inside the right segment (quiet), so C9's yield
        // machinery covers it — and it only exists when a position is given.
        let spans = hint_bar_right_spans(0, None, Some("↑12/300".into()), "SCROLL");
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, "↑12/300 SCROLL ");
        assert_eq!(spans[0].style, theme::quiet());

        let spans = hint_bar_right_spans(2, None, Some("↑12/300".into()), "SCROLL");
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, "◆ 2 needs you · Alt+a  ↑12/300 SCROLL ");
    }

    /// P21: the search prompt lives in C9's right segment — `/query▏` in
    /// `ink` (it is live input; quiet input is input you cannot proofread)
    /// with the `quiet` hit counter and the SEARCH mode word behind it. The
    /// prompt draws no dialog: the
    /// pane being searched must stay visible while the query narrows.
    #[test]
    fn hint_bar_right_carries_the_search_prompt_and_hit_counter() {
        use crate::core::app::Search;
        let lines: Vec<String> = ["alpha beta", "beta beta"].map(String::from).to_vec();
        let mut s = Search::over(lines, "beta", 1);
        let (query, position) = super::search_segment(Some(&s));
        let spans = hint_bar_right_spans(0, query, position, "SEARCH");
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, format!("/beta{} 2/3 SEARCH ", theme::RENAME_CURSOR));
        assert_eq!(spans[0].style, theme::ink(), "the typed query is legible, not dim");
        assert_eq!(spans[1].style, theme::quiet(), "the counter is a position token");

        // A query with no hits says so rather than hiding the counter — an
        // empty result is the answer, not the absence of one.
        s = Search::over(vec!["alpha beta".into()], "gamma", 0);
        let (query, position) = super::search_segment(Some(&s));
        let spans = hint_bar_right_spans(0, query, position, "SEARCH");
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, format!("/gamma{} 0/0 SEARCH ", theme::RENAME_CURSOR));

        // Outside a search there is no prompt at all.
        assert_eq!(super::search_segment(None), (None, None));
    }

    /// P21: hits are painted REVERSED and the current one additionally
    /// UNDERLINED — modifiers only (C17), and positioned by the same
    /// `banked − offset` arithmetic the jump uses, so a hit scrolled off
    /// the top of the view paints nothing.
    #[test]
    fn search_hits_are_reversed_with_the_current_one_underlined() {
        use crate::core::app::Search;
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        // Haystack lines 4 and 6 hold hits; the view's top row shows line 4.
        let lines: Vec<String> = (0..8)
            .map(|i| if i == 4 || i == 6 { "xxbetaxx".to_string() } else { "----".to_string() })
            .collect();
        // current = 1 → the line-6 hit.
        let search = Search::over(lines, "beta", 1);
        let inner = Rect::new(0, 0, 10, 4);
        let backend = TestBackend::new(10, 4);
        let mut term = Terminal::new(backend).unwrap();
        term.draw(|f| super::highlight_matches(f, inner, &search, 4)).unwrap();
        let buf = term.backend().buffer().clone();

        // Line 4 → screen row 0, cols 2..6: reversed, not underlined.
        for c in 2..6 {
            let cell = buf.cell((c, 0)).unwrap();
            assert!(cell.style().add_modifier.contains(Modifier::REVERSED), "col {c}");
            assert!(!cell.style().add_modifier.contains(Modifier::UNDERLINED), "col {c}");
        }
        // Line 6 → screen row 2: the current hit, so also underlined.
        for c in 2..6 {
            let cell = buf.cell((c, 2)).unwrap();
            assert!(cell.style().add_modifier.contains(Modifier::REVERSED), "col {c}");
            assert!(cell.style().add_modifier.contains(Modifier::UNDERLINED), "col {c}");
        }
        // Cells outside the runs are untouched.
        assert!(!buf.cell((1, 0)).unwrap().style().add_modifier.contains(Modifier::REVERSED));
        assert!(!buf.cell((6, 0)).unwrap().style().add_modifier.contains(Modifier::REVERSED));

        // A view scrolled past both hits paints nothing at all.
        let backend = TestBackend::new(10, 4);
        let mut term = Terminal::new(backend).unwrap();
        term.draw(|f| super::highlight_matches(f, inner, &search, 7)).unwrap();
        let buf = term.backend().buffer().clone();
        for y in 0..4 {
            for x in 0..10 {
                assert!(!buf.cell((x, y)).unwrap().style().add_modifier.contains(Modifier::REVERSED));
            }
        }
    }

    #[test]
    fn state_word_matches_c8_table() {
        assert_eq!(state_word(AgentStatus::Working), "working");
        assert_eq!(state_word(AgentStatus::NeedsInput), "needs you");
        assert_eq!(state_word(AgentStatus::Waiting), "your turn");
        assert_eq!(state_word(AgentStatus::Idle), "idle");
        assert_eq!(state_word(AgentStatus::Exited), "exited");
    }

    /// C8's name ramp, as amended by the theme-inherited stance: two text
    /// rungs, not three (Exited joins Waiting/Idle in the quiet one — its
    /// `✕` glyph and `exited` word already say it is dead), and a focused
    /// row is full-strength ink whatever its status, since the `RULE` fill
    /// that used to carry focus is gone.
    #[test]
    fn collapsed_name_style_by_state_and_focus() {
        assert_eq!(collapsed_name_style(Some(AgentStatus::Working), false), theme::ink());
        assert_eq!(collapsed_name_style(Some(AgentStatus::NeedsInput), false), theme::ink());
        assert_eq!(collapsed_name_style(Some(AgentStatus::Waiting), false), theme::quiet());
        assert_eq!(collapsed_name_style(Some(AgentStatus::Idle), false), theme::quiet());
        assert_eq!(collapsed_name_style(Some(AgentStatus::Exited), false), theme::quiet());
        // C27: a pane its tab has never spawned reads with the quiet rung —
        // it is not working, and it is not dead either.
        assert_eq!(collapsed_name_style(None, false), theme::quiet());
        // Focus overrides the ramp: the row you are on is never the quiet one.
        for s in [
            Some(AgentStatus::Working),
            Some(AgentStatus::NeedsInput),
            Some(AgentStatus::Waiting),
            Some(AgentStatus::Idle),
            Some(AgentStatus::Exited),
            None,
        ] {
            assert_eq!(collapsed_name_style(s, true), theme::ink(), "{s:?} focused");
        }
    }

    #[test]
    fn collapsed_row_shows_right_segment_when_it_fits() {
        let spans =
            collapsed_row_spans(40, false, Some(AgentStatus::Working), 2, "pi", "pi", true, false, theme::accent());
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.ends_with("pi · working "));
    }

    #[test]
    fn collapsed_row_no_dup_rule_drops_adapter_prefix_when_untitled() {
        // C8 no-dup rule (mirrors C4's badge_text): an untitled pane's name
        // is already the adapter/cwd fallback built in draw_pane, so the
        // right segment is the bare state word — "your turn", not
        // "shell · your turn". [DESIGN-ui.md amended 2026-07-22, ux #3.]
        let spans =
            collapsed_row_spans(40, false, Some(AgentStatus::Waiting), 2, "shell", "shell", false, false, theme::ink());
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.ends_with("your turn "));
        assert!(!text.contains("shell ·"));
    }

    #[test]
    fn collapsed_row_drops_right_segment_before_clipping_name() {
        let name = "a-fairly-long-pane-name";
        // Exactly enough room for "marker + glyph + ' ' + id + ' ' + name",
        // nothing more (U2: the id rides with the name on the left).
        let left_w = 5 + name.chars().count() as u16;
        let spans =
            collapsed_row_spans(left_w, false, Some(AgentStatus::Idle), 2, name, "shell", true, false, theme::quiet());
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, format!(" · 2 {name}"));
        assert!(!text.contains("shell"));
    }

    #[test]
    fn collapsed_row_clips_name_when_even_the_left_side_overflows() {
        let spans = collapsed_row_spans(
            4,
            false,
            Some(AgentStatus::Waiting),
            2,
            "a-very-long-pane-name",
            "shell",
            true,
            false,
            theme::ink(),
        );
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text.chars().count(), 4);
        assert!(!text.contains("shell"));
    }

    #[test]
    fn collapsed_row_focused_marker_is_accent() {
        let spans =
            collapsed_row_spans(40, true, Some(AgentStatus::Working), 2, "pi", "pi", true, false, theme::accent());
        assert_eq!(spans[0].content.as_ref(), theme::MARKER_ACTIVE.to_string());
        assert_eq!(spans[0].style, theme::accent());
    }

    #[test]
    fn collapsed_row_raw_gains_the_prefix_ahead_of_the_usual_right_segment() {
        let titled = collapsed_row_spans(60, false, Some(AgentStatus::Working), 2, "pi", "pi", true, true, theme::accent());
        let text: String = titled.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.ends_with("raw · pi · working "), "{text}");

        let untitled =
            collapsed_row_spans(60, false, Some(AgentStatus::Waiting), 2, "shell", "shell", false, true, theme::ink());
        let text: String = untitled.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.ends_with("raw · your turn "), "{text}");
    }

    #[test]
    fn stack_header_text_is_uppercase_and_right_aligned() {
        let text = stack_header_text(30, 3);
        assert_eq!(text.chars().count(), 30);
        assert!(text.starts_with(" STACK · 3 PANES"));
        assert!(text.ends_with("ALT+↑↓ "));
        assert_eq!(text, text.to_uppercase());
    }

    #[test]
    fn hint_pairs_normal_mode_is_exactly_the_seven_c9_pairs() {
        // U6 order (C9 amended 2026-07-27): `Alt+? keys` first so it yields
        // last, `Alt+r rename` last so it drops first.
        assert_eq!(
            hint_pairs(&Mode::Normal, false, false, false),
            vec![
                ("Alt+?", "keys"),
                ("Alt+n", "new"),
                ("Alt+↵", "launch"),
                ("Alt+s", "stack"),
                ("Alt+←↓↑→", "focus"),
                ("Alt+w", "close"),
                ("Alt+r", "rename"),
            ],
        );
    }

    /// U6, the live-QA case that started it: at 120 columns with the
    /// needs-you segment up (`◆ 1 needs you · Alt+a  NORMAL `, 30 cols),
    /// the bar used to drop `Alt+? keys` — the discoverability pointer —
    /// exactly when the fleet got busy. Now `Alt+r rename` goes first and
    /// the help pair survives; under enough pressure to drop six pairs, the
    /// one still standing is `Alt+? keys`.
    #[test]
    fn the_help_pair_is_the_last_hint_to_yield_and_rename_the_first() {
        let pairs = hint_pairs(&Mode::Normal, false, false, false);
        let pw = |p: &(&str, &str)| super::hint_pair_cols(p.0, p.1);
        let right_w = " ◆ 1 needs you · Alt+a  NORMAL ".chars().count() as u16 - 1;

        let shown = super::fit_hint_pairs(&pairs, right_w, 120);
        assert_eq!(shown, pairs.len() - 1, "exactly one pair yields at 120 cols");
        assert!(!pairs[..shown].contains(&("Alt+r", "rename")), "rename drops first");
        assert!(pairs[..shown].contains(&("Alt+?", "keys")), "the help pair survives");

        // Squeeze until a single pair is left: it must be the help pair.
        let width = right_w + pw(&pairs[0]);
        let shown = super::fit_hint_pairs(&pairs, right_w, width);
        assert_eq!(shown, 1);
        assert_eq!(pairs[0], ("Alt+?", "keys"));
    }

    /// C24's list as amended by U17: the mode's whole vocabulary, and it
    /// still has to fit beside the right segment at the 100-col floor —
    /// a hint bar that clips its own mode's keys is the U17 bug restated.
    #[test]
    fn hint_pairs_copy_mode_is_the_c24_list_amended_by_u17() {
        let pairs = hint_pairs(&Mode::Copy { cursor: (0, 0) }, false, false, false);
        assert_eq!(
            pairs,
            vec![
                ("hjkl", "move"),
                ("w/b/e", "word"),
                ("0/$", "ends"),
                ("v/V", "mark"),
                ("y/↵", "yank"),
                ("o", "open"),
                ("drag", "select"),
                ("Esc", "exit"),
            ],
        );
        let cols: u16 = pairs.iter().map(|(k, l)| super::hint_pair_cols(k, l)).sum();
        let right_w = super::hint_bar_right_spans(0, None, None, "COPY")
            .iter()
            .map(|s| mouse::display_width(&s.content))
            .sum::<u16>();
        assert!(
            cols + right_w <= 100,
            "copy hints are {cols} cols + {right_w} of segment; the 100-col floor clips them"
        );
    }

    #[test]
    fn hint_pairs_focused_raw_normal_is_exactly_one_pair() {
        // C23: every other hint would be a lie — nothing else is intercepted.
        assert_eq!(hint_pairs(&Mode::Normal, false, true, false), vec![("Alt+Shift+p", "exit raw")]);
    }

    #[test]
    fn hint_pairs_dead_beats_raw_when_somehow_both() {
        // A dead pane never raw-routes (`App::raw_routing_active` requires
        // it alive) — but the flag can still be *set* on a dead pane, so the
        // hint bar must show what's actually actionable (dead-pane keys),
        // not a raw-exit hint nothing would honor.
        assert_eq!(
            hint_pairs(&Mode::Normal, true, true, false),
            vec![("↵", "relaunch"), ("f", "fresh — drops resume"), ("Alt+w", "close"), ("Alt+q", "quit")],
        );
    }

    #[test]
    fn hint_bar_right_omits_needs_segment_at_zero() {
        let spans = hint_bar_right_spans(0, None, None, "NORMAL");
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, "NORMAL ");
        assert!(!text.contains('◆'));
    }

    #[test]
    fn hint_bar_right_shows_aggregate_before_mode_word_when_nonzero() {
        let spans = hint_bar_right_spans(3, None, None, "NORMAL");
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, "◆ 3 needs you · Alt+a  NORMAL ");
        assert_eq!(spans[0].style, theme::accent());
    }

    #[test]
    fn dialog_border_style_is_accent_with_no_modifiers() {
        // Pins C12: the old bright-fg/double-border/bold dialog look is
        // gone — one plain accent style for all three modals.
        assert_eq!(dialog_border_style(), theme::accent());
    }

    /// C15 amended: one column is sized to the widest key-column-plus-
    /// description line, not a fixed 52 that clips long descriptions
    /// mid-word. Every key pads to the same column, so the longest
    /// description decides it.
    #[test]
    fn help_content_width_fits_the_widest_row() {
        let widest = HELP_GROUPS
            .iter()
            .flat_map(|g| g.rows.iter())
            .max_by_key(|(k, d)| {
                mouse::display_width(&super::help_key_prefix(k)) + mouse::display_width(d)
            })
            .expect("the keymap has rows");
        assert_eq!(
            help_content_width(),
            mouse::display_width(&super::help_key_prefix(widest.0))
                + mouse::display_width(widest.1),
        );
    }

    #[test]
    fn help_dialog_clamps_to_the_screen_via_centered_near() {
        // `help_layout` clamps its own width to the body, and
        // `centered_near` clamps the placement — same as every other modal
        // (C15's anchoring is unchanged).
        let body = Rect::new(0, 1, 30, 20);
        let (w, h) = help_layout(body).size;
        assert!(w <= body.width);
        let rect = centered_near(body, body, w, h);
        assert!(rect.width <= body.width && rect.height <= body.height);
    }

    #[test]
    fn mode_word_matches_c9_table() {
        assert_eq!(mode_word(&Mode::Normal, false, false), "NORMAL");
        assert_eq!(
            mode_word(&Mode::Rename { buffer: String::new(), cursor: 0, target: RenameTarget::Pane }, false, false),
            "RENAME"
        );
        assert_eq!(mode_word(&Mode::Picker { selection: 0, filter: String::new(), cwd: 0, on_cwd: false }, false, false), "PICKER");
        assert_eq!(mode_word(&Mode::Scroll, false, false), "SCROLL");
        assert_eq!(mode_word(&Mode::Copy { cursor: (0, 0) }, false, false), "COPY");
        assert_eq!(mode_word(&Mode::Help { top: 0 }, false, false), "HELP");
    }

    #[test]
    fn mode_word_shows_zoom_pseudo_state_only_in_the_normal_slot() {
        // C21/amended C9: ZOOM shows only when the mode is Normal — every
        // other mode's own word wins regardless of the zoomed flag.
        assert_eq!(mode_word(&Mode::Normal, true, false), "ZOOM");
        assert_eq!(mode_word(&Mode::Normal, false, false), "NORMAL");
        assert_eq!(mode_word(&Mode::Scroll, true, false), "SCROLL");
        assert_eq!(mode_word(&Mode::Help { top: 0 }, true, false), "HELP");
    }

    #[test]
    fn mode_word_raw_beats_zoom_beats_normal_but_never_a_real_mode_word() {
        // C23/amended C9: in the Normal slot, RAW beats ZOOM beats NORMAL —
        // but any real (non-Normal) mode word still wins over both.
        assert_eq!(mode_word(&Mode::Normal, false, true), "RAW");
        assert_eq!(mode_word(&Mode::Normal, true, true), "RAW", "raw beats zoom");
        assert_eq!(mode_word(&Mode::Scroll, true, true), "SCROLL", "a real mode word always wins");
    }

    #[test]
    fn fit_hint_pairs_right_segment_wins_over_trailing_pairs() {
        // C9 yield order (re-amended 2026-07-22): pairs drop whole from the
        // right before the aggregate/mode-word segment ever yields. Measure
        // via the SAME `hint_pair_cols` the draw path uses — a +3/+4 drift
        // here once let the whole segment drop at widths 111-116 (D2).
        let pairs = hint_pairs(&Mode::Normal, false, false, false);
        let pw = |p: &(&str, &str)| super::hint_pair_cols(p.0, p.1);
        let all_w: u16 = pairs.iter().map(&pw).sum();

        // Roomy bar, no right segment: everything fits.
        assert_eq!(super::fit_hint_pairs(&pairs, 0, all_w + 10), pairs.len());
        // Invariant across every width/segment combo: the shown pairs plus
        // the right segment must ALWAYS fit — the segment is never clipped by
        // an over-optimistic fit. Sweep the band D2 lived in and beyond.
        for width in 80..=140u16 {
            for right_w in [0u16, 22, 30] {
                let shown = super::fit_hint_pairs(&pairs, right_w, width);
                let used: u16 = pairs[..shown].iter().map(&pw).sum();
                assert!(
                    used + right_w <= width || shown == 0,
                    "width={width} right_w={right_w}: shown={shown} used={used} overflows"
                );
                // And it's not needlessly stingy: one more pair really wouldn't fit.
                if shown < pairs.len() {
                    assert!(used + pw(&pairs[shown]) + right_w > width, "width={width}: too stingy");
                }
            }
        }
        // Degenerate: a bar narrower than the segment shows zero pairs (the
        // draw fn then right-aligns whatever of the segment still fits).
        assert_eq!(super::fit_hint_pairs(&pairs, 118, 120), 0);
    }

    #[test]
    fn hint_pairs_dead_focused_normal_offers_relaunch_not_new_pane() {
        let dead = hint_pairs(&Mode::Normal, true, false, false);
        assert_eq!(
            dead,
            vec![
                ("↵", "relaunch"),
                ("f", "fresh — drops resume"),
                ("Alt+w", "close"),
                ("Alt+q", "quit"),
            ],
        );
        // A live pane never offers "relaunch"; a dead one never offers "new".
        assert_ne!(dead, hint_pairs(&Mode::Normal, false, false, false));
    }

    /// C20's list as amended by U25: every key the feed actually answers to.
    /// PgUp/PgDn and `q` worked all along while appearing nowhere, and Enter
    /// is new — the whole point of making entries actionable is that people
    /// can tell they are.
    #[test]
    fn hint_pairs_feed_mode_lists_every_key_the_feed_answers_to() {
        assert_eq!(
            hint_pairs(&Mode::Feed { offset: 0 }, false, false, false),
            vec![
                ("↑↓", "select"),
                ("PgUp/Dn", "page"),
                ("↵", "go to pane"),
                ("q/Esc", "close"),
            ],
        );
    }

    /// C27/C9: the roster's own list, and the 68-column number the contract
    /// quotes — it has to fit beside the right segment at the 100-col floor,
    /// which is the only reason the number is in the contract at all. Note
    /// what is *not* here: the feed's `q`, because in a list you filter by
    /// typing, `q` is filter text (U20's rule).
    #[test]
    fn hint_pairs_roster_mode_is_the_c27_list_and_fits_the_floor() {
        let mode = Mode::Roster { cursor: 1, filter: String::new(), top: 0, status_filter: None };
        let pairs = hint_pairs(&mode, false, false, false);
        assert_eq!(
            pairs,
            vec![
                ("↑↓", "select"),
                ("PgUp/Dn", "page"),
                ("↵", "go to pane"),
                ("type", "filter"),
                ("Tab", "status"),
                ("Esc", "close"),
            ],
        );
        let cols: u16 = pairs.iter().map(|(k, l)| super::hint_pair_cols(k, l)).sum();
        assert_eq!(cols, 81, "C27 quotes 81 columns");
        assert!(cols < 100, "and it must fit beside the right segment at the floor");
        assert!(!pairs.iter().any(|(k, _)| k.contains('q')), "`q` filters, it does not close");
    }

    /// C15 (amended): the hint bar tells the truth in both cases. A keymap
    /// that fits is closed by any key and says so; a scrolled one advertises
    /// the reading keys and narrows the claim to "any *other* key", because
    /// the arrows have stopped closing it.
    #[test]
    fn the_help_hint_row_narrows_only_once_the_keymap_actually_scrolls() {
        let whole = hint_pairs(&Mode::Help { top: 0 }, false, false, false);
        assert_eq!(whole, vec![("Alt+?", "all keys"), ("any key", "close")]);
        let scrolled = hint_pairs(&Mode::Help { top: 0 }, false, false, true);
        assert_eq!(scrolled, vec![("↑↓ PgUp/Dn", "read on"), ("any other key", "close")]);
        let cols: u16 = scrolled.iter().map(|(k, l)| super::hint_pair_cols(k, l)).sum();
        assert!(cols < 100, "the scrolled row still fits beside the right segment: {cols}");
    }

    #[test]
    fn mode_word_roster_wins_regardless_of_zoom() {
        let mode = Mode::Roster { cursor: 1, filter: String::new(), top: 0, status_filter: None };
        assert_eq!(mode_word(&mode, true, false), "ROSTER");
    }

    #[test]
    fn mode_word_feed_wins_regardless_of_zoom() {
        assert_eq!(mode_word(&Mode::Feed { offset: 0 }, false, false), "FEED");
        assert_eq!(mode_word(&Mode::Feed { offset: 0 }, true, false), "FEED");
    }

    #[test]
    fn hint_pairs_rename_word_differs_pane_vs_tab() {
        let pane =
            hint_pairs(&Mode::Rename { buffer: String::new(), cursor: 0, target: RenameTarget::Pane }, false, false, false);
        let tab =
            hint_pairs(&Mode::Rename { buffer: String::new(), cursor: 0, target: RenameTarget::Tab }, false, false, false);
        assert_eq!(pane[0], ("type", "pane name"));
        assert_eq!(tab[0], ("type", "tab name"));
    }

    #[test]
    fn push_tab_spans_active_tab_uses_accent_marker_and_reset_bg() {
        // C2: active tab — marker ▎ `accent`, label `ink` on Color::Reset
        // (fuses with the terminal's own bg), glyph in its own style,
        // separator `rule`.
        let mut spans = Vec::new();
        push_tab_spans(&mut spans, 0, "main", true, theme::GLYPH_WORKING, theme::accent(), 3);
        assert_eq!(spans.len(), 9); // 8 parts + the count cell (C2, amended 2026-07-28)
        assert_eq!(spans[0].content.as_ref(), theme::MARKER_ACTIVE.to_string());
        assert_eq!(spans[0].style, theme::accent());
        assert_eq!(spans[2].content.as_ref(), "1 main");
        assert_eq!(spans[2].style, theme::active_tab_label());
        assert_eq!(spans[2].style.bg, Some(theme::ACTIVE_TAB_BG));
        assert_eq!(spans[4].content.as_ref(), theme::GLYPH_WORKING.to_string());
        assert_eq!(spans[4].style, theme::accent());
        // ...then the count, in the glyph's own style so `●3` is one token.
        assert_eq!(spans[5].content.as_ref(), "3");
        assert_eq!(spans[5].style, theme::accent());
        assert_eq!(spans[7].content.as_ref(), theme::TAB_SEPARATOR.to_string());
        assert_eq!(spans[7].style, theme::rule());
        assert_eq!(spans[8].content.as_ref(), " "); // trailing gutter
    }

    #[test]
    fn push_tab_spans_inactive_tab_uses_blank_marker_and_quiet_label() {
        // C2: inactive tab — marker is a plain space (no accent), label
        // `quiet`, and no bg override: since 2026-07-27 there is no strip
        // fill underneath it either (§2 background policy).
        let mut spans = Vec::new();
        push_tab_spans(&mut spans, 1, "api", false, theme::GLYPH_IDLE, theme::quiet(), 1);
        assert_eq!(spans.len(), 9); // 8 parts + the count cell (C2, amended 2026-07-28)
        assert_eq!(spans[0].content.as_ref(), " ");
        assert_eq!(spans[0].style.fg, None);
        assert_eq!(spans[2].content.as_ref(), "2 api");
        assert_eq!(spans[2].style, theme::quiet());
        assert_eq!(spans[2].style.bg, None);
        assert_eq!(spans[4].content.as_ref(), theme::GLYPH_IDLE.to_string());
        assert_eq!(spans[4].style, theme::quiet());
        // A count below 2 still spends its column — blank, so the width is
        // the same as the `3` above (C2's stability rule).
        assert_eq!(spans[5].content.as_ref(), " ");
    }

    /// C2 (amended 2026-07-28): the count cell's whole vocabulary — blank
    /// below 2, the digit through 9, `+` past that — and the invariant the
    /// tab-width formula rests on: exactly one column, every time.
    #[test]
    fn tab_count_cell_is_blank_below_two_a_digit_to_nine_then_plus() {
        assert_eq!(super::tab_count_cell(0), ' ');
        assert_eq!(super::tab_count_cell(1), ' ', "one is what a glyph already means");
        assert_eq!(super::tab_count_cell(2), '2');
        assert_eq!(super::tab_count_cell(9), '9');
        // Past nine the exact number stops being actionable and two digits
        // would break the one-column rule the hit math is built on.
        assert_eq!(super::tab_count_cell(10), '+');
        assert_eq!(super::tab_count_cell(37), '+');
        for n in [0usize, 1, 2, 5, 9, 10, 11, 100, 1000] {
            assert_eq!(super::tab_count_cell_cols(n), 1, "count {n} must be one column");
        }
    }

    /// The geometry rule the count exists under: a tab's drawn width is the
    /// same before and after its count appears, so nothing on the bar moves
    /// when a background agent's status flips. Measured through the real span
    /// builder, not the formula — that is the half a formula can't prove.
    #[test]
    fn a_tabs_drawn_width_does_not_move_when_its_count_appears() {
        let drawn = |count: usize| -> u16 {
            let mut spans = Vec::new();
            push_tab_spans(&mut spans, 0, "main", true, theme::GLYPH_NEEDS_INPUT, theme::accent(), count);
            spans.iter().map(|s| mouse::display_width(&s.content)).sum()
        };
        let expected = mouse::tab_width(0, "main");
        for count in [0usize, 1, 2, 9, 10, 42] {
            assert_eq!(drawn(count), expected, "count {count} changed the tab's width");
        }
    }

    /// C2 end to end through `draw()`: a tab holding three needy panes draws
    /// `◆3`, and one holding a single needy pane draws `◆` with a blank cell
    /// — the exact confusion the tribunal found (one diamond for any number
    /// of blocked agents), and the fix, in the same frame.
    #[test]
    fn the_tab_bar_counts_panes_in_the_summarized_state() {
        use crate::core::status::AgentStatus;
        use crate::ports::PaneBackend;
        use crate::ui::input::Action;
        use ratatui::backend::TestBackend;
        use ratatui::layout::Size;
        use ratatui::Terminal;

        let mut app = mk_app(Size::new(100, 30));
        app.apply(Action::NewPane);
        app.apply(Action::NewPane); // tab 0: three panes
        app.apply(Action::NewTab); // tab 1: one pane
        let bar = |app: &mut App<crate::ports::fakes::FakePane>| -> String {
            let mut term = Terminal::new(TestBackend::new(100, 30)).unwrap();
            term.draw(|f| super::draw(f, app)).unwrap();
            let buf = term.backend().buffer().clone();
            (0..100).filter_map(|x| buf.cell((x, 0)).map(|c| c.symbol().to_string())).collect()
        };

        for id in [1u64, 2, 3] {
            app.runtimes.get_mut(&id).unwrap().set_extension_status(AgentStatus::NeedsInput);
        }
        let row = bar(&mut app);
        assert!(row.contains("◆3"), "three needy panes count on the tab bar:\n{row}");

        // Knock two of them back down: the same tab, the same glyph, no count.
        for id in [2u64, 3] {
            app.runtimes.get_mut(&id).unwrap().set_extension_status(AgentStatus::Idle);
        }
        let quiet_row = bar(&mut app);
        assert!(!quiet_row.contains("◆3"), "the count follows the fleet:\n{quiet_row}");
        assert!(quiet_row.contains('◆'), "...but the glyph still reports it:\n{quiet_row}");
        // And the bar is the same length either way — the stability rule,
        // observed on the drawn row rather than derived from the formula.
        assert_eq!(row.trim_end().len(), quiet_row.trim_end().len());
    }

    #[test]
    fn collapsed_row_spans_at_zero_width_is_empty_not_panicking() {
        let spans =
            collapsed_row_spans(0, true, Some(AgentStatus::Working), 2, "pi", "pi", true, false, theme::accent());
        assert!(spans.is_empty());
    }

    #[test]
    fn stack_header_text_does_not_panic_when_width_is_smaller_than_content() {
        // The header is gated on the stack area's *height* (C6), not its
        // width, so a tall-but-narrow stack can still ask for a header
        // narrower than " STACK · N PANES" + "ALT+↑↓ ". Must degrade by
        // overflowing the string (Paragraph clips visually, same as the hint
        // bar), not panic.
        let text = stack_header_text(4, 3);
        assert!(text.contains("STACK · 3 PANES"));
        assert!(text.ends_with("ALT+↑↓ "));
    }

    // -- C20 activity feed ---------------------------------------------------

    #[test]
    fn feed_window_shows_the_newest_rows_when_offset_is_zero() {
        assert_eq!(feed_window(5, 0, 3), 2..5);
    }

    #[test]
    fn feed_window_scrolls_back_by_offset() {
        assert_eq!(feed_window(5, 2, 3), 0..3);
    }

    #[test]
    fn feed_window_clamps_offset_past_the_oldest_entry() {
        assert_eq!(feed_window(5, 999, 3), 0..1);
    }

    #[test]
    fn feed_window_shrinks_to_whatever_the_ring_actually_has() {
        assert_eq!(feed_window(2, 0, 10), 0..2);
    }

    #[test]
    fn feed_window_empty_ring_or_zero_rows_is_empty() {
        assert_eq!(feed_window(0, 0, 3), 0..0);
        assert_eq!(feed_window(5, 0, 0), 0..0);
    }

    /// C20's default row is the quiet rung throughout — timestamp and text
    /// alike. They used to be two different greys (DIM and MUTED); §2's
    /// two-rung ramp merged them, and nothing is lost: the timestamp is
    /// column-aligned, which is what actually separates it from the text.
    #[test]
    fn feed_entry_spans_default_styling_is_quiet_throughout() {
        let spans = feed_entry_spans("12:34:56", "spawned shell (shell)", false, false);
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, " 12:34:56  spawned shell (shell)");
        assert_eq!(spans[1].style, theme::quiet());
        assert_eq!(spans[2].style, theme::quiet());
    }

    /// U25: the selected entry — the one Enter acts on — is marked, in the
    /// leading column the unselected rows spend on a space, so the row's
    /// width and every column after it are unchanged.
    #[test]
    fn feed_entry_spans_mark_the_selected_row_without_shifting_a_column() {
        let plain = feed_entry_spans("12:34:56", "spawned shell", false, false);
        let picked = feed_entry_spans("12:34:56", "spawned shell", false, true);
        let text = |v: &[super::Span]| -> String { v.iter().map(|s| s.content.as_ref()).collect() };
        assert_eq!(text(&picked), format!("{}12:34:56  spawned shell", theme::PICKER_SELECTED));
        assert_eq!(text(&plain).chars().count(), text(&picked).chars().count());
        assert_eq!(picked[0].style, theme::accent());
        // Everything past the marker is styled identically either way.
        assert_eq!(picked[1].style, plain[1].style);
        assert_eq!(picked[2].style, plain[2].style);
    }

    #[test]
    fn feed_entry_spans_needs_input_line_gets_the_accent_diamond_and_ink_text() {
        let spans = feed_entry_spans("12:34:56", "pi: waiting → needs you", true, false);
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(
            text,
            format!(" 12:34:56  {} pi: waiting → needs you", theme::GLYPH_NEEDS_INPUT)
        );
        assert_eq!(spans[1].style, theme::quiet());
        assert_eq!(spans[2].style, theme::accent());
        assert_eq!(spans[3].style, theme::ink());
    }

    #[test]
    fn local_hh_mm_ss_formats_as_a_zero_padded_clock() {
        let s = super::local_hh_mm_ss(std::time::SystemTime::now());
        assert_eq!(s.len(), 8);
        assert_eq!(s.as_bytes()[2], b':');
        assert_eq!(s.as_bytes()[5], b':');
    }

    // -- C15/§8 help overlay ---------------------------------------------------

    /// §8's table and the drawn keymap are the same table: **every chord
    /// roost binds appears in the overlay**. This is the check the old ≤20
    /// cap made impossible to state — under a cap, a new chord's options
    /// were "merge a row" or "go undocumented", and the second is exactly
    /// what a keymap must never do.
    #[test]
    fn every_bound_chord_is_documented_in_the_keymap() {
        let text = HELP_GROUPS
            .iter()
            .flat_map(|g| g.rows.iter())
            .map(|(k, d)| format!("{k} {d}"))
            .collect::<Vec<_>>()
            .join("\n");
        for chord in [
            "Alt+n", "Alt+Enter", "Alt+r", "Alt+w", "Alt+u", "Alt+Shift+p", "Alt+Shift+←↓↑→",
            "Alt+s", "Alt+o", "Alt+g", "Alt+z", "Alt+f", "Alt+t", "Alt+1..9", "Alt+0", "Alt+i",
            "Alt+m", "Alt+Shift+i", "Alt+Shift+r", "Alt+a", "Alt+Shift+a", "Alt+e", "Alt+PgUp",
            "Alt+c", "Alt+/", "Alt+?", "Alt+q",
        ] {
            assert!(text.contains(chord), "the keymap must document {chord:?}");
        }
    }

    /// C28's pair is documented as a pair, directly under the chords it is
    /// the shifted form of — the adjacency *is* the explanation ("shift
    /// makes the tab chord take the pane with you").
    #[test]
    fn the_tabs_group_teaches_the_move_pane_chords_under_their_unshifted_siblings() {
        let tabs = HELP_GROUPS.iter().find(|g| g.title == "TABS").expect("a TABS group");
        let step = tabs.rows.iter().position(|(k, _)| *k == "Alt+i / Alt+m").expect("the step row");
        let move_row = tabs
            .rows
            .iter()
            .position(|(k, _)| k.starts_with("Alt+Shift+i"))
            .expect("the move row");
        assert_eq!(move_row, step + 1, "the shifted pair sits directly under the unshifted one");
        assert!(tabs.rows[move_row].1.contains("move this pane"));
    }

    /// U23: the legend is the C5 glyph table, not a hand-copied lookalike —
    /// retheming a glyph must break this, not silently leave the overlay
    /// teaching a symbol roost no longer draws.
    #[test]
    fn help_legend_row_matches_the_theme_glyph_table() {
        let (key, desc) = HELP_GROUPS
            .iter()
            .flat_map(|g| g.rows.iter())
            .find(|(k, _)| *k == "status")
            .copied()
            .expect("the overlay carries a status-glyph legend row");
        assert_eq!(key, "status");
        assert_eq!(desc, super::status_legend_text());
        // The two glyphs the live-QA drive greps the frame for.
        assert!(
            desc.contains(theme::GLYPH_WORKING) && desc.contains(theme::GLYPH_EXITED),
            "{desc}",
        );
    }

    /// C15 (amended): one column while the table fits, a second only when
    /// it does not *and* the terminal is wide enough for two — the fewest
    /// columns that fit, never columns for their own sake.
    #[test]
    fn help_takes_the_fewest_columns_that_fit() {
        let lines = help_lines().len() as u16;
        let content = help_content_width();

        // Tall enough for the whole table: one column, however wide the
        // terminal is.
        let tall = Rect::new(0, 1, content * 4, lines + 2);
        assert_eq!(help_layout(tall).columns.len(), 1, "a body that fits stays one column");

        // Too short, and wide enough for two: two columns.
        let wide = Rect::new(0, 1, content * 2 + super::HELP_GUTTER + 2, lines / 2 + 2);
        let two = help_layout(wide);
        assert_eq!(two.columns.len(), 2, "a short, wide body splits");
        assert_eq!(
            two.columns.iter().map(|c| c.len()).sum::<usize>(),
            lines as usize,
            "…and every single line survives the split",
        );

        // Too short and too narrow: one column, and it scrolls (see
        // `help_scroll_extent`).
        let narrow = Rect::new(0, 1, content + 2, 12);
        let one = help_layout(narrow);
        assert_eq!(one.columns.len(), 1);
        let (visible, total) = super::help_scroll_extent(narrow);
        assert!(visible < total, "a narrow, short body scrolls rather than dropping rows");
    }

    /// A column break never lands inside a group: a heading in one column
    /// with half its chords in the other is worse than either arrangement.
    #[test]
    fn a_second_column_always_starts_on_a_group_heading() {
        let content = help_content_width();
        let wide = Rect::new(0, 1, content * 2 + super::HELP_GUTTER + 2, 14);
        let layout = help_layout(wide);
        assert_eq!(layout.columns.len(), 2);
        assert!(
            matches!(layout.columns[1].first(), Some(HelpLine::Head(_))),
            "the right column opens on a heading: {:?}",
            layout.columns[1].first(),
        );
        assert!(
            matches!(layout.columns[0].first(), Some(HelpLine::Head(_))),
            "…as does the left",
        );
    }

    /// The keymap must be drawable at roost's own 80×24 floor — the case
    /// that used to force the ≤20-row cap. It scrolls there now, but every
    /// row is reachable and nothing is clipped horizontally.
    #[test]
    fn help_fits_the_eighty_column_floor_and_reaches_every_row() {
        let body = Rect::new(0, 1, 80, 22); // 80×24 minus the two bars
        let layout = help_layout(body);
        assert!(layout.size.0 <= 80, "the keymap is {} cols wide", layout.size.0);
        assert!(layout.size.1 <= body.height);
        let (visible, total) = super::help_scroll_extent(body);
        assert_eq!(total, help_lines().len(), "one column at the floor holds the whole table");
        assert!(visible >= 1);
    }

    /// U20: the picker's accelerator has to be visible to be pressed, and
    /// both row styles must agree on where the id starts (the marker column
    /// is the only difference between them).
    #[test]
    fn picker_rows_lead_with_their_number_accelerator() {
        assert_eq!(super::picker_row_body(0, "pi"), " 1 pi");
        assert_eq!(super::picker_row_body(2, "shell"), " 3 shell");
        // Past the ninth there is no accelerator, but the id stays aligned.
        assert_eq!(super::picker_row_body(9, "x"), "   x");
        assert_eq!(
            super::picker_row_body(9, "x").chars().count(),
            super::picker_row_body(0, "x").chars().count(),
        );
    }

    /// U16: the `▏` caret sits AT the insertion point, not always at the
    /// end — the visible half of the cursor motion. Out-of-range values
    /// clamp to the end rather than panicking on a bad slice, and the field
    /// slices on char boundaries so a multi-byte name survives.
    #[test]
    fn rename_field_puts_the_caret_at_the_insertion_point() {
        let caret = theme::RENAME_CURSOR;
        assert_eq!(super::rename_field("abcd", 4), format!("abcd{caret}"));
        assert_eq!(super::rename_field("abcd", 2), format!("ab{caret}cd"));
        assert_eq!(super::rename_field("abcd", 0), format!("{caret}abcd"));
        assert_eq!(super::rename_field("", 0), caret.to_string());
        // Clamped, not panicking.
        assert_eq!(super::rename_field("abcd", 99), format!("abcd{caret}"));
        // Multi-byte: the caret lands between chars, never inside one.
        assert_eq!(super::rename_field("héllo", 2), format!("hé{caret}llo"));
    }

    /// C14 (U20): the cwd column shows the last two path components, so a
    /// screenful of sibling checkouts stays distinguishable without paying
    /// for full paths.
    #[test]
    fn picker_cwd_labels_keep_the_last_two_components() {
        use std::path::Path;
        assert_eq!(super::picker_cwd_label(Path::new("/home/me/src/roost")), "src/roost");
        assert_eq!(super::picker_cwd_label(Path::new("/tmp")), "/tmp", "no doubled slash at the root");
        assert_eq!(super::picker_cwd_label(Path::new("roost")), "roost");
        assert_eq!(super::picker_cwd_label(Path::new("/")), "/");
    }

    /// C14 (U20): the three row states — the focused column's selection
    /// (marker + `ink`), the other column's selection (`ink`, no marker, so
    /// what a launch would use stays readable), everything else `quiet`.
    #[test]
    fn picker_rows_mark_only_the_focused_columns_selection() {
        let (marker, style) = super::row_marks(true, true);
        assert_eq!(marker.content.as_ref(), theme::PICKER_SELECTED.to_string());
        assert_eq!(marker.style, theme::accent());
        assert_eq!(style, theme::ink());

        let (marker, style) = super::row_marks(true, false);
        assert_eq!(marker.content.as_ref(), " ", "no marker in the column without focus");
        assert_eq!(style, theme::ink(), "but still readable as the selection");

        let (marker, style) = super::row_marks(false, true);
        assert_eq!(marker.content.as_ref(), " ");
        assert_eq!(style, theme::quiet());
    }

    /// C14 (U20): the dialog grows to fit the widest cwd label, and stays
    /// the pre-U20 32 columns when there is no cwd column to show.
    #[test]
    fn picker_dialog_width_covers_the_cwd_column() {
        use std::path::PathBuf;
        assert_eq!(super::picker_dialog_width(&[]), 32, "no cwds ⇒ the old dialog");
        let cwds = vec![PathBuf::from("/home/me/src/roost"), PathBuf::from("/tmp")];
        // widest label = "src/roost" (9) ⇒ 16 + 2 + 9 + 2 = 29, floored at
        // 32: the picker must never be narrower than the one it replaced.
        assert_eq!(super::picker_dialog_width(&cwds), 32);
        let long = vec![PathBuf::from("/home/me/a-rather-long/checkout-name")];
        assert_eq!(super::picker_dialog_width(&long), 16 + 2 + 27 + 2, "label = a-rather-long/checkout-name");
    }

    /// U20: the accelerator is on the hint bar too — a key you can only
    /// find by reading the source is not a feature. Amended by U20's second
    /// half: the type-ahead and the cwd column join it, and the list stays
    /// inside the 100-col floor beside the right segment.
    #[test]
    fn hint_pairs_picker_advertises_the_number_accelerators() {
        let pairs = hint_pairs(
            &Mode::Picker { selection: 0, filter: String::new(), cwd: 0, on_cwd: false },
            false,
            false,
            false,
        );
        assert_eq!(
            pairs,
            vec![
                ("↑↓", "choose"),
                ("↵", "open"),
                ("1..9", "launch"),
                ("type", "filter"),
                ("←→", "dir"),
                ("Esc", "cancel"),
            ],
        );
        let cols: u16 = pairs.iter().map(|(k, l)| super::hint_pair_cols(k, l)).sum();
        assert_eq!(cols, 71, "C9: the picker list must stay inside the 100-col floor");
    }

    /// U23: the overlay documents the mouse — the wheel, click-to-focus,
    /// drag-select and the Alt+click URL verb, which had lived only in the
    /// source until now.
    ///
    /// D2 (PR #46 design audit, C29 amendment): widened for double/triple-
    /// click and shift-click-extend — `every_bound_chord_is_documented_in_
    /// the_keymap` above only ever checked Alt chords, so it passed
    /// vacuously over these three (none is a chord); this is the test that
    /// actually has to know they exist.
    #[test]
    fn help_rows_document_every_mouse_verb() {
        let text = HELP_GROUPS
            .iter()
            .flat_map(|g| g.rows.iter())
            .map(|(k, d)| format!("{k} {d}"))
            .collect::<Vec<_>>()
            .join("\n");
        for token in ["mouse", "wheel", "click", "drag", "2x", "3x", "shift", "Alt+click", "URL"] {
            assert!(text.contains(token), "the help overlay must mention {token:?}");
        }
    }

    /// C15 (amended U23): one column may never be wider than the 80-col
    /// floor — `centered_near` would clamp it to the screen and clip a
    /// description mid-word, the exact failure the width rule exists to
    /// stop. The reference rows are long; this is their guard rail.
    #[test]
    fn one_help_column_fits_the_eighty_column_floor() {
        let w = help_layout(Rect::new(0, 1, 200, 200)).size.0;
        assert!(w <= 80, "the keymap is {w} cols wide; the 80-col floor would clip it");
    }

    /// ux P2-15: the overlay used to teach every chord and never mention
    /// that roost has a control CLI at all — the category differentiator
    /// (an outside actor can drive the fleet) was invisible from inside the
    /// product. This pins the fix: a group documenting the verbs, with the
    /// pane-id join (U2's badge/tab id) spelled out rather than assumed.
    #[test]
    fn help_documents_the_control_cli_and_the_pane_id_join() {
        let cli = HELP_GROUPS
            .iter()
            .find(|g| g.title == "CONTROL CLI")
            .expect("a CONTROL CLI group");
        let text =
            cli.rows.iter().map(|(k, d)| format!("{k} {d}")).collect::<Vec<_>>().join("\n");
        for verb in ["roost send", "roost read", "roost status", "roost spawn", "roost wait"] {
            assert!(text.contains(verb), "the control-CLI group must mention {verb:?}");
        }
        // The join key itself, not just the verbs that consume it — the
        // gap U2 named was that nothing said the badge/tab id is what a
        // caller passes.
        assert!(text.contains("<id>"), "the pane-id join must be spelled out:\n{text}");
        assert!(
            text.to_lowercase().contains("badge") || text.to_lowercase().contains("tab"),
            "the join must point back at where the id is shown on screen:\n{text}",
        );
    }

    // -- C24 keyboard copy cursor ----------------------------------------------

    #[test]
    fn cell_in_selection_matches_the_inclusive_reading_order_range() {
        let anchor = (2, 5);
        let cursor = (4, 3);
        assert!(cell_in_selection((2, 5), anchor, cursor), "anchor cell itself");
        assert!(cell_in_selection((4, 3), anchor, cursor), "cursor cell itself");
        assert!(cell_in_selection((3, 0), anchor, cursor), "a row strictly between is fully selected");
        assert!(!cell_in_selection((2, 0), anchor, cursor), "before the anchor column on its row");
        assert!(!cell_in_selection((4, 5), anchor, cursor), "past the cursor column on its row");
        // Order-independent: swapping anchor/cursor must not change the answer.
        assert!(cell_in_selection((3, 0), cursor, anchor));
    }

    #[test]
    fn paint_copy_cursor_reverses_and_underlines_only_inside_a_selection() {
        use crate::core::app::Selection;
        use ratatui::buffer::Buffer;
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let inner = Rect::new(0, 0, 10, 5);
        let render = |cursor: (u16, u16), selection: Option<Selection>| -> Buffer {
            let backend = TestBackend::new(10, 5);
            let mut term = Terminal::new(backend).unwrap();
            term.draw(|f| super::paint_copy_cursor(f, inner, cursor, selection)).unwrap();
            term.backend().buffer().clone()
        };

        // No selection: cursor cell is REVERSED only.
        let buf = render((1, 2), None);
        let cell = buf.cell((2, 1)).unwrap();
        assert!(cell.style().add_modifier.contains(Modifier::REVERSED));
        assert!(!cell.style().add_modifier.contains(Modifier::UNDERLINED));

        // Cursor inside an active selection: REVERSED + UNDERLINED.
        let sel = Selection { pane: 1, anchor: (0, 0), cursor: (2, 5), dragging: false };
        let buf = render((1, 2), Some(sel));
        let cell = buf.cell((2, 1)).unwrap();
        assert!(cell.style().add_modifier.contains(Modifier::REVERSED));
        assert!(cell.style().add_modifier.contains(Modifier::UNDERLINED));

        // Cursor outside the selection: REVERSED only, no UNDERLINED.
        let sel = Selection { pane: 1, anchor: (3, 0), cursor: (4, 5), dragging: false };
        let buf = render((1, 2), Some(sel));
        let cell = buf.cell((2, 1)).unwrap();
        assert!(cell.style().add_modifier.contains(Modifier::REVERSED));
        assert!(!cell.style().add_modifier.contains(Modifier::UNDERLINED));
    }

    #[test]
    fn paint_copy_cursor_out_of_bounds_is_a_safe_no_op() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let inner = Rect::new(0, 0, 10, 5);
        let backend = TestBackend::new(10, 5);
        let mut term = Terminal::new(backend).unwrap();
        // Must not panic even when the cursor sits outside the pane's inner
        // bounds (a stale cursor after a resize, before the next clamp).
        term.draw(|f| super::paint_copy_cursor(f, inner, (99, 99), None)).unwrap();
    }

    // -- full draw() smoke tests at degenerate sizes (fleet features pass) --

    fn mk_app(size: ratatui::layout::Size) -> App<crate::ports::fakes::FakePane> {
        use crate::agents;
        use crate::core::workspace::Workspace;
        use crate::ports::fakes::MemStore;
        use std::path::PathBuf;
        use std::sync::mpsc;

        let store = MemStore::default();
        let (tx, _rx) = mpsc::sync_channel(64);
        let ws = Workspace::default_in(PathBuf::from("/tmp"));
        App::<crate::ports::fakes::FakePane>::new(ws, agents::registry(), Box::new(store), tx, size, (0, 0), None)
            .unwrap()
    }

    /// Every chrome surface that has a drawn state, rendered through the
    /// real `draw()` — the shared fixture for the §2 mechanical gates below.
    /// `FakePane::screen()` is `None`, so nothing here is program output:
    /// every cell in these buffers is chrome roost drew itself.
    fn chrome_buffers() -> Vec<(&'static str, ratatui::buffer::Buffer)> {
        use crate::ui::input::Action;
        use ratatui::backend::TestBackend;
        use ratatui::layout::Size;
        use ratatui::Terminal;

        type Fixture = App<crate::ports::fakes::FakePane>;
        fn snap(app: &mut Fixture) -> ratatui::buffer::Buffer {
            let mut term = Terminal::new(TestBackend::new(100, 30)).unwrap();
            term.draw(|f| super::draw(f, app)).unwrap();
            term.backend().buffer().clone()
        }
        fn three_panes() -> Fixture {
            let mut app = mk_app(Size::new(100, 30));
            app.apply(Action::NewPane);
            app.apply(Action::NewPane);
            app
        }

        let mut out: Vec<(&'static str, ratatui::buffer::Buffer)> = Vec::new();

        let mut app = three_panes();
        out.push(("normal, three tiled panes", snap(&mut app)));

        let mut app = three_panes();
        app.apply(Action::ToggleStack);
        out.push(("collapsed stack rows", snap(&mut app)));

        let mut app = three_panes();
        app.set_flash("copied 12 chars");
        out.push(("flash", snap(&mut app)));

        let mut app = three_panes();
        app.note_key_seen(); // U4's evidence: keys arriving, none with Alt
        assert!(app.show_alt_hint());
        out.push(("alt-trap warning bar", snap(&mut app)));

        for (name, action) in [
            ("help overlay", Action::Help),
            ("picker", Action::QuickLaunch),
            ("activity feed", Action::ToggleFeed),
            ("fleet roster", Action::ToggleRoster),
            ("rename dialog", Action::RenamePane),
            ("scroll mode", Action::ScrollMode),
            ("copy mode", Action::CopyMode),
            ("raw pane", Action::ToggleRaw),
            ("floating scratch pane", Action::ToggleFloat),
            ("zoom", Action::ToggleZoom),
        ] {
            let mut app = three_panes();
            app.apply(action);
            out.push((name, snap(&mut app)));
        }

        // [design-supervisor, vacuous-gate] The "fleet roster" fixture above
        // is unfiltered and all-tied (fresh three_panes()), so the §2 gates
        // below never looked at a filtered title tag or a worst-first
        // reorder — the two things ux P2-11 added. A mixed fleet with a
        // status filter active exercises both: the ◆ pane (last in tab
        // order) sorts first, and the title carries its own colored glyph.
        {
            use crate::core::status::AgentStatus;
            use crate::ports::PaneBackend;
            let mut app = three_panes();
            let needy = app.pane_order()[2];
            app.runtimes.get_mut(&needy).unwrap().set_extension_status(AgentStatus::NeedsInput);
            app.apply(Action::ToggleRoster);
            app.handle_mode_key(crossterm::event::KeyEvent::from(crossterm::event::KeyCode::Tab));
            out.push(("fleet roster, filtered and reordered", snap(&mut app)));
        }

        let mut app = three_panes();
        app.apply(Action::ToggleZoom);
        app.apply(Action::ToggleHints); // the tab bar picks up the mode word
        out.push(("zoom with the hint bar hidden", snap(&mut app)));

        // C16: a dead pane's action bar. The fixture omitted every
        // pane-overlay state, which is exactly how a span-styled (rather than
        // widget-styled) bar hid from these gates — it reversed its ~76 text
        // columns and left the dead program's last output showing through the
        // rest of the row. Both shapes are drawn: with and without a spawn
        // error, since the error adds a second row to the same bar.
        let mut app = three_panes();
        let focused = app.focused;
        app.on_pty_exit(focused);
        out.push(("dead pane action bar", snap(&mut app)));

        let mut app = three_panes();
        let focused = app.focused;
        app.on_pty_exit(focused);
        app.dead.insert(focused, "no such file or directory".into());
        out.push(("dead pane with a spawn error", snap(&mut app)));

        // C17/C29 (PR #46 code review): a lit text selection. Copy mode's
        // REVERSED highlight (C17) and native selection's identical one
        // (C29) both paint through `highlight_selection`, but no fixture
        // above ever set `app.selection` — the "copy mode" entry enters
        // `Mode::Copy` without marking anything — so the §2 gates below
        // never looked at a single reversed-selection cell. A multi-cell
        // span, not a single cell, so the run isn't degenerate.
        let mut app = three_panes();
        let focused = app.focused;
        app.begin_selection(focused, 0, 0);
        app.extend_selection(0, 5);
        out.push(("lit text selection", snap(&mut app)));

        // C30: sub-two-row floor notice. The top-level draw() pre-empts all
        // chrome when area.height < 2; the §2 gates that verify
        // "every cell of every drawn chrome state" are vacuous over this case
        // without a fixture for it. Render the full 80×1 state: the notice
        // only and nothing else, no tab bar (would need height >= 2).
        // This is the one case draw() handles before the usual header/body
        // split, so the snapshot must come from a direct Terminal::draw
        // with the small area — mk_app() and three_panes() both build 100×30,
        // too large to trigger the early return.
        {
            use crate::agents;
            use crate::core::workspace::Workspace;
            use crate::ports::fakes::MemStore;
            use std::path::PathBuf;
            use std::sync::mpsc;

            let store = MemStore::default();
            let (tx, _rx) = mpsc::sync_channel(64);
            let ws = Workspace::default_in(PathBuf::from("/tmp"));
            let mut app = App::<crate::ports::fakes::FakePane>::new(
                ws,
                agents::registry(),
                Box::new(store),
                tx,
                Size::new(80, 1),
                (0, 0),
                None,
            )
            .unwrap();
            let mut term = Terminal::new(TestBackend::new(80, 1)).unwrap();
            term.draw(|f| super::draw(f, &mut app)).unwrap();
            out.push(("sub-two-row floor notice (80×1)", term.backend().buffer().clone()));
        }

        // P21/C17 amendment: a running scrollback search paints its hits
        // REVERSED (current hit also UNDERLINED) directly on the pane grid
        // — `highlight_matches` is unit-tested in isolation (see
        // `search_hits_are_reversed_with_the_current_one_underlined` below),
        // but no `chrome_buffers()` state ever populated `app.search`, so
        // the full `draw()` path — and therefore the three §2 mechanical
        // gates that scan this fixture — never looked at a frame containing
        // one. `Search::over` is the sanctioned test seam for rendering a
        // search without driving a whole app; its `pane` field defaults to
        // 1, so it's retargeted at whichever pane `three_panes()` left
        // focused. Lines span far enough (0..40) that a hit lands inside
        // the pane's inner height regardless of the fresh `FakePane`'s
        // (0, 0) scroll position.
        {
            use crate::core::app::Search;
            let mut app = three_panes();
            let focused = app.focused;
            let lines: Vec<String> = (0..40).map(|i| format!("mark line {i}")).collect();
            let mut search = Search::over(lines, "mark", 3);
            search.pane = focused;
            app.search = Some(search);
            out.push(("scrollback search hits", snap(&mut app)));
        }

        out
    }

    /// [design-supervisor pattern, SG-2's own shape] The three §2 gates below
    /// only audit whatever `chrome_buffers()` happens to produce — a drawn
    /// state the fixture never visits is checked vacuously, which is exactly
    /// how the too-small notice, a lit selection and the filtered roster
    /// each shipped uncovered until someone noticed and patched the fixture
    /// by hand. `chrome_buffers()` has no way to notice *itself* that a new
    /// drawn surface exists; only a human remembering does.
    ///
    /// This closes that gap for exactly one axis: `Mode` gates every modal
    /// chrome surface (C12-C16, C20, C22, C24, C27), and the match below has
    /// **no wildcard arm** — the compiler refuses to build the moment a new
    /// `Mode` variant is added without a decision here, which is "fails
    /// until covered" rather than "silently passes" (the brief's own ask).
    /// It is not a general solution: screen-size variants (C30), status
    /// combinations (the roster filter), and focus permutations (C7's
    /// unfocused expanded-stack edge — see the audit) are not enum-shaped,
    /// so nothing here catches a gap in those axes. That remainder is a
    /// human-must-remember list, recorded in the audit rather than faked
    /// into a check that only looks exhaustive.
    #[test]
    fn every_mode_variant_has_a_chrome_buffers_fixture() {
        // Exhaustive by construction: a new `Mode` variant is a compile
        // error here until this match is taught the fixture-name substring
        // that proves it. Add the `chrome_buffers()` case first, then the
        // label below — in that order, so the assertion loop actually
        // proves the fixture exists rather than describing one that doesn't.
        fn required_fixture_substring(m: &Mode) -> Option<&'static str> {
            match m {
                // The baseline: every fixture is Normal-mode chrome.
                Mode::Normal => None,
                Mode::Rename { .. } => Some("rename dialog"),
                Mode::Picker { .. } => Some("picker"),
                Mode::Scroll => Some("scroll mode"),
                Mode::Copy { .. } => Some("copy mode"),
                Mode::Help { .. } => Some("help overlay"),
                Mode::Feed { .. } => Some("activity feed"),
                Mode::Roster { .. } => Some("fleet roster"),
                Mode::Search { .. } => Some("scrollback search"),
            }
        }
        // One representative value per variant, purely to drive the
        // exhaustive match above — never rendered, so field values are
        // arbitrary placeholders.
        let samples = [
            Mode::Normal,
            Mode::Rename { buffer: String::new(), cursor: 0, target: RenameTarget::Pane },
            Mode::Picker { selection: 0, filter: String::new(), cwd: 0, on_cwd: false },
            Mode::Scroll,
            Mode::Copy { cursor: (0, 0) },
            Mode::Help { top: 0 },
            Mode::Feed { offset: 0 },
            Mode::Roster { cursor: 1, filter: String::new(), top: 0, status_filter: None },
            Mode::Search { copy_cursor: None },
        ];
        let names: Vec<&str> = chrome_buffers().iter().map(|(n, _)| *n).collect();
        for m in &samples {
            if let Some(needle) = required_fixture_substring(m) {
                assert!(
                    names.iter().any(|n| n.contains(needle)),
                    "a Mode variant has no chrome_buffers() fixture containing {needle:?}: {names:?}",
                );
            }
        }
    }

    /// C16 regression: the dead-pane action bar reverses its **whole inner
    /// width**, not just the columns its text happens to occupy. Styling the
    /// span instead of the widget left the dead program's last output visible
    /// at normal video across the rest of the row — three bars (C10 flash,
    /// C11 alt warning, C16 dead pane) meant to read as one treatment, one of
    /// them ragged. Caught by the design supervisor; the gate fixture now
    /// draws the state that hid it.
    #[test]
    fn the_dead_pane_bar_reverses_its_whole_row() {
        for (name, buf) in chrome_buffers() {
            if !name.starts_with("dead pane") {
                continue;
            }
            let area = *buf.area();
            // The bar is the bottom row of the dead pane's inner area. Find
            // it by its text, then assert the reversal runs across the row
            // rather than stopping where the message does.
            let row = (0..area.height)
                .find(|y| {
                    (0..area.width)
                        .map(|x| buf[(x, *y)].symbol())
                        .collect::<String>()
                        .contains("exited — Enter")
                })
                .unwrap_or_else(|| panic!("{name}: no dead-pane bar drawn"));
            let reversed: Vec<u16> = (0..area.width)
                .filter(|x| buf[(*x, row)].style().add_modifier.contains(Modifier::REVERSED))
                .collect();
            assert!(!reversed.is_empty(), "{name}: the bar is not reversed at all");
            let (first, last) = (reversed[0], *reversed.last().unwrap());
            assert_eq!(
                reversed.len() as u16,
                last - first + 1,
                "{name}: the bar's reversal is not contiguous across its row"
            );
            // Wider than the message itself — what the span-styled version
            // could not be.
            let text_cols =
                (0..area.width).filter(|x| buf[(*x, row)].symbol() != " ").count();
            assert!(
                (last - first + 1) as usize > text_cols,
                "{name}: the bar covers only its own glyphs, not the row"
            );
        }
    }

    /// §2 gate: **structure-only discipline.** `theme::rule()` (ANSI 8) is
    /// the one theme-variant colour roost spends, and it may never carry a
    /// word — a theme that renders ANSI 8 nearly invisible must cost a
    /// hairline, not a label. Mechanical, over the real drawn frames: any
    /// `DarkGray` cell has to be a rule glyph.
    #[test]
    fn structure_colour_never_carries_text() {
        // Plain single-line box drawing (pane/modal borders), the tab
        // separator, and blank fill. Nothing that spells anything.
        const RULE_GLYPHS: &[&str] =
            &[" ", "─", "│", "┌", "┐", "└", "┘", "├", "┤", "┬", "┴", "┼"];
        for (name, buf) in chrome_buffers() {
            let area = *buf.area();
            for y in area.y..area.y + area.height {
                for x in area.x..area.x + area.width {
                    let cell = buf.cell((x, y)).expect("cell inside the buffer");
                    if cell.style().fg != Some(Color::DarkGray) {
                        continue;
                    }
                    assert!(
                        RULE_GLYPHS.contains(&cell.symbol()),
                        "{name}: structure colour carries text {:?} at ({x},{y})",
                        cell.symbol(),
                    );
                }
            }
        }
    }

    /// §2 gate: **no chrome element pairs two theme-variant colours.** Chrome
    /// paints no colour fill at all — the two bars, the flash, the dead-pane
    /// and alt-warning bars and the focused collapsed row all lost theirs —
    /// so the only `bg` a chrome cell may carry is the `Color::Reset`
    /// sentinel. Attention surfaces reverse the terminal's own pair instead,
    /// which is contrasty in any theme by construction.
    #[test]
    fn chrome_paints_no_background_fill() {
        for (name, buf) in chrome_buffers() {
            let area = *buf.area();
            for y in area.y..area.y + area.height {
                for x in area.x..area.x + area.width {
                    let cell = buf.cell((x, y)).expect("cell inside the buffer");
                    match cell.style().bg {
                        None | Some(Color::Reset) => {}
                        Some(other) => panic!(
                            "{name}: chrome fills with {other:?} at ({x},{y}) — §2 allows no fill"
                        ),
                    }
                }
            }
        }
    }

    /// The heart of the 2026-07-27 stance: every word roost draws is the
    /// terminal's own foreground on its own background — the one contrast
    /// pair the user has already validated — optionally one rung quieter.
    /// A chrome cell carrying a *letter* may therefore only be `Reset`, the
    /// accent red, or unstyled; never a colour the theme is free to swallow.
    #[test]
    fn every_chrome_word_is_drawn_in_ink_the_user_already_reads() {
        let legible = [Color::Reset, Color::Red, Color::LightRed];
        for (name, buf) in chrome_buffers() {
            let area = *buf.area();
            for y in area.y..area.y + area.height {
                for x in area.x..area.x + area.width {
                    let cell = buf.cell((x, y)).expect("cell inside the buffer");
                    if !cell.symbol().chars().any(char::is_alphanumeric) {
                        continue;
                    }
                    let Some(fg) = cell.style().fg else { continue };
                    assert!(
                        legible.contains(&fg),
                        "{name}: the word cell {:?} at ({x},{y}) is drawn in {fg:?}",
                        cell.symbol(),
                    );
                }
            }
        }
    }

    /// U15: hiding the hint bar used to hide the only mode indication
    /// there was — a zoomed pane with hints off read as a one-pane tab, and
    /// SCROLL/COPY/RAW vanished entirely. The word moves to the tab bar
    /// exactly when the bar isn't carrying it, and only when there's
    /// something to say (plain Normal reports nothing, as always).
    #[test]
    fn the_tab_bar_carries_the_mode_word_only_when_the_hint_bar_is_gone() {
        use crate::ui::input::Action;
        use ratatui::layout::Size;

        let mut app = mk_app(Size::new(100, 30));
        app.apply(Action::ToggleZoom);
        assert!(app.hints_shown());
        assert_eq!(super::tab_status_word(&app), None, "the hint bar has it");

        app.apply(Action::ToggleHints); // Alt+/
        assert_eq!(super::tab_status_word(&app), Some("ZOOM"));
        app.apply(Action::ToggleZoom);
        assert_eq!(super::tab_status_word(&app), None, "plain Normal says nothing");

        // Real modes and the other pseudo-state come along too.
        app.apply(Action::ScrollMode);
        assert_eq!(super::tab_status_word(&app), Some("SCROLL"));
        app.mode = Mode::Normal;
        app.apply(Action::ToggleRaw);
        assert_eq!(super::tab_status_word(&app), Some("RAW"));
        app.apply(Action::ToggleRaw);

        // A terminal too short for the hint bar is "the bar is absent" too.
        let mut tiny = mk_app(Size::new(100, 2));
        tiny.apply(Action::ToggleZoom);
        assert!(!tiny.hints_shown());
        assert_eq!(super::tab_status_word(&tiny), Some("ZOOM"));
    }

    /// ...and it actually reaches the screen: the drawn tab bar carries
    /// `ZOOM · {cwd} · saved ✓` once hints are off.
    #[test]
    fn a_zoomed_pane_with_hints_hidden_still_says_zoom_on_the_tab_bar() {
        use crate::ui::input::Action;
        use ratatui::backend::TestBackend;
        use ratatui::layout::Size;
        use ratatui::Terminal;

        let mut app = mk_app(Size::new(100, 30));
        app.apply(Action::ToggleZoom);
        app.apply(Action::ToggleHints);
        let mut term = Terminal::new(TestBackend::new(100, 30)).unwrap();
        term.draw(|f| super::draw(f, &mut app)).unwrap();
        let row: String =
            (0..100).filter_map(|x| term.backend().buffer().cell((x, 0)).map(|c| c.symbol().to_string())).collect();
        assert!(row.contains("ZOOM · "), "tab bar row was: {row:?}");
        assert!(row.trim_end().ends_with(theme::SAVED), "the save word still trails: {row:?}");
    }

    /// C14 (U20), through the real `draw`: the picker paints two columns —
    /// numbered adapter rows on the left, recent directories on the right —
    /// the type-ahead query rides in the title so a narrowed list says why
    /// it is narrow, and `❯` marks only the column holding the keyboard.
    #[test]
    fn the_picker_draws_both_columns_and_the_live_filter() {
        use crate::core::app::Mode;
        use crate::ui::input::Action;
        use ratatui::backend::TestBackend;
        use ratatui::layout::Size;
        use ratatui::Terminal;

        let mut app = mk_app(Size::new(100, 30));
        app.apply(Action::QuickLaunch);
        let rows = |app: &mut App<crate::ports::fakes::FakePane>| -> Vec<String> {
            let backend = TestBackend::new(100, 30);
            let mut term = Terminal::new(backend).unwrap();
            term.draw(|f| super::draw(f, app)).unwrap();
            let buf = term.backend().buffer().clone();
            (0..30)
                .map(|y| {
                    (0..100)
                        .filter_map(|x| buf.cell((x, y)).map(|c| c.symbol().to_string()))
                        .collect::<String>()
                })
                .collect()
        };

        let drawn = rows(&mut app).join("\n");
        assert!(drawn.contains("pick agent"), "the unfiltered title:\n{drawn}");
        assert!(drawn.contains("1 pi"), "the numbered adapter column:\n{drawn}");
        // `mk_app`'s workspace lives in /tmp, so that is the seeded cwd row.
        assert!(drawn.contains("/tmp"), "the recent-cwd column:\n{drawn}");
        assert!(drawn.contains(theme::PICKER_SELECTED), "the adapter column has the marker");

        // Type-ahead: the title carries the query and the rows narrow. Two
        // characters, not one: a bare `p` also matches `opencode`, so it no
        // longer isolates a single row — and the point here is that the
        // filter narrows to exactly one, not that any particular adapter
        // happens to be alone under a one-letter query.
        for c in ['p', 'i'] {
            app.handle_mode_key(crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Char(c),
                crossterm::event::KeyModifiers::NONE,
            ));
        }
        let drawn = rows(&mut app).join("\n");
        assert!(drawn.contains(&format!("pi{}", theme::RENAME_CURSOR)), "the live query:\n{drawn}");
        assert!(drawn.contains("1 pi"), "`pi` keeps pi:\n{drawn}");
        assert!(!drawn.contains("claude"), "and drops the rest:\n{drawn}");
        assert!(!drawn.contains("opencode"), "including the other `p` match:\n{drawn}");
        assert!(!drawn.contains(" 2 "), "no second row survives the filter:\n{drawn}");
        assert!(matches!(app.mode, Mode::Picker { .. }));
    }

    /// C27, end to end through the real `draw()`: the roster groups panes
    /// under underlined tab headers, spells each pane row in C8's collapsed
    /// format (id + name + `{adapter} · {state word}`), marks the cursor with
    /// the picker's `❯`, lists a pane from a tab that is **not** active, and
    /// puts the live type-ahead query in the frame title.
    #[test]
    fn the_roster_draws_grouped_rows_a_cursor_and_the_live_filter() {
        use crate::ui::input::Action;
        use ratatui::backend::TestBackend;
        use ratatui::layout::Size;
        use ratatui::Terminal;

        let mut app = mk_app(Size::new(100, 30));
        app.apply(Action::NewPane); // tab0: two panes
        app.apply(Action::NewTab); // tab1 ("tab2"): one pane, now active
        let drawn = |app: &mut App<crate::ports::fakes::FakePane>| -> String {
            let mut term = Terminal::new(TestBackend::new(100, 30)).unwrap();
            term.draw(|f| super::draw(f, app)).unwrap();
            let buf = term.backend().buffer().clone();
            (0..30)
                .map(|y| {
                    (0..100)
                        .filter_map(|x| buf.cell((x, y)).map(|c| c.symbol().to_string()))
                        .collect::<String>()
                })
                .collect::<Vec<_>>()
                .join("\n")
        };

        app.apply(Action::ToggleRoster);
        let frame = drawn(&mut app);
        assert!(frame.contains(" fleet "), "the frame's title:\n{frame}");
        assert!(frame.contains("1 MAIN · 2 PANES"), "tab 1's group header:\n{frame}");
        assert!(frame.contains("2 TAB2 · 1 PANE"), "tab 2's group header:\n{frame}");
        // C8's row format, for a pane in the *inactive* tab — the whole point
        // of the feature. `shell · /tmp` untitled ⇒ the state word alone.
        assert!(frame.contains("1 shell · tmp"), "an inactive tab's pane row:\n{frame}");
        assert!(frame.contains("idle") || frame.contains("your turn"), "a state word:\n{frame}");
        assert!(frame.contains(theme::PICKER_SELECTED), "the cursor marker:\n{frame}");
        assert!(frame.contains("ROSTER"), "the C9 mode word:\n{frame}");

        // Type-ahead: the title carries the query, and a group with no
        // surviving pane loses its header too.
        for c in "3".chars() {
            app.handle_mode_key(crossterm::event::KeyEvent::from(
                crossterm::event::KeyCode::Char(c),
            ));
        }
        let frame = drawn(&mut app);
        assert!(frame.contains(&format!("fleet — 3{}", theme::RENAME_CURSOR)), "query:\n{frame}");
        assert!(frame.contains("2 TAB2 · 1 PANE"), "pane 3 lives in tab 2:\n{frame}");
        assert!(!frame.contains("1 MAIN · 2 PANES"), "tab 1 filtered away whole:\n{frame}");
    }

    /// [ux P2-11], end to end through the real `draw()`: `Tab` narrows the
    /// drawn rows to one severity tier and tags the frame title with that
    /// tier's own glyph — the same discoverability idiom the type-ahead
    /// query already has.
    #[test]
    fn the_roster_status_filter_narrows_rows_and_tags_the_title() {
        use crate::core::status::AgentStatus;
        use crate::ports::PaneBackend;
        use crate::ui::input::Action;
        use crossterm::event::{KeyCode, KeyEvent};
        use ratatui::backend::TestBackend;
        use ratatui::layout::Size;
        use ratatui::Terminal;

        let mut app = mk_app(Size::new(100, 30));
        app.apply(Action::NewPane); // panes 1 (needy below), 2 (stays idle)
        app.runtimes.get_mut(&1).unwrap().set_extension_status(AgentStatus::NeedsInput);
        app.apply(Action::ToggleRoster);
        app.handle_mode_key(KeyEvent::from(KeyCode::Tab)); // cycle to NeedsInput

        let mut term = Terminal::new(TestBackend::new(100, 30)).unwrap();
        term.draw(|f| super::draw(f, &mut app)).unwrap();
        let buf = term.backend().buffer().clone();
        let frame: String = (0..30)
            .map(|y| {
                (0..100)
                    .filter_map(|x| buf.cell((x, y)).map(|c| c.symbol().to_string()))
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            frame.contains(&format!("fleet — {} only", theme::GLYPH_NEEDS_INPUT)),
            "the title tags the active tier:\n{frame}"
        );
        assert!(frame.contains("needs you"), "the ◆ row survives:\n{frame}");
        assert!(!frame.contains("idle"), "the idle pane is filtered out:\n{frame}");
    }

    /// [ux P2-10], end to end through the real `draw()`: a quiet shell's
    /// heuristic `Waiting` draws `idle`/`·`, never `your turn`/`○` — C8's own
    /// state word, fed by `App::display_status` rather than the raw
    /// `row_status`. Checked on the corner badge's glyph (C4 carries no
    /// spelled-out word, only `theme::status_style`'s glyph) and the roster
    /// row's word (C27 reuses C8's format verbatim, glyph and word both).
    #[test]
    fn a_quiet_shells_waiting_draws_as_idle_not_your_turn() {
        use crate::core::status::AgentStatus;
        use crate::ports::PaneBackend;
        use crate::ui::input::Action;
        use ratatui::backend::TestBackend;
        use ratatui::layout::Size;
        use ratatui::Terminal;

        let mut app = mk_app(Size::new(100, 30));
        let id = app.pane_order()[0];
        app.runtimes.get_mut(&id).unwrap().set_extension_status(AgentStatus::Waiting);

        let drawn_lines = |app: &mut App<crate::ports::fakes::FakePane>| -> Vec<String> {
            let mut term = Terminal::new(TestBackend::new(100, 30)).unwrap();
            term.draw(|f| super::draw(f, app)).unwrap();
            let buf = term.backend().buffer().clone();
            (0..30)
                .map(|y| {
                    (0..100)
                        .filter_map(|x| buf.cell((x, y)).map(|c| c.symbol().to_string()))
                        .collect::<String>()
                })
                .collect()
        };

        // The corner badge, in the ordinary single-pane view: idle's `·`
        // glyph (theme::GLYPH_IDLE), not waiting's `○`. Scoped to the
        // badge's own row — the tab bar's separate (untouched by this fix,
        // by design: item 4 scopes out C2/C5's tab aggregate) glyph lives
        // above it and would otherwise collide with a whole-frame search.
        let lines = drawn_lines(&mut app);
        let badge_row = lines.iter().find(|l| l.contains("1 shell")).expect("the badge row");
        assert!(badge_row.contains(theme::GLYPH_IDLE), "the corner badge's glyph reads idle:\n{badge_row}");
        assert!(!badge_row.contains(theme::GLYPH_WAITING), "not waiting's ○:\n{badge_row}");

        // The roster row (C27 reuses C8's format verbatim) spells the word.
        app.apply(Action::ToggleRoster);
        let frame = drawn_lines(&mut app).join("\n");
        assert!(frame.contains("idle"), "the roster row reads idle:\n{frame}");
        assert!(!frame.contains("your turn"), "\"your turn\" is false for a shell:\n{frame}");
    }

    /// C15 (amended), end to end through the real `draw()`: the keymap
    /// draws its groups as underlined headings, spells each chord in the
    /// key column, and — on a body too short for the whole table — says so
    /// in its own title rather than silently ending mid-list.
    #[test]
    fn the_keymap_draws_grouped_rows_and_admits_when_it_is_scrolled() {
        use crate::ui::input::Action;
        use ratatui::backend::TestBackend;
        use ratatui::layout::Size;
        use ratatui::Terminal;

        let mut app = mk_app(Size::new(100, 30));
        app.apply(Action::Help);
        let mut term = Terminal::new(TestBackend::new(100, 30)).unwrap();
        term.draw(|f| super::draw(f, &mut app)).unwrap();
        let buf = term.backend().buffer().clone();
        let frame: String = (0..30)
            .map(|y| {
                (0..100)
                    .filter_map(|x| buf.cell((x, y)).map(|c| c.symbol().to_string()))
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");

        assert!(frame.contains("PANES"), "a group heading:\n{frame}");
        assert!(frame.contains("Alt+n"), "a chord from it:\n{frame}");
        assert!(frame.contains("any key closes"), "the way out is in the title:\n{frame}");

        // The heading wears C6's rule across its own column, exactly as the
        // roster's group rows do — not just uppercase text.
        let (hy, hx) = (0..30)
            .flat_map(|y| (0..100).map(move |x| (y, x)))
            .find(|&(y, x)| {
                buf.cell((x, y)).is_some_and(|c| c.symbol() == "P")
                    && (0..5).all(|i| {
                        buf.cell((x + i, y)).is_some_and(|c| c.symbol() == &"PANES"[i as usize..][..1])
                    })
            })
            .expect("the PANES heading is on screen");
        assert!(
            buf.cell((hx, hy)).unwrap().modifier.contains(Modifier::UNDERLINED),
            "the heading is underlined (C6's idiom)",
        );

        // A 30-row terminal cannot hold the whole table, so the title has to
        // own up to it — the alternative is a list that just stops.
        let (visible, total) = super::help_scroll_extent(app.body_area());
        assert!(visible < total, "the fixture is genuinely scrolled");
        assert!(frame.contains(&format!("/{total}")), "the title counts the rows:\n{frame}");
        assert!(frame.contains("↑↓ more"), "…and names the keys that reach them:\n{frame}");
    }

    /// C27 + C5, the headline restore case: a workspace reloaded from disk
    /// has runtimes for the active tab only (`spawn_active_tab`), so the
    /// first `Alt+Shift+a` of a session lists panes that have never been
    /// started. Those rows must read "not started" behind the tab bar's own
    /// `·`, never "exited" behind `✕` — the roster used to call every
    /// runtime-less pane dead, which showed a restored fleet as a morgue
    /// while the tab strip above it said the opposite in the same frame.
    #[test]
    fn a_restored_tab_that_has_never_spawned_reads_not_started_not_exited() {
        use crate::agents;
        use crate::ports::fakes::{FakePane, MemStore};
        use crate::ui::input::Action;
        use ratatui::backend::TestBackend;
        use ratatui::layout::Size;
        use ratatui::Terminal;
        use std::sync::mpsc;

        // Build a two-tab workspace, then *reload* it the way a restart
        // does: a fresh App over the same saved `Workspace`.
        let size = Size::new(100, 30);
        let mut built = mk_app(size);
        built.apply(Action::NewTab); // tab2, now active
        built.apply(Action::NewPane); // tab2: two panes
        built.apply(Action::PrevTab); // leave tab 1 as the active one
        let ws = built.ws.clone();
        let (tx, _rx) = mpsc::sync_channel(64);
        let mut app = App::<FakePane>::new(
            ws,
            agents::registry(),
            Box::new(MemStore::default()),
            tx,
            size,
            (0, 0),
            None,
        )
        .unwrap();

        // Precondition: tab 2's panes really are runtime-less — this test is
        // worthless if the restore spawned them after all.
        let unspawned: Vec<crate::core::layout::PaneId> =
            app.ws.tabs[1].panes.keys().copied().filter(|id| !app.runtimes.contains_key(id)).collect();
        assert_eq!(unspawned.len(), 2, "a non-active restored tab spawns nothing");
        for id in &unspawned {
            assert_eq!(app.row_status(*id), None, "pane {id} has no runtime and is not dead");
        }

        app.apply(Action::ToggleRoster);
        let mut term = Terminal::new(TestBackend::new(100, 30)).unwrap();
        term.draw(|f| super::draw(f, &mut app)).unwrap();
        let buf = term.backend().buffer().clone();
        let frame: String = (0..30)
            .map(|y| {
                (0..100)
                    .filter_map(|x| buf.cell((x, y)).map(|c| c.symbol().to_string()))
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");

        assert!(frame.contains("2 TAB2 · 2 PANES"), "the unspawned tab is listed:\n{frame}");
        assert_eq!(frame.matches("not started").count(), 2, "both its rows:\n{frame}");
        assert!(!frame.contains("exited"), "nothing in the roster is dead:\n{frame}");
        assert!(!frame.contains(theme::GLYPH_EXITED), "no ✕ glyph anywhere:\n{frame}");

        // …and the row's glyph is the same one the tab bar puts on that tab
        // in this very frame, which is the whole point of routing both
        // through `TabSummary::Unknown`.
        let (tab_glyph, _) = theme::tab_summary_style(crate::core::app::TabSummary::Unknown);
        let row: String = super::roster_row_spans(
            &app,
            &RosterRow::Pane { id: unspawned[0] },
            50,
            app.focused,
            theme::accent(),
        )
        .iter()
        .map(|s| s.content.to_string())
        .collect();
        assert!(row.contains(tab_glyph), "row {row:?} wears the tab's own glyph {tab_glyph:?}");
    }

    /// C27 borrows C6's header idiom: the group row is underlined edge to
    /// edge (text *and* the fill after it), so it reads as a rule rather than
    /// as another pane.
    #[test]
    fn roster_group_headers_are_underlined_across_the_whole_row() {
        let row = RosterRow::Group { label: " 1 MAIN · 2 PANES".to_string() };
        let mut app = mk_app(ratatui::layout::Size::new(100, 30));
        app.apply(crate::ui::input::Action::ToggleRoster);
        let spans = super::roster_row_spans(&app, &row, 40, 1, theme::accent());
        let text: String = spans.iter().map(|s| s.content.to_string()).collect();
        assert_eq!(mouse::display_width(&text), 40, "the label is padded to the row width");
        for s in &spans {
            assert!(
                s.style.add_modifier.contains(Modifier::UNDERLINED),
                "every cell of the header carries the rule: {s:?}"
            );
        }
    }

    /// C27's two marks mean two different things: `❯` is the cursor (what
    /// Enter acts on), `▎` is the focused pane (where you are). A row that is
    /// both carries both, and the pane row behind the cursor column is C8's
    /// own `collapsed_row_spans` output verbatim.
    #[test]
    fn roster_pane_rows_carry_the_cursor_and_focus_marks_separately() {
        let mut app = mk_app(ratatui::layout::Size::new(100, 30));
        app.apply(crate::ui::input::Action::NewPane);
        app.apply(crate::ui::input::Action::ToggleRoster);
        let focused = app.focused;
        let other = app.pane_order().into_iter().find(|id| *id != focused).expect("two panes");

        let render = |app: &App<crate::ports::fakes::FakePane>, id, cursor| -> String {
            super::roster_row_spans(app, &RosterRow::Pane { id }, 50, cursor, theme::accent())
                .iter()
                .map(|s| s.content.to_string())
                .collect()
        };
        let cursor_on_focused = render(&app, focused, focused);
        assert!(cursor_on_focused.starts_with(theme::PICKER_SELECTED), "{cursor_on_focused:?}");
        assert!(cursor_on_focused.contains(theme::MARKER_ACTIVE), "{cursor_on_focused:?}");

        let neither = render(&app, other, focused);
        assert!(!neither.starts_with(theme::PICKER_SELECTED), "{neither:?}");
        assert!(!neither.contains(theme::MARKER_ACTIVE), "{neither:?}");
        // The row's body is C8's, one column narrower than the roster row.
        let c8: String = collapsed_row_spans(
            49,
            false,
            Some(AgentStatus::Idle),
            other,
            &app.display_name(other),
            "shell",
            false,
            false,
            theme::quiet(),
        )
        .iter()
        .map(|s| s.content.to_string())
        .collect();
        assert_eq!(neither, format!(" {c8}"));
    }

    #[test]
    fn draw_does_not_panic_at_the_80x24_floor_with_float_and_feed_open() {
        // C22's stacking order draws the float under the feed modal; at the
        // spec's own 80x24 floor both are live at once whenever the float
        // was already shown before Alt+e (toggling feed doesn't hide it) —
        // a combination no existing test drove through the real draw()
        // pipeline end to end.
        use crate::ui::input::Action;
        use ratatui::backend::TestBackend;
        use ratatui::layout::Size;
        use ratatui::Terminal;

        let mut app = mk_app(Size::new(80, 24));
        app.apply(Action::ToggleFloat);
        app.apply(Action::ToggleFeed);
        assert!(matches!(app.mode, Mode::Feed { .. }));
        assert!(app.display_rects().len() >= 2, "float should still be in the display list");

        let backend = TestBackend::new(80, 24);
        let mut term = Terminal::new(backend).unwrap();
        term.draw(|f| super::draw(f, &mut app)).unwrap();
    }

    /// C27 shares C20's geometry, so it inherits the 80×24 proof — but the
    /// roster also draws *rows built from other rows* (a group header padded
    /// to the inner width, a C8 row one column narrower), which is where a
    /// width underflow would land. Driven through the real `draw()` at the
    /// spec's floor and at the 36×10 minimum, with a fleet big enough to
    /// overflow the window.
    #[test]
    fn the_roster_draws_at_the_floor_and_below_it_without_panicking() {
        use crate::ui::input::Action;
        use ratatui::backend::TestBackend;
        use ratatui::layout::Size;
        use ratatui::Terminal;

        for size in [Size::new(80, 24), Size::new(36, 10)] {
            let mut app = mk_app(size);
            for _ in 0..4 {
                app.apply(Action::NewPane);
            }
            app.apply(Action::NewTab);
            app.apply(Action::ToggleRoster);
            let mut term =
                Terminal::new(TestBackend::new(size.width, size.height)).unwrap();
            term.draw(|f| super::draw(f, &mut app)).unwrap();
        }
    }

    /// C15 (amended): the keymap draws at the 80×24 floor, at the 36×10
    /// split floor beneath it, and on a body so short the overlay has room
    /// for no content rows at all — the arithmetic the column/scroll layout
    /// added is exactly where an off-by-one would panic, and the one modal
    /// you open when lost must never be the one that crashes.
    #[test]
    fn the_keymap_draws_at_the_floor_and_below_it_without_panicking() {
        use crate::ui::input::Action;
        use ratatui::backend::TestBackend;
        use ratatui::layout::Size;
        use ratatui::Terminal;

        for size in [Size::new(80, 24), Size::new(200, 26), Size::new(36, 10), Size::new(20, 4)] {
            let mut app = mk_app(size);
            app.apply(Action::Help);
            let mut term = Terminal::new(TestBackend::new(size.width, size.height)).unwrap();
            term.draw(|f| super::draw(f, &mut app)).unwrap();
            // …and scrolled to its end, where `top` is at its largest.
            let (visible, total) = super::help_scroll_extent(app.body_area());
            app.mode = Mode::Help { top: total.saturating_sub(visible) };
            term.draw(|f| super::draw(f, &mut app)).unwrap();
        }
    }

    #[test]
    fn draw_does_not_panic_at_36x10_single_pane() {
        // 36x10 (app.rs's MIN_SPLIT_COLS/ROWS) is the smallest size roost's
        // own split gate considers usable; a lone unsplit pane should still
        // draw cleanly at that floor.
        use ratatui::backend::TestBackend;
        use ratatui::layout::Size;
        use ratatui::Terminal;

        let mut app = mk_app(Size::new(36, 10));
        let backend = TestBackend::new(36, 10);
        let mut term = Terminal::new(backend).unwrap();
        term.draw(|f| super::draw(f, &mut app)).unwrap();
    }

    /// ux P3-16: below the tab-bar-plus-one-body-row floor, `draw` used to
    /// `return` with nothing drawn — an empty screen with no explanation.
    /// `draw_too_small` is what it shows instead; pinned directly (no
    /// `App` needed — it only ever takes a `Rect`) at 1×1, the tightest
    /// non-zero case the reviewer's stress pass exercised, and at two
    /// shapes either side of it so the notice is proven to degrade with
    /// the terminal rather than just surviving one magic size.
    #[test]
    fn too_small_notice_is_never_blank_and_never_panics() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        for (w, h) in [(1u16, 1u16), (1, 20), (2, 10)] {
            let backend = TestBackend::new(w, h);
            let mut term = Terminal::new(backend).unwrap();
            term.draw(|f| super::draw_too_small(f, Rect::new(0, 0, w, h))).unwrap();
            let buf = term.backend().buffer().clone();
            let area = *buf.area();
            let non_blank = (area.y..area.y + area.height).any(|y| {
                (area.x..area.x + area.width).any(|x| buf.cell((x, y)).unwrap().symbol() != " ")
            });
            assert!(non_blank, "{w}x{h}: the too-small notice drew an entirely blank screen");
        }
    }

    /// The same guarantee through the real entry point: `draw` must reach
    /// `draw_too_small` at the exact boundary it bails at (`height < 2`,
    /// i.e. zero rows and one row) without panicking, at a realistic
    /// width — the case a resize event can actually hand it.
    #[test]
    fn draw_does_not_panic_below_the_two_row_floor() {
        use ratatui::backend::TestBackend;
        use ratatui::layout::Size;
        use ratatui::Terminal;

        for size in [Size::new(80, 0), Size::new(80, 1)] {
            let mut app = mk_app(size);
            let backend = TestBackend::new(size.width, size.height);
            let mut term = Terminal::new(backend).unwrap();
            term.draw(|f| super::draw(f, &mut app)).unwrap();
        }
    }

    /// Blit a parsed screen into a buffer of the same size and read the row
    /// back as `(symbol, style)` pairs. `blit_screen` is the only writer, so
    /// anything not written shows the buffer's reset default.
    fn blit_row(bytes: &[u8], cols: u16, inner_cols: u16) -> Vec<String> {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let mut parser = vt100::Parser::new(2, cols, 0);
        parser.process(bytes);
        let mut term = Terminal::new(TestBackend::new(cols, 2)).unwrap();
        term.draw(|f| {
            blit_screen(f, parser.screen(), Rect::new(0, 0, inner_cols, 2));
        })
        .unwrap();
        let buf = term.backend().buffer().clone();
        (0..cols)
            .map(|x| buf[(x, 0)].symbol().to_string())
            .collect()
    }

    /// U24: a wide glyph occupies its own cell exactly once, and the cell to
    /// its right is left blank for the terminal to paint the glyph's second
    /// half into. Before, the continuation was stamped with `" "`, drawing a
    /// space over the right half of every CJK/emoji glyph.
    #[test]
    fn wide_glyphs_blit_once_with_an_untouched_continuation_cell() {
        // `日本語x`: cols 0/2/4 carry a glyph, 1/3/5 are continuations, and
        // the narrow `x` lands at col 6 — not col 3, which is where a grid
        // measuring widths with the pre-P17 table would have put it.
        assert_eq!(
            blit_row("日本語x".as_bytes(), 10, 10),
            vec!["日", " ", "本", " ", "語", " ", "x", " ", " ", " "],
        );
        // Emoji, including a VS16 presentation sequence (P17): one cell each.
        assert_eq!(
            blit_row("\u{1f600}\u{2764}\u{fe0f}y".as_bytes(), 8, 8),
            vec!["\u{1f600}", " ", "\u{2764}\u{fe0f}", " ", "y", " ", " ", " "],
        );
    }

    /// P16: `cell_style` mapped only bold/italic/underline/inverse, so dimmed
    /// and struck text was attribute-identical to plain text — verified end to
    /// end before the fix. Claude Code leans on dim for secondary text, so a
    /// pane flattened into one equal-weight wall.
    #[test]
    fn dim_and_strikethrough_reach_ratatui_modifiers() {
        use super::cell_style;

        let mut p = vt100::Parser::new(2, 20, 0);
        p.process(b"\x1b[2mF\x1b[22;9mS\x1b[0mP\x1b[1;2;9mA");
        let s = p.screen();
        let faint = cell_style(s.cell(0, 0).unwrap());
        assert!(faint.add_modifier.contains(Modifier::DIM));
        assert!(!faint.add_modifier.contains(Modifier::CROSSED_OUT));

        let struck = cell_style(s.cell(0, 1).unwrap());
        assert!(struck.add_modifier.contains(Modifier::CROSSED_OUT));
        assert!(!struck.add_modifier.contains(Modifier::DIM));

        // The regression this item started from: plain text must stay plain.
        let plain = cell_style(s.cell(0, 2).unwrap());
        assert!(!plain.add_modifier.contains(Modifier::DIM));
        assert!(!plain.add_modifier.contains(Modifier::CROSSED_OUT));
        assert_ne!(plain, faint, "dim must be distinguishable from unstyled");
        assert_ne!(plain, struck, "strikethrough must be distinguishable");

        // Stacking with the attributes that already worked.
        let all = cell_style(s.cell(0, 3).unwrap());
        for m in [Modifier::BOLD, Modifier::DIM, Modifier::CROSSED_OUT] {
            assert!(all.add_modifier.contains(m), "missing {m:?}");
        }
    }

    /// U24 guard: a wide glyph whose second half would land outside the drawn
    /// area degrades to a space instead of bleeding a two-column symbol into
    /// the pane border ratatui draws in the next column.
    #[test]
    fn a_wide_glyph_clipped_by_the_pane_edge_degrades_to_a_space() {
        // Grid is 10 wide but only 5 columns are drawn: 語 starts at col 4,
        // so its right half is outside and it must not be emitted.
        assert_eq!(
            blit_row("日本語x".as_bytes(), 10, 5),
            vec!["日", " ", "本", " ", " ", " ", " ", " ", " ", " "],
        );
    }
}
