mod approval;
mod dashboard;
mod format;
mod input_view;
mod layout;
mod markdown;
mod model_picker;
mod resume;
mod star;
mod status;
mod status_bar;
mod terminal;
mod theme;
mod transcript_view;

use ratatui::{
    Frame,
    layout::{Constraint, Layout, Position, Rect},
    style::Style,
    text::{Line, Span},
    widgets::Paragraph,
};
use unicode_width::UnicodeWidthChar;

use crate::app::{App, TextSelection};
use crate::approval::ApprovalFocus;

use layout::box_top;
use theme::{ACCENT_COLOR, BG_COLOR};

pub use terminal::{terminal_content_width, terminal_height_for_app};

struct Document {
    lines: Vec<Line<'static>>,
    cursor_x: u16,
    cursor_y: u16,
}

pub fn render(frame: &mut Frame, app: &App) {
    if let Some(view) = &app.status_view {
        status::render_status_view(frame, app, view);
        return;
    }
    if let Some(picker) = &app.resume_picker {
        resume::render_resume_picker(frame, picker);
        return;
    }

    let terminal_height = terminal::terminal_height_for_app(app, frame.area().height);
    if terminal_height == 0 {
        render_document(frame, app, frame.area());
        return;
    }

    let chunks = Layout::vertical([Constraint::Min(1), Constraint::Length(terminal_height)])
        .split(frame.area());
    render_document(frame, app, chunks[0]);
    terminal::render_terminal(frame, app, chunks[1]);
}

fn render_document(frame: &mut Frame, app: &App, area: Rect) {
    let width = area.width.max(1);
    let mut document = document(app, width);
    let scroll = document_scroll_for_len(document.lines.len(), app.scroll, area.height);
    document.lines = apply_text_selection(document.lines, app.text_selection, width);

    frame.render_widget(
        Paragraph::new(document.lines)
            .scroll((scroll, 0))
            .style(Style::default().bg(BG_COLOR)),
        area,
    );
    if let Some(cursor_y) = visible_cursor_y(document.cursor_y, scroll, area.height) {
        frame.set_cursor_position(Position::new(
            area.x + document.cursor_x.min(width.saturating_sub(1)),
            area.y + cursor_y,
        ));
    }
}

pub fn document_scroll_top(app: &App, width: u16, height: u16) -> u16 {
    let document = document(app, width.max(1));
    document_scroll_for_len(document.lines.len(), app.scroll, height)
}

pub fn selected_text(app: &App, width: u16) -> Option<String> {
    let selection = app.text_selection?;
    let document = document(app, width.max(1));
    selected_text_from_lines(&document.lines, selection, width.max(1))
}

fn visible_cursor_y(cursor_y: u16, scroll: u16, height: u16) -> Option<u16> {
    if height == 0 || cursor_y < scroll {
        return None;
    }

    let visible_y = cursor_y - scroll;
    (visible_y < height).then_some(visible_y)
}

fn document_scroll_for_len(line_count: usize, scroll_offset: u16, height: u16) -> u16 {
    let max_scroll = line_count.saturating_sub(height as usize) as u16;
    max_scroll.saturating_sub(scroll_offset)
}

fn document(app: &App, width: u16) -> Document {
    let mut lines = dashboard::idle_panel_lines(app, width);
    let has_status_line = app.processing_elapsed().is_some()
        || app.run_notice.is_some()
        || app.last_turn_duration().is_some();

    lines.extend(transcript_view::transcript_lines(app, width));
    if !app.messages.is_empty() && !has_status_line {
        lines.push(Line::from(""));
    }

    let mut approval_cursor = None;
    if app.approval.is_some() {
        lines.extend(approval::approval_lines(app, width));
        if matches!(
            app.approval.as_ref().map(|approval| &approval.focus),
            Some(ApprovalFocus::Feedback)
        ) {
            approval_cursor = Some((
                approval::approval_feedback_cursor_x(app, width),
                lines.len() as u16 - 2,
            ));
        }
        lines.push(Line::from(""));
    }

    let mut needs_status_gap = false;
    if let Some(elapsed) = app.processing_elapsed() {
        lines.push(transcript_view::processing_line(elapsed));
        needs_status_gap = true;
    } else {
        if let Some(notice) = app.run_notice.as_deref() {
            lines.push(transcript_view::notice_line(notice));
        }
        if let Some(duration) = app.last_turn_duration() {
            lines.push(transcript_view::turn_duration_line(duration));
            needs_status_gap = true;
        }
    }
    if needs_status_gap {
        lines.push(Line::from(""));
    }

    let input_y = lines.len() as u16;
    lines.push(box_top("COMPOSER", width));
    lines.extend(
        input_view::input_rows(&app.input.value, width)
            .into_iter()
            .map(|row| layout::box_input_body(&row, width)),
    );
    lines.push(layout::box_bottom(width));
    if app.model_picker.is_some() {
        lines.extend(model_picker::model_picker_lines(app, width));
    } else if app.slash_menu_visible() {
        lines.extend(input_view::slash_command_lines(app, width));
    } else {
        lines.push(status_bar::info_line(app, width));
        lines.push(status_bar::context_line(app, width));
        if let Some(line) = status_bar::permission_line(app) {
            lines.push(line);
        }
    }

    let (input_cursor_x, input_cursor_row) = input_view::input_cursor_position(app, width);
    let (cursor_x, cursor_y) =
        approval_cursor.unwrap_or((input_cursor_x, input_y + input_cursor_row + 1));
    Document {
        lines,
        cursor_x,
        cursor_y,
    }
}

fn apply_text_selection(
    lines: Vec<Line<'static>>,
    selection: Option<TextSelection>,
    width: u16,
) -> Vec<Line<'static>> {
    let Some(selection) = selection else {
        return lines;
    };
    lines
        .into_iter()
        .enumerate()
        .map(|(row, line)| {
            let Some((start, end)) = selection_columns_for_row(row as u16, selection, width) else {
                return line;
            };
            highlight_line_selection(line, start, end)
        })
        .collect()
}

fn selected_text_from_lines(
    lines: &[Line<'static>],
    selection: TextSelection,
    width: u16,
) -> Option<String> {
    let (start, end) = selection.ordered()?;
    if lines.is_empty() || start.row as usize >= lines.len() {
        return None;
    }

    let end_row = end.row.min((lines.len() - 1) as u16);
    let rows = (start.row..=end_row)
        .map(|row| {
            let Some((start_column, end_column)) = selection_columns_for_row(row, selection, width)
            else {
                return String::new();
            };
            selected_text_from_line(&lines[row as usize], start_column, end_column)
        })
        .collect::<Vec<_>>();
    let text = rows.join("\n");

    text.chars()
        .any(|character| character != '\n')
        .then_some(text)
}

fn selected_text_from_line(line: &Line<'static>, start_column: u16, end_column: u16) -> String {
    let mut selected = String::new();
    let mut column = 0usize;
    let start_column = start_column as usize;
    let end_column = end_column as usize;

    for span in &line.spans {
        for character in span.content.chars() {
            let character_width = character.width().unwrap_or(0);
            let character_end = column + character_width;
            if character_end > start_column && column < end_column {
                selected.push(character);
            }
            column = character_end;
        }
    }

    selected
}

fn selection_columns_for_row(row: u16, selection: TextSelection, width: u16) -> Option<(u16, u16)> {
    let (start, end) = selection.ordered()?;
    if row < start.row || row > end.row {
        return None;
    }

    let start_column = if row == start.row { start.column } else { 0 }.min(width);
    let end_column = if row == end.row {
        end.column.saturating_add(1)
    } else {
        width
    }
    .min(width);

    (end_column > start_column).then_some((start_column, end_column))
}

fn highlight_line_selection(
    line: Line<'static>,
    start_column: u16,
    end_column: u16,
) -> Line<'static> {
    let Line {
        style,
        alignment,
        spans,
    } = line;
    let mut highlighted = Vec::new();
    let mut column = 0usize;
    let start_column = start_column as usize;
    let end_column = end_column as usize;

    for span in spans {
        let mut segment = String::new();
        let mut segment_selected = None;
        for character in span.content.chars() {
            let character_width = character.width().unwrap_or(0);
            let character_end = column + character_width;
            let selected = character_end > start_column && column < end_column;

            if segment_selected.is_some_and(|current| current != selected) {
                push_selection_span(
                    &mut highlighted,
                    std::mem::take(&mut segment),
                    span.style,
                    segment_selected.unwrap_or(false),
                );
            }

            segment_selected = Some(selected);
            segment.push(character);
            column = character_end;
        }
        push_selection_span(
            &mut highlighted,
            segment,
            span.style,
            segment_selected.unwrap_or(false),
        );
    }

    let blank_start = column.max(start_column);
    if end_column > blank_start {
        highlighted.push(Span::styled(
            " ".repeat(end_column - blank_start),
            selection_style(),
        ));
    }

    Line {
        style,
        alignment,
        spans: highlighted,
    }
}

fn push_selection_span(
    spans: &mut Vec<Span<'static>>,
    content: String,
    style: Style,
    selected: bool,
) {
    if content.is_empty() {
        return;
    }
    let style = if selected {
        style.patch(selection_style())
    } else {
        style
    };
    spans.push(Span::styled(content, style));
}

fn selection_style() -> Style {
    Style::default().fg(BG_COLOR).bg(ACCENT_COLOR)
}

#[cfg(test)]
mod tests {
    use super::format::{cached_suffix, context_bar_percent, context_usage_label};
    use super::layout::truncate_start_to_width;
    use super::model_picker::price_label;
    use super::terminal::{terminal_footer, terminal_tab_window_start};
    use super::theme::KEY_HINT_COLOR;
    use super::transcript_view::tool_output_preview;
    use super::*;
    use crate::app::TextPosition;
    use ratatui::style::{Color, Modifier};

    #[test]
    fn token_labels_show_cached_percentage() {
        assert_eq!(cached_suffix(Some(46)), "(46% cached)");
        assert_eq!(cached_suffix(None), "(— cached)");
        assert_eq!(context_usage_label(8_000, Some(1_000_000)), "0.8% of 1M");
        assert_eq!(context_usage_label(1_280, Some(256_000)), "0.5% of 256K");
        assert_eq!(context_usage_label(37_500, Some(100_000)), "37.5% of 100K");
        assert_eq!(context_usage_label(1_000, Some(65_536)), "1.5% of 65K");
        assert_eq!(context_usage_label(1, None), "—");
        assert_eq!(context_bar_percent(37_500, Some(100_000)), 37);
        assert_eq!(context_bar_percent(1, None), 0);
    }

    #[test]
    fn price_labels_include_provider_unit_when_present() {
        assert_eq!(price_label("input", "1.0", "RMB"), "input 1.0￥");
        assert_eq!(price_label("output", "2.0", "USD"), "output 2.0$");
        assert_eq!(price_label("input", "1.0", ""), "input 1.0");
    }

    #[test]
    fn truncates_paths_from_the_start() {
        assert_eq!(
            truncate_start_to_width("~/projects/glint", 16),
            "~/projects/glint"
        );
        assert_eq!(
            truncate_start_to_width("~/projects/glint", 10),
            "...s/glint"
        );
    }

    #[test]
    fn previews_tool_output_with_omitted_line_count() {
        assert_eq!(tool_output_preview("one\ntwo\nthree"), "one\ntwo\nthree");
        assert_eq!(
            tool_output_preview("one\ntwo\nthree\nfour\nfive\nsix\nseven\neight"),
            "one\ntwo\nthree\n...+5 lines omitted"
        );
    }

    #[test]
    fn terminal_content_width_reserves_tab_column_when_roomy() {
        assert_eq!(terminal_content_width(120), 104);
        assert_eq!(terminal_content_width(30), 26);
    }

    #[test]
    fn terminal_tab_window_tracks_active_tab() {
        assert_eq!(terminal_tab_window_start(0, 3, 6), 0);
        assert_eq!(terminal_tab_window_start(6, 10, 4), 4);
    }

    #[test]
    fn terminal_footer_uses_shared_key_hint_color() {
        let footer = terminal_footer(120);
        let text = footer
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>();

        assert!(text.contains("Ctrl+T switch cursor"));
        for key in ["Ctrl+T", "Alt+N", "Alt+1-9", "Alt+D"] {
            let span = footer
                .spans
                .iter()
                .find(|span| span.content.as_ref() == key)
                .expect("key span");
            assert_eq!(span.style.fg, Some(KEY_HINT_COLOR));
        }
    }

    #[test]
    fn cursor_y_tracks_document_scroll() {
        assert_eq!(visible_cursor_y(10, 0, 20), Some(10));
        assert_eq!(visible_cursor_y(10, 1, 20), Some(9));
        assert_eq!(visible_cursor_y(2, 5, 20), None);
        assert_eq!(visible_cursor_y(30, 1, 12), None);
        assert_eq!(visible_cursor_y(10, 10, 0), None);
    }

    #[test]
    fn text_selection_highlights_requested_columns() {
        let base_style = Style::default().fg(Color::Red).add_modifier(Modifier::BOLD);
        let line = Line::from(vec![Span::styled("hello", base_style)]);

        let highlighted = highlight_line_selection(line, 1, 4);
        let contents = highlighted
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<Vec<_>>();

        assert_eq!(contents, vec!["h", "ell", "o"]);
        assert_eq!(highlighted.spans[0].style.fg, Some(Color::Red));
        assert_eq!(highlighted.spans[1].style.fg, Some(BG_COLOR));
        assert_eq!(highlighted.spans[1].style.bg, Some(ACCENT_COLOR));
        assert!(
            highlighted.spans[1]
                .style
                .add_modifier
                .contains(Modifier::BOLD)
        );
    }

    #[test]
    fn text_selection_columns_cover_multiline_range() {
        let selection = TextSelection {
            anchor: TextPosition { row: 2, column: 3 },
            focus: TextPosition { row: 4, column: 1 },
            dragging: false,
        };

        assert_eq!(selection_columns_for_row(1, selection, 10), None);
        assert_eq!(selection_columns_for_row(2, selection, 10), Some((3, 10)));
        assert_eq!(selection_columns_for_row(3, selection, 10), Some((0, 10)));
        assert_eq!(selection_columns_for_row(4, selection, 10), Some((0, 2)));
        assert_eq!(selection_columns_for_row(5, selection, 10), None);
    }

    #[test]
    fn selected_text_extracts_multiline_rendered_text() {
        let lines = vec![
            Line::from("zero"),
            Line::from("abcdef"),
            Line::from(vec![Span::raw("gh"), Span::styled("ij", Style::default())]),
            Line::from("klmnop"),
        ];
        let selection = TextSelection {
            anchor: TextPosition { row: 1, column: 2 },
            focus: TextPosition { row: 3, column: 2 },
            dragging: false,
        };

        assert_eq!(
            selected_text_from_lines(&lines, selection, 20).as_deref(),
            Some("cdef\nghij\nklm")
        );
    }

    #[test]
    fn selected_text_ignores_empty_line_only_selection() {
        let lines = vec![Line::from(""), Line::from("")];
        let selection = TextSelection {
            anchor: TextPosition { row: 0, column: 0 },
            focus: TextPosition { row: 1, column: 5 },
            dragging: false,
        };

        assert_eq!(selected_text_from_lines(&lines, selection, 20), None);
    }
}
