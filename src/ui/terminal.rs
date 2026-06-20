use ratatui::{
    Frame,
    layout::{Position, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::{
    app::App,
    terminal::{TerminalCellStyle, TerminalColor, TerminalStatus, TerminalStyledLine},
};

use super::{
    layout::{box_bottom, box_top_spans, pad_to_width, truncate_end_to_width},
    theme::*,
};

const TERMINAL_TAB_COLUMN_WIDTH: u16 = 12;

pub fn terminal_height(total_height: u16) -> u16 {
    if total_height < 18 {
        return 0;
    }
    (total_height / 3)
        .clamp(6, 14)
        .min(total_height.saturating_sub(8))
}

pub fn terminal_height_for_app(app: &App, total_height: u16) -> u16 {
    if app.terminal_visible {
        terminal_height(total_height)
    } else {
        0
    }
}

pub fn terminal_content_width(total_width: u16) -> u16 {
    let tab_width = terminal_tab_column_width(total_width);
    if tab_width == 0 {
        return total_width.saturating_sub(4).max(1);
    }
    total_width.saturating_sub(tab_width + 4).max(1)
}

pub(super) fn render_terminal(frame: &mut Frame, app: &App, area: Rect) {
    if area.height == 0 {
        return;
    }

    let width = area.width.max(1);
    let title = vec![Span::styled(
        " Terminal ",
        Style::default()
            .fg(if app.terminal_focused {
                ACCENT_COLOR
            } else {
                BORDER_BRIGHT_COLOR
            })
            .add_modifier(Modifier::BOLD),
    )];

    let mut lines = vec![box_top_spans(title, width)];
    let body_height = area.height.saturating_sub(2) as usize;
    let body_width = terminal_content_width(width);
    let tab_start = terminal_tab_window_start(
        app.active_terminal_tab,
        app.terminal_tabs.len(),
        body_height,
    );
    let mut screen_lines = app
        .active_terminal_tab()
        .map(|tab| tab.styled_screen_lines(body_height as u16, body_width))
        .unwrap_or_else(|| {
            vec![TerminalStyledLine::plain(
                app.terminal_init_error
                    .as_deref()
                    .unwrap_or("agent terminal is unavailable")
                    .to_owned(),
            )]
        });

    if screen_lines.len() > body_height {
        screen_lines = screen_lines[screen_lines.len() - body_height..].to_vec();
    }
    while screen_lines.len() < body_height {
        screen_lines.push(TerminalStyledLine::default());
    }
    lines.extend(
        screen_lines
            .into_iter()
            .enumerate()
            .map(|(row, line)| terminal_body_line(app, row, tab_start, &line, width)),
    );
    if area.height > 1 {
        lines.push(terminal_footer(width));
    }

    frame.render_widget(
        Paragraph::new(lines).style(Style::default().bg(BG_COLOR)),
        area,
    );

    if app.terminal_focused
        && let Some(tab) = app.active_terminal_tab()
        && let Some((row, col)) = tab.cursor_position()
    {
        let body_height = area.height.saturating_sub(2);
        let body_width = terminal_content_width(area.width);
        frame.set_cursor_position(Position::new(
            area.x + terminal_content_x_offset(area.width) + col.min(body_width.saturating_sub(1)),
            area.y + 1 + row.min(body_height.saturating_sub(1)),
        ));
    }
}

pub(super) fn terminal_status_label(app: &App) -> String {
    if !app.terminal_visible {
        return " hidden ".to_owned();
    }

    let focus = if app.terminal_focused {
        "focused"
    } else {
        "attached"
    };
    let status = match app.active_terminal_tab().map(|terminal| terminal.status()) {
        Some(TerminalStatus::Idle) => "idle".to_owned(),
        Some(TerminalStatus::Running { description }) => format!("running {description}"),
        Some(TerminalStatus::TimedOut) => "timed out".to_owned(),
        Some(TerminalStatus::Error(error)) => format!("error {error}"),
        None => "unavailable".to_owned(),
    };
    format!(" {status} / {focus} ")
}

fn terminal_body_line(
    app: &App,
    row: usize,
    tab_start: usize,
    line: &TerminalStyledLine,
    width: u16,
) -> Line<'static> {
    let width = width as usize;
    if width < 4 {
        return Line::from(terminal_spans(line));
    }

    let tab_width = terminal_tab_column_width(width as u16) as usize;
    let content_width = terminal_content_width(width as u16) as usize;
    let text_width = terminal_line_width(line).min(content_width);
    let padding = content_width.saturating_sub(text_width);
    if tab_width == 0 {
        let mut spans = vec![Span::styled("│ ", Style::default().fg(BORDER_COLOR))];
        spans.extend(terminal_spans(line));
        spans.push(Span::raw(" ".repeat(padding)));
        spans.push(Span::styled(" │", Style::default().fg(BORDER_COLOR)));
        return Line::from(spans);
    }

    let mut spans = vec![Span::styled("│", Style::default().fg(BORDER_COLOR))];
    spans.extend(terminal_tab_spans(app, row, tab_start, tab_width));
    spans.push(Span::styled("│ ", Style::default().fg(PANEL_DIM_COLOR)));
    spans.extend(terminal_spans(line));
    spans.push(Span::raw(" ".repeat(padding)));
    spans.push(Span::styled("│", Style::default().fg(BORDER_COLOR)));
    Line::from(spans)
}

fn terminal_tab_column_width(width: u16) -> u16 {
    if width < 36 {
        return 0;
    }
    TERMINAL_TAB_COLUMN_WIDTH.min(width.saturating_sub(20))
}

fn terminal_content_x_offset(width: u16) -> u16 {
    let tab_width = terminal_tab_column_width(width);
    if tab_width == 0 { 2 } else { tab_width + 3 }
}

fn terminal_tab_spans(app: &App, row: usize, tab_start: usize, width: usize) -> Vec<Span<'static>> {
    let index = tab_start + row;
    let Some(tab) = app.terminal_tabs.get(index) else {
        return vec![Span::raw(" ".repeat(width))];
    };

    let selected = index == app.active_terminal_tab;
    let marker = if selected { ">" } else { " " };
    let label = truncate_end_to_width(&format!("{marker}{} {}", index + 1, tab.title()), width);
    let style = if selected {
        Style::default()
            .fg(ACCENT_COLOR)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(MUTED_TEXT_COLOR)
    };
    vec![Span::styled(pad_to_width(&label, width), style)]
}

pub(super) fn terminal_tab_window_start(active: usize, len: usize, height: usize) -> usize {
    if len <= height {
        return 0;
    }
    active.saturating_sub(height / 2).min(len - height)
}

pub(super) fn terminal_footer(width: u16) -> Line<'static> {
    let width = width as usize;
    if width < 2 {
        return Line::from(Span::styled(
            "─".repeat(width),
            Style::default().fg(BORDER_COLOR),
        ));
    }

    let content_width = width - 2;
    if content_width < 2 {
        return box_bottom(width as u16);
    }

    let hint_spans = terminal_footer_hint_spans(content_width.saturating_sub(1));
    let hint_width = spans_width(&hint_spans);
    let right = content_width.saturating_sub(1 + hint_width);

    let mut spans = vec![
        Span::styled("╰", Style::default().fg(BORDER_COLOR)),
        Span::styled("─", Style::default().fg(BORDER_COLOR)),
    ];
    spans.extend(hint_spans);
    spans.extend([
        Span::styled("─".repeat(right), Style::default().fg(BORDER_COLOR)),
        Span::styled("╯", Style::default().fg(BORDER_COLOR)),
    ]);
    Line::from(spans)
}

fn terminal_footer_hint_spans(max_width: usize) -> Vec<Span<'static>> {
    let chunks = [
        (" ", false),
        ("Ctrl+T", true),
        (" switch cursor | ", false),
        ("Alt+N", true),
        (" new tab | ", false),
        ("Alt+1-9", true),
        (" switch tab | ", false),
        ("Alt+D", true),
        (" close tab ", false),
    ];

    let mut spans = Vec::new();
    let mut remaining = max_width;
    for (text, is_key) in chunks {
        if remaining == 0 {
            break;
        }

        let text = truncate_text_to_width(text, remaining);
        if text.is_empty() {
            break;
        }
        remaining = remaining.saturating_sub(text.width());
        let color = if is_key {
            KEY_HINT_COLOR
        } else {
            MUTED_TEXT_COLOR
        };
        spans.push(Span::styled(text, Style::default().fg(color)));
    }
    spans
}

fn truncate_text_to_width(text: &str, max_width: usize) -> String {
    let mut truncated = String::new();
    let mut width = 0;
    for character in text.chars() {
        let character_width = character.width().unwrap_or(0);
        if width + character_width > max_width {
            break;
        }
        truncated.push(character);
        width += character_width;
    }
    truncated
}

fn spans_width(spans: &[Span<'_>]) -> usize {
    spans.iter().map(|span| span.content.as_ref().width()).sum()
}

fn terminal_spans(line: &TerminalStyledLine) -> Vec<Span<'static>> {
    line.spans
        .iter()
        .map(|span| Span::styled(span.text.clone(), terminal_cell_style(span.style)))
        .collect()
}

fn terminal_line_width(line: &TerminalStyledLine) -> usize {
    line.spans.iter().map(|span| span.text.width()).sum()
}

fn terminal_cell_style(cell: TerminalCellStyle) -> Style {
    let mut style = Style::default().fg(SOFT_TEXT_COLOR);
    if let Some(color) = terminal_color(cell.fg) {
        style = style.fg(color);
    }
    if let Some(color) = terminal_color(cell.bg) {
        style = style.bg(color);
    }

    let mut modifiers = Modifier::empty();
    if cell.bold {
        modifiers |= Modifier::BOLD;
    }
    if cell.italic {
        modifiers |= Modifier::ITALIC;
    }
    if cell.underline {
        modifiers |= Modifier::UNDERLINED;
    }
    if cell.inverse {
        modifiers |= Modifier::REVERSED;
    }

    style.add_modifier(modifiers)
}

fn terminal_color(color: TerminalColor) -> Option<Color> {
    match color {
        TerminalColor::Default => None,
        TerminalColor::Indexed(index) => Some(Color::Indexed(index)),
        TerminalColor::Rgb(red, green, blue) => Some(Color::Rgb(red, green, blue)),
    }
}
