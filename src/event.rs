use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

use crate::agent::AgentEvent;

pub enum AppEvent {
    Key(KeyInput),
    Mouse(MouseAction),
    Agent(AgentEvent),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseAction {
    LeftDown { column: u16, row: u16 },
    LeftDrag { column: u16, row: u16 },
    LeftUp { column: u16, row: u16 },
    ScrollUp { row: u16 },
    ScrollDown { row: u16 },
    None,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyAction {
    Quit,
    ForceQuit,
    ToggleTerminalFocus,
    NewTerminalTab,
    CloseTerminalTab,
    SelectTerminalTab(usize),
    Submit,
    Newline,
    Char(char),
    Backspace,
    Delete,
    Cut,
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
            KeyCode::Char('x') if key.modifiers.contains(KeyModifiers::CONTROL) => KeyAction::Cut,
            KeyCode::Char('k') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                KeyAction::CancelConversationPermission
            }
            KeyCode::Char('t') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                KeyAction::ToggleTerminalFocus
            }
            KeyCode::Char('n') if key.modifiers.contains(KeyModifiers::ALT) => {
                KeyAction::NewTerminalTab
            }
            KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::ALT) => {
                KeyAction::CloseTerminalTab
            }
            KeyCode::Char(char)
                if key.modifiers.contains(KeyModifiers::ALT) && matches!(char, '1'..='9') =>
            {
                KeyAction::SelectTerminalTab(terminal_tab_index(char))
            }
            KeyCode::Char(char) => KeyAction::Char(char),
            KeyCode::Enter if key.modifiers.contains(KeyModifiers::SHIFT) => KeyAction::Newline,
            KeyCode::Enter => KeyAction::Submit,
            KeyCode::Tab => KeyAction::Tab,
            KeyCode::Esc => KeyAction::Escape,
            KeyCode::Backspace => KeyAction::Backspace,
            KeyCode::Delete => KeyAction::Delete,
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

fn terminal_tab_index(char: char) -> usize {
    char.to_digit(10).unwrap_or(1).saturating_sub(1) as usize
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
            MouseEventKind::Down(MouseButton::Left) => Self::LeftDown {
                column: event.column,
                row: event.row,
            },
            MouseEventKind::Drag(MouseButton::Left) => Self::LeftDrag {
                column: event.column,
                row: event.row,
            },
            MouseEventKind::Up(MouseButton::Left) => Self::LeftUp {
                column: event.column,
                row: event.row,
            },
            MouseEventKind::ScrollUp => Self::ScrollUp { row: event.row },
            MouseEventKind::ScrollDown => Self::ScrollDown { row: event.row },
            _ => Self::None,
        }
    }
}

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyCode, KeyEvent, MouseEvent};

    use super::*;

    #[test]
    fn alt_n_creates_terminal_tab() {
        let input = KeyInput::from(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::ALT));

        assert_eq!(input.action, KeyAction::NewTerminalTab);
    }

    #[test]
    fn alt_d_closes_terminal_tab() {
        let input = KeyInput::from(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::ALT));

        assert_eq!(input.action, KeyAction::CloseTerminalTab);
    }

    #[test]
    fn alt_number_selects_terminal_tab_index() {
        let input = KeyInput::from(KeyEvent::new(KeyCode::Char('3'), KeyModifiers::ALT));

        assert_eq!(input.action, KeyAction::SelectTerminalTab(2));
    }

    #[test]
    fn alt_zero_does_not_select_terminal_tab() {
        let input = KeyInput::from(KeyEvent::new(KeyCode::Char('0'), KeyModifiers::ALT));

        assert_eq!(input.action, KeyAction::Char('0'));
    }

    #[test]
    fn ctrl_x_cuts_selection() {
        let input = KeyInput::from(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::CONTROL));

        assert_eq!(input.action, KeyAction::Cut);
    }

    #[test]
    fn delete_key_deletes_forward() {
        let input = KeyInput::from(KeyEvent::new(KeyCode::Delete, KeyModifiers::empty()));

        assert_eq!(input.action, KeyAction::Delete);
    }

    #[test]
    fn left_mouse_events_track_selection_coordinates() {
        let action = MouseAction::from(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 8,
            row: 3,
            modifiers: KeyModifiers::empty(),
        });

        assert_eq!(action, MouseAction::LeftDown { column: 8, row: 3 });

        let action = MouseAction::from(MouseEvent {
            kind: MouseEventKind::Drag(MouseButton::Left),
            column: 12,
            row: 5,
            modifiers: KeyModifiers::empty(),
        });

        assert_eq!(action, MouseAction::LeftDrag { column: 12, row: 5 });

        let action = MouseAction::from(MouseEvent {
            kind: MouseEventKind::Up(MouseButton::Left),
            column: 14,
            row: 6,
            modifiers: KeyModifiers::empty(),
        });

        assert_eq!(action, MouseAction::LeftUp { column: 14, row: 6 });
    }
}
