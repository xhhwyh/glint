use ratatui::{
    style::{Modifier, Style},
    text::{Line, Span},
};
use unicode_width::UnicodeWidthStr;

use crate::{
    app::{App, ModelPickerProviderRow, ModelPickerStage},
    configuration::{AvailableModel, AvailableProvider},
};

use super::{format::unit_suffix, layout::pad_to_width, theme::*};

pub(super) fn model_picker_lines(app: &App, width: u16) -> Vec<Line<'static>> {
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
        picker_separator_line(width),
    ];
    if let Some(error) = &picker.error {
        lines.push(Line::from(Span::styled(
            error.clone(),
            Style::default().fg(BORDER_BRIGHT_COLOR),
        )));
    }

    match picker.stage {
        ModelPickerStage::Provider => {
            let rows = app.model_picker_provider_rows();
            let name_width = rows
                .iter()
                .map(|row| match row {
                    ModelPickerProviderRow::Provider { display_name, .. } => display_name.width(),
                    ModelPickerProviderRow::AddModel => "Add model".width(),
                })
                .max()
                .unwrap_or(0);
            for (index, row) in rows.iter().enumerate() {
                let selected = index == picker.selected_provider;
                let style = if selected {
                    Style::default()
                        .fg(ACCENT_COLOR)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(MUTED_TEXT_COLOR)
                };
                let (name, summary) = match row {
                    ModelPickerProviderRow::Provider { id, display_name } => {
                        let summary = picker
                            .providers
                            .iter()
                            .find(|provider| provider.id == *id)
                            .map(|provider| provider_summary(app, provider))
                            .unwrap_or_default();
                        (display_name.as_str(), summary)
                    }
                    ModelPickerProviderRow::AddModel => (
                        "Add model",
                        "Configure providers and credentials".to_owned(),
                    ),
                };
                lines.push(Line::from(vec![
                    Span::raw("  "),
                    Span::styled(if selected { "❯ " } else { "  " }, style),
                    Span::styled(pad_to_width(name, name_width), style),
                    Span::styled("  ", style),
                    Span::styled(summary, Style::default().fg(MUTED_TEXT_COLOR)),
                ]));
            }
        }
        ModelPickerStage::Model => {
            let Some(provider) = picker.providers.get(picker.selected_provider) else {
                return lines;
            };
            lines.push(Line::from(vec![
                Span::styled("Provider ", Style::default().fg(MUTED_TEXT_COLOR)),
                Span::styled(
                    provider.display_name.clone(),
                    Style::default().fg(TEXT_COLOR).add_modifier(Modifier::BOLD),
                ),
            ]));

            let name_width = provider
                .models
                .iter()
                .map(|model| model.name.width())
                .max()
                .unwrap_or(0);
            for (model_index, model) in provider.models.iter().enumerate() {
                let selected = model_index == picker.selected_model;
                let current =
                    provider.id == app.config.llm.provider && model.name == app.config.llm.model;
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
                    Span::styled(pad_to_width(&model.name, name_width), style),
                    Span::styled("  ", style),
                    Span::styled(
                        model_summary(app, provider, model),
                        Style::default().fg(MUTED_TEXT_COLOR),
                    ),
                    Span::styled(current_marker, Style::default().fg(BORDER_BRIGHT_COLOR)),
                ]));
            }
        }
    }

    lines
}

fn picker_separator_line(width: u16) -> Line<'static> {
    Line::from(Span::styled(
        "─".repeat(width.max(1) as usize),
        Style::default().fg(MUTED_TEXT_COLOR),
    ))
}

fn model_picker_help_line(stage: ModelPickerStage) -> Line<'static> {
    match stage {
        ModelPickerStage::Provider => Line::from(vec![
            Span::styled(
                "Choose a configured provider or manage models. ",
                Style::default().fg(MUTED_TEXT_COLOR),
            ),
            Span::styled("Enter", Style::default().fg(KEY_HINT_COLOR)),
            Span::styled(" continues; ", Style::default().fg(MUTED_TEXT_COLOR)),
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

fn provider_summary(app: &App, provider: &AvailableProvider) -> String {
    app.config
        .model_catalog
        .providers
        .get(&provider.id)
        .map(|entry| entry.description.as_str())
        .filter(|description| !description.is_empty())
        .or((!provider.description.is_empty()).then_some(provider.description.as_str()))
        .unwrap_or(&provider.base_url)
        .to_owned()
}

fn model_summary(app: &App, provider: &AvailableProvider, model: &AvailableModel) -> String {
    let unit = app
        .config
        .model_catalog
        .providers
        .get(&provider.id)
        .map(|provider| provider.unit.as_str())
        .unwrap_or_default();
    let entry = &model.metadata;

    let mut parts = Vec::new();
    if !entry.positioning.is_empty() {
        parts.push(entry.positioning.clone());
    }
    if let Some(context) = entry.context {
        parts.push(format!("ctx {context}"));
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

#[cfg(test)]
mod tests {
    use crate::{
        app::App,
        event::{AppEvent, KeyAction, KeyInput},
    };

    use super::model_picker_lines;

    fn send_key(app: &mut App, action: KeyAction) {
        app.update(AppEvent::Key(KeyInput { action }));
    }

    fn open_model_picker(app: &mut App) {
        app.input.set("/model");
        send_key(app, KeyAction::Submit);
    }

    fn texts(app: &App) -> Vec<String> {
        model_picker_lines(app, 100)
            .into_iter()
            .map(|line| {
                line.spans
                    .into_iter()
                    .map(|span| span.content.into_owned())
                    .collect()
            })
            .collect()
    }

    #[test]
    fn provider_picker_hides_unconfigured_catalog_entries_and_adds_management_last() {
        let mut app = App::test_empty();

        open_model_picker(&mut app);
        let rows = texts(&app);

        assert!(rows.iter().any(|row| row.contains("test")));
        assert!(!rows.iter().any(|row| row.contains("DeepSeek")));
        assert!(!rows.iter().any(|row| row.contains("OpenRouter")));
        assert!(
            rows.last()
                .is_some_and(|row| row.trim().starts_with("Add model"))
        );
    }

    #[test]
    fn identical_custom_model_names_keep_the_selected_provider_visible() {
        let mut app = App::test_empty();
        app.configuration
            .save_custom(
                "Alpha Gateway",
                "https://alpha.example/v1",
                Some("alpha-key"),
                vec!["test-model".to_owned()],
            )
            .expect("second custom provider");
        open_model_picker(&mut app);
        while app.model_picker.as_ref().unwrap().selected_provider != 0 {
            send_key(&mut app, KeyAction::Up);
        }

        send_key(&mut app, KeyAction::Submit);
        let rows = texts(&app);

        assert!(
            rows.iter()
                .any(|row| row.contains("Provider Alpha Gateway"))
        );
        assert!(rows.iter().any(|row| row.contains("test-model")));
    }
}
