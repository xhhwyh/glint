use ratatui::{
    style::{Modifier, Style},
    text::{Line, Span},
};
use unicode_width::UnicodeWidthStr;

use crate::app::{App, ModelPickerStage};

use super::{format::unit_suffix, layout::pad_to_width, theme::*};

pub(super) fn model_picker_lines(app: &App, _width: u16) -> Vec<Line<'static>> {
    let Some(picker) = &app.model_picker else {
        return Vec::new();
    };

    let title = match picker.stage {
        ModelPickerStage::Provider => "Select Provider",
        ModelPickerStage::Model => "Select Model",
    };
    let mut lines = vec![
        Line::from(Span::styled(
            title,
            Style::default()
                .fg(ACCENT_COLOR)
                .add_modifier(Modifier::BOLD),
        )),
        model_picker_help_line(picker.stage),
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

fn model_picker_help_line(stage: ModelPickerStage) -> Line<'static> {
    match stage {
        ModelPickerStage::Provider => Line::from(vec![
            Span::styled(
                "Choose a provider endpoint. ",
                Style::default().fg(MUTED_TEXT_COLOR),
            ),
            Span::styled("Enter", Style::default().fg(KEY_HINT_COLOR)),
            Span::styled(
                " continues to model selection; ",
                Style::default().fg(MUTED_TEXT_COLOR),
            ),
            Span::styled("Backspace", Style::default().fg(KEY_HINT_COLOR)),
            Span::styled(" cancels.", Style::default().fg(MUTED_TEXT_COLOR)),
        ]),
        ModelPickerStage::Model => Line::from(vec![
            Span::styled(
                "Choose a model for the selected provider. ",
                Style::default().fg(MUTED_TEXT_COLOR),
            ),
            Span::styled("Enter", Style::default().fg(KEY_HINT_COLOR)),
            Span::styled(" switches; ", Style::default().fg(MUTED_TEXT_COLOR)),
            Span::styled("Backspace", Style::default().fg(KEY_HINT_COLOR)),
            Span::styled(" returns.", Style::default().fg(MUTED_TEXT_COLOR)),
        ]),
    }
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
    let unit = app
        .config
        .model_catalog
        .providers
        .get(provider)
        .map(|provider| provider.unit.as_str())
        .unwrap_or_default();
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
            price.push(price_label("input", &entry.input, unit));
        }
        if !entry.output.is_empty() {
            price.push(price_label("output", &entry.output, unit));
        }
        if !entry.cache_read.is_empty() {
            price.push(price_label("cache read", &entry.cache_read, unit));
        }
        if !entry.cache_write.is_empty() {
            price.push(price_label("cache write", &entry.cache_write, unit));
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

pub(super) fn price_label(label: &str, value: &str, unit: &str) -> String {
    let suffix = unit_suffix(unit);
    format!("{label} {value}{suffix}")
}
