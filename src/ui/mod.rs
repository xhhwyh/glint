use ratatui::{
    Frame,
    layout::Position,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::{
    app::{App, ModelPickerStage},
    approval::{ApprovalChoice, ApprovalFocus},
    message::Role,
};

mod markdown;
mod star;

const WELCOME_TEXT: &str = "Catch the glint. Shape the work.";

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

    let input_y = lines.len() as u16;
    lines.push(box_top("INPUT", width));
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
        lines.push(permission_line(app));
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

fn idle_panel_lines(_app: &App, width: u16) -> Vec<Line<'static>> {
    const ICON_PADDING_X: usize = 4;
    const WELCOME_PADDING_X: usize = 2;
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
    let left_min_width =
        (star::STAR_WIDTH + ICON_PADDING_X * 2).max(WELCOME_TEXT.width() + WELCOME_PADDING_X * 2);
    let has_split = inner_width >= left_min_width + GUTTER_WIDTH + RIGHT_MIN_WIDTH + 2;
    let left_width = if has_split {
        left_min_width
    } else {
        inner_width
    };
    let right_width = inner_width.saturating_sub(left_width + GUTTER_WIDTH);
    let left_rows = idle_left_rows();

    let mut lines = vec![dashboard_top(width as u16)];
    for (row, left_spans) in left_rows.into_iter().enumerate() {
        let mut row_spans = vec![Span::styled("┃", Style::default().fg(Color::Blue))];

        if has_split && right_width > 0 {
            let right_spans = idle_right_spans(row);
            row_spans.extend(center_spans(left_spans, left_width));
            row_spans.push(Span::styled(" ┃ ", Style::default().fg(Color::Blue)));
            row_spans.extend(pad_spans(right_spans, right_width));
        } else {
            row_spans.extend(center_spans(left_spans, inner_width));
        }

        row_spans.push(Span::styled("┃", Style::default().fg(Color::Blue)));
        lines.push(Line::from(row_spans));
    }
    lines.push(box_bottom(width as u16));
    lines
}

fn idle_left_rows() -> Vec<Vec<Span<'static>>> {
    let mut rows = star::glint_star_rows();
    rows.push(vec![Span::styled(
        WELCOME_TEXT,
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
    )]);
    rows
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

    let mut lines = vec![box_top("APPROVAL", width)];
    lines.extend(
        wrap_text(&approval.request.command, width.saturating_sub(6))
            .into_iter()
            .map(|row| {
                box_body_styled(
                    &format!("$ {row}"),
                    width,
                    Style::default()
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD),
                )
            }),
    );
    lines.extend(
        wrap_text(&approval.request.explanation, width.saturating_sub(6))
            .into_iter()
            .map(|row| box_body_styled(&row, width, Style::default().fg(Color::DarkGray))),
    );
    lines.push(box_body_styled("", width, Style::default()));

    for choice in [
        ApprovalChoice::Yes,
        ApprovalChoice::Always,
        ApprovalChoice::No,
    ] {
        let label = match choice {
            ApprovalChoice::Yes => "yes",
            ApprovalChoice::Always => approval.always_label(),
            ApprovalChoice::No => "no",
        };
        let selected = approval.selected == choice;
        let style = if selected {
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::White)
        };
        lines.push(box_body_styled(
            &format!("{} {label}", if selected { "›" } else { " " }),
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
                Style::default().fg(Color::White)
            } else {
                Style::default().fg(Color::DarkGray)
            };
            lines.push(box_body_styled(&format!("  {feedback}"), width, style));
        }
    }

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
        Span::styled("┃ ", Style::default().fg(Color::Blue)),
        Span::styled(text.to_owned(), style),
        Span::raw(" ".repeat(padding)),
        Span::styled(" ┃", Style::default().fg(Color::Blue)),
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

            let mut lines = vec![Line::from("")];
            if message.role == Role::Assistant && message.content.is_empty() {
                let activity = app
                    .agent_activity
                    .clone()
                    .unwrap_or_else(|| "Processing...".to_owned());
                lines.push(Line::from(vec![
                    Span::styled("  ", Style::default()),
                    Span::styled(
                        activity,
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

fn user_message_lines(message: &crate::message::Message, width: u16) -> Vec<Line<'static>> {
    let rule = user_rule(width);
    let mut lines = vec![Line::from(""), rule.clone()];
    let mut markdown_lines =
        markdown::render_markdown(&message.content, width.saturating_sub(4));
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
        Style::default().fg(Color::DarkGray),
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
            Style::default().fg(Color::Rgb(147, 197, 253)),
        ),
    ]));

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

    for row in wrap_text(result, width.saturating_sub(6)) {
        lines.push(Line::from(vec![
            Span::raw("    "),
            Span::styled(
                row,
                Style::default()
                    .fg(Color::DarkGray)
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

fn slash_command_lines(app: &App, _width: u16) -> Vec<Line<'static>> {
    let matches = app.slash_command_matches();
    if matches.is_empty() {
        return vec![Line::from(vec![
            Span::raw("  "),
            Span::styled("No matching slash command", Style::default().fg(Color::DarkGray)),
        ])];
    }

    matches
        .iter()
        .enumerate()
        .map(|(index, command)| {
            let selected = index == app.slash_command_selection;
            let style = if selected {
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::DarkGray)
            };
            Line::from(vec![
                Span::raw("  "),
                Span::styled(if selected { "› " } else { "  " }, style),
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
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(help, Style::default().fg(Color::DarkGray))),
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
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::DarkGray)
                };
                lines.push(Line::from(vec![
                    Span::raw("  "),
                    Span::styled(if selected { "› " } else { "  " }, style),
                    Span::styled(pad_to_width(&provider.name, name_width), style),
                    Span::styled("  ", style),
                    Span::styled(
                        provider_summary(app, provider),
                        Style::default().fg(Color::DarkGray),
                    ),
                ]));
            }
        }
        ModelPickerStage::Model => {
            let Some(provider) = app.config.llm.providers.get(picker.selected_provider) else {
                return lines;
            };
            lines.push(Line::from(vec![
                Span::styled("Provider ", Style::default().fg(Color::DarkGray)),
                Span::styled(
                    provider.name.clone(),
                    Style::default()
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD),
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
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::DarkGray)
                };
                let current_marker = if current { " current" } else { "" };
                lines.push(Line::from(vec![
                    Span::raw("  "),
                    Span::styled(if selected { "› " } else { "  " }, style),
                    Span::styled(pad_to_width(model, name_width), style),
                    Span::styled("  ", style),
                    Span::styled(
                        model_summary(app, model),
                        Style::default().fg(Color::DarkGray),
                    ),
                    Span::styled(
                        current_marker,
                        Style::default().fg(Color::Rgb(147, 197, 253)),
                    ),
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
        .map(|entry| entry.summary.as_str())
        .filter(|summary| !summary.is_empty())
        .unwrap_or(&provider.base_url)
        .to_owned()
}

fn model_summary(app: &App, model: &str) -> String {
    let Some(entry) = app.config.model_catalog.models.get(model) else {
        return "No model metadata".to_owned();
    };

    let mut parts = Vec::new();
    if !entry.positioning.is_empty() {
        parts.push(entry.positioning.clone());
    }
    if !entry.context.is_empty() {
        parts.push(format!("ctx {}", entry.context));
    }
    if !entry.price.is_empty() {
        parts.push(entry.price.clone());
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

fn context_line(app: &App, _width: u16) -> Line<'static> {
    let context_percent = app.usage.context_percent(app.config.llm.context_window);
    let cache_percent = app.usage.cache_percent();

    let mut spans = vec![
        metric_label("CONTEXT"),
        Span::styled(
            progress_bar(context_percent.unwrap_or(0), 12),
            Style::default().fg(Color::Rgb(34, 211, 238)),
        ),
        Span::styled(
            percent_text(context_percent),
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        metric_label("CACHE"),
        Span::styled(
            percent_text(cache_percent).trim().to_owned(),
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
    ];

    spans.push(Span::raw("      "));
    spans.push(metric_label("TOKENS"));
    spans.push(Span::styled(
        app.usage.total_tokens.to_string(),
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
    ));

    Line::from(spans)
}

fn permission_line(app: &App) -> Line<'static> {
    if app.conversation_permissions.edit_always_allowed {
        Line::from(vec![
            metric_label("PERMS"),
            Span::styled(
                "Edit auto-approved for this conversation",
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("  "),
            Span::styled("[CTRL+K] cancel", Style::default().fg(Color::DarkGray)),
        ])
    } else {
        Line::from(vec![
            metric_label("PERMS"),
            Span::styled(
                "standard approval policy",
                Style::default().fg(Color::DarkGray),
            ),
        ])
    }
}

fn percent_text(percent: Option<u8>) -> String {
    percent
        .map(|percent| format!(" {percent}%"))
        .unwrap_or_else(|| " —".to_owned())
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
