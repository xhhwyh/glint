use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind};

use crate::agent::AgentEvent;

pub enum AppEvent {
    Key(KeyAction),
    Mouse(MouseAction),
    Agent(AgentEvent),
}

pub enum MouseAction {
    ScrollUp,
    ScrollDown,
    None,
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

impl From<MouseEvent> for MouseAction {
    fn from(event: MouseEvent) -> Self {
        match event.kind {
            MouseEventKind::ScrollUp => Self::ScrollUp,
            MouseEventKind::ScrollDown => Self::ScrollDown,
            _ => Self::None,
        }
    }
}
