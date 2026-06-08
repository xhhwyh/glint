use ratatui::{
    Frame,
    layout::Position,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::{app::App, message::Role};

mod markdown;
mod star;

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
    let mut lines = idle_panel_lines(app, width);

    lines.extend(transcript_lines(app, width));
    if !app.messages.is_empty() {
        lines.push(Line::from(""));
    }

    let input_y = lines.len() as u16;
    lines.push(box_top("INPUT", width));
    lines.extend(
        input_rows(&app.input.value, width)
            .into_iter()
            .map(|row| box_input_body(&row, width)),
    );
    lines.push(box_bottom(width));
    lines.push(info_line(app, width));
    lines.push(context_line(width));

    let (cursor_x, cursor_row) = input_cursor_position(app, width);
    Document {
        lines,
        cursor_x,
        cursor_y: input_y + cursor_row + 1,
    }
}

fn idle_panel_lines(_app: &App, width: u16) -> Vec<Line<'static>> {
    const ICON_PADDING_X: usize = 4;
    const LEFT_WIDTH: usize = star::STAR_WIDTH + ICON_PADDING_X * 2;
    const MIN_SPLIT_WIDTH: usize = LEFT_WIDTH + GUTTER_WIDTH + RIGHT_MIN_WIDTH + 2;
    const RIGHT_MIN_WIDTH: usize = 32;
    const GUTTER_WIDTH: usize = 3;

    let width = width as usize;
    if width < 4 {
        return vec![Line::from(Span::styled(
            "━".repeat(width),
            Style::default().fg(Color::Blue),
        ))];
    }

    let inner_width = width.saturating_sub(2);
    let has_split = inner_width >= MIN_SPLIT_WIDTH;
    let left_width = if has_split { LEFT_WIDTH } else { inner_width };
    let right_width = inner_width.saturating_sub(left_width + GUTTER_WIDTH);
    let star_rows = star::glint_star_rows();

    let mut lines = vec![dashboard_top(width as u16)];
    for (row, left_spans) in star_rows.into_iter().enumerate() {
        let mut row_spans = vec![Span::styled("┃", Style::default().fg(Color::Blue))];

        if has_split && right_width > 0 {
            let right_spans = idle_right_spans(row);
            row_spans.extend(pad_spans(
                padded_icon_spans(left_spans, ICON_PADDING_X),
                left_width,
            ));
            row_spans.push(Span::styled(" ┃ ", Style::default().fg(Color::Blue)));
            row_spans.extend(pad_spans(right_spans, right_width));
        } else {
            row_spans.extend(pad_spans(
                padded_icon_spans(left_spans, ICON_PADDING_X),
                inner_width,
            ));
        }

        row_spans.push(Span::styled("┃", Style::default().fg(Color::Blue)));
        lines.push(Line::from(row_spans));
    }
    lines.push(box_bottom(width as u16));
    lines
}

fn padded_icon_spans(mut spans: Vec<Span<'static>>, padding: usize) -> Vec<Span<'static>> {
    let mut padded = Vec::with_capacity(spans.len() + 2);
    padded.push(Span::raw(" ".repeat(padding)));
    padded.append(&mut spans);
    padded.push(Span::raw(" ".repeat(padding)));
    padded
}

fn idle_right_spans(row: usize) -> Vec<Span<'static>> {
    match row {
        0 => vec![Span::styled(
            " ❖ WORKSPACE",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )],
        2 => vec![
            Span::styled("   [ENTER]", Style::default().fg(Color::Cyan)),
            Span::styled(" Send", Style::default().fg(Color::DarkGray)),
        ],
        3 => vec![
            Span::styled("   [SHIFT+ENTER]", Style::default().fg(Color::Cyan)),
            Span::styled(" Newline", Style::default().fg(Color::DarkGray)),
        ],
        4 => vec![
            Span::styled("   [CTRL+C]", Style::default().fg(Color::Cyan)),
            Span::styled(" Quit", Style::default().fg(Color::DarkGray)),
        ],
        5 => vec![
            Span::styled("   [SCROLL]", Style::default().fg(Color::Cyan)),
            Span::styled(" Scroll", Style::default().fg(Color::DarkGray)),
        ],
        8 => vec![Span::styled(
            " ❖ CAPABILITIES",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )],
        9 => vec![Span::styled(
            "   Reading, Writing, Execution",
            Style::default().fg(Color::DarkGray),
        )],
        _ => vec![],
    }
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

fn transcript_lines(app: &App, width: u16) -> Vec<Line<'static>> {
    app.messages
        .iter()
        .flat_map(|message| {
            let role = match message.role {
                Role::User => Line::from(vec![Span::styled(
                    " YOU ",
                    Style::default()
                        .fg(Color::White)
                        .bg(Color::Blue)
                        .add_modifier(Modifier::BOLD),
                )]),
                Role::Assistant => Line::from(vec![Span::styled(
                    " AGENT ",
                    Style::default()
                        .fg(Color::Black)
                        .bg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                )]),
            };

            let mut lines = vec![Line::from(""), role, Line::from("")];
            if message.role == Role::Assistant && message.content.is_empty() {
                lines.push(Line::from(vec![
                    Span::styled("  ", Style::default()),
                    Span::styled(
                        "Processing...",
                        Style::default()
                            .fg(Color::Cyan)
                            .add_modifier(Modifier::RAPID_BLINK | Modifier::ITALIC),
                    ),
                ]));
            } else {
                let markdown_lines =
                    markdown::render_markdown(&message.content, width.saturating_sub(2));
                for mut line in markdown_lines {
                    let mut spans = vec![Span::raw("  ")];
                    spans.append(&mut line.spans);
                    lines.push(Line::from(spans));
                }
            }
            lines
        })
        .collect()
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
        .map(|(index, row)| format!("{}{}", if index == 0 { "▶ " } else { "  " }, row))
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

fn info_line(app: &App, _width: u16) -> Line<'static> {
    Line::from(vec![
        metric_label("MODEL"),
        Span::styled(
            app.config.llm.model.clone(),
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        metric_label("CWD"),
        Span::styled(app.current_dir.clone(), Style::default().fg(Color::White)),
    ])
}

fn context_line(_width: u16) -> Line<'static> {
    const CONTEXT_PERCENT: u8 = 0;
    const CACHE_PERCENT: u8 = 0;
    const TOKEN_COUNT: u64 = 0;

    let mut spans = vec![
        metric_label("CONTEXT"),
        Span::styled(
            progress_bar(CONTEXT_PERCENT, 12),
            Style::default().fg(Color::Rgb(34, 211, 238)),
        ),
        Span::styled(
            format!(" {CONTEXT_PERCENT}%"),
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        metric_label("CACHE"),
        Span::styled(
            format!("{CACHE_PERCENT}%"),
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
    ];

    spans.push(Span::raw("      "));
    spans.push(metric_label("TOKENS"));
    spans.push(Span::styled(
        TOKEN_COUNT.to_string(),
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
    ));

    Line::from(spans)
}

fn metric_label(text: &'static str) -> Span<'static> {
    const LABEL_WIDTH: usize = 8;

    Span::styled(
        format!("{text:<LABEL_WIDTH$}"),
        Style::default()
            .fg(Color::Rgb(147, 197, 253))
            .add_modifier(Modifier::BOLD),
    )
}

fn progress_bar(percent: u8, width: usize) -> String {
    let filled = width * percent.min(100) as usize / 100;
    format!("[{}{}]", "█".repeat(filled), "░".repeat(width - filled))
}

fn dashboard_top(width: u16) -> Line<'static> {
    let title = vec![
        Span::styled(
            " GLINT ",
            Style::default()
                .fg(Color::Rgb(96, 165, 250))
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled("v0.1.0 ", Style::default().fg(Color::DarkGray)),
    ];
    box_top_spans(title, width)
}

fn box_top(title: &str, width: u16) -> Line<'static> {
    box_top_spans(
        vec![Span::styled(
            format!(" {} ", title.to_uppercase()),
            Style::default()
                .fg(Color::Cyan)
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
            "━".repeat(width),
            Style::default().fg(Color::Blue),
        ));
    }

    let right_len = width.saturating_sub(title_len + 3);
    let mut spans = vec![Span::styled("┏━", Style::default().fg(Color::Blue))];
    spans.extend(title);
    spans.push(Span::styled(
        "━".repeat(right_len),
        Style::default().fg(Color::Blue),
    ));
    spans.push(Span::styled("┓", Style::default().fg(Color::Blue)));
    Line::from(spans)
}

fn box_input_body(text: &str, width: u16) -> Line<'static> {
    let width = width as usize;
    if width < 4 {
        return Line::from(Span::raw(text.to_owned()));
    }

    let text_width = text.width();
    let padding = width.saturating_sub(text_width + 4);
    Line::from(vec![
        Span::styled("┃ ", Style::default().fg(Color::Blue)),
        Span::styled(text.to_owned(), Style::default().fg(Color::White)),
        Span::raw(" ".repeat(padding)),
        Span::styled(" ┃", Style::default().fg(Color::Blue)),
    ])
}

fn box_bottom(width: u16) -> Line<'static> {
    let width = width as usize;
    if width < 2 {
        return Line::from(Span::styled(
            "━".repeat(width),
            Style::default().fg(Color::Blue),
        ));
    }
    Line::from(vec![
        Span::styled("┗", Style::default().fg(Color::Blue)),
        Span::styled("━".repeat(width - 2), Style::default().fg(Color::Blue)),
        Span::styled("┛", Style::default().fg(Color::Blue)),
    ])
}
