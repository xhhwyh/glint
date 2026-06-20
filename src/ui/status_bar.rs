use ratatui::{
    style::{Modifier, Style},
    text::{Line, Span},
};
use unicode_width::UnicodeWidthStr;

use crate::app::App;

use super::{
    format::{cached_suffix, context_bar_percent, context_usage_label, progress_bar_spans},
    layout::truncate_start_to_width,
    theme::*,
};

pub(super) fn info_line(app: &App, width: u16) -> Line<'static> {
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

pub(super) fn context_line(app: &App, _width: u16) -> Line<'static> {
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

pub(super) fn permission_line(app: &App) -> Option<Line<'static>> {
    if app.edit_always_allowed() {
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
