use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};

use crate::message::Role;

use super::{format::duration_label, layout::wrap_text, markdown, progress, theme::*};

pub(super) fn message_lines(message: &crate::message::Message, width: u16) -> Vec<Line<'static>> {
    if message.role == Role::Progress {
        if let Some(update) = message.progress.as_ref() {
            return progress::transcript_lines(update, width);
        }
        return Vec::new();
    }

    if message.role == Role::Tool {
        return tool_message_lines(message, width);
    }

    if message.role == Role::User {
        return user_message_lines(message, width);
    }

    if message.role == Role::Assistant && message.content.is_empty() {
        return Vec::new();
    }

    let mut lines = vec![Line::from("")];
    let markdown_lines = markdown::render_markdown(&message.content, width.saturating_sub(2));
    for mut line in markdown_lines {
        let mut spans = vec![Span::raw("  ")];
        spans.append(&mut line.spans);
        lines.push(Line::from(spans));
    }
    lines
}

pub(super) fn processing_line(elapsed: std::time::Duration) -> Line<'static> {
    Line::from(vec![
        Span::raw("  "),
        Span::styled(
            "Processing",
            Style::default()
                .fg(ACCENT_COLOR)
                .add_modifier(Modifier::BOLD | Modifier::ITALIC | Modifier::RAPID_BLINK),
        ),
        Span::raw(" "),
        Span::styled(
            duration_label(elapsed.as_secs()),
            Style::default().fg(SOFT_TEXT_COLOR),
        ),
    ])
}

pub(super) fn notice_line(message: &str) -> Line<'static> {
    Line::from(vec![
        Span::raw("  "),
        Span::styled(
            message.to_owned(),
            Style::default()
                .fg(BORDER_BRIGHT_COLOR)
                .add_modifier(Modifier::BOLD),
        ),
    ])
}

pub(super) fn turn_duration_line(duration: std::time::Duration) -> Line<'static> {
    Line::from(vec![
        Span::raw("  "),
        Span::styled("Worked for ", Style::default().fg(MUTED_TEXT_COLOR)),
        Span::styled(
            duration_label(duration.as_secs()),
            Style::default().fg(TEXT_COLOR).add_modifier(Modifier::BOLD),
        ),
    ])
}

fn user_message_lines(message: &crate::message::Message, width: u16) -> Vec<Line<'static>> {
    let rule = user_rule(width);
    let mut lines = vec![Line::from(""), rule.clone()];
    let mut markdown_lines = markdown::render_markdown(&message.content, width.saturating_sub(4));
    trim_empty_lines(&mut markdown_lines);

    for (index, mut line) in markdown_lines.into_iter().enumerate() {
        let prefix = if index == 0 { "  ▶ " } else { "    " };
        let mut spans = vec![Span::raw(prefix)];
        spans.append(&mut line.spans);
        lines.push(Line::from(spans));
    }
    lines.push(rule);
    lines
}

fn trim_empty_lines(lines: &mut Vec<Line<'static>>) {
    while lines.last().is_some_and(line_is_empty) {
        lines.pop();
    }
    while lines.first().is_some_and(line_is_empty) {
        lines.remove(0);
    }
}

fn line_is_empty(line: &Line<'static>) -> bool {
    line.spans.iter().all(|span| span.content.is_empty())
}

fn user_rule(width: u16) -> Line<'static> {
    Line::from(Span::styled(
        "─".repeat(width as usize),
        Style::default().fg(PANEL_DIM_COLOR),
    ))
}

fn tool_message_lines(message: &crate::message::Message, width: u16) -> Vec<Line<'static>> {
    // `ui::document` projects Bash and linked Subagent messages before this generic fallback.
    let name = message.tool_name.as_deref().unwrap_or("Tool");
    let input = message.tool_input.as_deref().unwrap_or("");

    let mut lines = vec![Line::from("")];
    lines.push(Line::from(vec![
        Span::raw("  "),
        Span::styled(
            format!("◇ {name}"),
            Style::default()
                .fg(Color::Rgb(96, 165, 250))
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!(" {input}"),
            Style::default().fg(BORDER_BRIGHT_COLOR),
        ),
    ]));

    if let Some(description) = message.tool_description.as_deref() {
        for row in wrap_text(description, width.saturating_sub(6)) {
            lines.push(Line::from(vec![
                Span::raw("    "),
                Span::styled(row, Style::default().fg(MUTED_TEXT_COLOR)),
            ]));
        }
    }

    if name == "Read" {
        return lines;
    }

    let result = if message.content.is_empty() && !message.tool_finished {
        Some("Tooling...")
    } else if message.content.is_empty() {
        None
    } else {
        Some(message.content.as_str())
    };

    let Some(result) = result else {
        return lines;
    };

    for row in wrap_text(&tool_output_preview(result), width.saturating_sub(6)) {
        lines.push(Line::from(vec![
            Span::raw("    "),
            Span::styled(
                row,
                Style::default()
                    .fg(MUTED_TEXT_COLOR)
                    .add_modifier(if !message.tool_finished {
                        Modifier::ITALIC
                    } else {
                        Modifier::empty()
                    }),
            ),
        ]));
    }

    lines
}

pub(super) fn tool_output_preview(output: &str) -> String {
    const VISIBLE_LINES: usize = 3;

    let lines = output.lines().collect::<Vec<_>>();
    if lines.len() <= VISIBLE_LINES {
        return output.to_owned();
    }

    let omitted = lines.len() - VISIBLE_LINES;
    let mut preview = lines[..VISIBLE_LINES].join("\n");
    preview.push_str(&format!("\n...+{omitted} lines omitted"));
    preview
}
