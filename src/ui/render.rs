//! Rendering: tab bar + pane borders + vt100 grid blit (design doc §8).

use std::collections::HashSet;
use std::time::Duration;

use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Paragraph};
use ratatui::Frame;

use crate::core::app::{picker_items, App, Mode, RenameTarget, TabSummary};
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

    draw_tab_bar(f, app, tab_bar);

    // C6: header row above each stack tall enough to spare one — a separate
    // walk over the same tree `app.rects()` reads, since the header isn't a
    // `PaneRect` (it belongs to no pane; §5).
    for header in layout::stack_headers(&app.ws.active_tab().layout, body) {
        draw_stack_header(f, header);
    }
    // C7: which pane (if any) is the currently-expanded member of a stack —
    // computed once per frame, independent of whether that stack's header
    // row is shown.
    let mut stack_expanded = HashSet::new();
    layout::stack_expanded_ids(&app.ws.active_tab().layout, &mut stack_expanded);

    let rects = app.rects();
    for pr in &rects {
        draw_pane(f, app, *pr, stack_expanded.contains(&pr.id));
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
fn hint_pairs(mode: &Mode, focused_dead: bool) -> Vec<(&'static str, &'static str)> {
    match mode {
        Mode::Copy => vec![("drag", "select text"), ("Esc", "cancel")],
        Mode::Help => vec![("Alt+?", "all keys"), ("any key", "close")],
        Mode::Rename { target, .. } => {
            let what = match target {
                RenameTarget::Pane => "pane name",
                RenameTarget::Tab => "tab name",
            };
            vec![("type", what), ("↵", "save"), ("Esc", "cancel")]
        }
        Mode::Picker { .. } => vec![("↑↓", "choose"), ("↵", "open"), ("Esc", "cancel")],
        Mode::Scroll { .. } => {
            vec![("↑↓", "scroll"), ("PgUp/Dn", "page"), ("Esc", "exit")]
        }
        Mode::Normal if focused_dead => {
            vec![("↵", "relaunch"), ("f", "fresh — drops resume"), ("Alt+w", "close"), ("Alt+q", "quit")]
        }
        Mode::Normal => vec![
            ("Alt+n", "new"),
            ("Alt+↵", "launch"),
            ("Alt+s", "stack"),
            ("Alt+←↓↑→", "focus"),
            ("Alt+r", "rename"),
            ("Alt+w", "close"),
            ("Alt+?", "keys"),
        ],
    }
}

/// C9's right-segment uppercase mode word, one per `Mode` variant.
fn mode_word(mode: &Mode) -> &'static str {
    match mode {
        Mode::Normal => "NORMAL",
        Mode::Rename { .. } => "RENAME",
        Mode::Picker { .. } => "PICKER",
        Mode::Scroll { .. } => "SCROLL",
        Mode::Copy => "COPY",
        Mode::Help => "HELP",
    }
}

/// C9's right-aligned segment: the aggregate "◆ N needs you" — omitted at
/// `n == 0` rather than shown as a hollow "0 needs you" — then the
/// uppercase mode word, then one trailing space. Pure so the
/// omission-at-zero rule is unit-testable without a `Frame`.
fn hint_bar_right_spans(n: usize, word: &str) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    if n > 0 {
        spans.push(Span::styled(format!("◆ {n} needs you"), Style::default().fg(theme::ACCENT)));
        spans.push(Span::raw("  "));
    }
    spans.push(Span::styled(word.to_string(), Style::default().fg(theme::DIM)));
    spans.push(Span::raw(" "));
    spans
}

/// Zellij-style shortcut bar. Mode-aware: the keys shown match what you can
/// actually press right now. Precedence (C9): alt-warning, then flash, then
/// the hint pairs — each takes over the whole bar from the next.
fn draw_hint_bar<B: PaneBackend>(f: &mut Frame, app: &App<B>, area: Rect) {
    if app.show_alt_hint() {
        f.render_widget(
            Paragraph::new(
                " Alt keys aren't reaching roost? Enable \"Use Option as Meta Key\" in Terminal > Settings > Profiles > Keyboard ",
            )
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
    // MUTED, no chip bg.
    let hints = hint_pairs(&app.mode, app.focused_dead());
    let mut spans: Vec<Span> = Vec::with_capacity(hints.len() * 2 + 4);
    let mut used = 0u16;
    for (key, label) in hints {
        let key_span = format!(" {key} ");
        let label_span = format!("{label}  ");
        used += (key_span.chars().count() + label_span.chars().count()) as u16;
        spans.push(Span::styled(key_span, Style::default().fg(theme::ACCENT)));
        spans.push(Span::styled(label_span, Style::default().fg(theme::MUTED)));
    }

    // Right-aligned aggregate + mode word, drawn only when it still fits
    // after the hints (hints win on narrow widths).
    let right = hint_bar_right_spans(app.needs_input_count(), mode_word(&app.mode));
    let right_w: u16 = right.iter().map(|s| s.content.chars().count() as u16).sum();
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

fn draw_mode_overlay<B: PaneBackend>(f: &mut Frame, app: &App<B>, body: Rect, anchor: Rect) {
    match &app.mode {
        // Copy mode has no centered overlay — the selection is drawn in-pane.
        Mode::Normal | Mode::Scroll { .. } | Mode::Copy => {}
        Mode::Rename { buffer, target } => {
            let rect = centered_near(anchor, body, 44, 3);
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
            let rect = centered_near(anchor, body, 32, items.len() as u16 + 2);
            dim_backdrop(f, body, rect);
            f.render_widget(Clear, rect);
            let block = Block::bordered()
                .title(dialog_title(" new pane — pick agent "))
                .border_type(BorderType::Plain)
                .border_style(dialog_border_style());
            let inner = block.inner(rect);
            f.render_widget(block, rect);
            // C14: selected row is a `❯`-prefix + FG item text, no bg
            // highlight; unselected rows are plain MUTED text.
            let lines: Vec<Line> = items
                .iter()
                .enumerate()
                .map(|(i, item)| {
                    if i == *selection {
                        Line::from(vec![
                            Span::styled(theme::PICKER_SELECTED.to_string(), Style::default().fg(theme::ACCENT)),
                            Span::styled(format!(" {item}"), Style::default().fg(theme::FG)),
                        ])
                    } else {
                        Line::from(Span::styled(format!("  {item}"), Style::default().fg(theme::MUTED)))
                    }
                })
                .collect();
            f.render_widget(Paragraph::new(lines), inner);
        }
        Mode::Help => {
            // Full keymap — every binding, including the modal ones the compact
            // hint bar can't fit (scroll, copy, resize, flip, go-to-tab).
            let keys: &[(&str, &str)] = &[
                ("Alt+n", "new shell pane (auto split)"),
                ("Alt+Enter", "quick-launch picker (pi / claude / shell)"),
                ("Alt+←↓↑→ / hjkl", "move focus"),
                ("Alt+Shift+←↓↑→", "resize along that axis"),
                ("Alt+s", "toggle split ⇄ stack"),
                ("Alt+o", "flip split orientation"),
                ("Alt+r / Alt+Shift+r", "rename pane / tab"),
                ("Alt+t / Alt+1..9", "new tab / go to tab"),
                ("Alt+w", "close pane (confirm if busy / last)"),
                ("Alt+u", "undo — reopen last closed pane/tab"),
                ("Alt+c", "copy mode (drag to select)"),
                ("Alt+PgUp", "scroll mode"),
                ("Alt+/", "toggle hint bar"),
                ("Alt+q", "quit (workspace saved; sessions live)"),
            ];
            let h = keys.len() as u16 + 2;
            let rect = centered_near(anchor, body, 52, h.min(body.height));
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
                        Span::styled(format!(" {k:<18}"), Style::default().fg(theme::ACCENT)),
                        Span::styled(format!("{d}"), Style::default().fg(theme::MUTED)),
                    ])
                })
                .collect();
            f.render_widget(Paragraph::new(lines), inner);
        }
    }
}

/// C2: numbered tabs (marker + label + status glyph + separator) filling
/// the row edge-to-edge on `TAB_STRIP`, plus a right-aligned
/// "{cwd} · {save}" status area. Column bookkeeping here is the renderer
/// half of the mouse-hitbox lockstep rule (DESIGN-ui.md §4/§5) —
/// `mouse::tab_width`/`tab_at_x` mirror this exactly and change together.
fn draw_tab_bar<B: PaneBackend>(f: &mut Frame, app: &App<B>, area: Rect) {
    let cwd = app.focused_cwd();
    let saved = app.last_save_ok();
    let status_w = mouse::status_width(cwd.as_deref(), saved);
    let names: Vec<String> = app.ws.tabs.iter().map(|t| t.name.clone()).collect();
    let show_status = mouse::effective_status_width(&names, area.width, status_w) > 0;
    let tabs_end = mouse::tabs_visible_width(&names, area.width, status_w);
    let total_tabs_w = mouse::total_tabs_width(&names);

    // Left to right, one 7-part span group per tab (marker/label/glyph/
    // separator), stopping exactly where `tabs_visible_width` says to.
    let mut spans: Vec<Span> = Vec::with_capacity(names.len() * 7 + 3);
    let mut used = 0u16;
    for (i, tab) in app.ws.tabs.iter().enumerate() {
        if used >= tabs_end {
            break;
        }
        let active = i == app.ws.active_tab;
        let summary = app.tab_summary(i);
        let (glyph, base_color) = tab_summary_badge(summary);
        let glyph_color =
            if summary == TabSummary::Working { theme::pulse_phase(app.elapsed()) } else { base_color };
        push_tab_spans(&mut spans, i, &tab.name, active, glyph, glyph_color);
        used += mouse::tab_width(i, &tab.name);
    }

    // A single `…` marks the clip point when at least one tab didn't fit and
    // there's a spare column to show it in (overflow, C2).
    let budget = if show_status { area.width.saturating_sub(status_w) } else { area.width };
    if used < total_tabs_w && used < budget {
        spans.push(Span::styled(theme::TAB_OVERFLOW.to_string(), Style::default().fg(theme::MUTED)));
        used += 1;
    }

    if show_status {
        let (prefix, save_word) = mouse::status_parts(cwd.as_deref(), saved);
        let pad = area.width.saturating_sub(used).saturating_sub(status_w);
        if pad > 0 {
            spans.push(Span::raw(" ".repeat(pad as usize)));
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

/// One tab's 7-part span sequence (C2): marker, label, status glyph, and the
/// trailing separator — column count matches `mouse::tab_width` exactly.
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
}

/// Map a tab's aggregate summary to a tab-bar glyph + colour (theme::C5).
/// `Quiet` renders as a blank (no clutter for tabs with nothing to report);
/// `Unknown` is a faint dot so a not-yet-spawned background tab reads as
/// "unknown", not idle.
fn tab_summary_badge(s: crate::core::app::TabSummary) -> (char, Color) {
    theme::tab_summary_style(s)
}

/// C8's state-word table: the collapsed row's right-segment word for each
/// status. `Exited` is contracted bare — no exit code (SPEC-GAP-1: no
/// exit-code plumbing exists; `status.rs` tracks only a bool).
fn state_word(status: AgentStatus) -> &'static str {
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

/// C4's no-dup rule: an untitled pane's display `name` is the adapter/cwd
/// fallback built in `draw_pane`, which already embeds the adapter — so
/// appending `· {adapter}` again would duplicate it ("pi · repo · pi"). Only
/// a custom title needs the adapter spelled out separately.
fn badge_text(name: &str, adapter: &str, has_title: bool) -> String {
    if has_title {
        format!("{name} · {adapter}")
    } else {
        name.to_string()
    }
}

/// Render `parts` in order, stopping once `budget` columns are spent — the
/// part that doesn't fully fit is cut off mid-string rather than dropped
/// whole, so a badge/row degrades by trimming its tail instead of losing a
/// whole segment early. Shared by `corner_badge` (C4) and the collapsed-row
/// left side (C8) so both clip identically.
fn clip_spans(parts: &[(String, Style)], budget: u16) -> Vec<Span<'static>> {
    let mut spans = Vec::with_capacity(parts.len());
    let mut left = budget as usize;
    for (text, style) in parts {
        if left == 0 {
            break;
        }
        let count = text.chars().count();
        if count <= left {
            spans.push(Span::styled(text.clone(), *style));
            left -= count;
        } else {
            spans.push(Span::styled(text.chars().take(left).collect::<String>(), *style));
            left = 0;
        }
    }
    spans
}

fn draw_pane<B: PaneBackend>(f: &mut Frame, app: &mut App<B>, pr: PaneRect, stack_expanded: bool) {
    let focused = app.focused == pr.id;
    let (status, name, has_title, adapter) = {
        let spec = app.ws.active_tab().panes.get(&pr.id);
        let status = app
            .runtimes
            .get(&pr.id)
            .map(|rt| rt.status())
            .unwrap_or(AgentStatus::Exited);
        let has_title = spec.and_then(|s| s.title.as_ref()).is_some();
        let adapter = spec.map(|s| s.adapter.clone()).unwrap_or_else(|| "?".into());
        // Untitled panes on the same adapter are otherwise indistinguishable
        // (same badge, same idle glyph) — tag them with the cwd's last path
        // component so a bank of fresh shells can be told apart at a glance.
        let name = spec.and_then(|s| s.title.clone()).unwrap_or_else(|| {
            let cwd_tag = spec
                .and_then(|s| s.cwd.file_name())
                .and_then(|f| f.to_str())
                .map(|f| format!(" · {f}"))
                .unwrap_or_default();
            format!("{adapter}{cwd_tag}")
        });
        (status, name, has_title, adapter)
    };

    if pr.collapsed {
        // C8: collapsed stack member — a single-row fleet-view bar.
        draw_collapsed_row(f, pr.rect, focused, status, &name, &adapter, app.elapsed());
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

    if let Some(screen) = app.runtimes.get(&pr.id).and_then(|rt| rt.screen()) {
        blit_screen(f, screen, inner);
        if focused && status != AgentStatus::Exited {
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

    // C4: corner badge — the pane label, top-right. Drawn after the content
    // so it stays visible (a cell TUI can't do true translucency; MUTED text
    // reads as a watermark rather than content). Drawn on every pane,
    // focused included: occlusion of the inner app's own top-right cells is
    // accepted by design now that identity lives here, not a border title.
    let (glyph, glyph_base, pulses) = theme::status_style(status);
    let glyph_color = if pulses { theme::pulse_phase(app.elapsed()) } else { glyph_base };
    let text = badge_text(&name, &adapter, has_title);
    if let Some((rect, spans)) = corner_badge(inner, &text, glyph, glyph_color) {
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
            " ✕ exited — Enter: relaunch/resume · f: fresh (drops resume) · Alt+w: close ",
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
/// glyph, and name on the left; the right-aligned dim "adapter · word"
/// segment when there's room. The right segment drops first when narrow; if
/// even the left side overflows, the name (last in `left`) is what visibly
/// clips. Pure so the width-shedding order is unit-testable.
fn collapsed_row_spans(
    width: u16,
    focused: bool,
    status: AgentStatus,
    name: &str,
    adapter: &str,
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
        (format!(" {name}"), Style::default().fg(collapsed_name_color(status))),
    ];
    let left_w: u16 = left.iter().map(|(t, _)| t.chars().count() as u16).sum();
    let right = format!("{adapter} · {} ", state_word(status));
    let right_w = right.chars().count() as u16;

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
fn draw_collapsed_row(
    f: &mut Frame,
    rect: Rect,
    focused: bool,
    status: AgentStatus,
    name: &str,
    adapter: &str,
    elapsed: Duration,
) {
    let (_, base, pulses) = theme::status_style(status);
    let glyph_color = if pulses { theme::pulse_phase(elapsed) } else { base };
    let spans = collapsed_row_spans(rect.width, focused, status, name, adapter, glyph_color);
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
/// the text is MUTED, the glyph carries its own C5 status color. Returns the
/// 1-row rect and the clipped spans — or `None` if the pane is too small to
/// be worth badging. Pure so it can be unit-tested.
fn corner_badge(inner: Rect, text: &str, glyph: char, glyph_color: Color) -> Option<(Rect, Vec<Span<'static>>)> {
    if text.trim().is_empty() || inner.width < 3 || inner.height == 0 {
        return None;
    }
    let max = inner.width.saturating_sub(1);
    // One space of breathing room on the right edge (the trailing space in
    // the glyph part).
    let parts = [
        (format!(" {text} "), Style::default().fg(theme::MUTED)),
        (format!("{glyph} "), Style::default().fg(glyph_color)),
    ];
    let total: u16 = parts.iter().map(|(t, _)| t.chars().count() as u16).sum();
    let w = total.min(max);
    let spans = clip_spans(&parts, w);
    let x = inner.x + inner.width - w;
    Some((Rect::new(x, inner.y, w, 1), spans))
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

/// Copy the vt100 grid into the ratatui buffer.
/// NOTE: wide-char (CJK/emoji) handling is approximate in the scaffold.
fn blit_screen(f: &mut Frame, screen: &vt100::Screen, inner: Rect) {
    let (rows, cols) = screen.size();
    let buf = f.buffer_mut();
    for row in 0..inner.height.min(rows) {
        for col in 0..inner.width.min(cols) {
            let Some(cell) = screen.cell(row, col) else { continue };
            let x = inner.x + col;
            let y = inner.y + row;
            let Some(out) = buf.cell_mut((x, y)) else { continue };
            let contents = cell.contents();
            if contents.is_empty() {
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
    style
}

#[cfg(test)]
mod tests {
    use super::{
        badge_text, centered_near, collapsed_name_color, collapsed_row_spans, corner_badge,
        dialog_border_style, hint_bar_right_spans, hint_pairs, stack_header_text, state_word,
    };
    use crate::core::app::Mode;
    use crate::core::status::AgentStatus;
    use crate::ui::theme;
    use ratatui::layout::Rect;
    use ratatui::style::Style;

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
        // Untitled fallback name already embeds the adapter — don't repeat it.
        assert_eq!(badge_text("pi · myrepo", "pi", false), "pi · myrepo");
        // A custom title doesn't embed the adapter — spell it out.
        assert_eq!(badge_text("worker1", "claude", true), "worker1 · claude");
    }

    #[test]
    fn badge_is_two_toned_and_right_aligned_on_top_row() {
        // inner content area at (1,1) sized 40x20 (borders excluded)
        let inner = Rect::new(1, 1, 40, 20);
        let (rect, spans) = corner_badge(inner, "claude", theme::GLYPH_WORKING, theme::ACCENT).unwrap();
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
            corner_badge(inner, "a-very-long-name", theme::GLYPH_WORKING, theme::ACCENT).unwrap();
        let total: usize = spans.iter().map(|s| s.content.chars().count()).sum();
        assert!(total <= 5); // width-1 breathing room
        assert!(rect.x >= inner.x && rect.x + rect.width <= inner.x + inner.width);
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(!text.contains(theme::GLYPH_WORKING)); // too narrow even for the text alone
    }

    #[test]
    fn no_badge_for_tiny_or_empty() {
        assert!(corner_badge(Rect::new(0, 0, 2, 5), "x", theme::GLYPH_WORKING, theme::ACCENT).is_none());
        assert!(corner_badge(Rect::new(0, 0, 40, 0), "x", theme::GLYPH_WORKING, theme::ACCENT).is_none());
        assert!(corner_badge(Rect::new(0, 0, 40, 5), "   ", theme::GLYPH_WORKING, theme::ACCENT).is_none());
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
        let spans = collapsed_row_spans(40, false, AgentStatus::Working, "pi", "pi", theme::ACCENT);
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.ends_with("pi · working "));
    }

    #[test]
    fn collapsed_row_drops_right_segment_before_clipping_name() {
        let name = "a-fairly-long-pane-name";
        // Exactly enough room for "marker + glyph + ' ' + name", nothing more.
        let left_w = 3 + name.chars().count() as u16;
        let spans = collapsed_row_spans(left_w, false, AgentStatus::Idle, name, "shell", theme::DIM);
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, format!(" · {name}"));
        assert!(!text.contains("shell"));
    }

    #[test]
    fn collapsed_row_clips_name_when_even_the_left_side_overflows() {
        let spans =
            collapsed_row_spans(4, false, AgentStatus::Waiting, "a-very-long-pane-name", "shell", theme::FG);
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text.chars().count(), 4);
        assert!(!text.contains("shell"));
    }

    #[test]
    fn collapsed_row_focused_marker_is_accent() {
        let spans = collapsed_row_spans(40, true, AgentStatus::Working, "pi", "pi", theme::ACCENT);
        assert_eq!(spans[0].content.as_ref(), theme::MARKER_ACTIVE.to_string());
        assert_eq!(spans[0].style.fg, Some(theme::ACCENT));
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
        assert_eq!(
            hint_pairs(&Mode::Normal, false),
            vec![
                ("Alt+n", "new"),
                ("Alt+↵", "launch"),
                ("Alt+s", "stack"),
                ("Alt+←↓↑→", "focus"),
                ("Alt+r", "rename"),
                ("Alt+w", "close"),
                ("Alt+?", "keys"),
            ],
        );
    }

    #[test]
    fn hint_bar_right_omits_needs_segment_at_zero() {
        let spans = hint_bar_right_spans(0, "NORMAL");
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, "NORMAL ");
        assert!(!text.contains('◆'));
    }

    #[test]
    fn hint_bar_right_shows_aggregate_before_mode_word_when_nonzero() {
        let spans = hint_bar_right_spans(3, "NORMAL");
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, "◆ 3 needs you  NORMAL ");
        assert_eq!(spans[0].style.fg, Some(theme::ACCENT));
    }

    #[test]
    fn dialog_border_style_is_accent_with_no_modifiers() {
        // Pins C12: the old bright-fg/double-border/bold dialog look is
        // gone — one plain accent style for all three modals.
        assert_eq!(dialog_border_style(), Style::default().fg(theme::ACCENT));
    }
}
