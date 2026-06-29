use ratatui::{
    style::{Modifier, Style},
    text::{Line, Span},
};
use unicode_width::UnicodeWidthStr;

use crate::app::ResumePicker;

use super::{
    format::age_label, format::now, layout::pad_to_width, layout::truncate_end_to_width, theme::*,
};

pub(super) fn resume_picker_lines(
    picker: &ResumePicker,
    width: u16,
    height: usize,
) -> Vec<Line<'static>> {
    let width = width.max(1) as usize;
    let height = height.max(1);
    let now = now();
    let mut lines = Vec::new();

    lines.push(Line::from(Span::styled(
        "Resume a session",
        Style::default()
            .fg(ACCENT_COLOR)
            .add_modifier(Modifier::BOLD),
    )));
    lines.push(picker_separator_line(width));
    let footer_rows = 2;
    let list_height = height.saturating_sub(lines.len() + footer_rows);
    if picker.sessions.is_empty() {
        if list_height > 0 {
            lines.push(Line::from(Span::styled(
                "No saved sessions for this workspace",
                Style::default().fg(MUTED_TEXT_COLOR),
            )));
        }
    } else if list_height > 0 {
        let start = resume_window_start(picker.selected, picker.sessions.len(), list_height);
        let end = (start + list_height).min(picker.sessions.len());
        let age_width = picker.sessions[start..end]
            .iter()
            .map(|session| age_label(now.saturating_sub(session.last_timestamp)).width())
            .max()
            .unwrap_or(0)
            .max("time".width());

        for (offset, session) in picker.sessions[start..end].iter().enumerate() {
            let index = start + offset;
            let selected = index == picker.selected;
            let style = if selected {
                Style::default()
                    .fg(ACCENT_COLOR)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(TEXT_COLOR)
            };
            let age = pad_to_width(
                &age_label(now.saturating_sub(session.last_timestamp)),
                age_width,
            );
            let title_limit = width.saturating_sub(age_width + 6).max(1);
            lines.push(Line::from(vec![
                Span::styled(if selected { "❯ " } else { "  " }, style),
                Span::styled(age, style),
                Span::raw("  "),
                Span::styled(truncate_end_to_width(&session.title, title_limit), style),
            ]));
        }
    }

    if lines.len() + footer_rows <= height {
        lines.push(Line::from(Span::styled(
            "─".repeat(width),
            Style::default().fg(MUTED_TEXT_COLOR),
        )));
        lines.push(Line::from(vec![
            Span::styled("Enter", Style::default().fg(KEY_HINT_COLOR)),
            Span::styled(" select  ", Style::default().fg(MUTED_TEXT_COLOR)),
            Span::styled("↑/↓ ←/→", Style::default().fg(KEY_HINT_COLOR)),
            Span::styled(" switch  ", Style::default().fg(MUTED_TEXT_COLOR)),
            Span::styled("Esc", Style::default().fg(KEY_HINT_COLOR)),
            Span::styled(" exit", Style::default().fg(MUTED_TEXT_COLOR)),
        ]));
    }

    lines.truncate(height);
    lines
}

fn picker_separator_line(width: usize) -> Line<'static> {
    Line::from(Span::styled(
        "─".repeat(width),
        Style::default().fg(MUTED_TEXT_COLOR),
    ))
}

fn resume_window_start(selected: usize, len: usize, height: usize) -> usize {
    if len <= height {
        return 0;
    }
    selected.saturating_sub(height / 2).min(len - height)
}
