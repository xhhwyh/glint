use std::time::{Duration, Instant};

use super::{
    BuiltinFocus, ChatGptFocus, CustomFocus, DeleteFocus, SetupEffect, SetupScreen, SetupState,
    WelcomeFocus,
};
use crate::{event::KeyAction, input::InputState};

const TRANSITION: Duration = Duration::from_millis(150);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SetupTarget {
    Welcome(WelcomeFocus),
    Provider(usize),
    Builtin(BuiltinFocus),
    ChatGpt(ChatGptFocus),
    Custom(CustomFocus),
    Delete(DeleteFocus),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SetupMouseAction {
    Hover(Option<SetupTarget>),
    Click {
        target: SetupTarget,
        cursor: Option<usize>,
    },
    Scroll(isize),
    None,
}

#[derive(Clone, Debug, Default)]
pub struct SetupMouseState {
    hovered: Option<SetupTarget>,
    transitions: Vec<(SetupTarget, f32, f32, Instant)>,
}

impl SetupMouseState {
    pub fn hovered(&self) -> Option<SetupTarget> {
        self.hovered
    }

    pub fn progress(&self, target: SetupTarget, now: Instant) -> f32 {
        self.transitions
            .iter()
            .find(|(item, ..)| *item == target)
            .map(|(_, from, to, started)| {
                let t = (now.saturating_duration_since(*started).as_secs_f32()
                    / TRANSITION.as_secs_f32())
                .min(1.0);
                from + (to - from) * t
            })
            .unwrap_or(if self.hovered == Some(target) {
                1.0
            } else {
                0.0
            })
    }

    fn hover(&mut self, target: Option<SetupTarget>, now: Instant) {
        if self.hovered == target {
            return;
        }
        let old = self.hovered;
        let changes = [
            old.map(|item| (item, self.progress(item, now), 0.0)),
            target.map(|item| (item, self.progress(item, now), 1.0)),
        ];
        self.transitions.retain(|(item, _, _, started)| {
            Some(*item) != old
                && Some(*item) != target
                && now.saturating_duration_since(*started) < TRANSITION
        });
        self.transitions.extend(
            changes
                .into_iter()
                .flatten()
                .map(|(item, from, to)| (item, from, to, now)),
        );
        self.hovered = target;
    }
}

impl SetupState {
    pub fn target_input(&self, target: SetupTarget) -> Option<(&InputState, bool)> {
        match (&self.screen, target) {
            (SetupScreen::Builtin(form), SetupTarget::Builtin(BuiltinFocus::ApiKey)) => {
                Some((&form.api_key, true))
            }
            (SetupScreen::Custom(form), SetupTarget::Custom(focus)) => match focus {
                CustomFocus::Name if !form.name_is_read_only() => Some((&form.name, false)),
                CustomFocus::BaseUrl => Some((&form.base_url, false)),
                CustomFocus::ApiKey => Some((&form.api_key, true)),
                CustomFocus::Model(index) => form.models.get(index).map(|input| (input, false)),
                _ => None,
            },
            _ => None,
        }
    }

    pub fn update_mouse(&mut self, action: SetupMouseAction) -> Option<SetupEffect> {
        match action {
            SetupMouseAction::Hover(target) => self.mouse.hover(target, Instant::now()),
            SetupMouseAction::Click { target, cursor } => {
                let input = self.target_input(target).is_some();
                match (&mut self.screen, target) {
                    (SetupScreen::Welcome(state), SetupTarget::Welcome(focus)) => {
                        state.focus = focus
                    }
                    (SetupScreen::Providers(list), SetupTarget::Provider(index))
                        if index < list.rows.len() =>
                    {
                        list.focus = index
                    }
                    (SetupScreen::Builtin(form), SetupTarget::Builtin(focus)) => form.focus = focus,
                    (SetupScreen::ChatGpt(form), SetupTarget::ChatGpt(focus)) => form.focus = focus,
                    (SetupScreen::Custom(form), SetupTarget::Custom(focus)) => {
                        if matches!(focus, CustomFocus::Name) && form.name_is_read_only() {
                            return None;
                        }
                        if let CustomFocus::Model(i) | CustomFocus::DeleteModel(i) = focus
                            && i >= form.models.len()
                        {
                            return None;
                        }
                        form.focus = focus;
                    }
                    (SetupScreen::ConfirmDelete(state), SetupTarget::Delete(focus)) => {
                        state.focus = focus
                    }
                    _ => return None,
                }
                self.mouse = SetupMouseState::default();
                if input {
                    let field = match (&mut self.screen, target) {
                        (SetupScreen::Builtin(form), _) => &mut form.api_key,
                        (SetupScreen::Custom(form), SetupTarget::Custom(focus)) => match focus {
                            CustomFocus::Name => &mut form.name,
                            CustomFocus::BaseUrl => &mut form.base_url,
                            CustomFocus::ApiKey => &mut form.api_key,
                            CustomFocus::Model(index) => &mut form.models[index],
                            _ => return None,
                        },
                        _ => return None,
                    };
                    if let Some(cursor) =
                        cursor.filter(|cursor| field.value.is_char_boundary(*cursor))
                    {
                        field.cursor = cursor;
                    }
                } else {
                    return self.update(KeyAction::Submit);
                }
            }
            SetupMouseAction::Scroll(direction) => {
                return self.update(if direction < 0 {
                    KeyAction::Up
                } else {
                    KeyAction::Down
                });
            }
            SetupMouseAction::None => {}
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn hover_fades_without_changing_keyboard_focus() {
        let now = Instant::now();
        let target = SetupTarget::Welcome(WelcomeFocus::Exit);
        let mut hover = SetupMouseState::default();
        hover.hover(Some(target), now);
        assert_eq!(hover.progress(target, now), 0.0);
        assert!((hover.progress(target, now + Duration::from_millis(75)) - 0.5).abs() < 0.01);
        hover.hover(None, now + TRANSITION);
        assert_eq!(hover.progress(target, now + TRANSITION * 2), 0.0);
    }
}
