use ratatui::{
    Frame,
    layout::{Position, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::{
    app::{App, TerminalTabSwitcher},
    terminal::{TerminalCellStyle, TerminalColor, TerminalStatus, TerminalStyledLine},
};

use super::{
    layout::{box_bottom, box_top_spans, pad_to_width, truncate_end_to_width},
    theme::*,
};

const SWITCHER_CARD_WIDTH: u16 = 28;
const SWITCHER_CARD_GAP: u16 = 1;
const SWITCHER_CARD_HEIGHT: u16 = 1;

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
    total_width.saturating_sub(4).max(1)
}

pub fn terminal_tab_hitbox(
    app: &App,
    top_row: u16,
    width: u16,
    height: u16,
) -> Option<(u16, u16, u16, u16, usize, usize)> {
    terminal_tab_hitbox_for(
        app.terminal_tab_switcher.as_ref(),
        app.terminal_tabs.len(),
        top_row,
        width,
        height,
    )
}

pub(super) fn terminal_tab_hitbox_for(
    switcher: Option<&TerminalTabSwitcher>,
    tab_count: usize,
    top_row: u16,
    width: u16,
    height: u16,
) -> Option<(u16, u16, u16, u16, usize, usize)> {
    let switcher = switcher?;
    if tab_count == 0 || height <= 2 {
        return None;
    }
    let visible = terminal_switcher_visible_cards(width, tab_count);
    if visible == 0 {
        return None;
    }

    let body_rows = height.saturating_sub(2);
    let card_rows = SWITCHER_CARD_HEIGHT.min(body_rows);
    let first_tab = switcher.window_start.min(tab_count.saturating_sub(1));
    let visible_tabs = (tab_count - first_tab).min(visible) as u16;

    Some((
        top_row + 1,
        top_row + 1 + card_rows,
        2,
        2 + visible_tabs * SWITCHER_CARD_WIDTH + visible_tabs.saturating_sub(1) * SWITCHER_CARD_GAP,
        first_tab,
        visible_tabs as usize,
    ))
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
    let mut body_lines = terminal_content_lines(
        app,
        terminal_preview_tab_index(app),
        body_height,
        body_width,
    );
    if app.terminal_tab_switcher.is_some() {
        body_lines = terminal_lines_with_switcher(app, body_lines, body_height, body_width);
    }
    while body_lines.len() < body_height {
        body_lines.push(Line::from(""));
    }
    lines.extend(
        body_lines
            .into_iter()
            .take(body_height)
            .map(|line| terminal_body_line(line, width)),
    );
    if area.height > 1 {
        lines.push(terminal_footer(width));
    }

    frame.render_widget(
        Paragraph::new(lines).style(Style::default().bg(BG_COLOR)),
        area,
    );

    if app.terminal_focused
        && app.terminal_tab_switcher.is_none()
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

pub(super) fn terminal_preview_tab_index(app: &App) -> Option<usize> {
    app.terminal_tab_switcher
        .as_ref()
        .map(|switcher| switcher.candidate)
        .filter(|candidate| *candidate < app.terminal_tabs.len())
        .or_else(|| {
            (app.active_terminal_tab < app.terminal_tabs.len()).then_some(app.active_terminal_tab)
        })
}

fn terminal_content_lines(
    app: &App,
    tab_index: Option<usize>,
    height: usize,
    width: u16,
) -> Vec<Line<'static>> {
    let Some(tab) = tab_index.and_then(|index| app.terminal_tabs.get(index)) else {
        return vec![Line::from(vec![Span::styled(
            app.terminal_init_error
                .as_deref()
                .unwrap_or("agent terminal is unavailable")
                .to_owned(),
            Style::default().fg(MUTED_TEXT_COLOR),
        )])];
    };

    let mut lines = tab
        .styled_screen_lines(height as u16, width)
        .into_iter()
        .map(|line| Line::from(terminal_spans(&line)))
        .collect::<Vec<_>>();
    if lines.len() > height {
        lines = lines[lines.len() - height..].to_vec();
    }
    lines
}

fn terminal_lines_with_switcher(
    app: &App,
    mut content_lines: Vec<Line<'static>>,
    body_height: usize,
    body_width: u16,
) -> Vec<Line<'static>> {
    let switcher_height = SWITCHER_CARD_HEIGHT.min(body_height as u16) as usize;
    if switcher_height == 0 {
        return content_lines;
    }

    let mut lines = terminal_switcher_lines(app, body_width, switcher_height);
    let remaining = body_height.saturating_sub(lines.len());
    if remaining > 0 {
        content_lines = visible_tail(content_lines, remaining, 0);
        lines.extend(content_lines);
    }
    lines
}

fn visible_tail(lines: Vec<Line<'static>>, height: usize, scroll: usize) -> Vec<Line<'static>> {
    if height == 0 || lines.is_empty() {
        return Vec::new();
    }
    let end = lines
        .len()
        .saturating_sub(scroll)
        .max(height)
        .min(lines.len());
    let start = end.saturating_sub(height);
    lines[start..end].to_vec()
}

fn terminal_switcher_lines(app: &App, width: u16, height: usize) -> Vec<Line<'static>> {
    let Some(switcher) = app.terminal_tab_switcher.as_ref() else {
        return Vec::new();
    };
    let visible = terminal_switcher_visible_cards(width.saturating_add(4), app.terminal_tabs.len());
    let start = switcher
        .window_start
        .min(app.terminal_tabs.len().saturating_sub(1));
    let end = (start + visible).min(app.terminal_tabs.len());
    let mut rows = Vec::new();
    for row in 0..height {
        let mut spans = Vec::new();
        for (slot, index) in (start..end).enumerate() {
            if slot > 0 {
                spans.push(Span::raw(" ".repeat(SWITCHER_CARD_GAP as usize)));
            }
            spans.extend(terminal_switcher_card_spans(app, switcher, index, row));
        }
        let row_width = spans_width(&spans);
        let target_width = width as usize;
        if row_width < target_width {
            spans.push(Span::raw(" ".repeat(target_width - row_width)));
        }
        rows.push(Line::from(spans));
    }
    rows
}

fn terminal_switcher_card_spans(
    app: &App,
    switcher: &TerminalTabSwitcher,
    index: usize,
    _row: usize,
) -> Vec<Span<'static>> {
    let Some(tab) = app.terminal_tabs.get(index) else {
        return vec![Span::raw(" ".repeat(SWITCHER_CARD_WIDTH as usize))];
    };
    let selected = index == switcher.candidate;
    let active = index == app.active_terminal_tab;
    let marker = if selected {
        ">"
    } else if active {
        "*"
    } else {
        " "
    };
    let label = format!(
        "{marker}{} {} {} {}",
        index + 1,
        tab.kind_label(),
        terminal_status_short(tab.status()),
        tab.title()
    );
    let width = SWITCHER_CARD_WIDTH as usize;
    let inner = width.saturating_sub(2);
    let text = pad_to_width(&truncate_end_to_width(&label, inner), inner);
    let border_style = if selected {
        Style::default()
            .fg(ACCENT_COLOR)
            .add_modifier(Modifier::BOLD)
    } else if active {
        Style::default().fg(BORDER_BRIGHT_COLOR)
    } else {
        Style::default().fg(PANEL_DIM_COLOR)
    };
    let text_style = if selected {
        Style::default()
            .fg(BG_COLOR)
            .bg(ACCENT_COLOR)
            .add_modifier(Modifier::BOLD)
    } else if active {
        Style::default().fg(TEXT_COLOR).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(MUTED_TEXT_COLOR)
    };
    vec![
        Span::styled("[", border_style),
        Span::styled(text, text_style),
        Span::styled("]", border_style),
    ]
}

fn terminal_status_short(status: TerminalStatus) -> &'static str {
    match status {
        TerminalStatus::Idle => "idle",
        TerminalStatus::Error(_) => "error",
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
        Some(TerminalStatus::Error(error)) => format!("error {error}"),
        None => "unavailable".to_owned(),
    };
    format!(" {status} / {focus} ")
}

fn terminal_body_line(line: Line<'static>, width: u16) -> Line<'static> {
    let width = width as usize;
    if width < 4 {
        return line;
    }

    let content_width = terminal_content_width(width as u16) as usize;
    let text_width = spans_width(&line.spans).min(content_width);
    let padding = content_width.saturating_sub(text_width);

    let mut spans = vec![Span::styled("│ ", Style::default().fg(BORDER_COLOR))];
    spans.extend(line.spans);
    spans.push(Span::raw(" ".repeat(padding)));
    spans.push(Span::styled(" │", Style::default().fg(BORDER_COLOR)));
    Line::from(spans)
}

pub fn terminal_content_x_offset(width: u16) -> u16 {
    if width < 4 { 0 } else { 2 }
}

pub(super) fn terminal_switcher_visible_cards(width: u16, tab_count: usize) -> usize {
    if tab_count == 0 {
        return 0;
    }
    let inner_width = width.saturating_sub(4).max(SWITCHER_CARD_WIDTH);
    let slot = SWITCHER_CARD_WIDTH + SWITCHER_CARD_GAP;
    let visible = ((inner_width + SWITCHER_CARD_GAP) / slot).max(1) as usize;
    visible.min(tab_count)
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
        ("Ctrl+Down", true),
        (" tabs | ", false),
        ("Ctrl+Up", true),
        (" select | ", false),
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
