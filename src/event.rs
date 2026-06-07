use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::agent::AgentEvent;

pub enum AppEvent {
    Key(KeyAction),
    Agent(AgentEvent),
}

pub enum KeyAction {
    Quit,
    Submit,
    Char(char),
    Backspace,
    ScrollUp,
    ScrollDown,
    None,
}

impl From<KeyEvent> for KeyAction {
    fn from(key: KeyEvent) -> Self {
        match key.code {
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => Self::Quit,
            KeyCode::Char('q') if key.modifiers.is_empty() => Self::Quit,
            KeyCode::Char(char) => Self::Char(char),
            KeyCode::Enter => Self::Submit,
            KeyCode::Backspace => Self::Backspace,
            KeyCode::Up | KeyCode::PageUp => Self::ScrollUp,
            KeyCode::Down | KeyCode::PageDown => Self::ScrollDown,
            _ => Self::None,
        }
    }
}
