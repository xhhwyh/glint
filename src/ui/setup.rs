use ratatui::{
    Frame,
    layout::{Position, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Clear, Paragraph},
};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::{
    input::InputState,
    provider_catalog::ProviderCatalog,
    setup::{
        BuiltinFocus, CustomFocus, DeleteFocus, ProviderListRow, SetupScreen, SetupState,
        WelcomeFocus,
    },
};

use super::{
    layout::{box_body_styled, box_bottom, box_input_body_line, box_top, truncate_end_to_width},
    star,
    theme::{
        ACCENT_COLOR, BG_COLOR, BORDER_BRIGHT_COLOR, MUTED_TEXT_COLOR, SOFT_TEXT_COLOR, TEXT_COLOR,
    },
};

const STAR_X: u16 = 1;
const STAR_Y: u16 = 1;

pub fn render(frame: &mut Frame, state: &SetupState, _catalog: &ProviderCatalog) {
    let area = frame.area();
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(star_lines()).style(Style::default().bg(BG_COLOR)),
        star_area(area),
    );

    let panel = panel_area(area);
    let (lines, cursor) = screen_lines(state, panel.width);
    frame.render_widget(
        Paragraph::new(lines).style(Style::default().bg(BG_COLOR)),
        panel,
    );
    if let Some((x, y)) = cursor
        && x < panel.width
        && y < panel.height
    {
        frame.set_cursor_position(Position::new(panel.x + x, panel.y + y));
    }
}

fn star_lines() -> Vec<Line<'static>> {
    star::glint_star_rows()
        .into_iter()
        .map(Line::from)
        .collect()
}

fn star_area(area: Rect) -> Rect {
    Rect::new(
        area.x.saturating_add(STAR_X),
        area.y.saturating_add(STAR_Y),
        (star::STAR_WIDTH as u16).min(area.width.saturating_sub(STAR_X)),
        (star::STAR_HEIGHT as u16).min(area.height.saturating_sub(STAR_Y)),
    )
}

fn panel_area(area: Rect) -> Rect {
    let preferred_x = STAR_X
        .saturating_add(star::STAR_WIDTH as u16)
        .saturating_add(3);
    let x = if area.width > preferred_x.saturating_add(18) {
        preferred_x
    } else {
        1.min(area.width)
    };
    Rect::new(
        area.x.saturating_add(x),
        area.y.saturating_add(1),
        area.width.saturating_sub(x.saturating_add(1)),
        area.height.saturating_sub(2),
    )
}

fn screen_lines(state: &SetupState, width: u16) -> (Vec<Line<'static>>, Option<(u16, u16)>) {
    let (mut lines, mut cursor) = match &state.screen {
        SetupScreen::Welcome(welcome) => welcome_lines(width, welcome.focus),
        SetupScreen::Providers(list) => provider_lines(width, list),
        SetupScreen::Builtin(form) => builtin_lines(width, form),
        SetupScreen::Custom(form) => custom_lines(width, form),
        SetupScreen::ConfirmDelete(confirm) => confirm_delete_lines(width, confirm),
    };

    if let Some(error) = &state.error {
        lines.insert(
            1.min(lines.len()),
            box_body_styled(
                &truncate_end_to_width(error, width.saturating_sub(4) as usize),
                width,
                Style::default().fg(BORDER_BRIGHT_COLOR),
            ),
        );
        cursor = cursor.map(|(x, y)| (x, y + 1));
    }
    (lines, cursor)
}

fn welcome_lines(width: u16, focus: WelcomeFocus) -> (Vec<Line<'static>>, Option<(u16, u16)>) {
    (
        vec![
            box_top("Welcome", width),
            box_body_styled(
                "Add a model to begin chatting.",
                width,
                Style::default().fg(SOFT_TEXT_COLOR),
            ),
            action_line("Add model", focus == WelcomeFocus::AddModel, width),
            action_line("Exit", focus == WelcomeFocus::Exit, width),
            box_bottom(width),
        ],
        None,
    )
}

fn provider_lines(
    width: u16,
    list: &crate::setup::ProviderListState,
) -> (Vec<Line<'static>>, Option<(u16, u16)>) {
    let mut lines = vec![box_top("Model configuration", width)];
    for (index, row) in list.rows.iter().enumerate() {
        let selected = index == list.focus;
        let text = match row {
            ProviderListRow::Builtin { .. } | ProviderListRow::Custom { .. } => {
                let status = if row.needs_credential() {
                    "needs API key"
                } else if row.configured() {
                    "configured"
                } else {
                    "not configured"
                };
                format!(
                    "{}  ·  {}  ·  {} models",
                    row.display_name(),
                    status,
                    row.model_count()
                )
            }
            ProviderListRow::AddCustom => "Custom provider".into(),
            ProviderListRow::StartGlint => "Start Glint".into(),
        };
        lines.push(action_line(&text, selected, width));
    }
    lines.push(box_bottom(width));
    (lines, None)
}

fn builtin_lines(
    width: u16,
    form: &crate::setup::BuiltinProviderForm,
) -> (Vec<Line<'static>>, Option<(u16, u16)>) {
    let mut lines = vec![box_top(&form.display_name, width)];
    let (input, cursor_x) = input_line(
        "API key",
        &form.api_key,
        width,
        true,
        form.focus == BuiltinFocus::ApiKey,
    );
    let input_y = lines.len() as u16;
    lines.push(input);
    lines.push(box_body_styled(
        "Models",
        width,
        Style::default().fg(MUTED_TEXT_COLOR),
    ));
    for model in &form.models {
        lines.push(box_body_styled(
            model,
            width,
            Style::default().fg(SOFT_TEXT_COLOR),
        ));
    }
    lines.push(action_line("Save", form.focus == BuiltinFocus::Save, width));
    lines.push(action_line(
        "Cancel",
        form.focus == BuiltinFocus::Cancel,
        width,
    ));
    lines.push(box_bottom(width));
    (
        lines,
        (form.focus == BuiltinFocus::ApiKey).then_some((cursor_x, input_y)),
    )
}

fn custom_lines(
    width: u16,
    form: &crate::setup::CustomProviderForm,
) -> (Vec<Line<'static>>, Option<(u16, u16)>) {
    let mut lines = vec![box_top("Custom provider", width)];
    let mut cursor = None;
    for (label, input, focus) in [
        ("Name", &form.name, form.focus == CustomFocus::Name),
        (
            "Base URL",
            &form.base_url,
            form.focus == CustomFocus::BaseUrl,
        ),
        ("API key", &form.api_key, form.focus == CustomFocus::ApiKey),
    ] {
        let y = lines.len() as u16;
        let (line, x) = input_line(label, input, width, label == "API key", focus);
        lines.push(line);
        if focus {
            cursor = Some((x, y));
        }
    }
    lines.push(box_body_styled(
        "Models",
        width,
        Style::default().fg(MUTED_TEXT_COLOR),
    ));
    for (index, model) in form.models.iter().enumerate() {
        let y = lines.len() as u16;
        let focused = form.focus == CustomFocus::Model(index);
        let (line, x) = custom_model_line(model, width, focused);
        lines.push(line);
        if focused {
            cursor = Some((x, y));
        }
    }
    lines.push(action_line(
        "Add model",
        form.focus == CustomFocus::AddModel,
        width,
    ));
    lines.push(action_line("Save", form.focus == CustomFocus::Save, width));
    lines.push(action_line(
        "Cancel",
        form.focus == CustomFocus::Cancel,
        width,
    ));
    lines.push(box_bottom(width));
    (lines, cursor)
}

fn confirm_delete_lines(
    width: u16,
    confirm: &crate::setup::DeleteProviderState,
) -> (Vec<Line<'static>>, Option<(u16, u16)>) {
    (
        vec![
            box_top("Delete provider", width),
            box_body_styled(
                &format!("Remove {}?", confirm.display_name),
                width,
                Style::default().fg(SOFT_TEXT_COLOR),
            ),
            action_line("Delete", confirm.focus == DeleteFocus::Confirm, width),
            action_line("Cancel", confirm.focus == DeleteFocus::Cancel, width),
            box_bottom(width),
        ],
        None,
    )
}

fn action_line(text: &str, selected: bool, width: u16) -> Line<'static> {
    let style = if selected {
        Style::default()
            .fg(ACCENT_COLOR)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(SOFT_TEXT_COLOR)
    };
    box_body_styled(
        &format!("{} {}", if selected { "›" } else { " " }, text),
        width,
        style,
    )
}

fn input_line(
    label: &str,
    input: &InputState,
    width: u16,
    masked: bool,
    focused: bool,
) -> (Line<'static>, u16) {
    let prefix = format!("{label}: ");
    let body_width = width.saturating_sub(4) as usize;
    let available = body_width.saturating_sub(prefix.width());
    let (visible, cursor_column) = clipped_input(input, available, masked);
    let style = if focused {
        Style::default().fg(TEXT_COLOR).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(SOFT_TEXT_COLOR)
    };
    let line = box_input_body_line(
        Line::from(vec![
            Span::styled(prefix.clone(), Style::default().fg(MUTED_TEXT_COLOR)),
            Span::styled(visible, style),
        ]),
        width,
    );
    (
        line,
        (2 + prefix.width() + cursor_column).min(width.saturating_sub(1) as usize) as u16,
    )
}

fn custom_model_line(input: &InputState, width: u16, focused: bool) -> (Line<'static>, u16) {
    let body_width = width.saturating_sub(4) as usize;
    let available = body_width.saturating_sub(1);
    let (visible, cursor_column) = clipped_input(input, available, false);
    let style = if focused {
        Style::default().fg(TEXT_COLOR).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(SOFT_TEXT_COLOR)
    };
    let line = box_input_body_line(
        Line::from(vec![
            Span::styled(visible, style),
            Span::raw(
                " ".repeat(available.saturating_sub(input_visible_width(input, available, false))),
            ),
            Span::styled("×", Style::default().fg(MUTED_TEXT_COLOR)),
        ]),
        width,
    );
    (
        line,
        (2 + cursor_column).min(width.saturating_sub(1) as usize) as u16,
    )
}

fn input_visible_width(input: &InputState, width: usize, masked: bool) -> usize {
    clipped_input(input, width, masked).0.width()
}

fn clipped_input(input: &InputState, width: usize, masked: bool) -> (String, usize) {
    if width == 0 {
        return (String::new(), 0);
    }
    let characters = input.value.chars().collect::<Vec<_>>();
    let cursor = input.value[..input.cursor].chars().count();
    let mut start = cursor;
    let mut used = 0;
    while start > 0 {
        let character = if masked { '•' } else { characters[start - 1] };
        let next = character.width().unwrap_or(0);
        if used + next > width.saturating_sub(1) {
            break;
        }
        used += next;
        start -= 1;
    }
    let mut end = cursor;
    let mut visible_width = used;
    while end < characters.len() {
        let character = if masked { '•' } else { characters[end] };
        let next = character.width().unwrap_or(0);
        if visible_width + next > width {
            break;
        }
        visible_width += next;
        end += 1;
    }
    let visible = characters[start..end]
        .iter()
        .map(|character| if masked { '•' } else { *character })
        .collect();
    (visible, used)
}

#[cfg(test)]
mod tests {
    use ratatui::{Terminal, backend::TestBackend, layout::Rect};

    use crate::{provider_catalog::ProviderCatalog, setup::SetupState};

    use super::{panel_area, render, screen_lines};

    #[test]
    fn welcome_renders_star_and_only_primary_actions() {
        let state = SetupState::welcome(&test_catalog());
        let rendered = render_setup(&state, 100, 30);

        assert!(rendered.contains("Add model"));
        assert!(rendered.contains("Exit"));
        assert!(!rendered.contains("Start Glint"));
    }

    #[test]
    fn builtin_form_masks_key_and_lists_every_model() {
        let mut state = SetupState::builtin(test_catalog(), "deepseek");
        state.builtin_form_mut().unwrap().api_key.set("top-secret");
        let rendered = render_setup(&state, 100, 30);

        assert!(rendered.contains("deepseek-v4-flash"));
        assert!(rendered.contains("deepseek-v4-pro"));
        assert!(!rendered.contains("top-secret"));
    }

    #[test]
    fn custom_rows_have_no_generated_labels_and_trailing_delete_icons() {
        let mut state = SetupState::custom_provider(test_catalog());
        state.custom_form_mut().unwrap().add_model_row();
        let rendered = render_setup(&state, 100, 30);

        assert!(!rendered.contains("Model ID"));
        assert_eq!(rendered.matches('×').count(), 2);
    }

    #[test]
    fn custom_model_delete_icon_uses_the_last_content_column() {
        let state = SetupState::custom_provider(test_catalog());
        let backend = TestBackend::new(100, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        let catalog = test_catalog();
        terminal
            .draw(|frame| render(frame, &state, &catalog))
            .unwrap();

        let panel = panel_area(Rect::new(0, 0, 100, 30));
        assert_eq!(
            terminal
                .backend()
                .buffer()
                .cell((panel.x + panel.width - 3, panel.y + 5))
                .unwrap()
                .symbol(),
            "×"
        );
    }

    #[test]
    fn form_error_keeps_the_cursor_on_its_shifted_input_row() {
        let mut state = SetupState::builtin(test_catalog(), "deepseek");
        state.error = Some("Unable to save provider configuration.".into());

        let (_, cursor) = screen_lines(&state, 70);

        assert_eq!(cursor.map(|(_, y)| y), Some(2));
    }

    fn test_catalog() -> ProviderCatalog {
        ProviderCatalog::embedded().unwrap()
    }

    fn render_setup(state: &SetupState, width: u16, height: u16) -> String {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        let catalog = test_catalog();
        terminal
            .draw(|frame| render(frame, state, &catalog))
            .unwrap();
        terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect()
    }
}
