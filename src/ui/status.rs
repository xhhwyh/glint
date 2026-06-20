use std::collections::BTreeMap;

use ratatui::{
    Frame,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
};

use crate::app::{App, StatusTab, StatusView};

use super::{
    format::{cached_suffix, context_usage_label, now, parse_price_number, unit_suffix},
    layout::{pad_to_width, truncate_end_to_width},
    terminal::terminal_status_label,
    theme::*,
};

pub(super) fn render_status_view(frame: &mut Frame, app: &App, view: &StatusView) {
    let width = frame.area().width.max(1) as usize;
    let height = frame.area().height as usize;
    let footer_rows = 2;
    let mut lines = vec![
        Line::from(Span::styled(
            "Status",
            Style::default()
                .fg(ACCENT_COLOR)
                .add_modifier(Modifier::BOLD),
        )),
        status_tab_line(view.tab),
        Line::from(""),
    ];

    if let Some(error) = &view.error {
        lines.push(Line::from(vec![
            Span::styled(
                "Stats unavailable ",
                Style::default().fg(BORDER_BRIGHT_COLOR),
            ),
            Span::styled(
                truncate_end_to_width(error, width.saturating_sub(18)),
                Style::default().fg(MUTED_TEXT_COLOR),
            ),
        ]));
        lines.push(Line::from(""));
    }

    let available_rows = height.saturating_sub(lines.len() + footer_rows);
    let content = match view.tab {
        StatusTab::General => status_general_lines(app, width),
        StatusTab::Usage => status_usage_lines(app, width),
        StatusTab::Stat => status_stat_lines(view, width, available_rows),
    };
    lines.extend(content.into_iter().take(available_rows));

    while lines.len() + footer_rows < height {
        lines.push(Line::from(""));
    }
    lines.push(Line::from(Span::styled(
        "─".repeat(width),
        Style::default().fg(MUTED_TEXT_COLOR),
    )));
    lines.push(Line::from(vec![
        Span::styled("←/→", Style::default().fg(KEY_HINT_COLOR)),
        Span::styled(" tab  ", Style::default().fg(MUTED_TEXT_COLOR)),
        Span::styled("Tab", Style::default().fg(KEY_HINT_COLOR)),
        Span::styled(" next  ", Style::default().fg(MUTED_TEXT_COLOR)),
        Span::styled("Esc", Style::default().fg(KEY_HINT_COLOR)),
        Span::styled(" exit", Style::default().fg(MUTED_TEXT_COLOR)),
    ]));

    frame.render_widget(
        Paragraph::new(lines).style(Style::default().bg(BG_COLOR)),
        frame.area(),
    );
}

fn status_tab_line(selected: StatusTab) -> Line<'static> {
    let tabs = [
        (StatusTab::General, "General"),
        (StatusTab::Usage, "Usage"),
        (StatusTab::Stat, "Stat"),
    ];
    let mut spans = Vec::new();
    for (index, (tab, label)) in tabs.iter().enumerate() {
        if index > 0 {
            spans.push(Span::styled("  ", Style::default().fg(MUTED_TEXT_COLOR)));
        }
        let style = if *tab == selected {
            Style::default()
                .fg(ACCENT_COLOR)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(MUTED_TEXT_COLOR)
        };
        spans.push(Span::styled(format!(" {label} "), style));
    }
    Line::from(spans)
}

fn status_general_lines(app: &App, width: usize) -> Vec<Line<'static>> {
    let provider = app
        .config
        .llm
        .providers
        .iter()
        .find(|provider| provider.name == app.config.llm.provider);
    let terminal_mode = if app.terminal_visible {
        "TerminalRun"
    } else {
        "Bash"
    };
    let terminal_status = terminal_status_label(app).trim().to_owned();
    let edit_permission = if app.edit_always_allowed() {
        "edit auto-approved"
    } else {
        "default"
    };

    vec![
        status_section_line("Runtime"),
        status_kv_line("State", &format!("{:?}", app.status), width),
        status_kv_line("Workspace", &app.current_dir, width),
        status_kv_line("Terminal", &terminal_status, width),
        status_kv_line("Shell tool", terminal_mode, width),
        status_kv_line("Permissions", edit_permission, width),
        Line::from(""),
        status_section_line("Model"),
        status_kv_line("Provider", &app.config.llm.provider, width),
        status_kv_line("Model", &app.config.llm.model, width),
        status_kv_line("Endpoint", &app.config.llm.base_url, width),
        status_kv_line(
            "API key env",
            provider
                .map(|provider| provider.api_key_env.as_str())
                .unwrap_or(""),
            width,
        ),
        status_kv_line(
            "Temperature",
            &format!("{:.2}", app.config.llm.temperature),
            width,
        ),
        status_kv_line(
            "Max tokens",
            &format_number(app.config.llm.max_tokens as u64),
            width,
        ),
        status_kv_line(
            "Context window",
            &app.config
                .llm
                .context_window
                .map(format_number)
                .unwrap_or_else(|| "unknown".to_owned()),
            width,
        ),
        status_kv_line(
            "LSP servers",
            &app.config.lsp.servers.len().to_string(),
            width,
        ),
    ]
}

fn status_usage_lines(app: &App, width: usize) -> Vec<Line<'static>> {
    let usage = app.usage;
    let last_usage = usage.last_usage.unwrap_or_default();
    let billable_prompt = usage
        .total_prompt_tokens
        .saturating_sub(usage.total_cached_prompt_tokens);
    let context_tokens = usage
        .last_usage
        .map(|usage| usage.prompt_tokens)
        .unwrap_or(0);
    let price = current_model_price(app);
    let estimated_cost = estimate_cost(app, &price);
    let cost_label = estimated_cost
        .map(|cost| format_cost(cost, &price.unit))
        .unwrap_or_else(|| "unavailable".to_owned());

    vec![
        status_section_line("Current Conversation"),
        status_kv_line("Total tokens", &format_number(usage.total_tokens), width),
        status_kv_line(
            "Input tokens",
            &format_number(usage.total_prompt_tokens),
            width,
        ),
        status_kv_line(
            "Output tokens",
            &format_number(usage.total_completion_tokens),
            width,
        ),
        status_kv_line(
            "Cached input",
            &format_number(usage.total_cached_prompt_tokens),
            width,
        ),
        status_kv_line("Billable input", &format_number(billable_prompt), width),
        status_kv_line(
            "Last input",
            &format_number(last_usage.prompt_tokens),
            width,
        ),
        status_kv_line(
            "Last output",
            &format_number(last_usage.completion_tokens),
            width,
        ),
        status_kv_line(
            "Last cache",
            &cached_suffix(app.usage.cache_percent()),
            width,
        ),
        status_kv_line(
            "Context",
            &context_usage_label(context_tokens, app.config.llm.context_window),
            width,
        ),
        status_kv_line("Estimated cost", &cost_label, width),
        Line::from(""),
        status_section_line("Current Model Pricing"),
        status_kv_line(
            "Input / 1M",
            &price_rate_label(price.input, &price.unit),
            width,
        ),
        status_kv_line(
            "Output / 1M",
            &price_rate_label(price.output, &price.unit),
            width,
        ),
        status_kv_line(
            "Cache read / 1M",
            &price_rate_label(price.cache_read, &price.unit),
            width,
        ),
        status_kv_line(
            "Cache write / 1M",
            &price_rate_label(price.cache_write, &price.unit),
            width,
        ),
    ]
}

fn status_stat_lines(view: &StatusView, width: usize, available_rows: usize) -> Vec<Line<'static>> {
    let stats = &view.stats;
    let mut lines = vec![
        status_section_line("Workspace Total"),
        status_kv_line("Sessions", &stats.session_count.to_string(), width),
        status_kv_line("Turns", &stats.turn_count.to_string(), width),
        status_kv_line("Total tokens", &format_number(stats.total_tokens), width),
        status_kv_line(
            "Input tokens",
            &format_number(stats.total_prompt_tokens),
            width,
        ),
        status_kv_line(
            "Output tokens",
            &format_number(stats.total_completion_tokens),
            width,
        ),
        status_kv_line(
            "Cached input",
            &format_number(stats.total_cached_prompt_tokens),
            width,
        ),
        Line::from(""),
    ];

    let calendar_rows = usage_calendar_lines(stats, width);
    lines.extend(calendar_rows);
    lines.push(Line::from(""));
    lines.push(status_section_line("Top Models"));
    if stats.model_tokens.is_empty() {
        lines.push(Line::from(Span::styled(
            "  No usage recorded",
            Style::default().fg(MUTED_TEXT_COLOR),
        )));
    } else {
        let remaining = available_rows.saturating_sub(lines.len()).max(1);
        for model in stats.model_tokens.iter().take(remaining.min(6)) {
            lines.push(status_kv_line(
                &model.model,
                &format!("{} tokens", format_number(model.tokens)),
                width,
            ));
        }
    }

    lines
}

fn status_section_line(title: &str) -> Line<'static> {
    Line::from(Span::styled(
        title.to_owned(),
        Style::default()
            .fg(ACCENT_COLOR)
            .add_modifier(Modifier::BOLD),
    ))
}

fn status_kv_line(label: &str, value: &str, width: usize) -> Line<'static> {
    const LABEL_WIDTH: usize = 18;
    let value_limit = width.saturating_sub(LABEL_WIDTH + 3).max(1);
    Line::from(vec![
        Span::raw("  "),
        Span::styled(
            pad_to_width(&truncate_end_to_width(label, LABEL_WIDTH), LABEL_WIDTH),
            Style::default().fg(MUTED_TEXT_COLOR),
        ),
        Span::styled(
            truncate_end_to_width(value, value_limit),
            Style::default().fg(TEXT_COLOR),
        ),
    ])
}

#[derive(Clone, Debug, Default)]
struct ModelPrice {
    unit: String,
    input: Option<f64>,
    output: Option<f64>,
    cache_read: Option<f64>,
    cache_write: Option<f64>,
}

fn current_model_price(app: &App) -> ModelPrice {
    let unit = app
        .config
        .model_catalog
        .providers
        .get(&app.config.llm.provider)
        .map(|provider| provider.unit.clone())
        .unwrap_or_default();
    let Some(entry) = app
        .config
        .model_catalog
        .models
        .get(&app.config.llm.provider)
        .and_then(|models| models.get(&app.config.llm.model))
    else {
        return ModelPrice {
            unit,
            ..ModelPrice::default()
        };
    };

    ModelPrice {
        unit,
        input: parse_price_number(&entry.input),
        output: parse_price_number(&entry.output),
        cache_read: parse_price_number(&entry.cache_read),
        cache_write: parse_price_number(&entry.cache_write),
    }
}

fn estimate_cost(app: &App, price: &ModelPrice) -> Option<f64> {
    let input_rate = price.input?;
    let output_rate = price.output?;
    let cached_rate = price.cache_read.unwrap_or(input_rate);
    let billable_prompt = app
        .usage
        .total_prompt_tokens
        .saturating_sub(app.usage.total_cached_prompt_tokens);
    Some(
        (billable_prompt as f64 * input_rate
            + app.usage.total_cached_prompt_tokens as f64 * cached_rate
            + app.usage.total_completion_tokens as f64 * output_rate)
            / 1_000_000.0,
    )
}

fn price_rate_label(value: Option<f64>, unit: &str) -> String {
    value
        .map(|value| format!("{value:.4}{}", unit_suffix(unit)))
        .unwrap_or_else(|| "unavailable".to_owned())
}

fn format_cost(value: f64, unit: &str) -> String {
    format!("{value:.6}{}", unit_suffix(unit))
}

fn usage_calendar_lines(
    stats: &crate::transcript::WorkspaceUsageStats,
    width: usize,
) -> Vec<Line<'static>> {
    let day_tokens = stats
        .daily_tokens
        .iter()
        .map(|day| (day.day, day.tokens))
        .collect::<BTreeMap<_, _>>();
    let max_tokens = stats
        .daily_tokens
        .iter()
        .map(|day| day.tokens)
        .max()
        .unwrap_or(0);
    let today = (now() / 86_400) as i64;
    let end_week = today - weekday_index(today) as i64;
    let max_weeks = width.saturating_sub(5).checked_div(3).unwrap_or(1).max(1);
    let weeks = max_weeks.min(52);
    let start_week = end_week - (weeks.saturating_sub(1) * 7) as i64;

    let mut lines = Vec::new();
    let mut month_spans = vec![Span::raw("     ")];
    let mut previous_month = None;
    for week in 0..weeks {
        let day = start_week + (week * 7) as i64;
        let (_, month, _) = civil_from_days(day);
        let label = if previous_month != Some(month) {
            month_abbrev(month)
        } else {
            ""
        };
        previous_month = Some(month);
        month_spans.push(Span::styled(
            pad_to_width(label, 3),
            Style::default().fg(MUTED_TEXT_COLOR),
        ));
    }
    lines.push(Line::from(month_spans));

    for row in 0..7 {
        let mut spans = vec![Span::styled(
            format!("{:<5}", weekday_label(row)),
            Style::default().fg(MUTED_TEXT_COLOR),
        )];
        for week in 0..weeks {
            let day = start_week + (week * 7 + row) as i64;
            if day > today {
                spans.push(Span::raw("   "));
                continue;
            }
            let tokens = day_tokens.get(&day).copied().unwrap_or(0);
            spans.push(Span::styled(
                "██ ",
                Style::default().fg(calendar_color(tokens, max_tokens)),
            ));
        }
        lines.push(Line::from(spans));
    }
    lines.push(calendar_legend_line());

    lines
}

fn calendar_color(tokens: u64, max_tokens: u64) -> Color {
    if tokens == 0 || max_tokens == 0 {
        return CALENDAR_EMPTY_COLOR;
    }
    match tokens.saturating_mul(4).div_ceil(max_tokens).clamp(1, 4) {
        1 => CALENDAR_LEVEL_COLORS[0],
        2 => CALENDAR_LEVEL_COLORS[1],
        3 => CALENDAR_LEVEL_COLORS[2],
        _ => CALENDAR_LEVEL_COLORS[3],
    }
}

fn calendar_legend_line() -> Line<'static> {
    let mut spans = vec![
        Span::raw("     "),
        Span::styled("Less ", Style::default().fg(MUTED_TEXT_COLOR)),
        Span::styled("██ ", Style::default().fg(CALENDAR_EMPTY_COLOR)),
    ];
    for color in CALENDAR_LEVEL_COLORS {
        spans.push(Span::styled("██ ", Style::default().fg(color)));
    }
    spans.push(Span::styled("More", Style::default().fg(MUTED_TEXT_COLOR)));
    Line::from(spans)
}

const CALENDAR_EMPTY_COLOR: Color = Color::Rgb(219, 234, 254);
const CALENDAR_LEVEL_COLORS: [Color; 4] = [
    Color::Rgb(191, 219, 254),
    Color::Rgb(147, 197, 253),
    Color::Rgb(96, 165, 250),
    Color::Rgb(59, 130, 246),
];

fn weekday_label(index: usize) -> &'static str {
    match index {
        0 => "Mon",
        1 => "Tue",
        2 => "Wed",
        3 => "Thu",
        4 => "Fri",
        5 => "Sat",
        _ => "Sun",
    }
}

fn weekday_index(day: i64) -> usize {
    (day + 3).rem_euclid(7) as usize
}

fn month_abbrev(month: u32) -> &'static str {
    match month {
        1 => "Jan",
        2 => "Feb",
        3 => "Mar",
        4 => "Apr",
        5 => "May",
        6 => "Jun",
        7 => "Jul",
        8 => "Aug",
        9 => "Sep",
        10 => "Oct",
        11 => "Nov",
        _ => "Dec",
    }
}

fn civil_from_days(day: i64) -> (i32, u32, u32) {
    let day = day + 719_468;
    let era = if day >= 0 { day } else { day - 146_096 } / 146_097;
    let day_of_era = day - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year_day = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_param = (5 * year_day + 2) / 153;
    let day_of_month = year_day - (153 * month_param + 2) / 5 + 1;
    let month = month_param + if month_param < 10 { 3 } else { -9 };
    let year = year_of_era + era * 400 + if month <= 2 { 1 } else { 0 };

    (year as i32, month as u32, day_of_month as u32)
}

fn format_number(value: u64) -> String {
    let mut chars = value.to_string().chars().rev().collect::<Vec<_>>();
    let mut formatted = String::new();
    for index in 0..chars.len() {
        if index > 0 && index % 3 == 0 {
            formatted.push(',');
        }
        if let Some(char) = chars.get(index) {
            formatted.push(*char);
        }
    }
    chars.clear();
    formatted.chars().rev().collect()
}
