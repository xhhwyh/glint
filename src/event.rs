use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind};

use crate::agent::AgentEvent;

pub enum AppEvent {
    Key(KeyInput),
    Mouse(MouseAction),
    Agent(AgentEvent),
}

pub enum MouseAction {
    ScrollUp { row: u16 },
    ScrollDown { row: u16 },
    None,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyAction {
    Quit,
    ForceQuit,
    ToggleTerminalFocus,
    Submit,
    Newline,
    Char(char),
    Backspace,
    Left,
    Right,
    Up,
    Down,
    Tab,
    Escape,
    CancelConversationPermission,
    None,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyInput {
    pub action: KeyAction,
    pub terminal_input: Option<Vec<u8>>,
}

impl From<KeyEvent> for KeyInput {
    fn from(key: KeyEvent) -> Self {
        let action = match key.code {
            KeyCode::Char('q') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                KeyAction::ForceQuit
            }
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => KeyAction::Quit,
            KeyCode::Char('k') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                KeyAction::CancelConversationPermission
            }
            KeyCode::Char('t') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                KeyAction::ToggleTerminalFocus
            }
            KeyCode::Char(char) => KeyAction::Char(char),
            KeyCode::Enter if key.modifiers.contains(KeyModifiers::SHIFT) => KeyAction::Newline,
            KeyCode::Enter => KeyAction::Submit,
            KeyCode::Tab => KeyAction::Tab,
            KeyCode::Esc => KeyAction::Escape,
            KeyCode::Backspace => KeyAction::Backspace,
            KeyCode::Left => KeyAction::Left,
            KeyCode::Right => KeyAction::Right,
            KeyCode::Up | KeyCode::PageUp => KeyAction::Up,
            KeyCode::Down | KeyCode::PageDown => KeyAction::Down,
            _ => KeyAction::None,
        };

        Self {
            action,
            terminal_input: terminal_input_bytes(key),
        }
    }
}

fn terminal_input_bytes(key: KeyEvent) -> Option<Vec<u8>> {
    let bytes = match key.code {
        KeyCode::Backspace => vec![0x7f],
        KeyCode::Enter => b"\r".to_vec(),
        KeyCode::Left => escape_sequence(b"\x1b[D", key.modifiers),
        KeyCode::Right => escape_sequence(b"\x1b[C", key.modifiers),
        KeyCode::Up => escape_sequence(b"\x1b[A", key.modifiers),
        KeyCode::Down => escape_sequence(b"\x1b[B", key.modifiers),
        KeyCode::Home => escape_sequence(b"\x1b[H", key.modifiers),
        KeyCode::End => escape_sequence(b"\x1b[F", key.modifiers),
        KeyCode::PageUp => escape_sequence(b"\x1b[5~", key.modifiers),
        KeyCode::PageDown => escape_sequence(b"\x1b[6~", key.modifiers),
        KeyCode::Delete => escape_sequence(b"\x1b[3~", key.modifiers),
        KeyCode::Tab => b"\t".to_vec(),
        KeyCode::Esc => b"\x1b".to_vec(),
        KeyCode::Char(char) if key.modifiers.contains(KeyModifiers::CONTROL) => control_char(char)?,
        KeyCode::Char(char) => char_bytes(char, key.modifiers),
        _ => return None,
    };

    Some(bytes)
}

fn char_bytes(char: char, modifiers: KeyModifiers) -> Vec<u8> {
    let mut bytes = Vec::new();
    if modifiers.contains(KeyModifiers::ALT) {
        bytes.push(0x1b);
    }
    let mut encoded = [0; 4];
    bytes.extend_from_slice(char.encode_utf8(&mut encoded).as_bytes());
    bytes
}

fn escape_sequence(sequence: &[u8], modifiers: KeyModifiers) -> Vec<u8> {
    let mut bytes = Vec::new();
    if modifiers.contains(KeyModifiers::ALT) {
        bytes.push(0x1b);
    }
    bytes.extend_from_slice(sequence);
    bytes
}

fn control_char(char: char) -> Option<Vec<u8>> {
    let lower = char.to_ascii_lowercase();
    if lower.is_ascii_lowercase() {
        Some(vec![lower as u8 - b'a' + 1])
    } else if char == ' ' {
        Some(vec![0])
    } else {
        None
    }
}

impl From<MouseEvent> for MouseAction {
    fn from(event: MouseEvent) -> Self {
        match event.kind {
            MouseEventKind::ScrollUp => Self::ScrollUp { row: event.row },
            MouseEventKind::ScrollDown => Self::ScrollDown { row: event.row },
            _ => Self::None,
        }
    }
}
