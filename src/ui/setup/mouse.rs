use super::*;
use crate::{
    event::MouseAction,
    setup::{SetupMouseAction, SetupTarget},
};

pub(super) struct Hitbox {
    rect: Rect,
    target: SetupTarget,
    input: Option<(u16, usize)>, // text start and visible input width
}

struct TargetRow {
    row: usize,
    x: u16,
    width: u16,
    target: SetupTarget,
    input: Option<(u16, usize)>,
}

pub(super) fn hitboxes(state: &SetupState, layout: &PreparedSetup) -> Vec<Hitbox> {
    let width = layout.panel.width;
    let body_width = width.saturating_sub(4);
    let mut rows = Vec::new();
    let mut add = |row, target, prefix: Option<u16>, delete: bool| {
        let x = if delete { width.saturating_sub(3) } else { 2 };
        let w = if delete {
            1
        } else {
            body_width.saturating_sub(u16::from(matches!(
                target,
                SetupTarget::Custom(CustomFocus::Model(_))
            )))
        };
        let input = prefix.map(|prefix| (2 + prefix, usize::from(w.saturating_sub(prefix))));
        rows.push(TargetRow {
            row,
            x,
            width: w,
            target,
            input,
        });
    };
    let base_count = match &state.screen {
        SetupScreen::Welcome(welcome) => {
            let lines = welcome_lines(width, welcome.focus).0;
            for (index, focus) in [WelcomeFocus::AddModel, WelcomeFocus::Exit]
                .into_iter()
                .enumerate()
            {
                let row = lines.len() - 2 + index;
                let w = lines[row].width().min(usize::from(width)) as u16;
                rows.push(TargetRow {
                    row,
                    // Match Paragraph centering, including odd text widths.
                    x: (width / 2).saturating_sub(w / 2),
                    width: w,
                    target: SetupTarget::Welcome(focus),
                    input: None,
                });
            }
            lines.len()
        }
        SetupScreen::Providers(list) => {
            let mut row_index = 1;
            for (index, row) in list.rows.iter().enumerate() {
                if !matches!(row, ProviderListRow::StartGlint) {
                    add(row_index, SetupTarget::Provider(index), None, false);
                    row_index += 1;
                }
            }
            if let Some(index) = list
                .rows
                .iter()
                .position(|row| matches!(row, ProviderListRow::StartGlint))
            {
                add(row_index + 2, SetupTarget::Provider(index), None, false);
            }
            provider_lines(width, list).0.len()
        }
        SetupScreen::ChatGpt(form) => {
            let count = chatgpt_lines(width, form).0.len();
            add(
                count - 2,
                SetupTarget::ChatGpt(ChatGptFocus::SignIn),
                None,
                false,
            );
            add(
                count - 1,
                SetupTarget::ChatGpt(ChatGptFocus::Cancel),
                None,
                false,
            );
            count
        }
        SetupScreen::Builtin(form) => {
            add(
                1,
                SetupTarget::Builtin(BuiltinFocus::ApiKey),
                Some(9),
                false,
            );
            let count = builtin_lines(width, form).0.len();
            add(
                count - 2,
                SetupTarget::Builtin(BuiltinFocus::Save),
                None,
                false,
            );
            add(
                count - 1,
                SetupTarget::Builtin(BuiltinFocus::Cancel),
                None,
                false,
            );
            count
        }
        SetupScreen::Custom(form) => {
            if !form.name_is_read_only() {
                add(1, SetupTarget::Custom(CustomFocus::Name), Some(6), false);
            }
            add(
                2,
                SetupTarget::Custom(CustomFocus::BaseUrl),
                Some(10),
                false,
            );
            add(3, SetupTarget::Custom(CustomFocus::ApiKey), Some(9), false);
            for index in 0..form.models.len() {
                add(
                    5 + index,
                    SetupTarget::Custom(CustomFocus::Model(index)),
                    Some(0),
                    false,
                );
                add(
                    5 + index,
                    SetupTarget::Custom(CustomFocus::DeleteModel(index)),
                    None,
                    true,
                );
            }
            add(
                5 + form.models.len(),
                SetupTarget::Custom(CustomFocus::AddModel),
                None,
                false,
            );
            let count = custom_lines(width, form).0.len();
            add(
                count - 2,
                SetupTarget::Custom(CustomFocus::Save),
                None,
                false,
            );
            add(
                count - 1,
                SetupTarget::Custom(CustomFocus::Cancel),
                None,
                false,
            );
            count
        }
        SetupScreen::ConfirmDelete(confirm) => {
            add(2, SetupTarget::Delete(DeleteFocus::Confirm), None, false);
            add(3, SetupTarget::Delete(DeleteFocus::Cancel), None, false);
            confirm_delete_lines(width, confirm).0.len()
        }
    };
    let inserted = layout.lines.len().saturating_sub(base_count);
    rows.into_iter()
        .filter_map(
            |TargetRow {
                 row,
                 x,
                 width,
                 target,
                 input,
             }| {
                let row = row + if row >= 1 { inserted } else { 0 };
                let visible_row = row.checked_sub(layout.scroll)?;
                if visible_row >= usize::from(layout.panel.height) || width == 0 {
                    return None;
                }
                let rect = Rect::new(
                    layout.panel.x + x,
                    layout.panel.y + visible_row as u16,
                    width,
                    1,
                )
                .intersection(layout.panel);
                if rect.is_empty() {
                    return None;
                }
                Some(Hitbox {
                    rect,
                    target,
                    input: input.map(|(x, width)| (layout.panel.x + x, width)),
                })
            },
        )
        .collect()
}

pub fn mouse_action(
    state: &SetupState,
    mouse: MouseAction,
    width: u16,
    height: u16,
) -> SetupMouseAction {
    let layout = prepare(state, Rect::new(0, 0, width, height));
    let (column, row, click) = match mouse {
        MouseAction::Move { column, row } => (column, row, false),
        MouseAction::LeftDown { column, row } => (column, row, true),
        MouseAction::ScrollUp { column, row } | MouseAction::ScrollDown { column, row } => {
            return if layout.panel.contains(Position::new(column, row)) {
                SetupMouseAction::Scroll(if matches!(mouse, MouseAction::ScrollUp { .. }) {
                    -1
                } else {
                    1
                })
            } else {
                SetupMouseAction::None
            };
        }
        _ => return SetupMouseAction::None,
    };
    let hits = hitboxes(state, &layout);
    let hit = hits
        .iter()
        .find(|hit| hit.rect.contains(Position::new(column, row)));
    if !click {
        return SetupMouseAction::Hover(hit.map(|hit| hit.target));
    }
    let Some(hit) = hit else {
        return SetupMouseAction::None;
    };
    let cursor = hit.input.and_then(|(start, available)| {
        state.target_input(hit.target).map(|(input, masked)| {
            cursor_at_column(
                input,
                available,
                masked,
                usize::from(column.saturating_sub(start)),
            )
        })
    });
    SetupMouseAction::Click {
        target: hit.target,
        cursor,
    }
}

fn cursor_at_column(input: &InputState, available: usize, masked: bool, column: usize) -> usize {
    // Use the same left clipping as the rendered input; return a UTF-8 byte boundary.
    let (_, cursor_column) = clipped_input(input, available, masked);
    let mut start = input.cursor;
    let mut remaining = cursor_column;
    for (index, character) in input.value[..input.cursor].char_indices().rev() {
        let width = if masked {
            1
        } else {
            character.width().unwrap_or(0)
        };
        if width > remaining {
            break;
        }
        remaining -= width;
        start = index;
    }
    let mut used = 0;
    for (index, character) in input.value[start..].char_indices() {
        let width = if masked {
            1
        } else {
            character.width().unwrap_or(0)
        };
        if used + width > column.min(available) {
            return start + index;
        }
        used += width;
    }
    input.value.len()
}

pub(super) fn render_hover(frame: &mut Frame, state: &SetupState, hits: &[Hitbox]) {
    let now = std::time::Instant::now();
    for hit in hits {
        let progress = state.mouse.progress(hit.target, now);
        if progress <= 0.0 {
            continue;
        }
        let deleting = matches!(
            hit.target,
            SetupTarget::Custom(CustomFocus::DeleteModel(_))
                | SetupTarget::Delete(DeleteFocus::Confirm)
        );
        let target = if deleting {
            (100, 30, 45)
        } else {
            (15, 65, 85)
        };
        let mix = |base: u8, end: u8| {
            (f32::from(base) + (f32::from(end) - f32::from(base)) * progress) as u8
        };
        let color = Color::Rgb(mix(2, target.0), mix(6, target.1), mix(23, target.2));
        for x in hit.rect.left()..hit.rect.right() {
            if let Some(cell) = frame.buffer_mut().cell_mut((x, hit.rect.y)) {
                cell.set_bg(color);
                if deleting {
                    cell.set_fg(Color::Rgb(255, 110, 120));
                }
            }
        }
    }
    let is_option = |hit: &&Hitbox| {
        hit.input.is_none()
            && !matches!(hit.target, SetupTarget::Custom(CustomFocus::DeleteModel(_)))
    };
    if let Some(hovered) = hits
        .iter()
        .filter(is_option)
        .find(|hit| Some(hit.target) == state.mouse.hovered())
    {
        for hit in hits.iter().filter(is_option) {
            if let Some(cell) = frame.buffer_mut().cell_mut((hit.rect.x, hit.rect.y))
                && cell.symbol() == "›"
            {
                cell.set_symbol(" ");
            }
        }
        if let Some(cell) = frame
            .buffer_mut()
            .cell_mut((hovered.rect.x, hovered.rect.y))
        {
            cell.set_symbol("›").set_fg(ACCENT_COLOR);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend};

    #[test]
    fn centered_welcome_has_only_one_arrow_when_hovering_exit() {
        for width in [79, 80, 99, 100] {
            let catalog = ProviderCatalog::embedded().unwrap();
            let mut state = SetupState::welcome(&catalog);
            let layout = prepare(&state, Rect::new(0, 0, width, 30));
            let hits = hitboxes(&state, &layout);
            let exit = hits
                .iter()
                .find(|hit| hit.target == SetupTarget::Welcome(WelcomeFocus::Exit))
                .unwrap();
            state.update_mouse(mouse_action(
                &state,
                MouseAction::Move {
                    column: exit.rect.x,
                    row: exit.rect.y,
                },
                width,
                30,
            ));
            let mut terminal = Terminal::new(TestBackend::new(width, 30)).unwrap();
            terminal
                .draw(|frame| render(frame, &state, &catalog))
                .unwrap();
            let arrows = terminal
                .backend()
                .buffer()
                .content
                .iter()
                .filter(|cell| cell.symbol() == "›")
                .count();
            assert_eq!(arrows, 1, "terminal width {width}");
        }
    }

    #[test]
    fn option_arrow_follows_hover_and_returns_to_keyboard_selection_on_leave() {
        let catalog = ProviderCatalog::embedded().unwrap();
        let mut state = SetupState::welcome(&catalog);
        state.update(crate::event::KeyAction::Submit);
        let layout = prepare(&state, Rect::new(0, 0, 100, 30));
        let hits = hitboxes(&state, &layout);
        let first = hits[0].rect;
        let second = hits[1].rect;
        state.update_mouse(mouse_action(
            &state,
            MouseAction::Move {
                column: second.x,
                row: second.y,
            },
            100,
            30,
        ));
        let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
        terminal
            .draw(|frame| render(frame, &state, &catalog))
            .unwrap();
        assert_eq!(
            terminal
                .backend()
                .buffer()
                .cell((second.x, second.y))
                .unwrap()
                .symbol(),
            "›"
        );
        assert_eq!(
            terminal
                .backend()
                .buffer()
                .cell((first.x, first.y))
                .unwrap()
                .symbol(),
            " "
        );
        state.update_mouse(SetupMouseAction::Hover(None));
        terminal
            .draw(|frame| render(frame, &state, &catalog))
            .unwrap();
        assert_eq!(
            terminal
                .backend()
                .buffer()
                .cell((first.x, first.y))
                .unwrap()
                .symbol(),
            "›"
        );
    }

    #[test]
    fn clicking_clipped_unicode_and_masked_inputs_uses_byte_boundaries() {
        let mut input = InputState::default();
        input.set("中文abc");
        assert_eq!(cursor_at_column(&input, 20, false, 2), 3);
        assert_eq!(cursor_at_column(&input, 4, false, 0), 6);
        assert_eq!(cursor_at_column(&input, 4, false, 2), 8);
        assert_eq!(cursor_at_column(&input, 20, true, 2), 6);
        input.cursor = 0;
        assert_eq!(cursor_at_column(&input, 20, false, 1), 0);
    }

    #[test]
    fn scrolled_delete_targets_match_rendered_icons_after_notices_and_resize() {
        for (width, height) in [(100, 30), (40, 12), (24, 8)] {
            let catalog = ProviderCatalog::embedded().unwrap();
            let mut state = SetupState::custom_provider(catalog.clone());
            let form = state.custom_form_mut().unwrap();
            for _ in 0..15 {
                form.add_model_row();
            }
            form.focus = CustomFocus::Model(15);
            form.models[15].set("last-model");
            state.error = Some("Validation message that wraps on a narrow terminal.".into());
            let layout = prepare(&state, Rect::new(0, 0, width, height));
            let hits = hitboxes(&state, &layout);
            let hit = hits
                .iter()
                .find(|hit| hit.target == SetupTarget::Custom(CustomFocus::DeleteModel(15)))
                .unwrap();
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal
                .draw(|frame| render(frame, &state, &catalog))
                .unwrap();
            assert_eq!(
                terminal
                    .backend()
                    .buffer()
                    .cell((hit.rect.x, hit.rect.y))
                    .unwrap()
                    .symbol(),
                "×"
            );
            let action = mouse_action(
                &state,
                MouseAction::LeftDown {
                    column: hit.rect.x,
                    row: hit.rect.y,
                },
                width,
                height,
            );
            state.update_mouse(action);
            let form = state.custom_form_mut().unwrap();
            assert_eq!(form.models.len(), 15);
            assert_eq!(form.focus, CustomFocus::Model(14));
        }
    }

    #[test]
    fn hover_does_not_move_input_focus_and_blank_click_does_nothing() {
        let mut state = SetupState::custom_provider(ProviderCatalog::embedded().unwrap());
        let layout = prepare(&state, Rect::new(0, 0, 100, 30));
        let hits = hitboxes(&state, &layout);
        let hit = hits
            .iter()
            .find(|hit| hit.target == SetupTarget::Custom(CustomFocus::Save))
            .unwrap();
        let action = mouse_action(
            &state,
            MouseAction::Move {
                column: hit.rect.x,
                row: hit.rect.y,
            },
            100,
            30,
        );
        state.update_mouse(action);
        assert_eq!(state.custom_form_mut().unwrap().focus, CustomFocus::Name);
        assert!(matches!(
            mouse_action(&state, MouseAction::LeftDown { column: 0, row: 0 }, 100, 30),
            SetupMouseAction::None
        ));
    }
}
