use ratatui::{
    Frame,
    layout::Position,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::{app::App, markdown, message::Role};

struct Document {
    lines: Vec<Line<'static>>,
    cursor_x: u16,
    cursor_y: u16,
}

pub fn render(frame: &mut Frame, app: &App) {
    let width = frame.area().width.max(1);
    let document = document(app, width);
    let max_scroll = document
        .lines
        .len()
        .saturating_sub(frame.area().height as usize) as u16;
    let scroll = max_scroll.saturating_sub(app.scroll);

    frame.render_widget(
        Paragraph::new(document.lines).scroll((scroll, 0)),
        frame.area(),
    );

    if document.cursor_y >= scroll && document.cursor_y < scroll + frame.area().height {
        frame.set_cursor_position(Position::new(
            document.cursor_x.min(width.saturating_sub(1)),
            document.cursor_y - scroll,
        ));
    }
}

fn document(app: &App, width: u16) -> Document {
    let mut lines = vec![
        Line::from(box_top("idle", width)),
        Line::from(box_empty(width)),
        Line::from(box_bottom(width)),
    ];

    lines.extend(transcript_lines(app, width));
    if !app.messages.is_empty() {
        lines.push(Line::from(""));
    }

    let input_y = lines.len() as u16;
    lines.push(Line::from(box_top("input", width)));
    lines.extend(
        input_rows(&app.input.value, width)
            .into_iter()
            .map(|row| Line::from(box_body(&row, width))),
    );
    lines.push(Line::from(box_bottom(width)));
    lines.push(Line::from(info_line(app)));

    let (cursor_x, cursor_row) = input_cursor_position(app, width);
    Document {
        lines,
        cursor_x,
        cursor_y: input_y + cursor_row + 1,
    }
}

fn transcript_lines(app: &App, width: u16) -> Vec<Line<'static>> {
    app.messages
        .iter()
        .flat_map(|message| {
            let role = match message.role {
                Role::User => styled_line("You", Color::Cyan),
                Role::Assistant => styled_line("Assistant", Color::Green),
            };

            let mut lines = vec![Line::from(""), role];
            if message.role == Role::Assistant && message.content.is_empty() {
                lines.push(Line::from(Span::styled(
                    "thinking",
                    Style::default().fg(Color::DarkGray),
                )));
            } else {
                lines.extend(markdown::render_markdown(&message.content, width));
            }
            lines
        })
        .collect()
}

fn styled_line(text: &'static str, color: Color) -> Line<'static> {
    Line::from(Span::styled(
        text,
        Style::default().fg(color).add_modifier(Modifier::BOLD),
    ))
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
        .map(|(index, row)| format!("{}{}", if index == 0 { "> " } else { "  " }, row))
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

fn info_line(app: &App) -> String {
    format!(
        "model: {} · cwd: {}",
        app.config.llm.model,
        std::env::current_dir()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|_| "?".to_owned())
    )
}

fn box_top(title: &str, width: u16) -> String {
    let width = width as usize;
    if width < title.len() + 4 {
        return "─".repeat(width);
    }

    format!("┌ {title} {}┐", "─".repeat(width - title.len() - 4))
}

fn box_empty(width: u16) -> String {
    let width = width as usize;
    if width < 2 {
        return "│".repeat(width);
    }

    format!("│{}│", " ".repeat(width - 2))
}

fn box_body(text: &str, width: u16) -> String {
    let width = width as usize;
    if width < 4 {
        return text.to_owned();
    }

    let padding = width.saturating_sub(text.width() + 4);
    format!("│ {text}{} │", " ".repeat(padding))
}

fn box_bottom(width: u16) -> String {
    let width = width as usize;
    if width < 2 {
        return "─".repeat(width);
    }

    format!("└{}┘", "─".repeat(width - 2))
}
