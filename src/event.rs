use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::agent::AgentEvent;

pub enum AppEvent {
    Key(KeyAction),
    Agent(AgentEvent),
}

pub enum KeyAction {
    Quit,
    Submit,
    Newline,
    Char(char),
    Backspace,
    Left,
    Right,
    Up,
    Down,
    None,
}

impl From<KeyEvent> for KeyAction {
    fn from(key: KeyEvent) -> Self {
        match key.code {
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => Self::Quit,
            KeyCode::Char('q') if key.modifiers.is_empty() => Self::Quit,
            KeyCode::Char(char) => Self::Char(char),
            KeyCode::Enter if key.modifiers.contains(KeyModifiers::SHIFT) => Self::Newline,
            KeyCode::Enter => Self::Submit,
            KeyCode::Backspace => Self::Backspace,
            KeyCode::Left => Self::Left,
            KeyCode::Right => Self::Right,
            KeyCode::Up | KeyCode::PageUp => Self::Up,
            KeyCode::Down | KeyCode::PageDown => Self::Down,
            _ => Self::None,
        }
    }
}
