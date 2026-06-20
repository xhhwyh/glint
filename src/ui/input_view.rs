use ratatui::{
    style::{Modifier, Style},
    text::{Line, Span},
};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::app::App;

use super::{layout::wrap_text, theme::*};

const MAX_SLASH_COMMAND_ROWS: usize = 5;

pub(super) fn input_rows(value: &str, width: u16) -> Vec<String> {
    wrap_text(value, input_content_width(width) as u16)
        .into_iter()
        .enumerate()
        .map(|(index, row)| format!("{}{}", if index == 0 { "❯ " } else { "  " }, row))
        .collect()
}

pub(super) fn input_cursor_position(app: &App, width: u16) -> (u16, u16) {
    let content_width = input_content_width(width);
    let mut row = 0;
    let mut column = 0;

    for char in app.input.value[..app.input.cursor].chars() {
        if char == '\n' {
            row += 1;
            column = 0;
            continue;
        }

        let char_width = char.width().unwrap_or(0);
        if column + char_width > content_width && column > 0 {
            row += 1;
            column = 0;
        }
        column += char_width;
    }

    (column as u16 + 4, row as u16)
}

fn input_content_width(width: u16) -> usize {
    width.saturating_sub(6).max(1) as usize
}

pub(super) fn slash_command_lines(app: &App, _width: u16) -> Vec<Line<'static>> {
    let matches = app.slash_command_matches();
    if matches.is_empty() {
        return vec![Line::from(vec![
            Span::raw("  "),
            Span::styled(
                "No matching slash command",
                Style::default().fg(MUTED_TEXT_COLOR),
            ),
        ])];
    }

    let name_width = matches
        .iter()
        .map(|command| command.name.width())
        .max()
        .unwrap_or(0);

    let start = slash_command_window_start(
        app.slash_command_selection,
        matches.len(),
        MAX_SLASH_COMMAND_ROWS,
    );
    let end = (start + MAX_SLASH_COMMAND_ROWS).min(matches.len());

    matches[start..end]
        .iter()
        .enumerate()
        .map(|(offset, command)| {
            let index = start + offset;
            let selected = index == app.slash_command_selection;
            let style = if selected {
                Style::default()
                    .fg(ACCENT_COLOR)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(MUTED_TEXT_COLOR)
            };
            Line::from(vec![
                Span::raw("  "),
                Span::styled(if selected { "❯ " } else { "  " }, style),
                Span::styled(command.name, style),
                Span::styled(
                    " ".repeat(name_width.saturating_sub(command.name.width()) + 2),
                    style,
                ),
                Span::styled(command.description, style),
            ])
        })
        .collect()
}

fn slash_command_window_start(selected: usize, len: usize, height: usize) -> usize {
    if len <= height {
        return 0;
    }
    selected
        .min(len - 1)
        .saturating_sub(height - 1)
        .min(len - height)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slash_command_window_shows_five_rows_and_tracks_selection() {
        assert_eq!(slash_command_window_start(0, 7, 5), 0);
        assert_eq!(slash_command_window_start(4, 7, 5), 0);
        assert_eq!(slash_command_window_start(5, 7, 5), 1);
        assert_eq!(slash_command_window_start(6, 7, 5), 2);
        assert_eq!(slash_command_window_start(6, 5, 5), 0);
    }
}
