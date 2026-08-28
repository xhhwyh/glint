mod approval;
mod dashboard;
mod execution;
mod format;
mod input_view;
mod layout;
mod markdown;
mod mcp;
mod model_picker;
mod plugins;
mod progress;
mod resume;
mod star;
mod status;
mod status_bar;
mod subagent_tasks;
mod theme;
mod transcript_view;

use std::{
    collections::HashMap,
    time::{SystemTime, UNIX_EPOCH},
};

use ratatui::{
    Frame,
    layout::{Position, Rect},
    style::Style,
    text::{Line, Span},
    widgets::{Clear, Paragraph},
};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::app::{App, ExecutionExpansionMetrics, TextSelection};
use crate::approval::ApprovalFocus;
use crate::event::{ExtensionMouseAction, MouseAction};
use crate::execution::{ExecutionHitbox, ExecutionId, ExecutionRegion, MAX_EXPANDED_OUTPUT_ROWS};
use crate::message::{Message, Role};

use layout::box_top;
use theme::{ACCENT_COLOR, BG_COLOR};

const PROGRESS_ANIMATION_FRAME_MS: u128 = 180;

pub fn mcp_detail_max_scroll(app: &App, width: u16, height: u16) -> usize {
    mcp::detail_max_scroll(app, width, height)
}

pub fn plugins_detail_max_scroll(app: &App, width: u16, height: u16) -> usize {
    plugins::detail_max_scroll(app, width, height)
}

pub fn extension_mouse_action(
    app: &App,
    mouse: MouseAction,
    width: u16,
    height: u16,
) -> Option<ExtensionMouseAction> {
    if let Some(picker) = &app.resume_picker {
        Some(ExtensionMouseAction::Resume(resume::mouse_action(
            picker, mouse, width, height,
        )))
    } else if app.mcp_view.is_some() {
        Some(ExtensionMouseAction::Mcp(mcp::mouse_action(
            app, mouse, width, height,
        )))
    } else if app.plugins_view.is_some() {
        Some(ExtensionMouseAction::Plugins(plugins::mouse_action(
            app, mouse, width, height,
        )))
    } else {
        None
    }
}

struct Document {
    lines: Vec<Line<'static>>,
    line_meta: Vec<DocumentLineMeta>,
    execution_metrics: HashMap<ExecutionId, ExecutionExpansionMetrics>,
    cursor: Option<(u16, u16)>,
}

struct Composer {
    lines: Vec<Line<'static>>,
    input_body_y: u16,
    input_body_rows: u16,
    input_content_width: u16,
    cursor_x: u16,
    cursor_y: u16,
}

/// A single, immutable projection of the transcript and composer for one frame.
/// Layout reconciliation may update `App::scroll`; viewport-derived methods below
/// intentionally read that reconciled value without rebuilding the projection.
pub struct PreparedDocument {
    document: Document,
    composer: Composer,
    width: u16,
    height: u16,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct DocumentLineMeta {
    message_index: Option<usize>,
    role: Option<Role>,
    execution: Option<(ExecutionId, ExecutionRegion)>,
}

#[cfg(test)]
pub fn render(frame: &mut Frame, app: &App) {
    if let Some(picker) = &app.resume_picker {
        resume::render_resume_picker(frame, app, picker);
        return;
    }
    if let Some(view) = &app.mcp_view {
        mcp::render_mcp_view(frame, app, view);
        return;
    }
    if let Some(view) = &app.plugins_view {
        plugins::render_plugins_view(frame, app, view);
        return;
    }
    if let Some(view) = &app.status_view {
        status::render_status_view(frame, app, view);
        return;
    }
    let prepared = prepare_document(app, frame.area().width, frame.area().height);
    render_prepared_document(frame, app, &prepared);
}

pub fn prepare_document(app: &App, width: u16, height: u16) -> PreparedDocument {
    let width = width.max(1);
    PreparedDocument {
        document: document(app, width),
        composer: composer(app, width),
        width,
        height,
    }
}

pub fn render_prepared_document(frame: &mut Frame, app: &App, prepared: &PreparedDocument) {
    if let Some(picker) = &app.resume_picker {
        resume::render_resume_picker(frame, app, picker);
        return;
    }
    if let Some(view) = &app.mcp_view {
        mcp::render_mcp_view(frame, app, view);
        return;
    }
    if let Some(view) = &app.plugins_view {
        plugins::render_plugins_view(frame, app, view);
        return;
    }
    if let Some(view) = &app.status_view {
        status::render_status_view(frame, app, view);
        return;
    }
    render_document(frame, app, frame.area(), prepared);
}

fn render_document(frame: &mut Frame, app: &App, area: Rect, prepared: &PreparedDocument) {
    let width = prepared.width;
    let mut document_lines = prepared.document.lines.clone();
    let composer = &prepared.composer;
    let composer_height = composer_visible_height(composer, area.height);
    let document_height = area.height.saturating_sub(composer_height);
    let document_area = Rect::new(area.x, area.y, area.width, document_height);
    let composer_area = Rect::new(
        area.x,
        area.y + document_height,
        area.width,
        composer_height,
    );
    let scroll =
        document_scroll_for_len(prepared.document.lines.len(), app.scroll, document_height);
    let sticky_question =
        sticky_question_overlay(app, &prepared.document, scroll, document_height, width);
    let return_bottom_button_row = return_bottom_button_row(app.scroll, document_height);
    document_lines = apply_text_selection(document_lines, app.text_selection, width);

    if document_height > 0 {
        frame.render_widget(
            Paragraph::new(document_lines)
                .scroll((scroll, 0))
                .style(Style::default().bg(BG_COLOR)),
            document_area,
        );
        if let Some((cursor_x, cursor_y)) = prepared.document.cursor
            && let Some(visible_y) = visible_cursor_y(cursor_y, scroll, document_height)
        {
            frame.set_cursor_position(Position::new(
                document_area.x + cursor_x.min(width.saturating_sub(1)),
                document_area.y + visible_y,
            ));
        }
        if let Some((lines, sticky_height)) = sticky_question {
            let sticky_area = Rect::new(area.x, area.y, area.width, sticky_height);
            frame.render_widget(Clear, sticky_area);
            frame.render_widget(
                Paragraph::new(lines).style(Style::default().bg(BG_COLOR)),
                sticky_area,
            );
        }
        if let Some(button_row) = return_bottom_button_row
            && let Some(line) = return_bottom_button_line(app, width)
        {
            frame.render_widget(
                Paragraph::new(vec![line]).style(Style::default().bg(BG_COLOR)),
                Rect::new(area.x, area.y + button_row, area.width, 1),
            );
        }
    }

    render_composer(
        frame,
        composer,
        composer_area,
        prepared.document.cursor.is_none(),
    );
}

fn render_composer(frame: &mut Frame, composer: &Composer, area: Rect, show_cursor: bool) {
    if area.height == 0 {
        return;
    }

    let scroll = composer_scroll_for_height(composer.lines.len(), area.height);
    frame.render_widget(
        Paragraph::new(composer.lines.clone())
            .scroll((scroll, 0))
            .style(Style::default().bg(BG_COLOR)),
        area,
    );
    if show_cursor && let Some(cursor_y) = visible_cursor_y(composer.cursor_y, scroll, area.height)
    {
        let width = area.width.max(1);
        frame.set_cursor_position(Position::new(
            area.x + composer.cursor_x.min(width.saturating_sub(1)),
            area.y + cursor_y,
        ));
    }
}

#[cfg(test)]
pub fn document_scroll_top(app: &App, width: u16, height: u16) -> u16 {
    prepare_document(app, width, height).document_scroll_top(app)
}

#[cfg(test)]
pub fn document_viewport_height(app: &App, width: u16, height: u16) -> u16 {
    prepare_document(app, width, height).document_viewport_height()
}

#[cfg(test)]
pub fn execution_expansion_metrics(
    app: &App,
    width: u16,
) -> HashMap<ExecutionId, ExecutionExpansionMetrics> {
    prepare_document(app, width, 0).execution_expansion_metrics(app)
}

#[cfg(test)]
pub fn execution_hitboxes(app: &App, width: u16, height: u16) -> Vec<ExecutionHitbox> {
    prepare_document(app, width, height).execution_hitboxes(app)
}

impl PreparedDocument {
    pub fn size(&self) -> (u16, u16) {
        (self.width, self.height)
    }

    pub fn execution_expansion_metrics(
        &self,
        app: &App,
    ) -> HashMap<ExecutionId, ExecutionExpansionMetrics> {
        self.document
            .execution_metrics
            .iter()
            .filter(|(id, _)| app.is_execution_expanded(id))
            .map(|(id, metrics)| (id.clone(), *metrics))
            .collect()
    }

    pub fn document_viewport_height(&self) -> u16 {
        self.height
            .saturating_sub(composer_visible_height(&self.composer, self.height))
    }

    pub fn document_scroll_top(&self, app: &App) -> u16 {
        document_scroll_for_len(
            self.document.lines.len(),
            app.scroll,
            self.document_viewport_height(),
        )
    }

    pub fn execution_hitboxes(&self, app: &App) -> Vec<ExecutionHitbox> {
        let viewport_height = self.document_viewport_height();
        let scroll = self.document_scroll_top(app);
        let occluded_top =
            sticky_question_overlay(app, &self.document, scroll, viewport_height, self.width)
                .map(|(_, height)| height)
                .unwrap_or_default();
        let visible_end =
            (scroll as usize + viewport_height as usize).min(self.document.line_meta.len());
        let mut hitboxes = Vec::new();
        let mut row = scroll as usize;

        while row < visible_end {
            if (row - scroll as usize) < occluded_top as usize {
                row += 1;
                continue;
            }
            let Some((id, region)) = self.document.line_meta[row].execution.clone() else {
                row += 1;
                continue;
            };
            let start = row;
            row += 1;
            while row < visible_end
                && self.document.line_meta[row].execution.as_ref() == Some(&(id.clone(), region))
            {
                row += 1;
            }
            let metrics = self
                .document
                .execution_metrics
                .get(&id)
                .copied()
                .unwrap_or_default();
            hitboxes.push(ExecutionHitbox {
                id,
                region,
                start_row: (start - scroll as usize) as u16,
                end_row: (row - scroll as usize) as u16,
                start_column: 0,
                end_column: self.width,
                expandable: metrics.expandable,
                expansion_rows: metrics.expansion_rows,
                max_output_scroll: if region == ExecutionRegion::Output {
                    metrics.max_output_scroll
                } else {
                    0
                },
            });
        }

        hitboxes
    }

    pub fn composer_hitbox(&self) -> (u16, u16, u16) {
        composer_input_hitbox_for_height(&self.composer, self.height)
    }

    pub fn return_bottom_button_hitbox(&self, app: &App) -> Option<(u16, u16, u16)> {
        let row = return_bottom_button_row(app.scroll, self.document_viewport_height())?;
        let (start, end) = return_bottom_button_columns(self.width);
        Some((row, start, end))
    }
}

pub fn selected_text(app: &App, width: u16) -> Option<String> {
    let selection = app.text_selection?;
    let document = document(app, width.max(1));
    selected_text_from_lines(&document.lines, selection, width.max(1))
}

#[cfg(test)]
pub fn composer_hitbox(app: &App, width: u16, height: u16) -> (u16, u16, u16) {
    prepare_document(app, width, height).composer_hitbox()
}

#[cfg(test)]
pub fn return_bottom_button_hitbox(app: &App, width: u16, height: u16) -> Option<(u16, u16, u16)> {
    prepare_document(app, width, height).return_bottom_button_hitbox(app)
}

fn composer_visible_height(composer: &Composer, height: u16) -> u16 {
    (composer.lines.len() as u16).min(height)
}

fn composer_scroll_for_height(line_count: usize, height: u16) -> u16 {
    line_count.saturating_sub(height as usize) as u16
}

fn composer_panel_top(composer: &Composer, height: u16) -> u16 {
    height.saturating_sub(composer_visible_height(composer, height))
}

fn composer_input_hitbox_for_height(composer: &Composer, height: u16) -> (u16, u16, u16) {
    let visible_height = composer_visible_height(composer, height);
    if visible_height == 0 {
        return (height, 0, composer.input_content_width);
    }

    let scroll = composer_scroll_for_height(composer.lines.len(), visible_height);
    let visible_start = scroll as usize;
    let visible_end = visible_start + visible_height as usize;
    let input_start = composer.input_body_y as usize;
    let input_end = input_start + composer.input_body_rows as usize;
    let visible_input_start = input_start.max(visible_start);
    let visible_input_end = input_end.min(visible_end);

    if visible_input_end <= visible_input_start {
        return (height, 0, composer.input_content_width);
    }

    let row = composer_panel_top(composer, height)
        + visible_input_start.saturating_sub(visible_start) as u16;
    (
        row,
        (visible_input_end - visible_input_start) as u16,
        composer.input_content_width,
    )
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
    let mut line_meta = vec![DocumentLineMeta::default(); lines.len()];
    let mut execution_metrics = HashMap::new();
    let has_status_line = app.processing_elapsed().is_some()
        || app.run_notice.is_some()
        || app.last_turn_duration().is_some();

    for (message_index, message) in app.messages.iter().enumerate() {
        if let Some(card) = execution::execution_card(app, message) {
            let id = card.id.clone();
            let expanded = app.is_execution_expanded(&id);
            let output_scroll = app.execution_scroll(&id);
            let hover_progress = app.execution_hover_progress(&id);
            // Collapsed cards carry a bounded head/tail preview. Materialize
            // the complete execution output only when the card is expanded.
            let output = expanded.then(|| app.execution_output_view(&id)).flatten();
            let card_lines = execution::execution_card_lines(
                &card,
                output.as_deref(),
                width,
                expanded,
                output_scroll,
                hover_progress,
            );
            let expansion_rows = if expanded {
                card_lines
                    .output_rows
                    .saturating_sub(card_lines.preview_rows)
            } else if card_lines.expandable {
                MAX_EXPANDED_OUTPUT_ROWS.saturating_sub(card_lines.preview_rows)
            } else {
                0
            };
            let expansion_metrics = ExecutionExpansionMetrics {
                expandable: card_lines.expandable,
                expansion_rows,
                max_output_scroll: card_lines.max_output_scroll,
            };
            execution_metrics.insert(id.clone(), expansion_metrics);
            line_meta.extend(
                card_lines
                    .regions
                    .into_iter()
                    .map(|region| DocumentLineMeta {
                        message_index: Some(message_index),
                        role: Some(message.role),
                        execution: region.map(|region| (id.clone(), region)),
                    }),
            );
            lines.extend(card_lines.lines);
        } else {
            let message_lines = transcript_view::message_lines(message, width);
            let meta = DocumentLineMeta {
                message_index: Some(message_index),
                role: Some(message.role),
                execution: None,
            };
            line_meta.extend(vec![meta; message_lines.len()]);
            lines.extend(message_lines);
        }
    }
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

    line_meta.resize(lines.len(), DocumentLineMeta::default());
    Document {
        lines,
        line_meta,
        execution_metrics,
        cursor: approval_cursor,
    }
}

fn composer(app: &App, width: u16) -> Composer {
    let mut lines = composer_overlay_lines(app, width);
    let input_y = lines.len() as u16;
    let input_rows = input_view::input_rows(app, width);
    let input_body_y = input_y + 1;
    let input_body_rows = input_rows.len() as u16;
    let input_content_width = input_view::input_content_width(width) as u16;
    lines.push(box_top("COMPOSER", width));
    lines.extend(
        input_rows
            .into_iter()
            .map(|row| layout::box_input_body_line(row, width)),
    );
    lines.push(layout::box_bottom(width));
    lines.extend(composer_status_lines(app, width));

    let (input_cursor_x, input_cursor_row) = input_view::input_cursor_position(app, width);
    Composer {
        lines,
        input_body_y,
        input_body_rows,
        input_content_width,
        cursor_x: input_cursor_x,
        cursor_y: input_body_y + input_cursor_row,
    }
}

fn composer_overlay_lines(app: &App, width: u16) -> Vec<Line<'static>> {
    let interactive_lines = if app.model_picker.is_some() {
        model_picker::model_picker_lines(app, width)
    } else if app.slash_menu_visible() {
        input_view::slash_command_lines(app, width)
    } else {
        Vec::new()
    };

    let mut lines = Vec::new();

    if let Some(update) = app.pinned_progress() {
        append_composer_section(
            &mut lines,
            progress::pinned_lines(update, width, progress_animation_frame(app)),
        );
    }

    append_composer_section(
        &mut lines,
        subagent_tasks::running_lines(&app.task_snapshots(), width, subagent_animation_frame()),
    );
    append_composer_section(&mut lines, interactive_lines);

    lines
}

fn append_composer_section(lines: &mut Vec<Line<'static>>, section: Vec<Line<'static>>) {
    if section.is_empty() {
        return;
    }
    if !lines.is_empty() {
        lines.push(Line::from(""));
    }
    lines.extend(section);
}

fn progress_animation_frame(app: &App) -> usize {
    app.processing_elapsed()
        .map(|elapsed| (elapsed.as_millis() / PROGRESS_ANIMATION_FRAME_MS) as usize)
        .unwrap_or(0)
}

fn subagent_animation_frame() -> usize {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| (duration.as_millis() / PROGRESS_ANIMATION_FRAME_MS) as usize)
        .unwrap_or_default()
}

fn composer_status_lines(app: &App, width: u16) -> Vec<Line<'static>> {
    let mut lines = vec![
        status_bar::info_line(app, width),
        status_bar::context_line(app, width),
    ];
    if let Some(line) = status_bar::permission_line(app) {
        lines.push(line);
    }
    lines
}

fn sticky_question_lines(
    app: &App,
    document: &Document,
    scroll: u16,
    height: u16,
    width: u16,
) -> Option<Vec<Line<'static>>> {
    let question_index = sticky_question_index(&app.messages, &document.line_meta, scroll, height)?;
    let question = app.messages.get(question_index)?;

    Some(sticky_question_block(question, width))
}

fn sticky_question_overlay(
    app: &App,
    document: &Document,
    scroll: u16,
    height: u16,
    width: u16,
) -> Option<(Vec<Line<'static>>, u16)> {
    let lines = sticky_question_lines(app, document, scroll, height, width)?;
    let sticky_height = (lines.len() as u16).min(height);
    Some((lines, sticky_height))
}

fn sticky_question_index(
    messages: &[Message],
    line_meta: &[DocumentLineMeta],
    scroll: u16,
    height: u16,
) -> Option<usize> {
    if height == 0 || line_meta.is_empty() {
        return None;
    }

    let start = scroll as usize;
    let end = (start + height as usize).min(line_meta.len());
    let visible = line_meta.get(start..end)?;
    if visible.iter().any(|meta| meta.role == Some(Role::User)) {
        return None;
    }

    let current_message = visible.iter().find_map(|meta| match meta.role {
        Some(Role::Assistant | Role::Tool | Role::Progress) => meta.message_index,
        _ => None,
    })?;
    messages[..current_message]
        .iter()
        .rposition(|message| message.role == Role::User)
}

fn sticky_question_block(message: &Message, width: u16) -> Vec<Line<'static>> {
    let mut lines = transcript_view::message_lines(message, width);
    if !lines.is_empty() {
        lines.remove(0);
    }
    lines
}

const RETURN_BOTTOM_LABEL: &str = "↓ Bottom";

fn return_bottom_button_line(app: &App, width: u16) -> Option<Line<'static>> {
    if app.scroll == 0 {
        return None;
    }
    let (start, _) = return_bottom_button_columns(width);
    let button = return_bottom_button_text();
    Some(Line::from(vec![
        Span::raw(" ".repeat(start as usize)),
        Span::styled(button, Style::default().fg(BG_COLOR).bg(ACCENT_COLOR)),
    ]))
}

fn return_bottom_button_row(scroll_offset_from_bottom: u16, height: u16) -> Option<u16> {
    if scroll_offset_from_bottom == 0 || height == 0 {
        return None;
    }

    Some(height - 1)
}

fn return_bottom_button_columns(width: u16) -> (u16, u16) {
    let button_width = return_bottom_button_text().width() as u16;
    let start = width.saturating_sub(button_width) / 2;
    (start, (start + button_width).min(width))
}

fn return_bottom_button_text() -> String {
    format!(" {RETURN_BOTTOM_LABEL} ")
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
    use super::transcript_view::tool_output_preview;
    use super::*;
    use crate::{
        agent::AgentEvent,
        app::App,
        app::{ModelPicker, ModelPickerStage, TextPosition},
        event::AppEvent,
        execution::{ExecutionId, ExecutionRegion},
        progress::{TodoItem, TodoStatus, TodoUpdate},
        subagent_transcript::{SubagentTranscript, SubagentTranscriptSnapshot},
        tasks::{SubagentBackend, SubagentRequest, TaskStatus},
    };
    use ratatui::{
        Terminal,
        backend::TestBackend,
        style::{Color, Modifier},
    };

    fn finished_bash_message(id: &str, input: &str, output: &str) -> Message {
        let mut message = Message::tool_with_description(id, "Bash", input, None);
        message.content = output.to_owned();
        message.tool_finished = true;
        message
    }

    fn persisted_output_marker(path: &str) -> String {
        format!(
            "preview\n\n<persisted-output>\nFull Bash output was 60000 characters, exceeding the 50000 character tool-result budget. The full output was written to:\n{path}\nUse a narrower tool call if you need more focused output.\n</persisted-output>"
        )
    }

    fn buffer_rows(terminal: &Terminal<TestBackend>) -> Vec<String> {
        let buffer = terminal.backend().buffer();
        buffer
            .content()
            .chunks(buffer.area.width as usize)
            .map(|row| row.iter().map(|cell| cell.symbol()).collect())
            .collect()
    }

    fn render_execution_frame(terminal: &mut Terminal<TestBackend>, app: &App) -> Vec<String> {
        terminal
            .draw(|frame| render(frame, app))
            .expect("render execution fixture");
        buffer_rows(terminal)
    }

    fn assert_buffer_occurrences(
        terminal: &mut Terminal<TestBackend>,
        app: &App,
        expected_fetch: usize,
        expected_push: usize,
        frame: &str,
    ) {
        let buffer = render_execution_frame(terminal, app).concat();
        for (marker, expected) in [("(fetch)", expected_fetch), ("(push)", expected_push)] {
            assert_eq!(
                buffer.matches(marker).count(),
                expected,
                "{marker} occurrence count in {frame}"
            );
        }
    }

    #[test]
    fn execution_output_tabs_do_not_reach_ratatui_buffer() {
        let mut app = App::test_empty();
        app.messages.push(finished_bash_message(
            "call-git-remote",
            "git remote -v",
            "origin\thttps://github.com/xhhwyh/glint.git (fetch)",
        ));
        let mut terminal = Terminal::new(TestBackend::new(100, 12)).expect("test terminal");

        render_execution_frame(&mut terminal, &app);

        let symbols = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(
            !symbols.contains('\t'),
            "literal tabs can move the real terminal cursor outside Ratatui's width model"
        );
        assert!(
            buffer_rows(&terminal)
                .iter()
                .any(|row| row.contains("origin    https://github.com/xhhwyh/glint.git (fetch)")),
            "execution output did not expand the tab to four spaces"
        );
        assert!(
            app.messages[0].content.contains('\t'),
            "rendering must not mutate the persisted tool result"
        );
    }

    #[test]
    fn git_remote_output_survives_main_and_internal_scroll_without_stale_collapsed_rows() {
        let mut app = App::test_empty();
        let id = ExecutionId::Tool("call-git-remote".to_owned());
        app.messages.push(Message::assistant(
            (1..=34)
                .map(|line| format!("context line {line}"))
                .collect::<Vec<_>>()
                .join("\n"),
        ));
        app.messages.push(finished_bash_message(
            "call-git-remote",
            "git remote -v && git log -1 --format='%h %ci %s'",
            "origin\thttps://github.com/xhhwyh/glint.git (fetch)\norigin\thttps://github.com/xhhwyh/glint.git (push)\n186bba1 2026-08-12 11:44:19 +0800 merge: integrate subagent task control",
        ));
        app.toggle_execution(id.clone(), MAX_EXPANDED_OUTPUT_ROWS);
        let mut narrow_terminal = Terminal::new(TestBackend::new(25, 28)).expect("test terminal");
        let hitboxes = execution_hitboxes(&app, 25, 28);
        let max_output_scroll = hitboxes
            .iter()
            .filter(|hitbox| hitbox.id == id)
            .map(|hitbox| hitbox.max_output_scroll)
            .max()
            .expect("execution hitbox");
        assert_eq!(max_output_scroll, 3);
        assert!(
            max_output_scroll > 0,
            "narrow TestBackend fixture must expose internal output scrolling"
        );
        app.set_execution_hitboxes(hitboxes);

        for (output_scroll, document_scroll) in (0..=max_output_scroll).zip([0, 1, 0, 1]) {
            while app.execution_scroll(&id) < output_scroll {
                app.scroll_execution(&id, 1);
            }
            app.scroll = document_scroll;
            assert_buffer_occurrences(
                &mut narrow_terminal,
                &app,
                usize::from(output_scroll > 0),
                1,
                &format!("narrow expanded output {output_scroll}, document {document_scroll}"),
            );
        }

        let mut wide_terminal = Terminal::new(TestBackend::new(100, 28)).expect("test terminal");
        for document_scroll in [0, 1, 0] {
            app.scroll = document_scroll;
            assert_buffer_occurrences(
                &mut wide_terminal,
                &app,
                1,
                1,
                &format!("wide expanded document {document_scroll}"),
            );
        }

        app.toggle_execution(id, MAX_EXPANDED_OUTPUT_ROWS);
        assert_buffer_occurrences(&mut narrow_terminal, &app, 0, 0, "narrow collapsed");
    }

    #[test]
    fn execution_hitboxes_follow_rendered_document_rows_for_adjacent_fetch_and_push() {
        let mut app = App::test_empty();
        let id = ExecutionId::Tool("call-bash".to_owned());
        app.messages.push(finished_bash_message(
            "call-bash",
            "git remote update",
            "origin repository (fetch)\norigin repository (push)",
        ));

        let collapsed = execution_hitboxes(&app, 80, 30);
        let summary = collapsed
            .iter()
            .find(|hitbox| hitbox.id == id && hitbox.region == ExecutionRegion::Summary)
            .expect("summary hitbox");
        assert!(!summary.expandable);
        assert_eq!(summary.expansion_rows, 0);
        assert!(
            collapsed
                .iter()
                .any(|hitbox| hitbox.region == ExecutionRegion::Output)
        );

        app.toggle_execution(id.clone(), 8);
        let document = document(&app, 80);
        let rendered = document.lines.iter().map(line_text).collect::<Vec<_>>();
        let fetch_row = rendered
            .iter()
            .position(|line| line.contains("(fetch)"))
            .expect("fetch output");
        let push_row = rendered
            .iter()
            .position(|line| line.contains("(push)"))
            .expect("push output");
        assert_eq!(push_row, fetch_row + 1);

        let expanded = execution_hitboxes(&app, 80, 30);
        let output = expanded
            .iter()
            .find(|hitbox| hitbox.id == id && hitbox.region == ExecutionRegion::Output)
            .expect("output hitbox");
        assert!(output.end_row - output.start_row <= MAX_EXPANDED_OUTPUT_ROWS);
    }

    #[test]
    fn long_execution_with_zero_height_delta_can_expand_and_collapse_from_summary() {
        let mut app = App::test_empty();
        let id = ExecutionId::Tool("call-bash".to_owned());
        app.messages.push(finished_bash_message(
            "call-bash",
            "git log",
            &(1..=20)
                .map(|line| format!("line {line}"))
                .collect::<Vec<_>>()
                .join("\n"),
        ));

        let collapsed_hitboxes = execution_hitboxes(&app, 80, 30);
        let collapsed_summary = collapsed_hitboxes
            .iter()
            .find(|hitbox| hitbox.id == id && hitbox.region == ExecutionRegion::Summary)
            .expect("collapsed summary hitbox");
        assert!(collapsed_summary.expandable);
        assert_eq!(collapsed_summary.expansion_rows, 0);
        let collapsed_summary_row = collapsed_summary.start_row;
        app.set_execution_hitboxes(collapsed_hitboxes);

        app.update(AppEvent::Mouse(crate::event::MouseAction::LeftDown {
            column: 5,
            row: collapsed_summary_row,
        }));

        assert!(app.is_execution_expanded(&id));
        assert_eq!(
            app.take_execution_repaint_request(),
            Some(crate::app::ExecutionRepaintRequest::Full)
        );

        let expanded_hitboxes = execution_hitboxes(&app, 80, 30);
        let expanded_summary_row = expanded_hitboxes
            .iter()
            .find(|hitbox| hitbox.id == id && hitbox.region == ExecutionRegion::Summary)
            .expect("expanded summary hitbox")
            .start_row;
        app.set_execution_hitboxes(expanded_hitboxes);

        app.update(AppEvent::Mouse(crate::event::MouseAction::LeftDown {
            column: 5,
            row: expanded_summary_row,
        }));

        assert!(!app.is_execution_expanded(&id));
        assert_eq!(
            app.take_execution_repaint_request(),
            Some(crate::app::ExecutionRepaintRequest::Full)
        );
    }

    #[test]
    fn execution_hitboxes_clip_to_the_scrolled_document_viewport() {
        let mut app = App::test_empty();
        let id = ExecutionId::Tool("call-bash".to_owned());
        app.messages.push(finished_bash_message(
            "call-bash",
            "git log",
            &(1..=20)
                .map(|line| format!("line {line}"))
                .collect::<Vec<_>>()
                .join("\n"),
        ));
        app.toggle_execution(id.clone(), MAX_EXPANDED_OUTPUT_ROWS);
        app.scroll = 0;

        let viewport_height = document_viewport_height(&app, 80, 13);
        let hitboxes = execution_hitboxes(&app, 80, 13);

        assert!(
            hitboxes
                .iter()
                .all(|hitbox| hitbox.end_row <= viewport_height)
        );
        assert!(
            hitboxes.iter().any(|hitbox| {
                hitbox.id == id && hitbox.region == ExecutionRegion::Output && hitbox.start_row == 0
            }),
            "expected output hitbox clipped at viewport top: {hitboxes:?}"
        );
    }

    #[test]
    fn execution_expansion_metrics_include_offscreen_expanded_cards() {
        let mut app = App::test_empty();
        let first = ExecutionId::Tool("call-first".to_owned());
        let second = ExecutionId::Tool("call-second".to_owned());
        app.messages.push(finished_bash_message(
            "call-first",
            "git log first",
            "first output",
        ));
        app.messages.push(Message::assistant(
            (1..=40)
                .map(|line| format!("filler line {line}"))
                .collect::<Vec<_>>()
                .join("\n"),
        ));
        app.messages.push(finished_bash_message(
            "call-second",
            "git log second",
            "second output",
        ));
        app.toggle_execution(first.clone(), 8);
        app.toggle_execution(second.clone(), 8);
        app.scroll = 0;

        let visible_hitboxes = execution_hitboxes(&app, 80, 14);
        let metrics = execution_expansion_metrics(&app, 80);

        assert!(!visible_hitboxes.iter().any(|hitbox| hitbox.id == first));
        assert!(visible_hitboxes.iter().any(|hitbox| hitbox.id == second));
        assert_eq!(metrics[&first].expansion_rows, 0);
        assert_eq!(metrics[&second].expansion_rows, 0);
    }

    #[test]
    fn prepared_document_derives_hitboxes_after_anchor_reconciliation_without_reprojection() {
        let mut app = App::test_empty();
        let id = ExecutionId::Tool("call-bash".to_owned());
        app.messages.push(finished_bash_message(
            "call-bash",
            "git status",
            "one rendered output row",
        ));
        app.scroll = 5;
        app.toggle_execution(id.clone(), MAX_EXPANDED_OUTPUT_ROWS);

        let prepared = prepare_document(&app, 80, 20);
        let metrics = prepared.execution_expansion_metrics(&app);
        assert_eq!(metrics[&id].expansion_rows, 0);

        app.reconcile_execution_expansion_metrics(metrics);
        assert_eq!(app.scroll, 5);
        assert_eq!(
            prepared.execution_hitboxes(&app),
            execution_hitboxes(&app, 80, 20)
        );
    }

    #[test]
    fn sticky_question_occlusion_removes_top_execution_hitboxes_after_scroll() {
        let mut app = App::test_empty();
        app.messages.push(Message::user("What changed?"));
        app.messages
            .push(Message::assistant("I am checking the command output."));
        let id = ExecutionId::Tool("call-bash".to_owned());
        app.messages.push(finished_bash_message(
            "call-bash",
            "git log",
            &(1..=20)
                .map(|line| format!("line {line}"))
                .collect::<Vec<_>>()
                .join("\n"),
        ));
        app.toggle_execution(id.clone(), 8);
        app.scroll = 1;

        let document = document(&app, 80);
        let viewport_height = document_viewport_height(&app, 80, 14);
        let scroll = document_scroll_for_len(document.lines.len(), app.scroll, viewport_height);
        let sticky_height = sticky_question_lines(&app, &document, scroll, viewport_height, 80)
            .expect("sticky question")
            .len() as u16;
        let hitboxes = execution_hitboxes(&app, 80, 14);

        assert!(sticky_height > 0);
        assert!(hitboxes.iter().any(|hitbox| hitbox.id == id));
        assert!(
            hitboxes
                .iter()
                .all(|hitbox| hitbox.start_row >= sticky_height)
        );
    }

    #[test]
    fn collapsed_persisted_bash_hitbox_accounts_for_its_preview_row() {
        let mut app = App::test_empty();
        let id = ExecutionId::Tool("call-bash".to_owned());
        app.messages.push(finished_bash_message(
            "call-bash",
            "git log",
            &persisted_output_marker("/missing/bash-output.txt"),
        ));

        for width in [20, 80] {
            let hitbox = execution_hitboxes(&app, width, 30)
                .into_iter()
                .find(|hitbox| hitbox.id == id && hitbox.region == ExecutionRegion::Summary)
                .expect("summary hitbox");
            let expected_rows = if width == 20 {
                crate::execution::MAX_EXPANDED_OUTPUT_ROWS - 1
            } else {
                0
            };
            assert_eq!(hitbox.expansion_rows, expected_rows);
            assert_eq!(hitbox.max_output_scroll, 0);
        }
    }

    #[test]
    fn collapsed_persisted_subagent_hitbox_accounts_for_its_preview_row() {
        let mut app = App::test_empty();
        let id = ExecutionId::Task("task-1".to_owned());
        app.messages.push(Message::tool_with_description(
            "call-subagent",
            "Subagent",
            "inspect parser",
            None,
        ));
        app.subagent_transcripts.insert(
            "task-1".to_owned(),
            SubagentTranscript::from_snapshot(SubagentTranscriptSnapshot {
                task_id: "task-1".to_owned(),
                tool_call_id: "call-subagent".to_owned(),
                description: "inspect parser".to_owned(),
                prompt: "inspect parser behavior".to_owned(),
                messages: vec![finished_bash_message(
                    "nested-grep",
                    "needle",
                    &persisted_output_marker("/missing/subagent-output.txt"),
                )],
                activity: None,
                status: TaskStatus::Running,
                tool_use_count: 1,
            }),
        );

        for width in [20, 80] {
            let hitbox = execution_hitboxes(&app, width, 30)
                .into_iter()
                .find(|hitbox| hitbox.id == id && hitbox.region == ExecutionRegion::Summary)
                .expect("summary hitbox");
            assert_eq!(
                hitbox.expansion_rows,
                crate::execution::MAX_EXPANDED_OUTPUT_ROWS - 1
            );
            assert_eq!(hitbox.max_output_scroll, 0);
        }
    }

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
    fn document_viewport_and_composer_use_the_full_height() {
        let app = App::test_empty();
        let composer = composer(&app, 100);
        let composer_height = composer_visible_height(&composer, 30);
        let document_height = document_viewport_height(&app, 100, 30);

        assert_eq!(document_height + composer_height, 30);
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
    fn fixed_composer_reserves_viewport_height() {
        let app = crate::app::App::test_empty();
        let composer = composer(&app, 80);

        assert_eq!(
            document_viewport_height(&app, 80, 20),
            20 - composer.lines.len() as u16
        );
        assert_eq!(
            composer_hitbox(&app, 80, 20),
            (
                20 - composer.lines.len() as u16 + composer.input_body_y,
                composer.input_body_rows,
                composer.input_content_width
            )
        );
    }

    #[test]
    fn slash_commands_render_above_bottom_input_box() {
        let mut app = crate::app::App::test_empty();
        app.input.set("/");
        let lines = composer(&app, 80).lines;
        let texts = lines.iter().map(line_text).collect::<Vec<_>>();
        let slash_row = texts
            .iter()
            .position(|line| line.contains("/archive"))
            .expect("slash command row");
        let composer_row = texts
            .iter()
            .position(|line| line.contains(" COMPOSER "))
            .expect("composer row");
        let status_row = texts
            .iter()
            .position(|line| line.contains("Context "))
            .expect("status row");

        assert!(slash_row < composer_row);
        assert!(composer_row < status_row);
    }

    #[test]
    fn progress_renders_above_slash_commands_while_typing_slash() {
        let mut app = crate::app::App::test_empty();
        app.update(AppEvent::Agent(AgentEvent::TodoUpdated(TodoUpdate {
            explanation: None,
            todos: vec![
                TodoItem {
                    content: "Check layout".to_owned(),
                    active_form: "Checking layout".to_owned(),
                    status: TodoStatus::InProgress,
                },
                TodoItem {
                    content: "Verify ordering".to_owned(),
                    active_form: "Verifying ordering".to_owned(),
                    status: TodoStatus::Pending,
                },
            ],
        })));
        app.input.set("/");

        let lines = composer(&app, 80).lines;
        let texts = lines.iter().map(line_text).collect::<Vec<_>>();
        let progress_row = texts
            .iter()
            .position(|line| line.contains("Progress 0/2"))
            .expect("progress row");
        let slash_row = texts
            .iter()
            .position(|line| line.contains("/archive"))
            .expect("slash command row");
        let composer_row = texts
            .iter()
            .position(|line| line.contains(" COMPOSER "))
            .expect("composer row");

        assert!(progress_row < slash_row);
        assert!(slash_row < composer_row);
    }

    #[test]
    fn running_subagents_render_above_the_composer() {
        let mut app = crate::app::App::test_empty();
        app.test_start_subagent_task(&SubagentRequest {
            task_id: "a1".to_owned(),
            tool_call_id: "call-subagent".to_owned(),
            description: "Inspect parser".to_owned(),
            prompt: "Inspect the parser".to_owned(),
            agent: None,
            backend: SubagentBackend::Codex,
            cwd: "/workspace".to_owned(),
        });

        let texts = composer(&app, 80)
            .lines
            .iter()
            .map(line_text)
            .collect::<Vec<_>>();
        let tasks_row = texts
            .iter()
            .position(|line| line.contains("Subagents · 1 running"))
            .expect("subagent task row");
        let composer_row = texts
            .iter()
            .position(|line| line.contains(" COMPOSER "))
            .expect("composer row");

        assert!(tasks_row < composer_row);
        assert!(texts.iter().any(|line| line.contains("a1  Inspect parser")));
    }

    #[test]
    fn model_picker_renders_above_bottom_input_box() {
        let mut app = crate::app::App::test_empty();
        app.model_picker = Some(ModelPicker {
            stage: ModelPickerStage::Provider,
            selected_provider: 0,
            selected_model: 0,
        });
        let lines = composer(&app, 80).lines;
        let texts = lines.iter().map(line_text).collect::<Vec<_>>();
        let picker_row = texts
            .iter()
            .position(|line| line.contains("Select Provider"))
            .expect("model picker row");
        let help_row = texts
            .iter()
            .position(|line| line.contains("Choose a provider endpoint"))
            .expect("model picker help row");
        let separator_row = texts[help_row + 1..]
            .iter()
            .position(|line| line.starts_with("─"))
            .map(|offset| help_row + 1 + offset)
            .expect("model picker separator row");
        let provider_row = texts
            .iter()
            .position(|line| line.contains("test"))
            .expect("provider row");
        let composer_row = texts
            .iter()
            .position(|line| line.contains(" COMPOSER "))
            .expect("composer row");
        let status_row = texts
            .iter()
            .position(|line| line.contains("Context "))
            .expect("status row");

        assert!(picker_row < composer_row);
        assert!(help_row < separator_row);
        assert!(separator_row < provider_row);
        assert!(composer_row < status_row);
    }

    #[test]
    fn status_lines_stay_below_input_box() {
        let app = crate::app::App::test_empty();
        let lines = composer(&app, 80).lines;
        let texts = lines.iter().map(line_text).collect::<Vec<_>>();
        let composer_row = texts
            .iter()
            .position(|line| line.contains(" COMPOSER "))
            .expect("composer row");
        let input_bottom_row = texts
            .iter()
            .position(|line| line.starts_with("╰"))
            .expect("input bottom row");
        let status_row = texts
            .iter()
            .position(|line| line.contains("test-model · test"))
            .expect("status row");

        assert!(composer_row < input_bottom_row);
        assert!(input_bottom_row < status_row);
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
    fn sticky_question_pins_visible_reply_question() {
        let messages = vec![
            Message::user("first question"),
            Message::assistant("first answer"),
            Message::user("second question"),
            Message::assistant("second answer"),
        ];
        let meta = vec![
            message_meta(0, Role::User),
            message_meta(1, Role::Assistant),
            message_meta(1, Role::Assistant),
            message_meta(2, Role::User),
            message_meta(3, Role::Assistant),
            message_meta(3, Role::Assistant),
        ];

        assert_eq!(sticky_question_index(&messages, &meta, 1, 2), Some(0));
        assert_eq!(sticky_question_index(&messages, &meta, 4, 2), Some(2));
    }

    #[test]
    fn sticky_question_hides_when_question_is_visible() {
        let messages = vec![
            Message::user("first question"),
            Message::assistant("first answer"),
            Message::user("second question"),
            Message::assistant("second answer"),
        ];
        let meta = vec![
            message_meta(0, Role::User),
            message_meta(1, Role::Assistant),
            message_meta(1, Role::Assistant),
            message_meta(2, Role::User),
            message_meta(3, Role::Assistant),
        ];

        assert_eq!(sticky_question_index(&messages, &meta, 0, 2), None);
        assert_eq!(sticky_question_index(&messages, &meta, 3, 2), None);
    }

    #[test]
    fn sticky_question_uses_user_message_block_without_leading_gap() {
        let message = Message::user("first question\nsecond line");
        let sticky = sticky_question_block(&message, 40);
        let expected = transcript_view::message_lines(&message, 40)
            .into_iter()
            .skip(1)
            .collect::<Vec<_>>();

        assert_eq!(sticky, expected);
        assert_eq!(line_text(&sticky[1]), "  ▶ first question");
        assert_eq!(line_text(&sticky[2]), "    second line");
    }

    #[test]
    fn return_bottom_button_stays_on_bottom_row() {
        assert_eq!(return_bottom_button_row(2, 10), Some(9));
        assert_eq!(return_bottom_button_row(0, 10), None);
        assert_eq!(return_bottom_button_row(2, 0), None);
    }

    #[test]
    fn return_bottom_button_columns_are_centered() {
        let button_width = return_bottom_button_text().width() as u16;
        let (start, end) = return_bottom_button_columns(40);

        assert_eq!(start, (40 - button_width) / 2);
        assert_eq!(end, start + button_width);
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

    fn message_meta(message_index: usize, role: Role) -> DocumentLineMeta {
        DocumentLineMeta {
            message_index: Some(message_index),
            role: Some(role),
            execution: None,
        }
    }

    fn line_text(line: &Line<'static>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }
}
