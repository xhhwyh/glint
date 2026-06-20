use ratatui::{
    style::{Modifier, Style},
    text::{Line, Span},
};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::app::App;

use super::{
    format::context_usage_label,
    layout::{box_bottom, box_top_spans, truncate_end_to_width},
    star,
    terminal::terminal_status_label,
    theme::*,
};

const WELCOME_TEXT: &str = "Catch the glint. Shape the work.";

pub(super) fn idle_panel_lines(app: &App, width: u16) -> Vec<Line<'static>> {
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
                        .fg(KEY_HINT_COLOR)
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
                        .fg(KEY_HINT_COLOR)
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
                        .fg(KEY_HINT_COLOR)
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
                        .fg(KEY_HINT_COLOR)
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
