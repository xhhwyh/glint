use std::time::{SystemTime, UNIX_EPOCH};

use ratatui::{style::Style, text::Span};

use super::theme::*;

pub(super) fn age_label(seconds: u64) -> String {
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

pub(super) fn duration_label(seconds: u64) -> String {
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

pub(super) fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default()
}

pub(super) fn cached_suffix(percent: Option<u8>) -> String {
    percent
        .map(|percent| format!("({percent}% cached)"))
        .unwrap_or_else(|| "(— cached)".to_owned())
}

pub(super) fn unit_suffix(unit: &str) -> String {
    let normalized_unit = unit.to_ascii_uppercase();
    match normalized_unit.as_str() {
        "RMB" | "CNY" => "￥".to_owned(),
        "USD" => "$".to_owned(),
        "" => String::new(),
        _ => unit.to_owned(),
    }
}

pub(super) fn parse_price_number(value: &str) -> Option<f64> {
    let mut number = String::new();
    let mut started = false;
    for char in value.chars() {
        if char.is_ascii_digit() || char == '.' {
            number.push(char);
            started = true;
        } else if started {
            break;
        }
    }
    (!number.is_empty()).then(|| number.parse().ok()).flatten()
}

pub(super) fn context_usage_label(tokens: u64, context_window: Option<u64>) -> String {
    let Some(window) = context_window.filter(|window| *window > 0) else {
        return "—".to_owned();
    };

    format!(
        "{:.1}% of {}",
        context_percent(tokens, window),
        compact_context_window(window)
    )
}

pub(super) fn context_percent(tokens: u64, context_window: u64) -> f64 {
    ((tokens as f64 * 100.0) / context_window as f64).min(100.0)
}

pub(super) fn context_bar_percent(tokens: u64, context_window: Option<u64>) -> u8 {
    let Some(window) = context_window.filter(|window| *window > 0) else {
        return 0;
    };

    ((tokens.saturating_mul(100) / window).min(100)) as u8
}

pub(super) fn progress_bar_spans(percent: u8, width: usize) -> Vec<Span<'static>> {
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

pub(super) fn compact_context_window(tokens: u64) -> String {
    if tokens >= 1_000_000 {
        format!("{}M", tokens / 1_000_000)
    } else if tokens >= 1_000 {
        format!("{}K", tokens / 1_000)
    } else {
        tokens.to_string()
    }
}
