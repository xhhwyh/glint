use ratatui::{
    Frame,
    layout::{Position, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Clear, Paragraph},
};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::{
    input::InputState,
    provider_catalog::ProviderCatalog,
    setup::{
        BuiltinFocus, ChatGptFocus, CustomFocus, DeleteFocus, ProviderListRow, SetupScreen,
        SetupState, WelcomeFocus,
    },
};

use super::{
    layout::{box_body_styled, box_bottom, box_input_body_line, box_top, wrap_text},
    star,
    theme::{
        ACCENT_COLOR, BG_COLOR, BORDER_BRIGHT_COLOR, MUTED_TEXT_COLOR, SOFT_TEXT_COLOR, TEXT_COLOR,
    },
};

mod mouse;
pub use mouse::mouse_action;

struct PreparedSetup {
    panel: Rect,
    lines: Vec<Line<'static>>,
    cursor: Option<(u16, u16)>,
    scroll: usize,
    logo: Option<(Rect, bool)>,
}

fn prepare(state: &SetupState, area: Rect) -> PreparedSetup {
    let welcome = matches!(state.screen, SetupScreen::Welcome(_));
    let mut panel = panel_area(area);
    let (lines, cursor) = screen_lines(state, panel.width);
    let mut logo = None;
    if welcome {
        let content_height = u16::try_from(lines.len()).unwrap_or(u16::MAX);
        let full_logo = panel.height > content_height + star::STAR_HEIGHT as u16
            && panel.width >= star::STAR_WIDTH as u16;
        let logo_height = if full_logo {
            star::STAR_HEIGHT as u16
        } else {
            1
        };
        let gap = u16::from(panel.height > content_height + logo_height);
        let top = panel.y
            + panel
                .height
                .saturating_sub(content_height + logo_height + gap)
                / 2;
        let logo_width = (star::STAR_WIDTH as u16).min(panel.width);
        logo = Some((
            Rect::new(
                panel.x + (panel.width - logo_width) / 2,
                top,
                logo_width,
                logo_height,
            ),
            full_logo,
        ));
        let bottom = panel.bottom();
        panel.y = (top + logo_height + gap).min(bottom);
        panel.height = bottom.saturating_sub(panel.y);
    }
    // Keep the active input or action visible, including after inserted notices.
    let focus = cursor
        .map(|(_, y)| usize::from(y))
        .or_else(|| {
            lines.iter().position(|line| {
                line.spans.iter().any(|span| {
                    span.content.starts_with("› ")
                        || (span.content == "×" && span.style.add_modifier.contains(Modifier::BOLD))
                })
            })
        })
        .unwrap_or(0);
    let scroll = focus
        .saturating_add(2)
        .saturating_sub(usize::from(panel.height))
        .min(lines.len().saturating_sub(usize::from(panel.height)));
    PreparedSetup {
        panel,
        lines,
        cursor,
        scroll,
        logo,
    }
}

pub fn render(frame: &mut Frame, state: &SetupState, _catalog: &ProviderCatalog) {
    let area = frame.area();
    let prepared = prepare(state, area);
    let hits = mouse::hitboxes(state, &prepared);
    let PreparedSetup {
        panel,
        lines,
        cursor,
        scroll,
        logo,
    } = prepared;
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new("").style(Style::default().bg(BG_COLOR)),
        area,
    );
    if let Some((rect, full)) = logo {
        let rows = if full {
            star::glint_star_rows()
                .into_iter()
                .map(Line::from)
                .collect()
        } else {
            vec![Line::styled("✦", Style::default().fg(ACCENT_COLOR)).centered()]
        };
        frame.render_widget(Paragraph::new(rows), rect);
    }
    frame.render_widget(
        Paragraph::new(lines.into_iter().skip(scroll).collect::<Vec<_>>())
            .style(Style::default().bg(BG_COLOR)),
        panel,
    );
    mouse::render_hover(frame, state, &hits);
    if let Some((x, y)) = cursor
        && let Some(y) = usize::from(y).checked_sub(scroll)
        && x < panel.width
        && y < usize::from(panel.height)
    {
        frame.set_cursor_position(Position::new(panel.x + x, panel.y + y as u16));
    }
    let footer = Rect::new(
        area.x,
        area.bottom().saturating_sub(1),
        area.width,
        area.height.min(1),
    );
    frame.render_widget(
        Paragraph::new(keyboard_hint(state, area.width)).centered(),
        footer,
    );
}

fn panel_area(area: Rect) -> Rect {
    let width = area.width.saturating_sub(4).min(64);
    Rect::new(
        area.x + (area.width - width) / 2,
        area.y + u16::from(area.height > 2),
        width,
        area.height.saturating_sub(3),
    )
}

fn keyboard_hint(state: &SetupState, width: u16) -> Line<'static> {
    let mut hints = vec![("↑↓/Tab", "move"), ("Enter", "select")];
    if state.backspace_returns() {
        hints.push(("Backspace", "back"));
    }
    if let SetupScreen::Custom(form) = &state.screen
        && matches!(
            form.focus,
            CustomFocus::Model(_) | CustomFocus::DeleteModel(_)
        )
    {
        hints.push(("Del", "remove model"));
    }
    match &state.screen {
        SetupScreen::Builtin(_)
        | SetupScreen::Custom(_)
        | SetupScreen::ConfirmDelete(_)
        | SetupScreen::ChatGpt(_) => {
            hints.push(("Esc", "cancel"));
        }
        SetupScreen::Providers(list)
            if list
                .rows
                .get(list.focus)
                .is_some_and(ProviderListRow::can_delete) =>
        {
            hints.push(("Del", "remove"));
        }
        _ => {}
    }
    hints.push(("Ctrl+C", "quit"));
    if width >= 110 {
        hints.push(("Click", "select / edit"));
    }
    let full_width: usize = hints
        .iter()
        .map(|(key, label)| key.width() + label.width() + 1)
        .sum::<usize>()
        + (hints.len() - 1) * 3;
    let compact = full_width > usize::from(width);
    let mut spans = Vec::new();
    for (index, (key, label)) in hints.into_iter().enumerate() {
        if index > 0 {
            spans.push(Span::styled(
                if compact { " " } else { " · " },
                Style::default().fg(MUTED_TEXT_COLOR),
            ));
        }
        spans.push(Span::styled(
            key,
            Style::default()
                .fg(ACCENT_COLOR)
                .add_modifier(Modifier::BOLD),
        ));
        if !compact {
            spans.push(Span::styled(
                format!(" {label}"),
                Style::default().fg(MUTED_TEXT_COLOR),
            ));
        }
    }
    Line::from(spans)
}

fn screen_lines(state: &SetupState, width: u16) -> (Vec<Line<'static>>, Option<(u16, u16)>) {
    let (mut lines, mut cursor) = match &state.screen {
        SetupScreen::Welcome(welcome) => welcome_lines(width, welcome.focus),
        SetupScreen::Providers(list) => provider_lines(width, list),
        SetupScreen::Builtin(form) => builtin_lines(width, form),
        SetupScreen::Custom(form) => custom_lines(width, form),
        SetupScreen::ChatGpt(form) => chatgpt_lines(width, form),
        SetupScreen::ConfirmDelete(confirm) => confirm_delete_lines(width, confirm),
    };

    let mut inserted = 0_u16;
    if let Some(notice) = &state.notice {
        let notice_lines = wrapped_message_lines(notice, width, Style::default().fg(ACCENT_COLOR));
        inserted += u16::try_from(notice_lines.len()).unwrap_or(u16::MAX);
        lines.splice(1.min(lines.len())..1.min(lines.len()), notice_lines);
    }
    if let Some(error) = &state.error {
        let error_lines =
            wrapped_message_lines(error, width, Style::default().fg(BORDER_BRIGHT_COLOR));
        let error_index = usize::from(1 + inserted).min(lines.len());
        inserted += u16::try_from(error_lines.len()).unwrap_or(u16::MAX);
        lines.splice(error_index..error_index, error_lines);
    }
    cursor = cursor.map(|(x, y)| (x, y + inserted));
    (lines, cursor)
}

fn wrapped_message_lines(text: &str, width: u16, style: Style) -> Vec<Line<'static>> {
    wrap_text(text, width.saturating_sub(4))
        .into_iter()
        .map(|line| box_body_styled(&line, width, style))
        .collect()
}

fn welcome_lines(width: u16, focus: WelcomeFocus) -> (Vec<Line<'static>>, Option<(u16, u16)>) {
    let mut lines = vec![
        Line::styled(
            "Welcome to Glint",
            Style::default().fg(TEXT_COLOR).add_modifier(Modifier::BOLD),
        )
        .centered(),
    ];
    if width >= 28 {
        lines.push(
            Line::styled(
                "Add a model to begin chatting.",
                Style::default().fg(MUTED_TEXT_COLOR),
            )
            .centered(),
        );
    }
    lines.push(Line::default());
    for (label, selected) in [
        ("Add model", focus == WelcomeFocus::AddModel),
        ("Exit", focus == WelcomeFocus::Exit),
    ] {
        lines.push(
            Line::styled(
                format!("{} {label}  ", if selected { "›" } else { " " }),
                if selected {
                    Style::default()
                        .fg(ACCENT_COLOR)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(MUTED_TEXT_COLOR)
                },
            )
            .centered(),
        );
    }
    (lines, None)
}

fn provider_lines(
    width: u16,
    list: &crate::setup::ProviderListState,
) -> (Vec<Line<'static>>, Option<(u16, u16)>) {
    let mut lines = vec![box_top("Add model", width)];
    for (index, row) in list.rows.iter().enumerate() {
        if matches!(row, ProviderListRow::StartGlint) {
            continue;
        }
        let selected = index == list.focus;
        let (icon, color) = if row.status_unavailable() || row.needs_credential() {
            ("!", Color::Yellow)
        } else if row.configured() {
            ("✓", Color::Green)
        } else {
            (" ", MUTED_TEXT_COLOR)
        };
        let style = if selected {
            Style::default()
                .fg(ACCENT_COLOR)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(SOFT_TEXT_COLOR)
        };
        lines.push(box_input_body_line(
            Line::from(vec![
                Span::styled(if selected { "› " } else { "  " }, style),
                Span::styled(format!("{icon} "), Style::default().fg(color)),
                Span::styled(row.display_name().to_owned(), style),
            ]),
            width,
        ));
    }
    lines.push(box_bottom(width));
    if let Some(index) = list
        .rows
        .iter()
        .position(|row| matches!(row, ProviderListRow::StartGlint))
    {
        lines.push(Line::default());
        lines.push(form_action_line("Start Glint", index == list.focus));
    }
    if list.rows.iter().any(|row| row.status_unavailable()) {
        lines.push(Line::styled(
            "! status unavailable · Refresh providers to retry",
            Style::default().fg(MUTED_TEXT_COLOR),
        ));
    } else if list.rows.iter().any(|row| row.needs_credential()) {
        lines.push(Line::styled(
            "! needs API key · Select provider to repair",
            Style::default().fg(MUTED_TEXT_COLOR),
        ));
    }
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
    lines.push(box_input_body_line(
        Line::styled("Models:", Style::default().fg(MUTED_TEXT_COLOR)),
        width,
    ));
    for model in &form.models {
        lines.push(box_input_body_line(
            Line::styled(model.clone(), Style::default().fg(SOFT_TEXT_COLOR)),
            width,
        ));
    }
    lines.push(box_bottom(width));
    lines.push(Line::default());
    lines.push(form_action_line("Save", form.focus == BuiltinFocus::Save));
    lines.push(form_action_line(
        "Cancel",
        form.focus == BuiltinFocus::Cancel,
    ));
    (
        lines,
        (form.focus == BuiltinFocus::ApiKey).then_some((cursor_x, input_y)),
    )
}

fn chatgpt_lines(
    width: u16,
    form: &crate::setup::ChatGptForm,
) -> (Vec<Line<'static>>, Option<(u16, u16)>) {
    let mut lines = vec![box_top(crate::config::CHATGPT_PROVIDER_NAME, width)];
    for message in [
        "Use the Codex access included in your ChatGPT plan.",
        "Sign in securely in your browser. No API key needed.",
        "Glint manages your conversation and tools.",
    ] {
        lines.extend(wrapped_message_lines(
            message,
            width,
            Style::default().fg(SOFT_TEXT_COLOR),
        ));
    }
    if form.configured {
        lines.extend(wrapped_message_lines(
            "Connected · Sign in to refresh your account and models.",
            width,
            Style::default().fg(ACCENT_COLOR),
        ));
    }
    if let Some(url) = &form.url {
        lines.extend(wrapped_message_lines(
            "Open your browser to finish signing in:",
            width,
            Style::default().fg(ACCENT_COLOR),
        ));
        lines.extend(wrapped_message_lines(
            url,
            width,
            Style::default().fg(MUTED_TEXT_COLOR),
        ));
    } else if form.login.is_some() {
        lines.extend(wrapped_message_lines(
            "Connecting to ChatGPT…",
            width,
            Style::default().fg(ACCENT_COLOR),
        ));
    }
    lines.push(box_bottom(width));
    lines.push(Line::default());
    lines.push(form_action_line(
        if form.url.is_some() {
            "Open browser"
        } else if form.login.is_some() {
            "Signing in…"
        } else {
            "Sign in with ChatGPT"
        },
        form.focus == ChatGptFocus::SignIn,
    ));
    lines.push(form_action_line(
        "Cancel",
        form.focus == ChatGptFocus::Cancel,
    ));
    (lines, None)
}

fn custom_lines(
    width: u16,
    form: &crate::setup::CustomProviderForm,
) -> (Vec<Line<'static>>, Option<(u16, u16)>) {
    let mut lines = vec![box_top("Custom provider", width)];
    let mut cursor = None;
    for (label, input, focus) in [
        (
            if form.name_is_read_only() {
                "Name (read-only)"
            } else {
                "Name"
            },
            &form.name,
            form.focus == CustomFocus::Name,
        ),
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
    lines.push(box_input_body_line(
        Line::styled("Models:", Style::default().fg(MUTED_TEXT_COLOR)),
        width,
    ));
    for (index, model) in form.models.iter().enumerate() {
        let y = lines.len() as u16;
        let input_focused = form.focus == CustomFocus::Model(index);
        let delete_focused = form.focus == CustomFocus::DeleteModel(index);
        let (line, x) = custom_model_line(model, width, input_focused, delete_focused);
        lines.push(line);
        if input_focused {
            cursor = Some((x, y));
        }
    }
    lines.push(action_line(
        "Add model",
        form.focus == CustomFocus::AddModel,
        width,
    ));
    lines.push(box_bottom(width));
    lines.push(Line::default());
    lines.push(form_action_line("Save", form.focus == CustomFocus::Save));
    lines.push(form_action_line(
        "Cancel",
        form.focus == CustomFocus::Cancel,
    ));
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

fn form_action_line(text: &str, selected: bool) -> Line<'static> {
    let style = if selected {
        Style::default()
            .fg(ACCENT_COLOR)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(SOFT_TEXT_COLOR)
    };
    Line::from(vec![
        Span::raw("  "),
        Span::styled(
            format!("{} {text}", if selected { "›" } else { " " }),
            style,
        ),
    ])
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

fn custom_model_line(
    input: &InputState,
    width: u16,
    input_focused: bool,
    delete_focused: bool,
) -> (Line<'static>, u16) {
    let body_width = width.saturating_sub(4) as usize;
    let available = body_width.saturating_sub(1);
    let (visible, cursor_column) = clipped_input(input, available, false);
    let style = if input_focused {
        Style::default().fg(TEXT_COLOR).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(SOFT_TEXT_COLOR)
    };
    let delete_style = if delete_focused {
        Style::default()
            .fg(ACCENT_COLOR)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(MUTED_TEXT_COLOR)
    };
    let line = box_input_body_line(
        Line::from(vec![
            Span::styled(visible, style),
            Span::raw(
                " ".repeat(available.saturating_sub(input_visible_width(input, available, false))),
            ),
            Span::styled("×", delete_style),
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
    use std::path::PathBuf;

    use ratatui::{Terminal, backend::TestBackend, layout::Rect};

    use crate::{
        configuration::ConfigurationManager,
        credentials::FileCredentialStore,
        paths::GlintPaths,
        persistence::UserConfigStore,
        provider_catalog::ProviderCatalog,
        setup::{SetupScreen, SetupState},
    };

    use super::{panel_area, render, screen_lines};

    #[test]
    fn chatgpt_login_actions_are_outside_box_and_mouse_cancel_returns() {
        use crate::{
            event::{KeyAction, MouseAction},
            setup::ProviderListRow,
        };
        let mut state = SetupState::welcome(&test_catalog());
        state.update(KeyAction::Submit);
        if let SetupScreen::Providers(list) = &mut state.screen {
            list.focus = list
                .rows
                .iter()
                .position(|row| matches!(row, ProviderListRow::ChatGpt { .. }))
                .unwrap();
        }
        state.update(KeyAction::Submit);
        let rows = render_rows(&state, 100, 30);
        let sign_in = rows
            .iter()
            .position(|row| row.contains("Sign in with ChatGPT"))
            .unwrap();
        let cancel = rows.iter().position(|row| row.contains("Cancel")).unwrap();
        assert!(!rows[sign_in].contains('│'));
        assert!(!rows[cancel].contains('│'));
        assert!(!rows.iter().any(|row| row.contains("API key:")));
        let panel = panel_area(Rect::new(0, 0, 100, 30));
        let hover = super::mouse_action(
            &state,
            MouseAction::Move {
                column: panel.x + 3,
                row: cancel as u16,
            },
            100,
            30,
        );
        state.update_mouse(hover);
        let hovered = render_rows(&state, 100, 30);
        assert_eq!(hovered.join("").matches('›').count(), 1);
        assert!(hovered[cancel].contains('›'));
        let click = super::mouse_action(
            &state,
            MouseAction::LeftDown {
                column: panel.x + 3,
                row: cancel as u16,
            },
            100,
            30,
        );
        state.update_mouse(click);
        assert!(matches!(state.screen, SetupScreen::Providers(_)));
    }

    #[test]
    fn mouse_click_opens_provider_and_deletes_custom_model() {
        use crate::{
            event::MouseAction,
            setup::{CustomFocus, SetupScreen},
        };
        let mut state = SetupState::welcome(&test_catalog());
        let action = super::mouse_action(
            &state,
            MouseAction::LeftDown {
                column: 50,
                row: 22,
            },
            100,
            30,
        );
        state.update_mouse(action);
        assert!(matches!(state.screen, SetupScreen::Providers(_)));
        let mut state = SetupState::custom_provider(test_catalog());
        state.custom_form_mut().unwrap().add_model_row();
        state.custom_form_mut().unwrap().models[0].set("first");
        state.custom_form_mut().unwrap().models[1].set("second");
        let panel = panel_area(Rect::new(0, 0, 100, 30));
        let action = super::mouse_action(
            &state,
            MouseAction::LeftDown {
                column: panel.x + panel.width - 3,
                row: panel.y + 5,
            },
            100,
            30,
        );
        state.update_mouse(action);
        let form = state.custom_form_mut().unwrap();
        assert_eq!(form.models.len(), 1);
        assert_eq!(form.models[0].value, "second");
        assert_eq!(form.focus, CustomFocus::Model(0));
    }

    #[test]
    fn welcome_renders_star_and_only_primary_actions() {
        let state = SetupState::welcome(&test_catalog());
        let rendered = render_setup(&state, 100, 30);

        assert!(rendered.contains("Add model"));
        assert!(rendered.contains("Exit"));
        assert!(!rendered.contains("Start Glint"));
    }

    #[test]
    fn welcome_stacks_logo_above_title_and_keeps_footer_at_bottom() {
        let state = SetupState::welcome(&test_catalog());
        let rows = render_rows(&state, 100, 30);
        let title_row = rows
            .iter()
            .position(|row| row.contains("Welcome to Glint"))
            .unwrap();
        assert!(title_row >= super::star::STAR_HEIGHT);
        assert!(rows.last().unwrap().contains("Enter"));
        for (width, height) in [(40, 12), (24, 8)] {
            let compact = render_setup(&state, width, height);
            assert!(compact.contains("Add model"));
            assert!(compact.contains("Exit"));
            assert!(compact.contains("Enter"));
        }
    }

    #[test]
    fn provider_rows_use_configured_icon_without_status_or_count() {
        let mut state = SetupState::welcome(&test_catalog());
        state.update(crate::event::KeyAction::Submit);
        if let SetupScreen::Providers(list) = &mut state.screen {
            if let crate::setup::ProviderListRow::Builtin { configured, .. } = &mut list.rows[0] {
                *configured = true;
            }
        }
        let rendered = render_setup(&state, 100, 30);
        assert!(rendered.contains("✓"));
        assert!(!rendered.contains("configured"));
        assert!(!rendered.contains(" models"));
    }

    #[test]
    fn short_provider_list_keeps_last_selection_visible_above_footer() {
        let mut state = SetupState::welcome(&test_catalog());
        state.update(crate::event::KeyAction::Submit);
        if let SetupScreen::Providers(list) = &mut state.screen {
            list.focus = list.rows.len() - 1;
        }
        let rendered = render_setup(&state, 40, 8);
        assert!(rendered.contains("Custom provider"));
        assert!(rendered.contains("Enter"));
    }

    #[test]
    fn short_form_scrolls_input_cursor_and_save_action_into_view() {
        let mut state = SetupState::custom_provider(test_catalog());
        let form = state.custom_form_mut().unwrap();
        for _ in 0..12 {
            form.add_model_row();
        }
        form.models[12].set("last-model");
        form.focus = crate::setup::CustomFocus::Model(12);
        let mut terminal = Terminal::new(TestBackend::new(40, 10)).unwrap();
        terminal
            .draw(|frame| render(frame, &state, &test_catalog()))
            .unwrap();
        let (x, y) = terminal.get_cursor_position().map(|p| (p.x, p.y)).unwrap();
        assert!(y < 8);
        assert_eq!(
            terminal
                .backend()
                .buffer()
                .cell((x - 1, y))
                .unwrap()
                .symbol(),
            "l"
        );
        state.custom_form_mut().unwrap().focus = crate::setup::CustomFocus::Save;
        let rendered = render_setup(&state, 40, 10);
        assert!(rendered.contains("› Save"));
        assert!(rendered.contains("Esc"));
    }

    #[test]
    fn footer_shows_backspace_only_when_it_navigates_back() {
        let mut state = SetupState::builtin(test_catalog(), "deepseek");
        assert!(
            render_rows(&state, 100, 30)
                .last()
                .unwrap()
                .contains("Backspace back")
        );
        state.builtin_form_mut().unwrap().api_key.set("secret");
        let rows = render_rows(&state, 100, 30);
        assert!(!rows.last().unwrap().contains("Backspace"));
        assert!(rows.last().unwrap().contains("Esc cancel"));
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
    fn custom_model_delete_icon_visibly_renders_keyboard_focus() {
        let mut state = SetupState::custom_provider(test_catalog());
        for _ in 0..4 {
            state.update(crate::event::KeyAction::Tab);
        }
        let backend = TestBackend::new(100, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        let catalog = test_catalog();
        terminal
            .draw(|frame| render(frame, &state, &catalog))
            .unwrap();

        let panel = panel_area(Rect::new(0, 0, 100, 30));
        let icon = terminal
            .backend()
            .buffer()
            .cell((panel.x + panel.width - 3, panel.y + 5))
            .unwrap();
        assert_eq!(icon.symbol(), "×");
        assert_eq!(icon.fg, super::ACCENT_COLOR);
        assert!(icon.modifier.contains(ratatui::style::Modifier::BOLD));
    }

    #[test]
    fn existing_custom_provider_renders_name_as_read_only_and_focuses_base_url() {
        let state = existing_custom_provider_state();
        let rendered = render_setup(&state, 100, 30);
        let (_, cursor) = screen_lines(&state, 100);

        assert!(rendered.contains("Name (read-only): Gateway"));
        assert_eq!(cursor.map(|(_, y)| y), Some(2));
    }

    #[test]
    fn form_error_keeps_the_cursor_on_its_shifted_input_row() {
        let mut state = SetupState::builtin(test_catalog(), "deepseek");
        state.error = Some("Unable to save provider configuration.".into());

        let (_, cursor) = screen_lines(&state, 70);

        assert_eq!(cursor.map(|(_, y)| y), Some(2));
    }

    #[test]
    fn degraded_credential_notice_renders_separately_from_form_errors() {
        let mut state = SetupState::builtin(test_catalog(), "deepseek");
        state.notice = Some(
            "The system credential store is unavailable.\nRe-enter an API key to repair this provider.\nGlint will switch to its protected auth.json file.".into(),
        );
        state.error = Some("Enter an API key for this provider.".into());

        let rendered = render_setup(&state, 100, 30);

        assert!(rendered.contains("system credential store is unavailable"));
        assert!(rendered.contains("protected auth.json file."), "{rendered}");
        assert!(rendered.contains("Enter an API key for this provider."));
    }

    #[test]
    fn committed_refresh_failure_renders_unknown_status_and_reachable_retry() {
        let state = committed_refresh_failure_state();

        let rendered = render_setup(&state, 100, 30);

        assert!(rendered.contains("Changes were saved"));
        assert!(rendered.contains("status unavailable"));
        assert!(rendered.contains("Refresh providers"));
        assert!(!rendered.contains("sentinel-secret"));
    }

    fn test_catalog() -> ProviderCatalog {
        ProviderCatalog::embedded().unwrap()
    }

    fn render_setup(state: &SetupState, width: u16, height: u16) -> String {
        render_rows(state, width, height).concat()
    }

    fn render_rows(state: &SetupState, width: u16, height: u16) -> Vec<String> {
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
            .chunks(usize::from(width))
            .map(|row| row.iter().map(|cell| cell.symbol()).collect())
            .collect()
    }

    fn existing_custom_provider_state() -> SetupState {
        let home = std::env::temp_dir().join(format!(
            "glint-setup-render-existing-{}",
            uuid::Uuid::new_v4()
        ));
        let paths = GlintPaths::from_home(&home);
        let catalog = test_catalog();
        let mut manager = ConfigurationManager::new(
            paths.clone(),
            PathBuf::from("/workspace"),
            catalog.clone(),
            Box::new(UserConfigStore::new(paths.clone())),
            Box::new(FileCredentialStore::new(paths.auth())),
        )
        .unwrap();
        manager
            .save_custom(
                "Gateway",
                "https://old.example/v1",
                Some("secret"),
                vec!["model".into()],
            )
            .unwrap();
        let mut state = SetupState::provider_list(&catalog, &manager).unwrap();
        let custom_index = match &state.screen {
            SetupScreen::Providers(list) => list
                .rows
                .iter()
                .position(|row| row.provider_id() == Some("Gateway"))
                .unwrap(),
            _ => unreachable!(),
        };
        for _ in 0..custom_index {
            state.update(crate::event::KeyAction::Down);
        }
        state.update(crate::event::KeyAction::Submit);
        std::fs::remove_dir_all(home).ok();
        state
    }

    fn committed_refresh_failure_state() -> SetupState {
        let home = std::env::temp_dir().join(format!(
            "glint-setup-render-refresh-failure-{}",
            uuid::Uuid::new_v4()
        ));
        let paths = GlintPaths::from_home(&home);
        let catalog = test_catalog();
        let mut manager = ConfigurationManager::new(
            paths.clone(),
            PathBuf::from("/workspace"),
            catalog.clone(),
            Box::new(UserConfigStore::new(paths.clone())),
            Box::new(FileCredentialStore::new(paths.auth())),
        )
        .unwrap();
        manager
            .save_builtin("deepseek", Some("sentinel-secret"))
            .unwrap();
        let mut state = SetupState::welcome(&catalog);
        state.show_saved_refresh_failure(&manager);
        std::fs::remove_dir_all(home).ok();
        state
    }
}
