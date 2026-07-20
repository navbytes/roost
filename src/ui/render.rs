//! Rendering: tab bar + pane borders + vt100 grid blit (design doc §8).

use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Paragraph};
use ratatui::Frame;

use crate::core::app::{App, Mode, RenameTarget, PICKER_ITEMS};
use crate::core::status::AgentStatus;
use crate::core::layout::PaneRect;
use crate::ports::PaneBackend;

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

    let rects = app.rects();
    for pr in &rects {
        draw_pane(f, app, *pr);
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

/// Zellij-style shortcut bar. Mode-aware: the keys shown match what you can
/// actually press right now.
fn draw_hint_bar<B: PaneBackend>(f: &mut Frame, app: &App<B>, area: Rect) {
    if app.show_alt_hint() {
        f.render_widget(
            Paragraph::new(
                " Alt keys aren't reaching roost? Enable \"Use Option as Meta Key\" in Terminal > Settings > Profiles > Keyboard ",
            )
            .style(Style::default().fg(Color::Black).bg(Color::Yellow)),
            area,
        );
        return;
    }

    // (key, what it does) pairs for the current context.
    let hints: Vec<(&str, &str)> = match &app.mode {
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
        Mode::Normal if app.focused_dead() => {
            vec![("↵", "relaunch"), ("f", "fresh"), ("Alt+q", "quit")]
        }
        Mode::Normal => vec![
            ("Alt+n", "split"),
            ("Alt+↵", "launch"),
            ("Alt+s", "stack"),
            ("Alt+t", "tab"),
            ("Alt+r", "rename"),
            ("Alt+←↓↑→", "focus"),
            ("Alt+w", "close"),
            ("Alt+/", "hide"),
            ("Alt+q", "quit"),
        ],
    };

    let mut spans: Vec<Span> = Vec::with_capacity(hints.len() * 3);
    for (key, label) in hints {
        spans.push(Span::styled(
            format!(" {key} "),
            Style::default().fg(Color::Black).bg(Color::DarkGray).add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::styled(format!(" {label}   "), Style::default().fg(Color::Gray)));
    }
    // Paragraph truncates (no wrap) so a narrow terminal just clips the tail.
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

/// Border style for floating dialogs: a fixed color + double border, so it
/// never blends with a pane's own (status-colored, single-line) border.
fn dialog_border_style() -> Style {
    Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)
}

fn draw_mode_overlay<B: PaneBackend>(f: &mut Frame, app: &App<B>, body: Rect, anchor: Rect) {
    match &app.mode {
        Mode::Normal | Mode::Scroll { .. } => {}
        Mode::Rename { buffer, target } => {
            let rect = centered_near(anchor, body, 44, 3);
            dim_backdrop(f, body, rect);
            f.render_widget(Clear, rect);
            let heading = match target {
                RenameTarget::Pane => " rename pane ",
                RenameTarget::Tab => " rename tab ",
            };
            let block = Block::bordered()
                .title(heading)
                .border_type(BorderType::Double)
                .border_style(dialog_border_style());
            let inner = block.inner(rect);
            f.render_widget(block, rect);
            f.render_widget(Paragraph::new(format!("{buffer}▏")), inner);
        }
        Mode::Picker { selection } => {
            let rect = centered_near(anchor, body, 32, PICKER_ITEMS.len() as u16 + 2);
            dim_backdrop(f, body, rect);
            f.render_widget(Clear, rect);
            let block = Block::bordered()
                .title(" new pane — pick agent ")
                .border_type(BorderType::Double)
                .border_style(dialog_border_style());
            let inner = block.inner(rect);
            f.render_widget(block, rect);
            let lines: Vec<Line> = PICKER_ITEMS
                .iter()
                .enumerate()
                .map(|(i, item)| {
                    let style = if i == *selection {
                        Style::default().fg(Color::Black).bg(Color::Yellow)
                    } else {
                        Style::default()
                    };
                    Line::from(Span::styled(format!("  {item:<28}"), style))
                })
                .collect();
            f.render_widget(Paragraph::new(lines), inner);
        }
    }
}

fn draw_tab_bar<B: PaneBackend>(f: &mut Frame, app: &App<B>, area: Rect) {
    let mut spans: Vec<Span> = vec![Span::styled(
        crate::ui::mouse::TABBAR_PREFIX,
        Style::default().fg(Color::Black).bg(Color::Yellow).add_modifier(Modifier::BOLD),
    )];
    for (i, tab) in app.ws.tabs.iter().enumerate() {
        let style = if i == app.ws.active_tab {
            Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::DarkGray)
        };
        // Shared label with the mouse hit-tester so clicks land on the right tab.
        spans.push(Span::styled(crate::ui::mouse::tab_label(i, &tab.name), style));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn status_color(s: AgentStatus) -> Color {
    match s {
        AgentStatus::Working => Color::Green,
        AgentStatus::NeedsInput => Color::Magenta,
        AgentStatus::Waiting => Color::Yellow,
        AgentStatus::Idle => Color::DarkGray,
        AgentStatus::Exited => Color::Red,
    }
}

fn draw_pane<B: PaneBackend>(f: &mut Frame, app: &mut App<B>, pr: PaneRect) {
    let focused = app.focused == pr.id;
    let (title, title_line, status, name) = {
        let spec = app.ws.active_tab().panes.get(&pr.id);
        let status = app
            .runtimes
            .get(&pr.id)
            .map(|rt| rt.status())
            .unwrap_or(AgentStatus::Exited);
        // Untitled panes on the same adapter are otherwise indistinguishable
        // (same badge, same idle glyph) — tag them with the cwd's last path
        // component so a bank of fresh shells can be told apart at a glance.
        let name = spec.and_then(|s| s.title.clone()).unwrap_or_else(|| {
            let adapter = spec.map(|s| s.adapter.clone()).unwrap_or_else(|| "?".into());
            let cwd_tag = spec
                .and_then(|s| s.cwd.file_name())
                .and_then(|f| f.to_str())
                .map(|f| format!(" · {f}"))
                .unwrap_or_default();
            format!("{adapter}{cwd_tag}")
        });
        let scroll_tag = if focused && matches!(app.mode, Mode::Scroll { .. }) {
            " [scroll]"
        } else {
            ""
        };
        let title = format!(" {} {}{} ", status.badge(), name, scroll_tag);
        // Color the status glyph itself (not just the focused border) so
        // idle vs. waiting vs. needs-input reads at a glance on every pane,
        // not only the one currently focused.
        let title_line = Line::from(vec![
            Span::raw(" "),
            Span::styled(
                status.badge(),
                Style::default().fg(status_color(status)).add_modifier(Modifier::BOLD),
            ),
            Span::raw(format!(" {name}{scroll_tag} ")),
        ]);
        (title, title_line, status, name)
    };

    if pr.collapsed {
        // Collapsed stack member: a single-row title bar (the fleet view).
        let style = if focused {
            Style::default().fg(Color::Black).bg(status_color(status))
        } else {
            Style::default().fg(status_color(status))
        };
        f.render_widget(Paragraph::new(title).style(style), pr.rect);
        return;
    }

    let border_style = if focused {
        Style::default().fg(status_color(status)).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    let block = Block::bordered().title(title_line).border_style(border_style);
    let inner = block.inner(pr.rect);
    f.render_widget(block, pr.rect);

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

    // iTerm2-style corner badge: the pane label, faint, top-right. Drawn
    // after the content so it stays visible (a cell TUI can't do true
    // translucency; dim gray reads as a watermark rather than content).
    if let Some((rect, text)) = corner_badge(inner, &name) {
        f.render_widget(
            Paragraph::new(text).style(Style::default().fg(Color::DarkGray)),
            rect,
        );
    }

    // Dead pane: overlay the relaunch hint (and spawn error, if any) on the
    // bottom rows. The last screen contents stay visible above.
    if status == AgentStatus::Exited && inner.height > 0 {
        let mut lines: Vec<Line> = Vec::new();
        if let Some(err) = app.dead.get(&pr.id) {
            lines.push(Line::from(Span::styled(
                format!(" spawn failed: {err} "),
                Style::default().fg(Color::Red),
            )));
        }
        lines.push(Line::from(Span::styled(
            " ✕ exited — Enter: relaunch/resume · f: fresh session ",
            Style::default().fg(Color::Black).bg(Color::Red),
        )));
        let n = lines.len() as u16;
        let y = inner.y + inner.height.saturating_sub(n);
        let overlay = Rect::new(inner.x, y, inner.width, n.min(inner.height));
        f.render_widget(Paragraph::new(lines), overlay);
    }
}

/// Top-right corner badge (iTerm2-style label). Returns the 1-row rect and
/// the space-padded, right-aligned, clipped text — or None if the pane is too
/// small to be worth badging. Pure so it can be unit-tested.
fn corner_badge(inner: Rect, label: &str) -> Option<(Rect, String)> {
    if label.trim().is_empty() || inner.width < 3 || inner.height == 0 {
        return None;
    }
    // One space of breathing room on the right edge.
    let max = inner.width.saturating_sub(1) as usize;
    let padded = format!(" {label} ");
    let text: String = if padded.chars().count() > max {
        padded.chars().take(max).collect()
    } else {
        padded
    };
    let w = text.chars().count() as u16;
    let x = inner.x + inner.width - w;
    Some((Rect::new(x, inner.y, w, 1), text))
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
    use super::{centered_near, corner_badge};
    use ratatui::layout::Rect;

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
    fn badge_is_right_aligned_on_top_row() {
        // inner content area at (1,1) sized 40x20 (borders excluded)
        let inner = Rect::new(1, 1, 40, 20);
        let (rect, text) = corner_badge(inner, "claude").unwrap();
        assert_eq!(text, " claude ");
        assert_eq!(rect.y, inner.y); // top row of the content
        assert_eq!(rect.height, 1);
        // right edge: badge ends one col shy of the inner right edge is fine;
        // here it butts to the edge because label fits.
        assert_eq!(rect.x + rect.width, inner.x + inner.width);
    }

    #[test]
    fn badge_clips_when_label_too_long_for_pane() {
        let inner = Rect::new(0, 0, 6, 5);
        let (rect, text) = corner_badge(inner, "a-very-long-name").unwrap();
        assert!(text.chars().count() <= 5); // width-1 breathing room
        assert!(rect.x >= inner.x && rect.x + rect.width <= inner.x + inner.width);
    }

    #[test]
    fn no_badge_for_tiny_or_empty() {
        assert!(corner_badge(Rect::new(0, 0, 2, 5), "x").is_none()); // too narrow
        assert!(corner_badge(Rect::new(0, 0, 40, 0), "x").is_none()); // no height
        assert!(corner_badge(Rect::new(0, 0, 40, 5), "   ").is_none()); // blank label
    }
}
