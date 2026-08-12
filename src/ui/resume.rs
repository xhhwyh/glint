use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Position, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Paragraph, Wrap},
};
use unicode_width::UnicodeWidthStr;

use crate::{
    app::{App, ResumePicker},
    event::{MouseAction, ResumeMouseAction},
    transcript::TranscriptSessionSummary,
};

use super::{
    format::{age_label, now},
    layout::{pad_to_width, truncate_end_to_width, truncate_start_to_width, wrap_text},
    theme::*,
};

const SELECTED_BG_COLOR: Color = Color::Rgb(15, 23, 42);

pub(super) fn render_resume_picker(frame: &mut Frame, app: &App, picker: &ResumePicker) {
    frame.render_widget(
        Block::default().style(Style::default().bg(BG_COLOR)),
        frame.area(),
    );

    let areas = resume_view_areas(frame.area());
    render_header(frame, app, picker, areas[0]);
    render_body(frame, picker, areas[1]);
    render_footer(frame, areas[2]);
}

pub(super) fn mouse_action(
    picker: &ResumePicker,
    mouse: MouseAction,
    width: u16,
    height: u16,
) -> ResumeMouseAction {
    if picker.sessions.is_empty() {
        return ResumeMouseAction::None;
    }

    let body = resume_view_areas(Rect::new(0, 0, width, height))[1];
    let list_panel = session_panel_areas(body)[0];
    let inner = panel_block("Saved sessions", true).inner(list_panel);
    let rows = Rect::new(
        inner.x,
        inner.y.saturating_add(1),
        inner.width,
        inner.height.saturating_sub(1),
    );
    let visible = rows.height as usize;
    let selected = picker.selected.min(picker.sessions.len() - 1);
    let start = list_window_start(selected, picker.sessions.len(), visible);

    match mouse {
        MouseAction::LeftDown { column, row } if rows.contains(Position::new(column, row)) => {
            let index = start + usize::from(row.saturating_sub(rows.y));
            if index < picker.sessions.len() {
                ResumeMouseAction::SelectSession(index)
            } else {
                ResumeMouseAction::None
            }
        }
        MouseAction::ScrollUp { column, row }
            if list_panel.contains(Position::new(column, row)) =>
        {
            ResumeMouseAction::MoveSelection(-3)
        }
        MouseAction::ScrollDown { column, row }
            if list_panel.contains(Position::new(column, row)) =>
        {
            ResumeMouseAction::MoveSelection(3)
        }
        _ => ResumeMouseAction::None,
    }
}

fn resume_view_areas(area: Rect) -> [Rect; 3] {
    let areas = Layout::vertical([
        Constraint::Length(4),
        Constraint::Min(1),
        Constraint::Length(2),
    ])
    .split(area);
    [areas[0], areas[1], areas[2]]
}

fn render_header(frame: &mut Frame, app: &App, picker: &ResumePicker, area: Rect) {
    let count = picker.sessions.len();
    let title = Line::from(vec![
        Span::styled(
            " Resume ",
            Style::default()
                .fg(ACCENT_COLOR)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("{count} saved session{}", plural(count)),
            Style::default().fg(MUTED_TEXT_COLOR),
        ),
    ]);
    let workspace_width = area.width.saturating_sub(14) as usize;
    let workspace = truncate_start_to_width(&app.current_dir, workspace_width);
    let block = Block::default()
        .borders(Borders::BOTTOM)
        .border_style(Style::default().fg(BORDER_COLOR));
    frame.render_widget(
        Paragraph::new(vec![
            title,
            Line::from(Span::styled(
                " Continue a previous conversation from this workspace",
                Style::default().fg(SOFT_TEXT_COLOR),
            )),
            Line::from(vec![
                Span::styled(" ● ", Style::default().fg(ACCENT_COLOR)),
                Span::styled("Workspace  ", Style::default().fg(MUTED_TEXT_COLOR)),
                Span::styled(workspace, Style::default().fg(SOFT_TEXT_COLOR)),
            ]),
        ])
        .block(block)
        .style(Style::default().bg(BG_COLOR)),
        area,
    );
}

fn render_body(frame: &mut Frame, picker: &ResumePicker, area: Rect) {
    let panels = session_panel_areas(area);
    render_session_list(frame, picker, panels[0]);
    render_session_details(frame, picker, panels[1]);
}

fn session_panel_areas(area: Rect) -> [Rect; 2] {
    let direction = session_panel_direction(area.width, area.height);
    let constraints = match direction {
        Direction::Horizontal => [Constraint::Percentage(60), Constraint::Percentage(40)],
        Direction::Vertical => [Constraint::Percentage(55), Constraint::Percentage(45)],
    };
    let panels = Layout::default()
        .direction(direction)
        .constraints(constraints)
        .spacing(1)
        .split(area);
    [panels[0], panels[1]]
}

fn session_panel_direction(width: u16, height: u16) -> Direction {
    if width >= 88 && height >= 12 {
        Direction::Horizontal
    } else {
        Direction::Vertical
    }
}

fn render_session_list(frame: &mut Frame, picker: &ResumePicker, area: Rect) {
    let block = panel_block("Saved sessions", true);
    let inner = block.inner(area);
    let lines = if picker.sessions.is_empty() {
        empty_session_lines()
    } else {
        session_list_lines(picker, inner.width as usize, inner.height as usize)
    };
    frame.render_widget(
        Paragraph::new(lines)
            .block(block)
            .style(Style::default().bg(BG_COLOR)),
        area,
    );
}

fn session_list_lines(picker: &ResumePicker, width: usize, height: usize) -> Vec<Line<'static>> {
    if height == 0 {
        return Vec::new();
    }

    let visible = height.saturating_sub(1);
    let selected = picker.selected.min(picker.sessions.len() - 1);
    let start = list_window_start(selected, picker.sessions.len(), visible);
    let end = (start + visible).min(picker.sessions.len());
    let now = now();
    let age_width = picker.sessions[start..end]
        .iter()
        .map(|session| age_label(now.saturating_sub(session.last_timestamp)).width())
        .max()
        .unwrap_or(0)
        .max("Updated".width())
        .min(width.saturating_sub(4));
    let heading = pad_to_width(&truncate_end_to_width("Updated", age_width), age_width);
    let mut lines = vec![Line::from(vec![
        Span::raw("  "),
        Span::styled(heading, Style::default().fg(MUTED_TEXT_COLOR)),
        Span::styled("  Conversation", Style::default().fg(MUTED_TEXT_COLOR)),
    ])];

    lines.extend(
        picker.sessions[start..end]
            .iter()
            .enumerate()
            .map(|(offset, session)| {
                session_line(session, start + offset == selected, width, age_width, now)
            }),
    );
    lines
}

fn session_line(
    session: &TranscriptSessionSummary,
    selected: bool,
    width: usize,
    age_width: usize,
    now: u64,
) -> Line<'static> {
    let marker = if selected { "› " } else { "  " };
    let age = age_label(now.saturating_sub(session.last_timestamp));
    let age = pad_to_width(&truncate_end_to_width(&age, age_width), age_width);
    let title_width = width.saturating_sub(marker.width() + age_width + 2);
    let title = truncate_end_to_width(&session.title, title_width);
    let used = marker.width() + age_width + 2 + title.width();
    let padding = " ".repeat(width.saturating_sub(used));
    let row_style = if selected {
        Style::default()
            .bg(SELECTED_BG_COLOR)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
    };

    Line::from(vec![
        Span::styled(
            marker.to_owned(),
            row_style.fg(if selected {
                ACCENT_COLOR
            } else {
                MUTED_TEXT_COLOR
            }),
        ),
        Span::styled(age, row_style.fg(MUTED_TEXT_COLOR)),
        Span::styled("  ", row_style),
        Span::styled(
            title,
            row_style.fg(if selected {
                TEXT_COLOR
            } else {
                SOFT_TEXT_COLOR
            }),
        ),
        Span::styled(padding, row_style),
    ])
}

fn render_session_details(frame: &mut Frame, picker: &ResumePicker, area: Rect) {
    let block = panel_block("Session details", false);
    let inner = block.inner(area);
    let lines = picker
        .sessions
        .get(picker.selected)
        .map(|session| session_detail_lines(session, inner.width as usize))
        .unwrap_or_else(empty_detail_lines);
    frame.render_widget(
        Paragraph::new(lines)
            .block(block)
            .wrap(Wrap { trim: false })
            .style(Style::default().bg(BG_COLOR)),
        area,
    );
}

fn session_detail_lines(session: &TranscriptSessionSummary, width: usize) -> Vec<Line<'static>> {
    let now = now();
    let mut lines = vec![
        Line::from(vec![
            Span::styled("◆ ", Style::default().fg(ACCENT_COLOR)),
            Span::styled(
                truncate_end_to_width(&session.title, width.saturating_sub(2)),
                Style::default().fg(TEXT_COLOR).add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::default(),
        field_line(
            "Last active",
            &age_label(now.saturating_sub(session.last_timestamp)),
            width,
        ),
    ];
    lines.extend(field_lines("Session ID", &session.session_id, width));
    lines.extend(field_lines(
        "Transcript",
        &session.path.to_string_lossy(),
        width,
    ));
    lines.push(Line::default());
    lines.extend(
        wrap_text(
            "Press Enter to continue this conversation in Glint.",
            width.max(1) as u16,
        )
        .into_iter()
        .map(|line| Line::from(Span::styled(line, Style::default().fg(MUTED_TEXT_COLOR)))),
    );
    lines
}

fn field_line(label: &str, value: &str, width: usize) -> Line<'static> {
    field_lines(label, value, width)
        .into_iter()
        .next()
        .unwrap_or_default()
}

fn field_lines(label: &str, value: &str, width: usize) -> Vec<Line<'static>> {
    const LABEL_WIDTH: usize = 14;
    if width <= LABEL_WIDTH {
        let mut lines = vec![Line::from(Span::styled(
            label.to_owned(),
            Style::default().fg(MUTED_TEXT_COLOR),
        ))];
        lines.extend(
            wrap_text(value, width.max(1) as u16)
                .into_iter()
                .map(|line| Line::from(Span::styled(line, Style::default().fg(SOFT_TEXT_COLOR)))),
        );
        return lines;
    }

    let prefix = format!("{label:<LABEL_WIDTH$}");
    let mut rows = wrap_text(value, (width - LABEL_WIDTH) as u16).into_iter();
    let first = rows.next().unwrap_or_default();
    let mut lines = vec![Line::from(vec![
        Span::styled(prefix, Style::default().fg(MUTED_TEXT_COLOR)),
        Span::styled(first, Style::default().fg(SOFT_TEXT_COLOR)),
    ])];
    lines.extend(rows.map(|line| {
        Line::from(vec![
            Span::raw(" ".repeat(LABEL_WIDTH)),
            Span::styled(line, Style::default().fg(SOFT_TEXT_COLOR)),
        ])
    }));
    lines
}

fn empty_session_lines() -> Vec<Line<'static>> {
    vec![
        Line::from(Span::styled(
            "○ No saved sessions",
            Style::default().fg(TEXT_COLOR).add_modifier(Modifier::BOLD),
        )),
        Line::default(),
        Line::from(Span::styled(
            "Start a conversation in this workspace to create one.",
            Style::default().fg(MUTED_TEXT_COLOR),
        )),
    ]
}

fn empty_detail_lines() -> Vec<Line<'static>> {
    vec![
        Line::from(Span::styled(
            "Nothing selected",
            Style::default().fg(TEXT_COLOR).add_modifier(Modifier::BOLD),
        )),
        Line::default(),
        Line::from(Span::styled(
            "Saved session metadata will appear here.",
            Style::default().fg(MUTED_TEXT_COLOR),
        )),
    ]
}

fn render_footer(frame: &mut Frame, area: Rect) {
    let block = Block::default()
        .borders(Borders::TOP)
        .border_style(Style::default().fg(BORDER_COLOR));
    frame.render_widget(
        Paragraph::new(key_help(&[
            ("↑/↓  ←/→", "select"),
            ("Enter", "resume"),
            ("Esc", "close"),
        ]))
        .block(block)
        .style(Style::default().bg(BG_COLOR)),
        area,
    );
}

fn key_help(items: &[(&str, &str)]) -> Line<'static> {
    let mut spans = vec![Span::raw(" ")];
    for (index, (key, action)) in items.iter().enumerate() {
        if index > 0 {
            spans.push(Span::styled("  │  ", Style::default().fg(BORDER_COLOR)));
        }
        spans.push(Span::styled(
            (*key).to_owned(),
            Style::default()
                .fg(KEY_HINT_COLOR)
                .add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::styled(
            format!(" {action}"),
            Style::default().fg(MUTED_TEXT_COLOR),
        ));
    }
    Line::from(spans)
}

fn panel_block(title: &str, emphasized: bool) -> Block<'static> {
    Block::default()
        .title(Span::styled(
            format!(" {title} "),
            Style::default()
                .fg(if emphasized {
                    ACCENT_COLOR
                } else {
                    BORDER_BRIGHT_COLOR
                })
                .add_modifier(Modifier::BOLD),
        ))
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(if emphasized {
            BORDER_BRIGHT_COLOR
        } else {
            BORDER_COLOR
        }))
        .style(Style::default().bg(BG_COLOR))
}

fn list_window_start(selected: usize, len: usize, visible: usize) -> usize {
    if visible == 0 || len <= visible {
        0
    } else {
        selected
            .saturating_sub(visible.saturating_sub(1))
            .min(len - visible)
    }
}

fn plural(count: usize) -> &'static str {
    if count == 1 { "" } else { "s" }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use ratatui::{Terminal, backend::TestBackend};

    use super::*;

    fn session(index: usize) -> TranscriptSessionSummary {
        TranscriptSessionSummary {
            path: PathBuf::from(format!("/workspace/.glint/session-{index}.jsonl")),
            session_id: format!("session-{index}"),
            title: format!("Investigate request latency {index}"),
            last_timestamp: now().saturating_sub(index as u64 * 60),
        }
    }

    #[test]
    fn resume_picker_uses_full_screen_panel_layout() {
        let mut app = App::test_empty();
        app.current_dir = "/workspace/glint".to_owned();
        app.resume_picker = Some(ResumePicker {
            sessions: vec![session(1)],
            selected: 0,
        });
        let backend = TestBackend::new(120, 30);
        let mut terminal = Terminal::new(backend).expect("test terminal");

        terminal
            .draw(|frame| crate::ui::render(frame, &app))
            .expect("render resume picker");

        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(rendered.contains("1 saved session"));
        assert!(rendered.contains("Saved sessions"));
        assert!(rendered.contains("Session details"));
        assert!(rendered.contains("Investigate request latency 1"));
        assert!(rendered.contains("session-1"));
        assert!(rendered.contains("Enter resume"));
        assert!(!rendered.contains("COMPOSER"));
    }

    #[test]
    fn session_panels_stack_on_narrow_terminals() {
        assert_eq!(session_panel_direction(120, 30), Direction::Horizontal);
        assert_eq!(session_panel_direction(80, 30), Direction::Vertical);
        assert_eq!(session_panel_direction(120, 10), Direction::Vertical);
    }

    #[test]
    fn list_window_keeps_selection_visible() {
        assert_eq!(list_window_start(0, 10, 4), 0);
        assert_eq!(list_window_start(5, 10, 4), 2);
        assert_eq!(list_window_start(9, 10, 4), 6);
    }

    #[test]
    fn mouse_targets_session_rows_and_scrolls_selection() {
        let picker = ResumePicker {
            sessions: vec![session(1), session(2)],
            selected: 0,
        };

        assert_eq!(
            mouse_action(
                &picker,
                MouseAction::LeftDown { column: 2, row: 6 },
                120,
                30,
            ),
            ResumeMouseAction::SelectSession(0)
        );
        assert_eq!(
            mouse_action(
                &picker,
                MouseAction::ScrollDown { column: 2, row: 6 },
                120,
                30,
            ),
            ResumeMouseAction::MoveSelection(3)
        );
    }
}
