use ratatui::{
    style::{Modifier, Style},
    text::{Line, Span},
};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::app::App;

use super::theme::*;

const MAX_SLASH_COMMAND_ROWS: usize = 5;

struct InputRow {
    text: String,
    start: usize,
}

pub(super) fn input_rows(app: &App, width: u16) -> Vec<Line<'static>> {
    visual_input_rows(&app.input.value, input_content_width(width))
        .into_iter()
        .enumerate()
        .map(|(index, row)| input_row_line(index, row, app.input_selection_range()))
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

pub(super) fn input_content_width(width: u16) -> usize {
    width.saturating_sub(6).max(1) as usize
}

fn visual_input_rows(value: &str, width: usize) -> Vec<InputRow> {
    let width = width.max(1);
    let mut rows = Vec::new();
    let mut row_start = 0;
    let mut row_width = 0;

    for (index, character) in value.char_indices() {
        if character == '\n' {
            rows.push(InputRow {
                text: value[row_start..index].to_owned(),
                start: row_start,
            });
            row_start = index + character.len_utf8();
            row_width = 0;
            continue;
        }

        let character_width = character.width().unwrap_or(0);
        if row_width + character_width > width && row_width > 0 {
            rows.push(InputRow {
                text: value[row_start..index].to_owned(),
                start: row_start,
            });
            row_start = index;
            row_width = 0;
        }
        row_width += character_width;
    }

    rows.push(InputRow {
        text: value[row_start..].to_owned(),
        start: row_start,
    });
    rows
}

fn input_row_line(index: usize, row: InputRow, selection: Option<(usize, usize)>) -> Line<'static> {
    let mut spans = vec![Span::styled(
        if index == 0 { "❯ " } else { "  " },
        Style::default().fg(TEXT_COLOR),
    )];
    push_input_row_spans(&mut spans, &row, selection);
    Line::from(spans)
}

fn push_input_row_spans(
    spans: &mut Vec<Span<'static>>,
    row: &InputRow,
    selection: Option<(usize, usize)>,
) {
    let mut segment = String::new();
    let mut segment_selected = None;
    let (selection_start, selection_end) = selection.unwrap_or((0, 0));

    for (relative_index, character) in row.text.char_indices() {
        let absolute_index = row.start + relative_index;
        let character_end = absolute_index + character.len_utf8();
        let selected = selection_start < character_end && absolute_index < selection_end;

        if segment_selected.is_some_and(|current| current != selected) {
            push_input_span(
                spans,
                std::mem::take(&mut segment),
                segment_selected.unwrap_or(false),
            );
        }

        segment_selected = Some(selected);
        segment.push(character);
    }
    push_input_span(spans, segment, segment_selected.unwrap_or(false));
}

fn push_input_span(spans: &mut Vec<Span<'static>>, content: String, selected: bool) {
    if content.is_empty() {
        return;
    }
    let style = if selected {
        Style::default().fg(BG_COLOR).bg(ACCENT_COLOR)
    } else {
        Style::default().fg(TEXT_COLOR)
    };
    spans.push(Span::styled(content, style));
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
                Span::styled(command.name.clone(), style),
                Span::styled(
                    " ".repeat(name_width.saturating_sub(command.name.width()) + 2),
                    style,
                ),
                Span::styled(command.description.clone(), style),
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

    #[test]
    fn visual_input_rows_track_wrapped_byte_offsets() {
        let rows = visual_input_rows("abcdef", 3);

        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].text, "abc");
        assert_eq!(rows[0].start, 0);
        assert_eq!(rows[1].text, "def");
        assert_eq!(rows[1].start, 3);
    }

    #[test]
    fn input_row_line_highlights_selected_text_only() {
        let row = InputRow {
            text: "hello".to_owned(),
            start: 0,
        };
        let line = input_row_line(0, row, Some((1, 4)));
        let spans = line
            .spans
            .iter()
            .map(|span| (span.content.as_ref(), span.style.fg, span.style.bg))
            .collect::<Vec<_>>();

        assert_eq!(
            spans,
            vec![
                ("❯ ", Some(TEXT_COLOR), None),
                ("h", Some(TEXT_COLOR), None),
                ("ell", Some(BG_COLOR), Some(ACCENT_COLOR)),
                ("o", Some(TEXT_COLOR), None),
            ]
        );
    }
}
