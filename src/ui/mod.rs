use std::time::{SystemTime, UNIX_EPOCH};

use ratatui::{
    Frame,
    layout::{Constraint, Layout, Position, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::{
    app::{App, ModelPickerStage, ResumePicker},
    approval::{ApprovalChoice, ApprovalFocus},
    message::Role,
    terminal::{TerminalCellStyle, TerminalColor, TerminalStatus, TerminalStyledLine},
};

mod markdown;
mod star;

const WELCOME_TEXT: &str = "Catch the glint. Shape the work.";
const TERMINAL_TAB_COLUMN_WIDTH: u16 = 12;

struct Document {
    lines: Vec<Line<'static>>,
    cursor_x: u16,
    cursor_y: u16,
}

pub fn render(frame: &mut Frame, app: &App) {
    if let Some(picker) = &app.resume_picker {
        render_resume_picker(frame, picker);
        return;
    }

    let terminal_height = terminal_height_for_app(app, frame.area().height);
    if terminal_height == 0 {
        render_document(frame, app, frame.area());
        return;
    }

    let chunks = Layout::vertical([Constraint::Min(1), Constraint::Length(terminal_height)])
        .split(frame.area());
    render_document(frame, app, chunks[0]);
    render_terminal(frame, app, chunks[1]);
}

fn render_document(frame: &mut Frame, app: &App, area: Rect) {
    let width = area.width.max(1);
    let document = document(app, width);
    let max_scroll = document.lines.len().saturating_sub(area.height as usize) as u16;
    let scroll = max_scroll.saturating_sub(app.scroll);

    frame.render_widget(
        Paragraph::new(document.lines)
            .scroll((scroll, 0))
            .style(Style::default().bg(BG_COLOR)),
        area,
    );

    if !app.terminal_focused
        && document.cursor_y >= scroll
        && document.cursor_y < scroll + area.height
    {
        frame.set_cursor_position(Position::new(
            area.x + document.cursor_x.min(width.saturating_sub(1)),
            area.y + document.cursor_y - scroll,
        ));
    }
}

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

fn render_terminal(frame: &mut Frame, app: &App, area: Rect) {
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

fn terminal_status_label(app: &App) -> String {
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

fn terminal_tab_window_start(active: usize, len: usize, height: usize) -> usize {
    if len <= height {
        return 0;
    }
    active.saturating_sub(height / 2).min(len - height)
}

fn terminal_footer(width: u16) -> Line<'static> {
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

    let hint = " Ctrl+T focus | Alt+N new tab | Alt+1-9/0 switch tab | Alt+D close tab ";
    let hint = truncate_end_to_width(hint, content_width.saturating_sub(1));
    let right = content_width.saturating_sub(1 + hint.width());

    Line::from(vec![
        Span::styled("╰", Style::default().fg(BORDER_COLOR)),
        Span::styled("─", Style::default().fg(BORDER_COLOR)),
        Span::styled(hint, Style::default().fg(MUTED_TEXT_COLOR)),
        Span::styled("─".repeat(right), Style::default().fg(BORDER_COLOR)),
        Span::styled("╯", Style::default().fg(BORDER_COLOR)),
    ])
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

fn render_resume_picker(frame: &mut Frame, picker: &ResumePicker) {
    let width = frame.area().width.max(1) as usize;
    let height = frame.area().height as usize;
    let now = now();
    let mut lines = Vec::new();

    lines.push(Line::from(Span::styled(
        "Resume a session",
        Style::default()
            .fg(ACCENT_COLOR)
            .add_modifier(Modifier::BOLD),
    )));
    lines.push(Line::from(""));

    let footer_rows = 2;
    let list_height = height.saturating_sub(lines.len() + footer_rows);
    if picker.sessions.is_empty() {
        lines.push(Line::from(Span::styled(
            "No saved sessions for this workspace",
            Style::default().fg(MUTED_TEXT_COLOR),
        )));
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

    while lines.len() + footer_rows < height {
        lines.push(Line::from(""));
    }
    lines.push(Line::from(Span::styled(
        "─".repeat(width),
        Style::default().fg(MUTED_TEXT_COLOR),
    )));
    lines.push(Line::from(vec![
        Span::styled("Enter", Style::default().fg(ACCENT_COLOR)),
        Span::styled(" select  ", Style::default().fg(MUTED_TEXT_COLOR)),
        Span::styled("↑/↓ ←/→", Style::default().fg(ACCENT_COLOR)),
        Span::styled(" switch  ", Style::default().fg(MUTED_TEXT_COLOR)),
        Span::styled("Esc", Style::default().fg(ACCENT_COLOR)),
        Span::styled(" exit", Style::default().fg(MUTED_TEXT_COLOR)),
    ]));

    frame.render_widget(Paragraph::new(lines), frame.area());
}

fn resume_window_start(selected: usize, len: usize, height: usize) -> usize {
    if len <= height {
        return 0;
    }
    selected.saturating_sub(height / 2).min(len - height)
}

fn document(app: &App, width: u16) -> Document {
    let mut lines = idle_panel_lines(app, width);
    let has_status_line = app.processing_elapsed().is_some()
        || app.run_notice.is_some()
        || app.last_turn_duration().is_some();

    lines.extend(transcript_lines(app, width));
    if !app.messages.is_empty() && !has_status_line {
        lines.push(Line::from(""));
    }

    let mut approval_cursor = None;
    if app.approval.is_some() {
        lines.extend(approval_lines(app, width));
        if matches!(
            app.approval.as_ref().map(|approval| &approval.focus),
            Some(ApprovalFocus::Feedback)
        ) {
            approval_cursor = Some((
                approval_feedback_cursor_x(app, width),
                lines.len() as u16 - 2,
            ));
        }
        lines.push(Line::from(""));
    }

    if let Some(elapsed) = app.processing_elapsed() {
        lines.push(processing_line(elapsed));
    } else {
        if let Some(notice) = app.run_notice.as_deref() {
            lines.push(notice_line(notice));
        }
        if let Some(duration) = app.last_turn_duration() {
            lines.push(turn_duration_line(duration));
        }
    }

    let input_y = lines.len() as u16;
    lines.push(box_top("COMPOSER", width));
    lines.extend(
        input_rows(&app.input.value, width)
            .into_iter()
            .map(|row| box_input_body(&row, width)),
    );
    lines.push(box_bottom(width));
    if app.model_picker.is_some() {
        lines.extend(model_picker_lines(app, width));
    } else if app.slash_menu_visible() {
        lines.extend(slash_command_lines(app, width));
    } else {
        lines.push(info_line(app, width));
        lines.push(context_line(app, width));
        if let Some(line) = permission_line(app) {
            lines.push(line);
        }
    }

    let (input_cursor_x, input_cursor_row) = input_cursor_position(app, width);
    let (cursor_x, cursor_y) =
        approval_cursor.unwrap_or((input_cursor_x, input_y + input_cursor_row + 1));
    Document {
        lines,
        cursor_x,
        cursor_y,
    }
}

fn idle_panel_lines(app: &App, width: u16) -> Vec<Line<'static>> {
    let width = width as usize;
    if width < 4 {
        return vec![Line::from(Span::styled(
            "─".repeat(width),
            Style::default().fg(BORDER_COLOR),
        ))];
    }

    let mut lines = vec![dashboard_top(width as u16)];
    let inner_width = width.saturating_sub(2);

    if inner_width >= 76 {
        let gutter = 3;
        let left_width = compact_signal_width(inner_width.saturating_sub(gutter + 28));
        let right_width = inner_width.saturating_sub(gutter + left_width);
        let left_rows = core_signal_panel(left_width);
        let right_rows = workspace_panel(app, right_width);
        let row_count = left_rows.len().max(right_rows.len());

        for row in 0..row_count {
            let mut spans = vec![Span::styled("│", Style::default().fg(BORDER_COLOR))];
            spans.extend(pad_spans(
                left_rows.get(row).cloned().unwrap_or_else(Vec::new),
                left_width,
            ));
            spans.push(Span::raw(" ".repeat(gutter / 2)));
            spans.push(Span::styled("│", Style::default().fg(PANEL_DIM_COLOR)));
            spans.push(Span::raw(" ".repeat(gutter - gutter / 2 - 1)));
            spans.extend(pad_spans(
                right_rows.get(row).cloned().unwrap_or_else(Vec::new),
                right_width,
            ));
            spans.push(Span::styled("│", Style::default().fg(BORDER_COLOR)));
            lines.push(Line::from(spans));
        }
    } else {
        for row in core_signal_panel(inner_width) {
            let mut spans = vec![Span::styled("│", Style::default().fg(BORDER_COLOR))];
            spans.extend(pad_spans(row, inner_width));
            spans.push(Span::styled("│", Style::default().fg(BORDER_COLOR)));
            lines.push(Line::from(spans));
        }
        for row in workspace_panel(app, inner_width) {
            let mut spans = vec![Span::styled("│", Style::default().fg(BORDER_COLOR))];
            spans.extend(pad_spans(row, inner_width));
            spans.push(Span::styled("│", Style::default().fg(BORDER_COLOR)));
            lines.push(Line::from(spans));
        }
    }

    lines.push(box_bottom(width as u16));
    lines
}

fn compact_signal_width(max_width: usize) -> usize {
    let desired = star::STAR_WIDTH
        .max(WELCOME_TEXT.width())
        .saturating_add(10);
    desired.min(max_width).max(star::STAR_WIDTH.min(max_width))
}

fn core_signal_panel(width: usize) -> Vec<Vec<Span<'static>>> {
    let mut rows = Vec::new();
    rows.push(vec![]);

    for star_row in star::glint_star_rows() {
        rows.push(center_spans(star_row, width));
    }

    rows.push(vec![]);
    rows.push(center_spans(
        vec![Span::styled(
            WELCOME_TEXT,
            Style::default().fg(TEXT_COLOR).add_modifier(Modifier::BOLD),
        )],
        width,
    ));
    rows.push(vec![]);
    rows
}

fn workspace_panel(app: &App, width: usize) -> Vec<Vec<Span<'static>>> {
    if width < 56 {
        let mut rows = workspace_hud_box(app, width);
        rows.push(vec![]);
        rows.extend(quick_actions_box(width));
        return rows;
    }

    let gutter = 2;
    let workspace_width = (width - gutter) / 2;
    let actions_width = width.saturating_sub(gutter + workspace_width);
    let mut rows = side_by_side_boxes(
        workspace_hud_box(app, workspace_width),
        workspace_width,
        quick_actions_box(actions_width),
        actions_width,
        gutter,
    );
    rows.push(vec![]);
    rows.extend(project_info_box(app, width));
    rows
}

fn workspace_hud_box(app: &App, width: usize) -> Vec<Vec<Span<'static>>> {
    vec![
        mini_box_top("WORKSPACE HUD", width),
        metric_row("STATE", "● READY", width),
        metric_row("MODEL", &app.config.llm.model, width),
        metric_row("PROVIDER", &app.config.llm.provider, width),
        metric_row("MODE", "agent", width),
        mini_box_bottom(width),
    ]
}

fn quick_actions_box(width: usize) -> Vec<Vec<Span<'static>>> {
    vec![
        mini_box_top("QUICK ACTIONS", width),
        mini_box_body(
            vec![
                Span::styled(
                    "↵",
                    Style::default()
                        .fg(ACCENT_COLOR)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(" send", Style::default().fg(SOFT_TEXT_COLOR)),
            ],
            width,
        ),
        mini_box_body(
            vec![
                Span::styled(
                    "⇧↵",
                    Style::default()
                        .fg(ACCENT_COLOR)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(" newline", Style::default().fg(SOFT_TEXT_COLOR)),
            ],
            width,
        ),
        mini_box_body(
            vec![
                Span::styled(
                    "^C",
                    Style::default()
                        .fg(ACCENT_COLOR)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(" quit", Style::default().fg(SOFT_TEXT_COLOR)),
            ],
            width,
        ),
        mini_box_body(
            vec![
                Span::styled(
                    "wheel",
                    Style::default()
                        .fg(ACCENT_COLOR)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(" scroll", Style::default().fg(SOFT_TEXT_COLOR)),
            ],
            width,
        ),
        mini_box_bottom(width),
    ]
}

fn project_info_box(app: &App, width: usize) -> Vec<Vec<Span<'static>>> {
    let context_tokens = app
        .usage
        .last_usage
        .map(|usage| usage.prompt_tokens)
        .unwrap_or(0);
    let context = context_usage_label(context_tokens, app.config.llm.context_window);
    let tokens = app.usage.total_tokens.to_string();
    let terminal = terminal_status_label(app).trim().to_owned();

    vec![
        mini_box_top("PROJECT", width),
        metric_row("CWD", &app.current_dir, width),
        metric_row("CONTEXT", &context, width),
        metric_row("TOKENS", &tokens, width),
        metric_row("TERMINAL", &terminal, width),
        mini_box_bottom(width),
    ]
}

fn side_by_side_boxes(
    left_rows: Vec<Vec<Span<'static>>>,
    left_width: usize,
    right_rows: Vec<Vec<Span<'static>>>,
    right_width: usize,
    gutter: usize,
) -> Vec<Vec<Span<'static>>> {
    let row_count = left_rows.len().max(right_rows.len());
    (0..row_count)
        .map(|row| {
            let mut spans = pad_spans(
                left_rows.get(row).cloned().unwrap_or_else(Vec::new),
                left_width,
            );
            spans.push(Span::raw(" ".repeat(gutter)));
            spans.extend(pad_spans(
                right_rows.get(row).cloned().unwrap_or_else(Vec::new),
                right_width,
            ));
            spans
        })
        .collect()
}

fn mini_box_top(title: &str, width: usize) -> Vec<Span<'static>> {
    if width < 4 {
        return vec![Span::styled(
            "─".repeat(width),
            Style::default().fg(PANEL_DIM_COLOR),
        )];
    }

    let title = format!(" {title} ");
    let title_width = title.width();
    if width < title_width + 4 {
        return vec![Span::styled(
            "─".repeat(width),
            Style::default().fg(PANEL_DIM_COLOR),
        )];
    }

    vec![
        Span::styled("╭─", Style::default().fg(PANEL_DIM_COLOR)),
        Span::styled(
            title,
            Style::default()
                .fg(ACCENT_COLOR)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            "─".repeat(width - title_width - 3),
            Style::default().fg(PANEL_DIM_COLOR),
        ),
        Span::styled("╮", Style::default().fg(PANEL_DIM_COLOR)),
    ]
}

fn mini_box_body(spans: Vec<Span<'static>>, width: usize) -> Vec<Span<'static>> {
    if width < 4 {
        return pad_spans(spans, width);
    }
    let mut row = vec![Span::styled("│ ", Style::default().fg(PANEL_DIM_COLOR))];
    row.extend(pad_spans(spans, width.saturating_sub(4)));
    row.push(Span::styled(" │", Style::default().fg(PANEL_DIM_COLOR)));
    row
}

fn mini_box_bottom(width: usize) -> Vec<Span<'static>> {
    if width < 2 {
        return vec![Span::styled(
            "─".repeat(width),
            Style::default().fg(PANEL_DIM_COLOR),
        )];
    }
    vec![
        Span::styled("╰", Style::default().fg(PANEL_DIM_COLOR)),
        Span::styled("─".repeat(width - 2), Style::default().fg(PANEL_DIM_COLOR)),
        Span::styled("╯", Style::default().fg(PANEL_DIM_COLOR)),
    ]
}

fn metric_row(label: &str, value: &str, width: usize) -> Vec<Span<'static>> {
    let content_width = width.saturating_sub(4);
    let label_width = label.width();
    let value_limit = content_width.saturating_sub(label_width + 2).max(1);
    let value = truncate_end_to_width(value, value_limit);
    let spacer = content_width.saturating_sub(label_width + value.width());

    mini_box_body(
        vec![
            Span::styled(label.to_owned(), Style::default().fg(MUTED_TEXT_COLOR)),
            Span::raw(" ".repeat(spacer)),
            Span::styled(
                value,
                Style::default().fg(TEXT_COLOR).add_modifier(Modifier::BOLD),
            ),
        ],
        width,
    )
}

fn pad_spans(mut spans: Vec<Span<'static>>, width: usize) -> Vec<Span<'static>> {
    let current_width: usize = spans.iter().map(|s| s.width()).sum();
    if current_width < width {
        spans.push(Span::raw(" ".repeat(width - current_width)));
    } else if current_width > width {
        let mut truncated = Vec::new();
        let mut w = 0;
        for span in spans {
            let span_w = span.width();
            if w + span_w <= width {
                truncated.push(span);
                w += span_w;
            } else {
                let diff = width - w;
                let mut content = String::new();
                let mut cw = 0;
                for c in span.content.chars() {
                    let char_w = c.width().unwrap_or(0);
                    if cw + char_w > diff {
                        break;
                    }
                    content.push(c);
                    cw += char_w;
                }
                if cw < diff {
                    content.push_str(&" ".repeat(diff - cw));
                }
                truncated.push(Span::styled(content, span.style));
                break;
            }
        }
        spans = truncated;
    }
    spans
}

fn center_spans(mut spans: Vec<Span<'static>>, width: usize) -> Vec<Span<'static>> {
    let current_width: usize = spans.iter().map(|s| s.width()).sum();
    if current_width >= width {
        return pad_spans(spans, width);
    }

    let left_padding = (width - current_width) / 2;
    let right_padding = width - current_width - left_padding;
    let mut centered = Vec::with_capacity(spans.len() + 2);
    centered.push(Span::raw(" ".repeat(left_padding)));
    centered.append(&mut spans);
    centered.push(Span::raw(" ".repeat(right_padding)));
    centered
}

fn approval_lines(app: &App, width: u16) -> Vec<Line<'static>> {
    let Some(approval) = &app.approval else {
        return Vec::new();
    };

    let mut lines = vec![box_top("SECURITY CHECK / APPROVAL REQUIRED", width)];
    lines.push(box_body_styled("", width, Style::default()));
    lines.push(box_body_styled(
        "COMMAND",
        width,
        Style::default()
            .fg(ACCENT_COLOR)
            .add_modifier(Modifier::BOLD),
    ));
    lines.extend(
        wrap_text(&approval.request.command, width.saturating_sub(6))
            .into_iter()
            .map(|row| {
                box_body_styled(
                    &format!("$ {row}"),
                    width,
                    Style::default().fg(TEXT_COLOR).add_modifier(Modifier::BOLD),
                )
            }),
    );
    lines.push(box_body_styled("", width, Style::default()));
    lines.push(box_body_styled(
        "REASON",
        width,
        Style::default()
            .fg(ACCENT_COLOR)
            .add_modifier(Modifier::BOLD),
    ));
    lines.extend(
        wrap_text(&approval.request.explanation, width.saturating_sub(6))
            .into_iter()
            .map(|row| box_body_styled(&row, width, Style::default().fg(MUTED_TEXT_COLOR))),
    );
    lines.push(box_body_styled("", width, Style::default()));

    for choice in [
        ApprovalChoice::Yes,
        ApprovalChoice::Always,
        ApprovalChoice::No,
    ] {
        let label = match choice {
            ApprovalChoice::Yes => "allow once",
            ApprovalChoice::Always => approval.always_label(),
            ApprovalChoice::No => "deny",
        };
        let selected = approval.selected == choice;
        let style = if selected {
            Style::default()
                .fg(ACCENT_COLOR)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(SOFT_TEXT_COLOR)
        };
        lines.push(box_body_styled(
            &format!("{} {label}", if selected { "❯" } else { " " }),
            width,
            style,
        ));
        if choice == ApprovalChoice::No {
            let feedback = if approval.feedback.value.is_empty() {
                "feedback: ".to_owned()
            } else {
                format!("feedback: {}", approval.feedback.value)
            };
            let style = if approval.focus == ApprovalFocus::Feedback {
                Style::default().fg(TEXT_COLOR)
            } else {
                Style::default().fg(MUTED_TEXT_COLOR)
            };
            lines.push(box_body_styled(&format!("  {feedback}"), width, style));
        }
    }

    lines.push(box_body_styled("", width, Style::default()));
    lines.push(box_bottom(width));
    lines
}

fn approval_feedback_cursor_x(app: &App, width: u16) -> u16 {
    let Some(approval) = &app.approval else {
        return 0;
    };
    let prefix_width = "  feedback: ".width();
    let value_width = approval.feedback.value[..approval.feedback.cursor].width();
    (prefix_width + value_width + 2).min(width.saturating_sub(1) as usize) as u16
}

fn box_body_styled(text: &str, width: u16, style: Style) -> Line<'static> {
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

fn transcript_lines(app: &App, width: u16) -> Vec<Line<'static>> {
    app.messages
        .iter()
        .flat_map(|message| {
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
            let markdown_lines =
                markdown::render_markdown(&message.content, width.saturating_sub(2));
            for mut line in markdown_lines {
                let mut spans = vec![Span::raw("  ")];
                spans.append(&mut line.spans);
                lines.push(Line::from(spans));
            }
            lines
        })
        .collect()
}

fn processing_line(elapsed: std::time::Duration) -> Line<'static> {
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

fn notice_line(message: &str) -> Line<'static> {
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

fn turn_duration_line(duration: std::time::Duration) -> Line<'static> {
    Line::from(vec![
        Span::raw("  "),
        Span::styled("Worked ", Style::default().fg(MUTED_TEXT_COLOR)),
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

fn tool_output_preview(output: &str) -> String {
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

fn wrap_text(text: &str, width: u16) -> Vec<String> {
    text.split('\n')
        .flat_map(|line| wrap_line(line, width.max(1) as usize))
        .collect()
}

fn input_rows(value: &str, width: u16) -> Vec<String> {
    wrap_text(value, input_content_width(width) as u16)
        .into_iter()
        .enumerate()
        .map(|(index, row)| format!("{}{}", if index == 0 { "❯ " } else { "  " }, row))
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

fn input_cursor_position(app: &App, width: u16) -> (u16, u16) {
    let content_width = input_content_width(width);
    let mut row = 0;
    let mut column = 0;

    for char in app.input.value[..app.input.cursor].chars() {
        if char == '\n' {
            row += 1;
            column = 0;
            continue;
        }

        let char_width = char.width().unwrap_or(0);
        if column + char_width > content_width && column > 0 {
            row += 1;
            column = 0;
        }
        column += char_width;
    }

    (column as u16 + 4, row as u16)
}

fn input_content_width(width: u16) -> usize {
    width.saturating_sub(6).max(1) as usize
}

fn slash_command_lines(app: &App, _width: u16) -> Vec<Line<'static>> {
    let matches = app.slash_command_matches();
    if matches.is_empty() {
        return vec![Line::from(vec![
            Span::raw("  "),
            Span::styled(
                "No matching slash command",
                Style::default().fg(MUTED_TEXT_COLOR),
            ),
        ])];
    }

    matches
        .iter()
        .enumerate()
        .map(|(index, command)| {
            let selected = index == app.slash_command_selection;
            let style = if selected {
                Style::default()
                    .fg(ACCENT_COLOR)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(MUTED_TEXT_COLOR)
            };
            Line::from(vec![
                Span::raw("  "),
                Span::styled(if selected { "❯ " } else { "  " }, style),
                Span::styled(command.name, style),
                Span::styled("  ", style),
                Span::styled(command.description, style),
            ])
        })
        .collect()
}

fn model_picker_lines(app: &App, _width: u16) -> Vec<Line<'static>> {
    let Some(picker) = &app.model_picker else {
        return Vec::new();
    };

    let title = match picker.stage {
        ModelPickerStage::Provider => "Select Provider",
        ModelPickerStage::Model => "Select Model",
    };
    let help = match picker.stage {
        ModelPickerStage::Provider => {
            "Choose a provider endpoint. Enter continues to model selection; Backspace cancels."
        }
        ModelPickerStage::Model => {
            "Choose a model for the selected provider. Enter switches; Backspace returns."
        }
    };
    let mut lines = vec![
        Line::from(Span::styled(
            title,
            Style::default()
                .fg(ACCENT_COLOR)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(help, Style::default().fg(MUTED_TEXT_COLOR))),
    ];

    match picker.stage {
        ModelPickerStage::Provider => {
            let name_width = app
                .config
                .llm
                .providers
                .iter()
                .map(|provider| provider.name.width())
                .max()
                .unwrap_or(0);
            for (index, provider) in app.config.llm.providers.iter().enumerate() {
                let selected = index == picker.selected_provider;
                let style = if selected {
                    Style::default()
                        .fg(ACCENT_COLOR)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(MUTED_TEXT_COLOR)
                };
                lines.push(Line::from(vec![
                    Span::raw("  "),
                    Span::styled(if selected { "❯ " } else { "  " }, style),
                    Span::styled(pad_to_width(&provider.name, name_width), style),
                    Span::styled("  ", style),
                    Span::styled(
                        provider_summary(app, provider),
                        Style::default().fg(MUTED_TEXT_COLOR),
                    ),
                ]));
            }
        }
        ModelPickerStage::Model => {
            let Some(provider) = app.config.llm.providers.get(picker.selected_provider) else {
                return lines;
            };
            lines.push(Line::from(vec![
                Span::styled("Provider ", Style::default().fg(MUTED_TEXT_COLOR)),
                Span::styled(
                    provider.name.clone(),
                    Style::default().fg(TEXT_COLOR).add_modifier(Modifier::BOLD),
                ),
            ]));

            let name_width = provider
                .models
                .iter()
                .map(|model| model.width())
                .max()
                .unwrap_or(0);
            for (model_index, model) in provider.models.iter().enumerate() {
                let selected = model_index == picker.selected_model;
                let current =
                    provider.name == app.config.llm.provider && model == &app.config.llm.model;
                let style = if selected {
                    Style::default()
                        .fg(ACCENT_COLOR)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(MUTED_TEXT_COLOR)
                };
                let current_marker = if current { " current" } else { "" };
                lines.push(Line::from(vec![
                    Span::raw("  "),
                    Span::styled(if selected { "❯ " } else { "  " }, style),
                    Span::styled(pad_to_width(model, name_width), style),
                    Span::styled("  ", style),
                    Span::styled(
                        model_summary(app, &provider.name, model),
                        Style::default().fg(MUTED_TEXT_COLOR),
                    ),
                    Span::styled(current_marker, Style::default().fg(BORDER_BRIGHT_COLOR)),
                ]));
            }
        }
    }

    lines
}

fn provider_summary(app: &App, provider: &crate::config::LlmProviderConfig) -> String {
    app.config
        .model_catalog
        .providers
        .get(&provider.name)
        .map(|entry| entry.description.as_str())
        .filter(|description| !description.is_empty())
        .unwrap_or(&provider.base_url)
        .to_owned()
}

fn model_summary(app: &App, provider: &str, model: &str) -> String {
    let Some(entry) = app
        .config
        .model_catalog
        .models
        .get(provider)
        .and_then(|models| models.get(model))
    else {
        return "No model metadata".to_owned();
    };

    let mut parts = Vec::new();
    if !entry.positioning.is_empty() {
        parts.push(entry.positioning.clone());
    }
    if !entry.context.is_empty() {
        parts.push(format!("ctx {}", entry.context));
    }
    if !entry.max_tokens.is_empty() {
        parts.push(format!("max {}", entry.max_tokens));
    }
    if !entry.price.is_empty() {
        parts.push(entry.price.clone());
    } else {
        let mut price = Vec::new();
        if !entry.input.is_empty() {
            price.push(format!("input {}", entry.input));
        }
        if !entry.output.is_empty() {
            price.push(format!("output {}", entry.output));
        }
        if !entry.cache_read.is_empty() {
            price.push(format!("cache read {}", entry.cache_read));
        }
        if !entry.cache_write.is_empty() {
            price.push(format!("cache write {}", entry.cache_write));
        }
        if !price.is_empty() {
            parts.push(price.join(", "));
        }
    }

    if parts.is_empty() {
        "No model metadata".to_owned()
    } else {
        parts.join(" | ")
    }
}

fn pad_to_width(text: &str, width: usize) -> String {
    let padding = width.saturating_sub(text.width());
    format!("{text}{}", " ".repeat(padding))
}

fn truncate_end_to_width(text: &str, width: usize) -> String {
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

fn truncate_start_to_width(text: &str, width: usize) -> String {
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

fn age_label(seconds: u64) -> String {
    if seconds < 60 {
        format!("{seconds}s ago")
    } else if seconds < 60 * 60 {
        format!("{}m ago", seconds / 60)
    } else if seconds < 60 * 60 * 24 {
        format!("{}h ago", seconds / (60 * 60))
    } else {
        format!("{}d ago", seconds / (60 * 60 * 24))
    }
}

fn duration_label(seconds: u64) -> String {
    let days = seconds / (60 * 60 * 24);
    let hours = (seconds / (60 * 60)) % 24;
    let minutes = (seconds / 60) % 60;
    let seconds = seconds % 60;

    if days > 0 {
        format!("{days}d {hours}h {minutes}m {seconds}s")
    } else if hours > 0 {
        format!("{hours}h {minutes}m {seconds}s")
    } else if minutes > 0 {
        format!("{minutes}m {seconds}s")
    } else {
        format!("{seconds}s")
    }
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default()
}

fn info_line(app: &App, width: u16) -> Line<'static> {
    let left_width = format!("{} · {}", app.config.llm.model, app.config.llm.provider).width();
    let cwd_limit = (width as usize).saturating_sub(left_width + 2);
    let cwd = if cwd_limit == 0 {
        String::new()
    } else {
        truncate_start_to_width(&app.current_dir, cwd_limit)
    };
    let spacer = (width as usize).saturating_sub(left_width + cwd.width());

    Line::from(vec![
        Span::styled(
            app.config.llm.model.clone(),
            Style::default().fg(TEXT_COLOR).add_modifier(Modifier::BOLD),
        ),
        Span::styled(" · ", Style::default().fg(MUTED_TEXT_COLOR)),
        Span::styled(
            app.config.llm.provider.clone(),
            Style::default()
                .fg(ACCENT_COLOR)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" ".repeat(spacer)),
        Span::styled(cwd, Style::default().fg(MUTED_TEXT_COLOR)),
    ])
}

fn context_line(app: &App, _width: u16) -> Line<'static> {
    let cache_percent = app.usage.cache_percent();
    let context_tokens = app
        .usage
        .last_usage
        .map(|usage| usage.prompt_tokens)
        .unwrap_or(0);

    let mut spans = vec![Span::styled(
        "Context ",
        Style::default().fg(MUTED_TEXT_COLOR),
    )];
    spans.extend(progress_bar_spans(
        context_bar_percent(context_tokens, app.config.llm.context_window),
        CONTEXT_BAR_WIDTH,
    ));
    spans.push(Span::styled(
        context_usage_label(context_tokens, app.config.llm.context_window),
        Style::default().fg(TEXT_COLOR).add_modifier(Modifier::BOLD),
    ));

    if let Some(usage) = app.usage.last_usage {
        spans.push(Span::raw("   "));
        spans.push(Span::styled(
            "Input Tokens ",
            Style::default().fg(MUTED_TEXT_COLOR),
        ));
        spans.push(Span::styled(
            usage.prompt_tokens.to_string(),
            Style::default()
                .fg(BORDER_BRIGHT_COLOR)
                .add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::raw(" "));
        spans.push(Span::styled(
            cached_suffix(cache_percent),
            Style::default().fg(ACCENT_COLOR),
        ));
        spans.push(Span::raw("   "));
        spans.push(Span::styled(
            "Output Tokens ",
            Style::default().fg(MUTED_TEXT_COLOR),
        ));
        spans.push(Span::styled(
            usage.completion_tokens.to_string(),
            Style::default().fg(TEXT_COLOR).add_modifier(Modifier::BOLD),
        ));
    }

    Line::from(spans)
}

fn permission_line(app: &App) -> Option<Line<'static>> {
    if app.conversation_permissions.edit_always_allowed {
        Some(Line::from(vec![
            Span::styled(
                "Permissions ",
                Style::default()
                    .fg(BORDER_BRIGHT_COLOR)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                "edit auto-approved for this conversation",
                Style::default().fg(TEXT_COLOR).add_modifier(Modifier::BOLD),
            ),
            Span::raw("  "),
            Span::styled("[CTRL+K] cancel", Style::default().fg(MUTED_TEXT_COLOR)),
        ]))
    } else {
        None
    }
}

fn cached_suffix(percent: Option<u8>) -> String {
    percent
        .map(|percent| format!("({percent}% cached)"))
        .unwrap_or_else(|| "(— cached)".to_owned())
}

const BG_COLOR: Color = Color::Rgb(2, 6, 23);
const BORDER_COLOR: Color = Color::Rgb(30, 64, 175);
const PANEL_DIM_COLOR: Color = Color::Rgb(30, 64, 175);
const BORDER_BRIGHT_COLOR: Color = Color::Rgb(96, 165, 250);
const ACCENT_COLOR: Color = Color::Rgb(34, 211, 238);
const TEXT_COLOR: Color = Color::Rgb(248, 250, 252);
const SOFT_TEXT_COLOR: Color = Color::Rgb(226, 232, 240);
const MUTED_TEXT_COLOR: Color = Color::Rgb(148, 163, 184);
const CONTEXT_BAR_WIDTH: usize = 24;

fn context_usage_label(tokens: u64, context_window: Option<u64>) -> String {
    let Some(window) = context_window.filter(|window| *window > 0) else {
        return "—".to_owned();
    };

    format!(
        "{:.1}% of {}",
        context_percent(tokens, window),
        compact_context_window(window)
    )
}

fn context_percent(tokens: u64, context_window: u64) -> f64 {
    ((tokens as f64 * 100.0) / context_window as f64).min(100.0)
}

fn context_bar_percent(tokens: u64, context_window: Option<u64>) -> u8 {
    let Some(window) = context_window.filter(|window| *window > 0) else {
        return 0;
    };

    ((tokens.saturating_mul(100) / window).min(100)) as u8
}

fn progress_bar_spans(percent: u8, width: usize) -> Vec<Span<'static>> {
    let filled = width * percent.min(100) as usize / 100;
    vec![
        Span::styled("[".to_owned(), Style::default().fg(ACCENT_COLOR)),
        Span::styled("█".repeat(filled), Style::default().fg(ACCENT_COLOR)),
        Span::styled(
            "░".repeat(width - filled),
            Style::default().fg(ACCENT_COLOR),
        ),
        Span::styled("] ".to_owned(), Style::default().fg(ACCENT_COLOR)),
    ]
}

fn compact_context_window(tokens: u64) -> String {
    if tokens >= 1_000_000 {
        format!("{}M", tokens / 1_000_000)
    } else if tokens >= 1_000 {
        format!("{}K", tokens / 1_000)
    } else {
        tokens.to_string()
    }
}

fn dashboard_top(width: u16) -> Line<'static> {
    let title = vec![
        Span::styled(
            " GLINT ",
            Style::default().fg(TEXT_COLOR).add_modifier(Modifier::BOLD),
        ),
        Span::styled("v0.1.0 ", Style::default().fg(BORDER_BRIGHT_COLOR)),
    ];
    box_top_spans(title, width)
}

fn box_top(title: &str, width: u16) -> Line<'static> {
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

fn box_top_spans(title: Vec<Span<'static>>, width: u16) -> Line<'static> {
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

fn box_input_body(text: &str, width: u16) -> Line<'static> {
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

fn box_bottom(width: u16) -> Line<'static> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_labels_show_cached_percentage() {
        assert_eq!(cached_suffix(Some(46)), "(46% cached)");
        assert_eq!(cached_suffix(None), "(— cached)");
        assert_eq!(context_usage_label(8_000, Some(1_000_000)), "0.8% of 1M");
        assert_eq!(context_usage_label(1_280, Some(256_000)), "0.5% of 256K");
        assert_eq!(context_usage_label(37_500, Some(100_000)), "37.5% of 100K");
        assert_eq!(context_usage_label(1_000, Some(65_536)), "1.5% of 65K");
        assert_eq!(context_usage_label(1, None), "—");
        assert_eq!(context_bar_percent(37_500, Some(100_000)), 37);
        assert_eq!(context_bar_percent(1, None), 0);
    }

    #[test]
    fn truncates_paths_from_the_start() {
        assert_eq!(
            truncate_start_to_width("~/projects/glint", 16),
            "~/projects/glint"
        );
        assert_eq!(
            truncate_start_to_width("~/projects/glint", 10),
            "...s/glint"
        );
    }

    #[test]
    fn previews_tool_output_with_omitted_line_count() {
        assert_eq!(tool_output_preview("one\ntwo\nthree"), "one\ntwo\nthree");
        assert_eq!(
            tool_output_preview("one\ntwo\nthree\nfour\nfive\nsix\nseven\neight"),
            "one\ntwo\nthree\n...+5 lines omitted"
        );
    }

    #[test]
    fn terminal_content_width_reserves_tab_column_when_roomy() {
        assert_eq!(terminal_content_width(120), 104);
        assert_eq!(terminal_content_width(30), 26);
    }

    #[test]
    fn terminal_tab_window_tracks_active_tab() {
        assert_eq!(terminal_tab_window_start(0, 3, 6), 0);
        assert_eq!(terminal_tab_window_start(6, 10, 4), 4);
    }
}
