use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

use crate::agent::AgentEvent;

pub enum AppEvent {
    Key(KeyInput),
    Mouse(MouseAction),
    ExtensionMouse(ExtensionMouseAction),
    Agent(AgentEvent),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExtensionMouseAction {
    Resume(ResumeMouseAction),
    Mcp(McpMouseAction),
    Plugins(PluginsMouseAction),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResumeMouseAction {
    SelectSession(usize),
    MoveSelection(isize),
    None,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum McpMouseAction {
    SelectServer(usize),
    OpenSelected,
    MoveServerSelection(isize),
    ScrollDetails(isize),
    None,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PluginsMouseAction {
    SelectTab(PluginsMouseTab),
    SelectItem(usize),
    MoveSelection(isize),
    OpenSelected,
    ScrollDetails(isize),
    None,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PluginsMouseTab {
    Installed,
    Marketplaces,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseAction {
    Move { column: u16, row: u16 },
    LeftDown { column: u16, row: u16 },
    LeftDrag { column: u16, row: u16 },
    LeftUp { column: u16, row: u16 },
    ScrollUp { column: u16, row: u16 },
    ScrollDown { column: u16, row: u16 },
    None,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyAction {
    Quit,
    ForceQuit,
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
    CtrlUp,
    CtrlDown,
    Tab,
    Escape,
    CancelConversationPermission,
    None,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyInput {
    pub action: KeyAction,
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
            KeyCode::Char(char) => KeyAction::Char(char),
            KeyCode::Enter if key.modifiers.contains(KeyModifiers::SHIFT) => KeyAction::Newline,
            KeyCode::Enter => KeyAction::Submit,
            KeyCode::Tab => KeyAction::Tab,
            KeyCode::Esc => KeyAction::Escape,
            KeyCode::Backspace => KeyAction::Backspace,
            KeyCode::Delete => KeyAction::Delete,
            KeyCode::Left => KeyAction::Left,
            KeyCode::Right => KeyAction::Right,
            KeyCode::Up if key.modifiers.contains(KeyModifiers::CONTROL) => KeyAction::CtrlUp,
            KeyCode::Down if key.modifiers.contains(KeyModifiers::CONTROL) => KeyAction::CtrlDown,
            KeyCode::Up | KeyCode::PageUp => KeyAction::Up,
            KeyCode::Down | KeyCode::PageDown => KeyAction::Down,
            _ => KeyAction::None,
        };

        Self { action }
    }
}

impl From<MouseEvent> for MouseAction {
    fn from(event: MouseEvent) -> Self {
        match event.kind {
            MouseEventKind::Moved => Self::Move {
                column: event.column,
                row: event.row,
            },
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
            MouseEventKind::ScrollUp => Self::ScrollUp {
                column: event.column,
                row: event.row,
            },
            MouseEventKind::ScrollDown => Self::ScrollDown {
                column: event.column,
                row: event.row,
            },
            _ => Self::None,
        }
    }
}

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyCode, KeyEvent, MouseEvent};

    use super::*;

    #[test]
    fn former_terminal_shortcuts_are_regular_chat_input() {
        let ctrl_t = KeyInput::from(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL));
        let alt_n = KeyInput::from(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::ALT));

        assert_eq!(ctrl_t.action, KeyAction::Char('t'));
        assert_eq!(alt_n.action, KeyAction::Char('n'));
    }

    #[test]
    fn ctrl_x_cuts_selection() {
        let input = KeyInput::from(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::CONTROL));

        assert_eq!(input.action, KeyAction::Cut);
    }

    #[test]
    fn ctrl_c_keeps_quit_semantics() {
        let input = KeyInput::from(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));

        assert_eq!(input.action, KeyAction::Quit);
    }

    #[test]
    fn delete_key_deletes_forward() {
        let input = KeyInput::from(KeyEvent::new(KeyCode::Delete, KeyModifiers::empty()));

        assert_eq!(input.action, KeyAction::Delete);
    }

    #[test]
    fn ctrl_arrows_keep_navigation_actions() {
        let up = KeyInput::from(KeyEvent::new(KeyCode::Up, KeyModifiers::CONTROL));
        let down = KeyInput::from(KeyEvent::new(KeyCode::Down, KeyModifiers::CONTROL));

        assert_eq!(up.action, KeyAction::CtrlUp);
        assert_eq!(down.action, KeyAction::CtrlDown);
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

    #[test]
    fn scroll_mouse_events_keep_pointer_coordinates() {
        let action = MouseAction::from(MouseEvent {
            kind: MouseEventKind::ScrollUp,
            column: 8,
            row: 3,
            modifiers: KeyModifiers::empty(),
        });

        assert_eq!(action, MouseAction::ScrollUp { column: 8, row: 3 });
    }

    #[test]
    fn mouse_move_keeps_pointer_coordinates() {
        let action = MouseAction::from(MouseEvent {
            kind: MouseEventKind::Moved,
            column: 17,
            row: 9,
            modifiers: KeyModifiers::empty(),
        });

        assert_eq!(action, MouseAction::Move { column: 17, row: 9 });
    }
}
