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
    text::Line,
    widgets::Paragraph,
};

use crate::app::App;
use crate::approval::ApprovalFocus;

use layout::box_top;
use theme::BG_COLOR;

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
    let document = document(app, width);
    let max_scroll = document.lines.len().saturating_sub(area.height as usize) as u16;
    let scroll = max_scroll.saturating_sub(app.scroll);

    frame.render_widget(
        Paragraph::new(document.lines)
            .scroll((scroll, 0))
            .style(Style::default().bg(BG_COLOR)),
        area,
    );
    frame.set_cursor_position(Position::new(
        area.x + document.cursor_x.min(width.saturating_sub(1)),
        area.y + document.cursor_y,
    ));
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

    if let Some(elapsed) = app.processing_elapsed() {
        lines.push(transcript_view::processing_line(elapsed));
    } else {
        if let Some(notice) = app.run_notice.as_deref() {
            lines.push(transcript_view::notice_line(notice));
        }
        if let Some(duration) = app.last_turn_duration() {
            lines.push(transcript_view::turn_duration_line(duration));
        }
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

#[cfg(test)]
mod tests {
    use super::format::{cached_suffix, context_bar_percent, context_usage_label};
    use super::layout::truncate_start_to_width;
    use super::model_picker::price_label;
    use super::terminal::terminal_tab_window_start;
    use super::transcript_view::tool_output_preview;
    use super::*;

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
}
