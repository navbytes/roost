//! Rendering: tab bar + pane borders + vt100 grid blit (design doc §8).

use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph};
use ratatui::Frame;

use crate::core::app::{App, Mode, PICKER_ITEMS};
use crate::core::status::AgentStatus;
use crate::core::layout::{compute_rects, PaneRect};
use crate::ports::PaneBackend;

pub fn draw<B: PaneBackend>(f: &mut Frame, app: &mut App<B>) {
    let area = f.area();
    if area.height < 2 {
        return;
    }
    let tab_bar = Rect::new(area.x, area.y, area.width, 1);
    let body = Rect::new(area.x, area.y + 1, area.width, area.height - 1);

    draw_tab_bar(f, app, tab_bar);

    let mut rects: Vec<PaneRect> = Vec::new();
    compute_rects(&app.ws.active_tab().layout, body, &mut rects);
    for pr in rects {
        draw_pane(f, app, pr);
    }

    draw_mode_overlay(f, app, body);
}

/// Centered floating rect of the given size, clamped to `area`.
fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let w = width.min(area.width);
    let h = height.min(area.height);
    Rect::new(
        area.x + (area.width - w) / 2,
        area.y + (area.height - h) / 2,
        w,
        h,
    )
}

fn draw_mode_overlay<B: PaneBackend>(f: &mut Frame, app: &App<B>, body: Rect) {
    match &app.mode {
        Mode::Normal | Mode::Scroll { .. } => {}
        Mode::Rename { buffer } => {
            let rect = centered(body, 44, 3);
            f.render_widget(Clear, rect);
            let block = Block::bordered()
                .title(" rename pane ")
                .border_style(Style::default().fg(Color::Yellow));
            let inner = block.inner(rect);
            f.render_widget(block, rect);
            f.render_widget(Paragraph::new(format!("{buffer}▏")), inner);
        }
        Mode::Picker { selection } => {
            let rect = centered(body, 32, PICKER_ITEMS.len() as u16 + 2);
            f.render_widget(Clear, rect);
            let block = Block::bordered()
                .title(" new pane — pick agent ")
                .border_style(Style::default().fg(Color::Yellow));
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
        " roost ",
        Style::default().fg(Color::Black).bg(Color::Yellow).add_modifier(Modifier::BOLD),
    )];
    for (i, tab) in app.ws.tabs.iter().enumerate() {
        let style = if i == app.ws.active_tab {
            Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::DarkGray)
        };
        spans.push(Span::styled(format!("  {} {}", i + 1, tab.name), style));
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
    let (title, status) = {
        let spec = app.ws.active_tab().panes.get(&pr.id);
        let status = app
            .runtimes
            .get(&pr.id)
            .map(|rt| rt.status())
            .unwrap_or(AgentStatus::Exited);
        let name = spec
            .and_then(|s| s.title.clone())
            .or_else(|| spec.map(|s| s.adapter.clone()))
            .unwrap_or_else(|| "?".into());
        let scroll_tag = if focused && matches!(app.mode, Mode::Scroll { .. }) {
            " [scroll]"
        } else {
            ""
        };
        (format!(" {} {}{} ", status.badge(), name, scroll_tag), status)
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
    let block = Block::bordered().title(title).border_style(border_style);
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
