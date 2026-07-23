use ratatui::{
    style::{Modifier, Style},
    text::{Line, Span},
};

use crate::progress::{TodoItem, TodoStatus, TodoUpdate};

use super::{layout::wrap_text, theme::*};

const IN_PROGRESS_ICONS: [&str; 4] = ["◐", "◓", "◑", "◒"];

pub(super) fn pinned_lines(
    update: &TodoUpdate,
    width: u16,
    animation_frame: usize,
) -> Vec<Line<'static>> {
    checklist_lines(update, width, Some(animation_frame))
}

pub(super) fn transcript_lines(update: &TodoUpdate, width: u16) -> Vec<Line<'static>> {
    let mut lines = vec![Line::from("")];
    lines.extend(checklist_lines(update, width, None));
    lines
}

fn checklist_lines(
    update: &TodoUpdate,
    width: u16,
    animation_frame: Option<usize>,
) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    lines.push(header_line(update));
    if let Some(explanation) = update
        .explanation
        .as_deref()
        .map(str::trim)
        .filter(|explanation| !explanation.is_empty())
    {
        for row in wrap_text(explanation, width.saturating_sub(4)) {
            lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(row, Style::default().fg(MUTED_TEXT_COLOR)),
            ]));
        }
    }
    for item in &update.todos {
        lines.extend(todo_lines(item, width, animation_frame));
    }
    lines
}

fn header_line(update: &TodoUpdate) -> Line<'static> {
    let completed = update.completed_count();
    let total = update.todos.len();
    let title = if update.is_all_completed() {
        "Completed"
    } else {
        "Progress"
    };
    let mut spans = vec![
        Span::raw("  "),
        Span::styled(
            title,
            Style::default()
                .fg(ACCENT_COLOR)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" "),
        Span::styled(
            format!("{completed}/{total}"),
            Style::default().fg(TEXT_COLOR).add_modifier(Modifier::BOLD),
        ),
    ];
    if let Some(active) = update.active_label() {
        spans.push(Span::styled(" · ", Style::default().fg(MUTED_TEXT_COLOR)));
        spans.push(Span::styled(
            active.to_owned(),
            Style::default()
                .fg(BORDER_BRIGHT_COLOR)
                .add_modifier(Modifier::BOLD),
        ));
    }
    Line::from(spans)
}

fn todo_lines(item: &TodoItem, width: u16, animation_frame: Option<usize>) -> Vec<Line<'static>> {
    let (icon, text, style) = match item.status {
        TodoStatus::Pending => (
            "○",
            item.content.trim(),
            Style::default().fg(MUTED_TEXT_COLOR),
        ),
        TodoStatus::InProgress => (
            in_progress_icon(animation_frame.unwrap_or(0)),
            item.active_form.trim(),
            Style::default()
                .fg(ACCENT_COLOR)
                .add_modifier(Modifier::BOLD),
        ),
        TodoStatus::Completed => (
            "●",
            item.content.trim(),
            Style::default()
                .fg(SOFT_TEXT_COLOR)
                .add_modifier(Modifier::DIM),
        ),
    };

    let rows = wrap_text(text, width.saturating_sub(6).max(1));
    if rows.is_empty() {
        return vec![Line::from(vec![
            Span::raw("  "),
            Span::styled(icon.to_owned(), style),
        ])];
    }

    rows.into_iter()
        .enumerate()
        .map(|(index, row)| {
            if index == 0 {
                Line::from(vec![
                    Span::raw("  "),
                    Span::styled(icon.to_owned(), style),
                    Span::raw(" "),
                    Span::styled(row, style),
                ])
            } else {
                Line::from(vec![Span::raw("    "), Span::styled(row, style)])
            }
        })
        .collect()
}

fn in_progress_icon(animation_frame: usize) -> &'static str {
    IN_PROGRESS_ICONS[animation_frame % IN_PROGRESS_ICONS.len()]
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn checklist_uses_requested_status_icons() {
        let update = TodoUpdate::from_tool_arguments(&json!({
            "todos": [
                {"content": "Done", "active_form": "Doing done", "status": "completed"},
                {"content": "Work", "active_form": "Working", "status": "in_progress"},
                {"content": "Next", "active_form": "Doing next", "status": "pending"}
            ]
        }))
        .unwrap();

        let text = pinned_lines(&update, 80, 0)
            .into_iter()
            .flat_map(|line| line.spans.into_iter().map(|span| span.content.into_owned()))
            .collect::<Vec<_>>()
            .join("");

        assert!(text.contains("● Done"));
        assert!(text.contains("◐ Working"));
        assert!(text.contains("○ Next"));
    }

    #[test]
    fn pinned_in_progress_icon_animates_by_frame() {
        let update = TodoUpdate::from_tool_arguments(&json!({
            "todos": [
                {"content": "Work", "active_form": "Working", "status": "in_progress"}
            ]
        }))
        .unwrap();

        let frames = (0..4)
            .map(|frame| {
                pinned_lines(&update, 80, frame)
                    .into_iter()
                    .flat_map(|line| line.spans.into_iter().map(|span| span.content.into_owned()))
                    .collect::<Vec<_>>()
                    .join("")
            })
            .collect::<Vec<_>>();

        assert!(frames[0].contains("◐ Working"));
        assert!(frames[1].contains("◓ Working"));
        assert!(frames[2].contains("◑ Working"));
        assert!(frames[3].contains("◒ Working"));
    }
}
