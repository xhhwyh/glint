use ratatui::{
    style::{Modifier, Style},
    text::{Line, Span},
};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use super::theme::*;

pub(super) fn wrap_text(text: &str, width: u16) -> Vec<String> {
    text.split('\n')
        .flat_map(|line| wrap_line(line, width.max(1) as usize))
        .collect()
}

fn wrap_line(line: &str, width: usize) -> Vec<String> {
    let mut rows = Vec::new();
    let mut row = String::new();
    let mut row_width = 0;

    for char in line.chars() {
        let char_width = char.width().unwrap_or(0);
        if row_width + char_width > width && !row.is_empty() {
            rows.push(row);
            row = String::new();
            row_width = 0;
        }
        row.push(char);
        row_width += char_width;
    }

    rows.push(row);
    rows
}

pub(super) fn box_body_styled(text: &str, width: u16, style: Style) -> Line<'static> {
    let width = width as usize;
    if width < 4 {
        return Line::from(Span::styled(text.to_owned(), style));
    }

    let text_width = text.width();
    let padding = width.saturating_sub(text_width + 4);
    Line::from(vec![
        Span::styled("│ ", Style::default().fg(BORDER_COLOR)),
        Span::styled(text.to_owned(), style),
        Span::raw(" ".repeat(padding)),
        Span::styled(" │", Style::default().fg(BORDER_COLOR)),
    ])
}

pub(super) fn pad_to_width(text: &str, width: usize) -> String {
    let padding = width.saturating_sub(text.width());
    format!("{text}{}", " ".repeat(padding))
}

pub(super) fn truncate_end_to_width(text: &str, width: usize) -> String {
    const SUFFIX: &str = "...";
    let text_width = text.width();
    if text_width <= width {
        return text.to_owned();
    }
    if width <= SUFFIX.width() {
        return SUFFIX.chars().take(width).collect();
    }

    let mut truncated = String::new();
    let mut truncated_width = 0;
    let available = width - SUFFIX.width();
    for char in text.chars() {
        let char_width = char.width().unwrap_or(0);
        if truncated_width + char_width > available {
            break;
        }
        truncated.push(char);
        truncated_width += char_width;
    }
    truncated.push_str(SUFFIX);
    truncated
}

pub(super) fn truncate_start_to_width(text: &str, width: usize) -> String {
    const PREFIX: &str = "...";
    let text_width = text.width();
    if text_width <= width {
        return text.to_owned();
    }
    if width <= PREFIX.width() {
        return PREFIX.chars().take(width).collect();
    }

    let mut suffix = String::new();
    let mut suffix_width = 0;
    let available = width - PREFIX.width();
    for char in text.chars().rev() {
        let char_width = char.width().unwrap_or(0);
        if suffix_width + char_width > available {
            break;
        }
        suffix.insert(0, char);
        suffix_width += char_width;
    }

    format!("{PREFIX}{suffix}")
}

pub(super) fn box_top(title: &str, width: u16) -> Line<'static> {
    box_top_spans(
        vec![Span::styled(
            format!(" {} ", title.to_uppercase()),
            Style::default()
                .fg(ACCENT_COLOR)
                .add_modifier(Modifier::BOLD),
        )],
        width,
    )
}

pub(super) fn box_top_spans(title: Vec<Span<'static>>, width: u16) -> Line<'static> {
    let width = width as usize;
    let title_len: usize = title.iter().map(|span| span.width()).sum();

    if width < title_len + 4 {
        return Line::from(Span::styled(
            "─".repeat(width),
            Style::default().fg(BORDER_COLOR),
        ));
    }

    let right_len = width.saturating_sub(title_len + 3);
    let mut spans = vec![Span::styled("╭─", Style::default().fg(BORDER_COLOR))];
    spans.extend(title);
    spans.push(Span::styled(
        "─".repeat(right_len),
        Style::default().fg(BORDER_COLOR),
    ));
    spans.push(Span::styled("╮", Style::default().fg(BORDER_COLOR)));
    Line::from(spans)
}

pub(super) fn box_input_body(text: &str, width: u16) -> Line<'static> {
    let width = width as usize;
    if width < 4 {
        return Line::from(Span::styled(
            text.to_owned(),
            Style::default().fg(TEXT_COLOR),
        ));
    }

    let text_width = text.width();
    let padding = width.saturating_sub(text_width + 4);
    Line::from(vec![
        Span::styled("│ ", Style::default().fg(ACCENT_COLOR)),
        Span::styled(text.to_owned(), Style::default().fg(TEXT_COLOR)),
        Span::raw(" ".repeat(padding)),
        Span::styled(" │", Style::default().fg(ACCENT_COLOR)),
    ])
}

pub(super) fn box_bottom(width: u16) -> Line<'static> {
    let width = width as usize;
    if width < 2 {
        return Line::from(Span::styled(
            "─".repeat(width),
            Style::default().fg(BORDER_COLOR),
        ));
    }
    Line::from(vec![
        Span::styled("╰", Style::default().fg(BORDER_COLOR)),
        Span::styled("─".repeat(width - 2), Style::default().fg(BORDER_COLOR)),
        Span::styled("╯", Style::default().fg(BORDER_COLOR)),
    ])
}
