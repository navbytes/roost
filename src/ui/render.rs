//! Rendering: tab bar + pane borders + vt100 grid blit (design doc §8).

use std::collections::{HashSet, VecDeque};

use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Paragraph};
use ratatui::Frame;
use unicode_width::UnicodeWidthChar;

use crate::core::app::{
    feed_overlay_size, picker_items, App, FeedEntry, Mode, RenameTarget,
    Selection, TabSummary,
};
use crate::core::status::AgentStatus;
use crate::core::layout::{self, PaneRect};
use crate::ports::PaneBackend;
use crate::ui::mouse;
use crate::ui::theme;

pub fn draw<B: PaneBackend>(f: &mut Frame, app: &mut App<B>) {
    let area = f.area();
    if area.height < 2 {
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
    let pulse = theme::pulse_phase(app.elapsed());

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
    draw_mode_overlay(f, app, body, anchor);
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
fn hint_pairs(mode: &Mode, focused_dead: bool, focused_raw: bool) -> Vec<(&'static str, &'static str)> {
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
        Mode::Help => vec![("Alt+?", "all keys"), ("any key", "close")],
        Mode::Rename { target, .. } => {
            let what = match target {
                RenameTarget::Pane => "pane name",
                RenameTarget::Tab => "tab name",
            };
            vec![("type", what), ("↵", "save"), ("Esc", "cancel")]
        }
        // [Amended, U20] `1..9` joins the list: the accelerator is the
        // fastest way through the picker and the only one that wasn't
        // advertised anywhere.
        Mode::Picker { .. } => {
            vec![("↑↓", "choose"), ("↵", "open"), ("1..9", "launch"), ("Esc", "cancel")]
        }
        Mode::Scroll { .. } => {
            vec![("↑↓", "scroll"), ("PgUp/Dn", "page"), ("Esc", "exit")]
        }
        Mode::Feed { .. } => vec![("↑↓", "scroll"), ("Esc", "close")],
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
        Mode::Scroll { .. } => "SCROLL",
        Mode::Copy { .. } => "COPY",
        Mode::Help => "HELP",
        Mode::Feed { .. } => "FEED",
    }
}

/// C9's right-aligned segment: the aggregate "◆ N needs you · Alt+a" —
/// omitted at `n == 0` rather than shown as a hollow "0 needs you" — then
/// (Scroll mode only, U3) the dim `↑N/M` position, then the uppercase mode
/// word, then one trailing space. The position rides inside the segment so
/// C9's fit/yield machinery covers it for free: pairs drop whole before any
/// of it clips. Pure so the omission rules are unit-testable without a
/// `Frame`.
fn hint_bar_right_spans(n: usize, position: Option<String>, word: &str) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    if n > 0 {
        spans.push(Span::styled(
            format!("◆ {n} needs you · Alt+a"),
            Style::default().fg(theme::ACCENT),
        ));
        spans.push(Span::raw("  "));
    }
    if let Some(pos) = position {
        spans.push(Span::styled(format!("{pos} "), Style::default().fg(theme::DIM)));
    }
    spans.push(Span::styled(word.to_string(), Style::default().fg(theme::DIM)));
    spans.push(Span::raw(" "));
    spans
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
        // TERM_PROGRAM and picks the real menu path where there is one.
        f.render_widget(
            Paragraph::new(app.alt_hint_line())
                .style(Style::default().fg(theme::FG).bg(theme::ACCENT_DIM)),
            area,
        );
        return;
    }

    // A transient action result (e.g. "copied") takes over the bar briefly.
    if let Some(msg) = app.flash() {
        f.render_widget(
            Paragraph::new(format!(" {msg} ")).style(Style::default().fg(theme::FG).bg(theme::RULE)),
            area,
        );
        return;
    }

    // (key, what it does) pairs for the current context: key ACCENT, label
    // MUTED, no chip bg. The right segment (aggregate + mode word) WINS over
    // the pairs (C9 yield order): the mode word is a modal-safety affordance
    // and "◆ N needs you" the fleet's primary signal — trailing pairs drop
    // whole until the segment fits.
    let focused_raw = app.is_raw(app.focused);
    let hints = hint_pairs(&app.mode, app.focused_dead(), focused_raw);
    // U3: Scroll mode's right segment shows where in history the view sits
    // — `↑N/M` from the backend's grid-clamped (offset, banked) pair, so it
    // can never report a phantom row the grid refused (U9's overshoot).
    let position = match &app.mode {
        Mode::Scroll { .. } => {
            let (off, total) = app.scroll_position();
            Some(format!("{}{off}/{total}", theme::SCROLLED))
        }
        _ => None,
    };
    let right = hint_bar_right_spans(
        app.needs_input_count(),
        position,
        mode_word(&app.mode, app.zoomed(), focused_raw),
    );
    let right_w: u16 = right.iter().map(|s| s.content.chars().count() as u16).sum();

    let shown = fit_hint_pairs(&hints, right_w, area.width);
    let mut spans: Vec<Span> = Vec::with_capacity(shown * 2 + 4);
    let mut used = 0u16;
    for (key, label) in &hints[..shown] {
        used += hint_pair_cols(key, label); // same source as fit_hint_pairs
        spans.push(Span::styled(format!(" {key} "), Style::default().fg(theme::ACCENT)));
        spans.push(Span::styled(format!("{label}  "), Style::default().fg(theme::MUTED)));
    }
    if used.saturating_add(right_w) <= area.width {
        let pad = area.width.saturating_sub(used).saturating_sub(right_w);
        spans.push(Span::raw(" ".repeat(pad as usize)));
        spans.extend(right);
    }

    // Paragraph truncates (no wrap) so a narrow terminal just clips the
    // tail; the bg fills the row edge-to-edge regardless of span coverage.
    f.render_widget(Paragraph::new(Line::from(spans)).style(Style::default().bg(theme::BAR)), area);
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
    Style::default().fg(theme::ACCENT)
}

/// C12's title style: regular weight, primary ink — shared by all three
/// modals so there's exactly one place that sets it.
fn dialog_title(text: &'static str) -> Line<'static> {
    Line::from(text).style(Style::default().fg(theme::FG))
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

/// The help overlay's key-column prefix: the key label left-padded to a
/// fixed column so every description lines up underneath it. Shared by the
/// width computation and the row-rendering loop below so they can't drift.
fn help_key_prefix(key: &str) -> String {
    format!(" {key:<18}")
}

/// C15 (amended): the help overlay's width — the widest content line (key
/// column + its description), plus the 2 border columns. `centered_near`
/// still clamps this to the screen. The old fixed 52-col width predated this
/// key list and clipped long descriptions mid-word ("…(pi / claud"). Pure so
/// the sizing has a unit-test seam.
fn help_dialog_width(keys: &[(&str, &str)]) -> u16 {
    let content = keys
        .iter()
        .map(|(k, d)| mouse::display_width(&help_key_prefix(k)) + mouse::display_width(d))
        .max()
        .unwrap_or(0);
    content + 2
}

/// C15/§8: the help overlay's row list — the canonical key table, verbatim
/// and in this order (merged rows stay merged), then U23's three reference
/// rows: the status-glyph legend and the two mouse rows. Hard cap: ≤ 20 rows
/// (§8; pinned by `help_keys_fit_the_cap`). The single source
/// `draw_mode_overlay` reads for `Mode::Help`.
///
/// U23 (2026-07-27): the overlay taught chords and nothing else — neither
/// `●◆○·✕`, the product's core language, nor a word about the mouse, so the
/// one surface that exists to explain roost explained only half of it. Paid
/// for by merging three natural chord pairs (split/flip, zoom/float,
/// close/undo) rather than by scrolling the overlay, so C15's "any key
/// closes it" and the ≤20 cap both survive untouched.
const HELP_KEYS: &[(&str, &str)] = &[
    ("Alt+n", "new shell pane (auto split)"),
    ("Alt+Enter", "quick-launch picker (pi / claude / shell; 1..9)"),
    ("Alt+←↓↑→ / hjkl", "move focus"),
    ("Alt+Shift+←↓↑→", "resize along that axis"),
    ("Alt+s / Alt+o", "toggle split ⇄ stack / flip orientation"),
    ("Alt+g", "cycle layout: grid / main+stack / all-stack"),
    ("Alt+z / Alt+f", "zoom pane (view only) / floating scratch shell"),
    ("Alt+a", "jump to next pane that needs you"),
    ("Alt+e", "activity feed (status / spawns / exits / control)"),
    ("Alt+r / Alt+Shift+r", "rename pane / tab"),
    ("Alt+t / Alt+1..9 / Alt+0", "new tab / go to tab / last tab"),
    ("Alt+i / Alt+m", "previous / next tab (wraps)"),
    ("Alt+w / Alt+u", "close pane (confirm if busy) / undo close"),
    ("Alt+c / Alt+PgUp", "copy mode (hjkl w b e 0 $ v V y o) / scroll mode"),
    ("Alt+Shift+p", "raw pass-through for this pane (same chord exits)"),
    ("Alt+/ / Alt+?", "toggle hint bar / this keymap"),
    ("Alt+q", "quit (workspace saved; sessions live)"),
    ("status", "● working ◆ needs you ○ waiting · idle ✕ exited"),
    ("mouse", "wheel scrolls · click focuses · drag selects"),
    ("Alt+click / o", "open the URL under the pointer / copy cursor"),
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
    dialog_rect(&app.mode, body, anchor)
}

/// The pure half of `modal_rect`: mode + geometry in, dialog rect out.
fn dialog_rect(mode: &Mode, body: Rect, anchor: Rect) -> Option<Rect> {
    match mode {
        // Copy mode has no centered overlay — the cursor/selection are
        // drawn in-pane (C17/C24).
        Mode::Normal | Mode::Scroll { .. } | Mode::Copy { .. } => None,
        Mode::Rename { .. } => Some(centered_near(anchor, body, 44, 3)),
        Mode::Picker { .. } => {
            Some(centered_near(anchor, body, 32, picker_items().len() as u16 + 2))
        }
        Mode::Help => {
            let h = HELP_KEYS.len() as u16 + 2;
            Some(centered_near(anchor, body, help_dialog_width(HELP_KEYS), h.min(body.height)))
        }
        Mode::Feed { .. } => {
            let (w, h) = feed_overlay_size(body);
            Some(centered_near(anchor, body, w, h))
        }
    }
}

fn draw_mode_overlay<B: PaneBackend>(f: &mut Frame, app: &App<B>, body: Rect, anchor: Rect) {
    let Some(rect) = dialog_rect(&app.mode, body, anchor) else { return };
    match &app.mode {
        Mode::Normal | Mode::Scroll { .. } | Mode::Copy { .. } => {}
        Mode::Rename { buffer, target } => {
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
                Paragraph::new(format!("{buffer}{}", theme::RENAME_CURSOR))
                    .style(Style::default().fg(theme::FG)),
                inner,
            );
        }
        Mode::Picker { selection } => {
            let items = picker_items();
            dim_backdrop(f, body, rect);
            f.render_widget(Clear, rect);
            let block = Block::bordered()
                .title(dialog_title(" new pane — pick agent "))
                .border_type(BorderType::Plain)
                .border_style(dialog_border_style());
            let inner = block.inner(rect);
            f.render_widget(block, rect);
            // C14: selected row is a `❯`-prefix + FG item text, no bg
            // highlight; unselected rows are plain MUTED text. [Amended,
            // U20] Each row leads with its `1..9` accelerator — an
            // accelerator nothing shows is one nobody presses.
            let lines: Vec<Line> = items
                .iter()
                .enumerate()
                .map(|(i, item)| {
                    let body = picker_row_body(i, item);
                    if i == *selection {
                        Line::from(vec![
                            Span::styled(theme::PICKER_SELECTED.to_string(), Style::default().fg(theme::ACCENT)),
                            Span::styled(body, Style::default().fg(theme::FG)),
                        ])
                    } else {
                        Line::from(Span::styled(format!(" {body}"), Style::default().fg(theme::MUTED)))
                    }
                })
                .collect();
            f.render_widget(Paragraph::new(lines), inner);
        }
        Mode::Help => {
            // Full keymap — the §8 key table, verbatim and in order (C15
            // amended). `HELP_KEYS` is the single source; `help_keys_fit_the_cap`
            // pins the ≤ 20-row hard cap this relies on (80×24 body = 22
            // rows = 20 content + 2 border, zero slack).
            let keys = HELP_KEYS;
            dim_backdrop(f, body, rect);
            f.render_widget(Clear, rect);
            let block = Block::bordered()
                .title(dialog_title(" keys — any key to close "))
                .border_type(BorderType::Plain)
                .border_style(dialog_border_style());
            let inner = block.inner(rect);
            f.render_widget(block, rect);
            let lines: Vec<Line> = keys
                .iter()
                .map(|(k, d)| {
                    Line::from(vec![
                        Span::styled(help_key_prefix(k), Style::default().fg(theme::ACCENT)),
                        Span::styled(d.to_string(), Style::default().fg(theme::MUTED)),
                    ])
                })
                .collect();
            f.render_widget(Paragraph::new(lines), inner);
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
            Paragraph::new(format!("{}{text}", " ".repeat(pad as usize)))
                .style(Style::default().fg(theme::DIM)),
            Rect::new(inner.x, y, inner.width, 1),
        );
        return;
    }
    let range = feed_window(feed.len(), offset, inner.height as usize);
    let lines: Vec<Line> = feed
        .iter()
        .skip(range.start)
        .take(range.len())
        .map(|e| Line::from(feed_entry_spans(&local_hh_mm_ss(e.at), &e.text, e.needs_input)))
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

/// C20's per-row rule: `" HH:MM:SS  {text}"`, timestamp DIM, text MUTED —
/// except a status line landing on NeedsInput, which gets the `◆ ` ACCENT
/// prefix and FG text (the one red in the feed, same meaning as everywhere,
/// C5). Pure so the exception is unit-tested without a `Frame`.
fn feed_entry_spans(hhmmss: &str, text: &str, needs_input: bool) -> Vec<Span<'static>> {
    let mut spans = vec![Span::styled(format!(" {hhmmss}  "), Style::default().fg(theme::DIM))];
    if needs_input {
        spans.push(Span::styled(
            format!("{} ", theme::GLYPH_NEEDS_INPUT),
            Style::default().fg(theme::ACCENT),
        ));
        spans.push(Span::styled(text.to_string(), Style::default().fg(theme::FG)));
    } else {
        spans.push(Span::styled(text.to_string(), Style::default().fg(theme::MUTED)));
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
/// the row edge-to-edge on `TAB_STRIP`, plus a right-aligned
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

fn draw_tab_bar<B: PaneBackend>(f: &mut Frame, app: &App<B>, area: Rect, pulse: Color) {
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
        spans.push(Span::styled(theme::TAB_OVERFLOW.to_string(), Style::default().fg(theme::MUTED)));
        used += strip.x0;
    }
    // Left to right, one 8-part span group per tab (marker/label/glyph/
    // separator/gutter), stopping exactly where `tab_strip` says to.
    for (i, tab) in app.ws.tabs.iter().enumerate().take(strip.end).skip(strip.start) {
        let active = i == app.ws.active_tab;
        let summary = app.tab_summary(i);
        let (glyph, base_color) = tab_summary_badge(summary);
        let glyph_color = if summary == TabSummary::Working { pulse } else { base_color };
        push_tab_spans(&mut spans, i, &tab.name, active, glyph, glyph_color);
        used += mouse::tab_width(i, &tab.name);
    }

    // ...and a trailing `…` when tabs remain past the right edge and a
    // spare column is left to show it in (overflow, C2).
    if strip.right_marker {
        spans.push(Span::styled(theme::TAB_OVERFLOW.to_string(), Style::default().fg(theme::MUTED)));
        used += 1;
    }

    if let Some(fit) = fit.filter(|_| show_status) {
        let (mode_word, prefix, save_word) = mouse::status_parts(fit.mode, fit.cwd, saved);
        let pad = area.width.saturating_sub(used).saturating_sub(status_w);
        if pad > 0 {
            spans.push(Span::raw(" ".repeat(pad as usize)));
        }
        // U15: the mode word reads a step brighter than the cwd beside it —
        // it's state, not context — while staying inside the ink ramp (no
        // new token, and never the accent: it isn't an alarm).
        if !mode_word.is_empty() {
            spans.push(Span::styled(mode_word, Style::default().fg(theme::MUTED)));
        }
        if !prefix.is_empty() {
            spans.push(Span::styled(prefix, Style::default().fg(theme::DIM)));
        }
        let save_color = if saved { theme::DIM } else { theme::ACCENT };
        spans.push(Span::styled(format!("{save_word} "), Style::default().fg(save_color)));
    }

    // Base fill first (edge-to-edge TAB_STRIP, including any empty middle):
    // Paragraph's own `.style()` fills the whole `area`, so cells no span
    // touches (the gap before the status area, or the entire row past the
    // last tab when there's no overflow) still get the strip background.
    f.render_widget(
        Paragraph::new(Line::from(spans)).style(Style::default().bg(theme::TAB_STRIP)),
        area,
    );
}

/// One tab's 8-part span sequence (C2): marker, label, status glyph, the
/// separator, and a trailing gutter space — column count matches
/// `mouse::tab_width` (`display_width(label) + 7`) exactly.
fn push_tab_spans(
    spans: &mut Vec<Span<'static>>,
    index: usize,
    name: &str,
    active: bool,
    glyph: char,
    glyph_color: Color,
) {
    if active {
        spans.push(Span::styled(theme::MARKER_ACTIVE.to_string(), Style::default().fg(theme::ACCENT)));
    } else {
        spans.push(Span::raw(" "));
    }
    spans.push(Span::raw(" "));

    let label_style = if active {
        Style::default().fg(theme::FG).bg(theme::ACTIVE_TAB_BG)
    } else {
        Style::default().fg(theme::MUTED)
    };
    spans.push(Span::styled(mouse::tab_label(index, name), label_style));
    spans.push(Span::raw(" "));

    spans.push(Span::styled(glyph.to_string(), Style::default().fg(glyph_color)));
    spans.push(Span::raw(" "));

    spans.push(Span::styled(theme::TAB_SEPARATOR.to_string(), Style::default().fg(theme::RULE)));
    // C2 (amended 2026-07-23): trailing gutter — one space after the separator
    // gives every divider symmetric 1-cell padding, so adjacent tabs read
    // `│ ▎` not `│▎`. Counts as this tab's own column (mouse::tab_width +7).
    spans.push(Span::raw(" "));
}

/// Map a tab's aggregate summary to a tab-bar glyph + colour (theme::C5).
/// `Quiet` renders as a blank (no clutter for tabs with nothing to report);
/// `Unknown` is a faint dot so a not-yet-spawned background tab reads as
/// "unknown", not idle.
fn tab_summary_badge(s: crate::core::app::TabSummary) -> (char, Color) {
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

/// C8's collapsed-row name color, by status.
fn collapsed_name_color(status: AgentStatus) -> Color {
    match status {
        AgentStatus::Working | AgentStatus::NeedsInput => theme::FG,
        AgentStatus::Waiting | AgentStatus::Idle => theme::MUTED,
        AgentStatus::Exited => theme::DIM,
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
    pulse: Color,
) {
    let focused = app.focused == pr.id;
    let raw = app.is_raw(pr.id);
    let (status, name, has_title, adapter) = {
        // find_spec, not the active tab's map directly: C22 learns the
        // float here too (its spec lives on the `Float`, not `Tab::panes`).
        let spec = app.find_spec(pr.id);
        let status = app
            .runtimes
            .get(&pr.id)
            .map(|rt| rt.status())
            .unwrap_or(AgentStatus::Exited);
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
    let border_style = if focused {
        Style::default().fg(theme::ACCENT)
    } else {
        Style::default().fg(theme::RULE)
    };
    let block = Block::bordered().border_style(border_style);
    let inner = block.inner(pr.rect);
    f.render_widget(block, pr.rect);

    // C7: an unfocused expanded stack member gets its left border column
    // overpainted with the accent-dim edge marker. Suppressed when focused —
    // the full accent border is already the stronger signal, and stacking
    // ACCENT_DIM inside an ACCENT frame would smear the one-red discipline.
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
    // so it stays visible (a cell TUI can't do true translucency; MUTED text
    // reads as a watermark rather than content). Drawn on every pane,
    // focused included: occlusion of the inner app's own top-right cells is
    // accepted by design now that identity lives here, not a border title.
    // U3: a scrolled pane's badge gains the dim ↑N token; N1: its Working
    // glyph stops pulsing while the view is frozen (badge_glyph_color).
    let (glyph, glyph_base, pulses) = theme::status_style(status);
    let scrolled = app.scroll_offset(pr.id);
    let glyph_color = badge_glyph_color(pulses, scrolled, pulse, glyph_base);
    let text = badge_text(pr.id, &name, &adapter, has_title);
    if let Some((rect, spans)) = corner_badge(inner, &text, raw, scrolled, glyph, glyph_color) {
        f.render_widget(Paragraph::new(Line::from(spans)), rect);
    }

    // C16: dead pane — overlay the relaunch hint (and spawn error, if any)
    // on the bottom rows. The last screen contents stay visible above.
    if status == AgentStatus::Exited && inner.height > 0 {
        let mut lines: Vec<Line> = Vec::new();
        if let Some(err) = app.dead.get(&pr.id) {
            lines.push(Line::from(Span::styled(
                format!(" spawn failed: {err} "),
                Style::default().fg(theme::ACCENT),
            )));
        }
        lines.push(Line::from(Span::styled(
            format!(
                " {} exited — Enter: relaunch/resume · f: fresh (drops resume) · Alt+w: close ",
                theme::GLYPH_EXITED
            ),
            Style::default().fg(theme::FG).bg(theme::ACCENT_DIM),
        )));
        let n = lines.len() as u16;
        let y = inner.y + inner.height.saturating_sub(n);
        let overlay = Rect::new(inner.x, y, inner.width, n.min(inner.height));
        f.render_widget(Paragraph::new(lines), overlay);
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
            cell.set_style(Style::default().fg(theme::ACCENT_DIM));
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
#[allow(clippy::too_many_arguments)]
fn collapsed_row_spans(
    width: u16,
    focused: bool,
    status: AgentStatus,
    id: layout::PaneId,
    name: &str,
    adapter: &str,
    has_title: bool,
    raw: bool,
    glyph_color: Color,
) -> Vec<Span<'static>> {
    let (glyph, ..) = theme::status_style(status);
    let marker = if focused {
        (theme::MARKER_ACTIVE.to_string(), Style::default().fg(theme::ACCENT))
    } else {
        (" ".to_string(), Style::default())
    };
    let left: Vec<(String, Style)> = vec![
        marker,
        (glyph.to_string(), Style::default().fg(glyph_color)),
        (format!(" {id} {name}"), Style::default().fg(collapsed_name_color(status))),
    ];
    let left_w: u16 = left.iter().map(|(t, _)| mouse::display_width(t)).sum();
    let right = if has_title {
        format!("{adapter} · {} ", state_word(status))
    } else {
        format!("{} ", state_word(status))
    };
    let right = if raw { format!("raw · {right}") } else { right };
    let right_w = mouse::display_width(&right);

    if width >= left_w + right_w {
        let pad = width - left_w - right_w;
        let mut spans: Vec<Span> = left.into_iter().map(|(t, s)| Span::styled(t, s)).collect();
        spans.push(Span::raw(" ".repeat(pad as usize)));
        spans.push(Span::styled(right, Style::default().fg(theme::DIM)));
        spans
    } else {
        clip_spans(&left, width)
    }
}

/// C8: render one collapsed stack member's row. Focused rows additionally
/// paint `RULE` across the full row width; unfocused rows have no bg
/// (background policy, §2).
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
    pulse: Color,
) {
    let (_, base, pulses) = theme::status_style(status);
    let glyph_color = if pulses { pulse } else { base };
    let spans =
        collapsed_row_spans(rect.width, focused, status, id, name, adapter, has_title, raw, glyph_color);
    let style = if focused { Style::default().bg(theme::RULE) } else { Style::default() };
    f.render_widget(Paragraph::new(Line::from(spans)).style(style), rect);
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
            .style(Style::default().fg(theme::DIM).add_modifier(Modifier::UNDERLINED)),
        header.rect,
    );
}

/// Top-right corner badge (C4): pane name (+ adapter, when titled) and the
/// status glyph, right-aligned with one column of breathing room. Two-tone:
/// the text is MUTED, the glyph carries its own C5 status color. [Amended,
/// C23] a raw pane's badge gains a `raw` token between the text and the
/// glyph, in its own `ACCENT_DIM` span (never folded into the MUTED text —
/// it needs its own color). [Amended, U3] a scrolled pane's badge carries a
/// dim `↑N` token — its grid-clamped view offset, 0 = live tail = no token
/// — glyph-adjacent (after `raw`), same `ACCENT_DIM` family. Returns the
/// 1-row rect and the clipped spans — or `None` if the pane is too small to
/// be worth badging. Pure so it can be unit-tested.
fn corner_badge(
    inner: Rect,
    text: &str,
    raw: bool,
    scrolled: usize,
    glyph: char,
    glyph_color: Color,
) -> Option<(Rect, Vec<Span<'static>>)> {
    if text.trim().is_empty() || inner.width < 3 || inner.height == 0 {
        return None;
    }
    let max = inner.width.saturating_sub(1);
    // One space of breathing room on the right edge (the trailing space in
    // the glyph part).
    let mut parts: Vec<(String, Style)> = Vec::with_capacity(4);
    if raw || scrolled > 0 {
        parts.push((format!(" {text} · "), Style::default().fg(theme::MUTED)));
    } else {
        parts.push((format!(" {text} "), Style::default().fg(theme::MUTED)));
    }
    if raw {
        let token = if scrolled > 0 { "raw · " } else { "raw " };
        parts.push((token.to_string(), Style::default().fg(theme::ACCENT_DIM)));
    }
    if scrolled > 0 {
        parts.push((
            format!("{}{scrolled} ", theme::SCROLLED),
            Style::default().fg(theme::ACCENT_DIM),
        ));
    }
    parts.push((format!("{glyph} "), Style::default().fg(glyph_color)));
    let total: u16 = parts.iter().map(|(t, _)| mouse::display_width(t)).sum();
    let w = total.min(max);
    let spans = clip_spans(&parts, w);
    let x = inner.x + inner.width - w;
    Some((Rect::new(x, inner.y, w, 1), spans))
}

/// N1: the badge glyph's color — the C5 pulse only while the pane's view is
/// at the live tail. A pulsing `●` asserts "alive right now", which a frozen
/// (scrolled) view must not do; the glyph keeps its steady base color (the
/// status itself stays truthful — the agent IS working) while the C4 `↑N`
/// token carries the frozen-view signal. Any path that resets the offset
/// resumes the pulse. Collapsed rows and the tab bar keep pulsing: they
/// show no grid, so there is no frozen view to lie about.
fn badge_glyph_color(pulses: bool, scrolled: usize, pulse: Color, base: Color) -> Color {
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

fn conv_color(c: vt100::Color) -> Option<Color> {
    match c {
        vt100::Color::Default => None,
        vt100::Color::Idx(i) => Some(Color::Indexed(i)),
        vt100::Color::Rgb(r, g, b) => Some(Color::Rgb(r, g, b)),
    }
}

fn cell_style(cell: &vt100::Cell) -> Style {
    let mut style = Style::default();
    if let Some(fg) = conv_color(cell.fgcolor()) {
        style = style.fg(fg);
    }
    if let Some(bg) = conv_color(cell.bgcolor()) {
        style = style.bg(bg);
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
        badge_text, blit_screen, cell_in_selection, centered_near, collapsed_name_color,
        collapsed_row_spans, corner_badge, dialog_border_style, feed_entry_spans, feed_window,
        help_dialog_width, hint_bar_right_spans, hint_pairs, mode_word, push_tab_spans,
        should_place_cursor, stack_header_text, state_word, HELP_KEYS,
    };
    use crate::core::app::{Mode, RenameTarget};
    use crate::core::status::AgentStatus;
    use crate::ui::mouse;
    use crate::ui::theme;
    use ratatui::layout::Rect;
    use ratatui::style::{Modifier, Style};

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
        let (rect, spans) = corner_badge(inner, "claude", false, 0, theme::GLYPH_WORKING, theme::ACCENT).unwrap();
        assert_eq!(rect.y, inner.y); // top row of the content
        assert_eq!(rect.height, 1);
        // right edge: badge ends one col shy of the inner right edge is fine;
        // here it butts to the edge because the text fits.
        assert_eq!(rect.x + rect.width, inner.x + inner.width);
        assert_eq!(spans.len(), 2);
        assert_eq!(spans[0].content.as_ref(), " claude ");
        assert_eq!(spans[0].style.fg, Some(theme::MUTED));
        assert_eq!(spans[1].content.as_ref(), format!("{} ", theme::GLYPH_WORKING));
        assert_eq!(spans[1].style.fg, Some(theme::ACCENT));
    }

    #[test]
    fn badge_clips_and_drops_the_glyph_first_when_pane_too_small() {
        let inner = Rect::new(0, 0, 6, 5);
        let (rect, spans) =
            corner_badge(inner, "a-very-long-name", false, 0, theme::GLYPH_WORKING, theme::ACCENT).unwrap();
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
        let (rect, spans) = corner_badge(inner, "日本語", false, 0, theme::GLYPH_IDLE, theme::DIM).unwrap();
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
        assert!(corner_badge(Rect::new(0, 0, 2, 5), "x", false, 0, theme::GLYPH_WORKING, theme::ACCENT).is_none());
        assert!(corner_badge(Rect::new(0, 0, 40, 0), "x", false, 0, theme::GLYPH_WORKING, theme::ACCENT).is_none());
        assert!(corner_badge(Rect::new(0, 0, 40, 5), "   ", false, 0, theme::GLYPH_WORKING, theme::ACCENT).is_none());
    }

    // -- C23 raw indication ---------------------------------------------------

    #[test]
    fn badge_gains_a_raw_token_in_its_own_accent_dim_color() {
        let inner = Rect::new(0, 0, 40, 20);
        let (_, spans) =
            corner_badge(inner, "scratch · shell", true, 0, theme::GLYPH_IDLE, theme::DIM).unwrap();
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, format!(" scratch · shell · raw {} ", theme::GLYPH_IDLE));
        let raw_span = spans.iter().find(|s| s.content.as_ref() == "raw ").expect("raw token span");
        assert_eq!(raw_span.style.fg, Some(theme::ACCENT_DIM));
        // The raw token must be its own span, not folded into the muted text.
        assert!(spans.iter().any(|s| s.style.fg == Some(theme::MUTED)));
    }

    #[test]
    fn badge_without_raw_has_no_raw_token() {
        let inner = Rect::new(0, 0, 40, 20);
        let (_, spans) = corner_badge(inner, "pi", false, 0, theme::GLYPH_IDLE, theme::DIM).unwrap();
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(!text.contains("raw"));
    }

    // -- U3 scrollback indication ---------------------------------------------

    #[test]
    fn badge_gains_a_scrolled_token_in_accent_dim() {
        // U3: a frozen view must say so — `↑N` (grid-clamped offset), same
        // ACCENT_DIM family as the raw token, glyph-adjacent.
        let inner = Rect::new(0, 0, 40, 20);
        let (_, spans) =
            corner_badge(inner, "3 pi", false, 42, theme::GLYPH_WORKING, theme::ACCENT).unwrap();
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, format!(" 3 pi · ↑42 {} ", theme::GLYPH_WORKING));
        let token = spans.iter().find(|s| s.content.as_ref() == "↑42 ").expect("↑N token span");
        assert_eq!(token.style.fg, Some(theme::ACCENT_DIM));
    }

    #[test]
    fn badge_scrolled_and_raw_tokens_compose_in_order() {
        // raw · ↑N — input state first, then view state, then the glyph.
        let inner = Rect::new(0, 0, 40, 20);
        let (_, spans) =
            corner_badge(inner, "3 pi", true, 7, theme::GLYPH_IDLE, theme::DIM).unwrap();
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, format!(" 3 pi · raw · ↑7 {} ", theme::GLYPH_IDLE));
    }

    #[test]
    fn badge_at_live_tail_has_no_scrolled_token() {
        let inner = Rect::new(0, 0, 40, 20);
        let (_, spans) =
            corner_badge(inner, "3 pi", false, 0, theme::GLYPH_WORKING, theme::ACCENT).unwrap();
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(!text.contains('↑'), "{text}");
    }

    #[test]
    fn badge_glyph_pulse_yields_while_the_view_is_frozen() {
        // N1: pulsing red means "alive right now" — a scrolled pane's glyph
        // holds the steady base color instead; the tail resumes the pulse.
        let phase = theme::ACCENT_DIM; // pretend mid-pulse phase B
        assert_eq!(super::badge_glyph_color(true, 0, phase, theme::ACCENT), phase);
        assert_eq!(super::badge_glyph_color(true, 12, phase, theme::ACCENT), theme::ACCENT);
        // Non-pulsing statuses are steady either way.
        assert_eq!(super::badge_glyph_color(false, 0, phase, theme::FG), theme::FG);
        assert_eq!(super::badge_glyph_color(false, 12, phase, theme::FG), theme::FG);
    }

    #[test]
    fn hint_bar_right_carries_the_scroll_position_ahead_of_the_mode_word() {
        // U3: `↑N/M` rides inside the right segment (DIM), so C9's yield
        // machinery covers it — and it only exists when a position is given.
        let spans = hint_bar_right_spans(0, Some("↑12/300".into()), "SCROLL");
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, "↑12/300 SCROLL ");
        assert_eq!(spans[0].style.fg, Some(theme::DIM));

        let spans = hint_bar_right_spans(2, Some("↑12/300".into()), "SCROLL");
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, "◆ 2 needs you · Alt+a  ↑12/300 SCROLL ");
    }

    #[test]
    fn state_word_matches_c8_table() {
        assert_eq!(state_word(AgentStatus::Working), "working");
        assert_eq!(state_word(AgentStatus::NeedsInput), "needs you");
        assert_eq!(state_word(AgentStatus::Waiting), "your turn");
        assert_eq!(state_word(AgentStatus::Idle), "idle");
        assert_eq!(state_word(AgentStatus::Exited), "exited");
    }

    #[test]
    fn collapsed_name_color_by_state() {
        assert_eq!(collapsed_name_color(AgentStatus::Working), theme::FG);
        assert_eq!(collapsed_name_color(AgentStatus::NeedsInput), theme::FG);
        assert_eq!(collapsed_name_color(AgentStatus::Waiting), theme::MUTED);
        assert_eq!(collapsed_name_color(AgentStatus::Idle), theme::MUTED);
        assert_eq!(collapsed_name_color(AgentStatus::Exited), theme::DIM);
    }

    #[test]
    fn collapsed_row_shows_right_segment_when_it_fits() {
        let spans =
            collapsed_row_spans(40, false, AgentStatus::Working, 2, "pi", "pi", true, false, theme::ACCENT);
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
            collapsed_row_spans(40, false, AgentStatus::Waiting, 2, "shell", "shell", false, false, theme::FG);
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
            collapsed_row_spans(left_w, false, AgentStatus::Idle, 2, name, "shell", true, false, theme::DIM);
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, format!(" · 2 {name}"));
        assert!(!text.contains("shell"));
    }

    #[test]
    fn collapsed_row_clips_name_when_even_the_left_side_overflows() {
        let spans = collapsed_row_spans(
            4,
            false,
            AgentStatus::Waiting,
            2,
            "a-very-long-pane-name",
            "shell",
            true,
            false,
            theme::FG,
        );
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text.chars().count(), 4);
        assert!(!text.contains("shell"));
    }

    #[test]
    fn collapsed_row_focused_marker_is_accent() {
        let spans =
            collapsed_row_spans(40, true, AgentStatus::Working, 2, "pi", "pi", true, false, theme::ACCENT);
        assert_eq!(spans[0].content.as_ref(), theme::MARKER_ACTIVE.to_string());
        assert_eq!(spans[0].style.fg, Some(theme::ACCENT));
    }

    #[test]
    fn collapsed_row_raw_gains_the_prefix_ahead_of_the_usual_right_segment() {
        let titled = collapsed_row_spans(60, false, AgentStatus::Working, 2, "pi", "pi", true, true, theme::ACCENT);
        let text: String = titled.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.ends_with("raw · pi · working "), "{text}");

        let untitled =
            collapsed_row_spans(60, false, AgentStatus::Waiting, 2, "shell", "shell", false, true, theme::FG);
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
            hint_pairs(&Mode::Normal, false, false),
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
        let pairs = hint_pairs(&Mode::Normal, false, false);
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
        let pairs = hint_pairs(&Mode::Copy { cursor: (0, 0) }, false, false);
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
        let right_w = super::hint_bar_right_spans(0, None, "COPY")
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
        assert_eq!(hint_pairs(&Mode::Normal, false, true), vec![("Alt+Shift+p", "exit raw")]);
    }

    #[test]
    fn hint_pairs_dead_beats_raw_when_somehow_both() {
        // A dead pane never raw-routes (`App::raw_routing_active` requires
        // it alive) — but the flag can still be *set* on a dead pane, so the
        // hint bar must show what's actually actionable (dead-pane keys),
        // not a raw-exit hint nothing would honor.
        assert_eq!(
            hint_pairs(&Mode::Normal, true, true),
            vec![("↵", "relaunch"), ("f", "fresh — drops resume"), ("Alt+w", "close"), ("Alt+q", "quit")],
        );
    }

    #[test]
    fn hint_bar_right_omits_needs_segment_at_zero() {
        let spans = hint_bar_right_spans(0, None, "NORMAL");
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, "NORMAL ");
        assert!(!text.contains('◆'));
    }

    #[test]
    fn hint_bar_right_shows_aggregate_before_mode_word_when_nonzero() {
        let spans = hint_bar_right_spans(3, None, "NORMAL");
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, "◆ 3 needs you · Alt+a  NORMAL ");
        assert_eq!(spans[0].style.fg, Some(theme::ACCENT));
    }

    #[test]
    fn dialog_border_style_is_accent_with_no_modifiers() {
        // Pins C12: the old bright-fg/double-border/bold dialog look is
        // gone — one plain accent style for all three modals.
        assert_eq!(dialog_border_style(), Style::default().fg(theme::ACCENT));
    }

    #[test]
    fn help_dialog_width_fits_the_widest_content_line() {
        // C15 amended: width must be sized to the widest key-column-plus-
        // description line, not a fixed 52 that clips long descriptions
        // mid-word. Both keys pad to the same column, so the longer
        // description ("bb"'s) must be the one that decides the width.
        let keys: &[(&str, &str)] = &[("a", "short"), ("bb", "a much longer description here")];
        let w = help_dialog_width(keys);
        let expected =
            super::help_key_prefix("bb").chars().count() as u16 + "a much longer description here".len() as u16 + 2;
        assert_eq!(w, expected);
    }

    #[test]
    fn help_dialog_width_clamps_to_the_screen_via_centered_near() {
        // help_dialog_width itself doesn't clamp — centered_near does, same
        // as every other modal (C15's anchoring is unchanged).
        let keys: &[(&str, &str)] = &[("k", "a description so long it would exceed a narrow body")];
        let body = Rect::new(0, 1, 30, 20);
        let anchor = Rect::new(0, 1, 30, 20);
        let w = help_dialog_width(keys);
        assert!(w > body.width); // the ideal width doesn't fit
        let rect = centered_near(anchor, body, w, 3);
        assert!(rect.width <= body.width); // but the placed dialog does
    }

    #[test]
    fn mode_word_matches_c9_table() {
        assert_eq!(mode_word(&Mode::Normal, false, false), "NORMAL");
        assert_eq!(
            mode_word(&Mode::Rename { buffer: String::new(), target: RenameTarget::Pane }, false, false),
            "RENAME"
        );
        assert_eq!(mode_word(&Mode::Picker { selection: 0 }, false, false), "PICKER");
        assert_eq!(mode_word(&Mode::Scroll { offset: 0 }, false, false), "SCROLL");
        assert_eq!(mode_word(&Mode::Copy { cursor: (0, 0) }, false, false), "COPY");
        assert_eq!(mode_word(&Mode::Help, false, false), "HELP");
    }

    #[test]
    fn mode_word_shows_zoom_pseudo_state_only_in_the_normal_slot() {
        // C21/amended C9: ZOOM shows only when the mode is Normal — every
        // other mode's own word wins regardless of the zoomed flag.
        assert_eq!(mode_word(&Mode::Normal, true, false), "ZOOM");
        assert_eq!(mode_word(&Mode::Normal, false, false), "NORMAL");
        assert_eq!(mode_word(&Mode::Scroll { offset: 0 }, true, false), "SCROLL");
        assert_eq!(mode_word(&Mode::Help, true, false), "HELP");
    }

    #[test]
    fn mode_word_raw_beats_zoom_beats_normal_but_never_a_real_mode_word() {
        // C23/amended C9: in the Normal slot, RAW beats ZOOM beats NORMAL —
        // but any real (non-Normal) mode word still wins over both.
        assert_eq!(mode_word(&Mode::Normal, false, true), "RAW");
        assert_eq!(mode_word(&Mode::Normal, true, true), "RAW", "raw beats zoom");
        assert_eq!(mode_word(&Mode::Scroll { offset: 0 }, true, true), "SCROLL", "a real mode word always wins");
    }

    #[test]
    fn fit_hint_pairs_right_segment_wins_over_trailing_pairs() {
        // C9 yield order (re-amended 2026-07-22): pairs drop whole from the
        // right before the aggregate/mode-word segment ever yields. Measure
        // via the SAME `hint_pair_cols` the draw path uses — a +3/+4 drift
        // here once let the whole segment drop at widths 111-116 (D2).
        let pairs = hint_pairs(&Mode::Normal, false, false);
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
        let dead = hint_pairs(&Mode::Normal, true, false);
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
        assert_ne!(dead, hint_pairs(&Mode::Normal, false, false));
    }

    #[test]
    fn hint_pairs_feed_mode_is_scroll_and_close() {
        assert_eq!(
            hint_pairs(&Mode::Feed { offset: 0 }, false, false),
            vec![("↑↓", "scroll"), ("Esc", "close")]
        );
    }

    #[test]
    fn mode_word_feed_wins_regardless_of_zoom() {
        assert_eq!(mode_word(&Mode::Feed { offset: 0 }, false, false), "FEED");
        assert_eq!(mode_word(&Mode::Feed { offset: 0 }, true, false), "FEED");
    }

    #[test]
    fn hint_pairs_rename_word_differs_pane_vs_tab() {
        let pane =
            hint_pairs(&Mode::Rename { buffer: String::new(), target: RenameTarget::Pane }, false, false);
        let tab =
            hint_pairs(&Mode::Rename { buffer: String::new(), target: RenameTarget::Tab }, false, false);
        assert_eq!(pane[0], ("type", "pane name"));
        assert_eq!(tab[0], ("type", "tab name"));
    }

    #[test]
    fn push_tab_spans_active_tab_uses_accent_marker_and_reset_bg() {
        // C2: active tab — marker ▎ ACCENT, label FG on Color::Reset (fuses
        // with the body bg), glyph in its own color, separator RULE.
        let mut spans = Vec::new();
        push_tab_spans(&mut spans, 0, "main", true, theme::GLYPH_WORKING, theme::ACCENT);
        assert_eq!(spans.len(), 8); // 7 parts + trailing gutter (C2, amended 2026-07-23)
        assert_eq!(spans[0].content.as_ref(), theme::MARKER_ACTIVE.to_string());
        assert_eq!(spans[0].style.fg, Some(theme::ACCENT));
        assert_eq!(spans[2].content.as_ref(), "1 main");
        assert_eq!(spans[2].style.fg, Some(theme::FG));
        assert_eq!(spans[2].style.bg, Some(theme::ACTIVE_TAB_BG));
        assert_eq!(spans[4].content.as_ref(), theme::GLYPH_WORKING.to_string());
        assert_eq!(spans[4].style.fg, Some(theme::ACCENT));
        assert_eq!(spans[6].content.as_ref(), theme::TAB_SEPARATOR.to_string());
        assert_eq!(spans[6].style.fg, Some(theme::RULE));
        assert_eq!(spans[7].content.as_ref(), " "); // trailing gutter
    }

    #[test]
    fn push_tab_spans_inactive_tab_uses_blank_marker_and_muted_label() {
        // C2: inactive tab — marker is a plain space (no ACCENT), label MUTED
        // with no bg override (TAB_STRIP comes from the paragraph-level fill,
        // not a per-span bg).
        let mut spans = Vec::new();
        push_tab_spans(&mut spans, 1, "api", false, theme::GLYPH_IDLE, theme::DIM);
        assert_eq!(spans.len(), 8); // 7 parts + trailing gutter (C2, amended 2026-07-23)
        assert_eq!(spans[0].content.as_ref(), " ");
        assert_eq!(spans[0].style.fg, None);
        assert_eq!(spans[2].content.as_ref(), "2 api");
        assert_eq!(spans[2].style.fg, Some(theme::MUTED));
        assert_eq!(spans[2].style.bg, None);
        assert_eq!(spans[4].content.as_ref(), theme::GLYPH_IDLE.to_string());
        assert_eq!(spans[4].style.fg, Some(theme::DIM));
    }

    #[test]
    fn collapsed_row_spans_at_zero_width_is_empty_not_panicking() {
        let spans =
            collapsed_row_spans(0, true, AgentStatus::Working, 2, "pi", "pi", true, false, theme::ACCENT);
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

    #[test]
    fn feed_entry_spans_default_styling_is_dim_timestamp_muted_text() {
        let spans = feed_entry_spans("12:34:56", "spawned shell (shell)", false);
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, " 12:34:56  spawned shell (shell)");
        assert_eq!(spans[0].style.fg, Some(theme::DIM));
        assert_eq!(spans[1].style.fg, Some(theme::MUTED));
    }

    #[test]
    fn feed_entry_spans_needs_input_line_gets_the_accent_diamond_and_fg_text() {
        let spans = feed_entry_spans("12:34:56", "pi: waiting → needs you", true);
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(
            text,
            format!(" 12:34:56  {} pi: waiting → needs you", theme::GLYPH_NEEDS_INPUT)
        );
        assert_eq!(spans[0].style.fg, Some(theme::DIM));
        assert_eq!(spans[1].style.fg, Some(theme::ACCENT));
        assert_eq!(spans[2].style.fg, Some(theme::FG));
    }

    #[test]
    fn local_hh_mm_ss_formats_as_a_zero_padded_clock() {
        let s = super::local_hh_mm_ss(std::time::SystemTime::now());
        assert_eq!(s.len(), 8);
        assert_eq!(s.as_bytes()[2], b':');
        assert_eq!(s.as_bytes()[5], b':');
    }

    // -- C15/§8 help overlay ---------------------------------------------------

    #[test]
    fn help_keys_fit_the_cap() {
        // §8: hard cap, ≤ 20 content rows (the 80×24 floor's body is exactly
        // 22 rows = 20 content + 2 border, zero slack).
        assert!(HELP_KEYS.len() <= 20, "help overlay must never exceed the 20-row cap, got {}", HELP_KEYS.len());
    }

    #[test]
    fn help_keys_match_the_c8_key_table_verbatim_and_in_order() {
        // The exact §8 table — six fleet rows (a/e/z/f/Shift+p/g) folded
        // into the prior 14-row list at their §8 positions, then U7's tab
        // reach chords (2026-07-27): the tab row gained Alt+0 and a
        // previous/next row joined it, paid for by merging Alt+/ and Alt+?
        // into one row so the ≤20 cap still holds. Wording matches §8.
        // U23 (2026-07-27): three more merges (s/o, z/f, w/u) buy the three
        // reference rows that close the table — the status legend and the
        // two mouse rows.
        assert_eq!(
            HELP_KEYS,
            &[
                ("Alt+n", "new shell pane (auto split)"),
                ("Alt+Enter", "quick-launch picker (pi / claude / shell; 1..9)"),
                ("Alt+←↓↑→ / hjkl", "move focus"),
                ("Alt+Shift+←↓↑→", "resize along that axis"),
                ("Alt+s / Alt+o", "toggle split ⇄ stack / flip orientation"),
                ("Alt+g", "cycle layout: grid / main+stack / all-stack"),
                ("Alt+z / Alt+f", "zoom pane (view only) / floating scratch shell"),
                ("Alt+a", "jump to next pane that needs you"),
                ("Alt+e", "activity feed (status / spawns / exits / control)"),
                ("Alt+r / Alt+Shift+r", "rename pane / tab"),
                ("Alt+t / Alt+1..9 / Alt+0", "new tab / go to tab / last tab"),
                ("Alt+i / Alt+m", "previous / next tab (wraps)"),
                ("Alt+w / Alt+u", "close pane (confirm if busy) / undo close"),
                ("Alt+c / Alt+PgUp", "copy mode (hjkl w b e 0 $ v V y o) / scroll mode"),
                ("Alt+Shift+p", "raw pass-through for this pane (same chord exits)"),
                ("Alt+/ / Alt+?", "toggle hint bar / this keymap"),
                ("Alt+q", "quit (workspace saved; sessions live)"),
                ("status", "● working ◆ needs you ○ waiting · idle ✕ exited"),
                ("mouse", "wheel scrolls · click focuses · drag selects"),
                ("Alt+click / o", "open the URL under the pointer / copy cursor"),
            ],
        );
        assert_eq!(HELP_KEYS.len(), 20);
    }

    /// U23: the legend is the C5 glyph table, not a hand-copied lookalike —
    /// retheming a glyph must break this, not silently leave the overlay
    /// teaching a symbol roost no longer draws.
    #[test]
    fn help_legend_row_matches_the_theme_glyph_table() {
        let (key, desc) = HELP_KEYS
            .iter()
            .find(|(k, _)| *k == "status")
            .copied()
            .expect("the overlay carries a status-glyph legend row");
        assert_eq!(key, "status");
        assert_eq!(desc, super::status_legend_text());
        // The two glyphs the live-QA drive greps the frame for.
        assert!(desc.contains(theme::GLYPH_WORKING) && desc.contains(theme::GLYPH_EXITED));
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

    /// U20: the accelerator is on the hint bar too — a key you can only
    /// find by reading the source is not a feature.
    #[test]
    fn hint_pairs_picker_advertises_the_number_accelerators() {
        assert_eq!(
            hint_pairs(&Mode::Picker { selection: 0 }, false, false),
            vec![("↑↓", "choose"), ("↵", "open"), ("1..9", "launch"), ("Esc", "cancel")],
        );
    }

    /// U23: the overlay documents the mouse — the wheel, click-to-focus,
    /// drag-select and the Alt+click URL verb, which had lived only in the
    /// source until now.
    #[test]
    fn help_rows_document_every_mouse_verb() {
        let text = HELP_KEYS.iter().map(|(k, d)| format!("{k} {d}")).collect::<Vec<_>>().join("\n");
        for token in ["mouse", "wheel", "click", "drag", "Alt+click", "URL"] {
            assert!(text.contains(token), "the help overlay must mention {token:?}");
        }
    }

    /// C15 (amended U23): the overlay may never be wider than the 80-col
    /// floor — `centered_near` would clamp it to the screen and clip a
    /// description mid-word, the exact failure the width rule exists to
    /// stop. The three reference rows are long; this is their guard rail.
    #[test]
    fn help_dialog_fits_the_eighty_column_floor() {
        let w = help_dialog_width(HELP_KEYS);
        assert!(w <= 80, "help overlay is {w} cols wide; the 80-col floor would clip it");
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
