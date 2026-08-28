use std::borrow::Cow;

use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};
use unicode_width::UnicodeWidthStr;

use crate::{
    app::App,
    execution::{ExecutionId, ExecutionOutputPreview, ExecutionRegion, MAX_EXPANDED_OUTPUT_ROWS},
    message::{Message, Role},
    tasks::TaskStatus,
};

use super::{
    layout::{truncate_end_to_width, wrap_text},
    theme::{
        ACCENT_COLOR, BG_COLOR, BORDER_BRIGHT_COLOR, EXECUTION_OUTPUT_COLOR, MUTED_TEXT_COLOR,
        TEXT_COLOR,
    },
};

pub(super) struct ExecutionCardView<'a> {
    pub id: ExecutionId,
    pub name: &'a str,
    pub summary: &'a str,
    pub description: Option<&'a str>,
    pub status: String,
    pub preview: ExecutionOutputPreview,
    pub finished: bool,
    pub is_error: bool,
    pub streaming: bool,
}

pub(super) struct ExecutionCardLines {
    pub lines: Vec<Line<'static>>,
    pub regions: Vec<Option<ExecutionRegion>>,
    pub output_rows: u16,
    pub preview_rows: u16,
    pub expandable: bool,
    pub max_output_scroll: u16,
}

pub(super) fn execution_card<'a>(
    app: &'a App,
    message: &'a Message,
) -> Option<ExecutionCardView<'a>> {
    if message.role != Role::Tool {
        return None;
    }

    let call_id = message.tool_call_id.as_deref()?;
    match message.tool_name.as_deref()? {
        "Bash" => {
            let id = ExecutionId::Tool(call_id.to_owned());
            Some(ExecutionCardView {
                id: id.clone(),
                name: "Bash",
                summary: message.tool_input.as_deref().unwrap_or(""),
                description: message.tool_description.as_deref(),
                status: tool_status(message),
                preview: app.execution_output_preview(&id).unwrap_or_default(),
                finished: message.tool_finished,
                is_error: message.tool_is_error,
                streaming: !message.tool_finished,
            })
        }
        "Subagent" => {
            let transcript = app
                .subagent_transcripts
                .values()
                .find(|transcript| transcript.tool_call_id() == call_id)?;
            let id = ExecutionId::Task(transcript.task_id().to_owned());
            let status = transcript.status();
            Some(ExecutionCardView {
                id: id.clone(),
                name: "Subagent",
                summary: transcript
                    .activity()
                    .unwrap_or_else(|| transcript.description()),
                description: Some(transcript.description()),
                status: status.label().to_owned(),
                preview: app.execution_output_preview(&id).unwrap_or_default(),
                finished: status.is_terminal(),
                is_error: matches!(status, TaskStatus::Failed | TaskStatus::Cancelled),
                streaming: status.is_running(),
            })
        }
        _ => None,
    }
}

pub(super) fn execution_card_lines(
    card: &ExecutionCardView<'_>,
    output: Option<&str>,
    width: u16,
    expanded: bool,
    output_scroll: u16,
    hover_fraction: f32,
) -> ExecutionCardLines {
    let width = width.max(1) as usize;
    let preview = collapsed_preview(&card.preview, width.saturating_sub(4));
    if !expanded {
        let summary = card_summary(
            card,
            width,
            false,
            card.preview.total_lines(),
            preview.expandable,
            hover_fraction,
        );
        let preview_rows = preview.rows.len().min(u16::MAX as usize) as u16;
        let mut lines = vec![Line::from(""), Line::from(summary)];
        let mut regions = vec![None, Some(ExecutionRegion::Summary)];
        for row in preview.rows {
            let color = if row.omission {
                MUTED_TEXT_COLOR
            } else {
                EXECUTION_OUTPUT_COLOR
            };
            lines.push(Line::from(vec![
                Span::raw("    "),
                Span::styled(row.text, Style::default().fg(color)),
            ]));
            regions.push(Some(ExecutionRegion::Output));
        }
        return ExecutionCardLines {
            lines,
            regions,
            output_rows: 0,
            preview_rows,
            expandable: preview.expandable,
            max_output_scroll: 0,
        };
    }

    let output = normalize_execution_output(output.unwrap_or_default());
    let output_lines = wrap_text(&output, (width.saturating_sub(4)) as u16);
    let summary = card_summary(
        card,
        width,
        expanded,
        card.preview.total_lines(),
        true,
        hover_fraction,
    );
    let mut lines = vec![Line::from(""), Line::from(summary)];
    let mut regions = vec![None, Some(ExecutionRegion::Summary)];

    let (output_rows, max_output_scroll) = output_metrics(output_lines.len());
    let output_scroll = output_scroll.min(max_output_scroll) as usize;
    let end = output_lines.len().saturating_sub(output_scroll);
    let start = end.saturating_sub(output_rows as usize);

    for output in &output_lines[start..end] {
        lines.push(Line::from(vec![
            Span::raw("    "),
            Span::styled(output.clone(), Style::default().fg(EXECUTION_OUTPUT_COLOR)),
        ]));
        regions.push(Some(ExecutionRegion::Output));
    }

    ExecutionCardLines {
        lines,
        regions,
        output_rows,
        preview_rows: preview.rows.len().min(u16::MAX as usize) as u16,
        expandable: preview.expandable,
        max_output_scroll,
    }
}

struct CollapsedPreviewRow {
    text: String,
    omission: bool,
}

struct CollapsedPreview {
    rows: Vec<CollapsedPreviewRow>,
    expandable: bool,
}

fn collapsed_preview(preview: &ExecutionOutputPreview, width: usize) -> CollapsedPreview {
    fn push_output(
        rows: &mut Vec<CollapsedPreviewRow>,
        truncated: &mut bool,
        line: &str,
        width: usize,
    ) {
        let line = normalize_execution_output(line);
        let rendered = truncate_end_to_width(&line, width);
        *truncated |= rendered != line;
        rows.push(CollapsedPreviewRow {
            text: rendered,
            omission: false,
        });
    }

    let width = width.max(1);
    let mut rows = Vec::new();
    let mut truncated = false;

    if preview.is_abridged() {
        for line in preview
            .leading_lines()
            .iter()
            .take(crate::execution::COLLAPSED_PREVIEW_EDGE_LINES)
        {
            push_output(&mut rows, &mut truncated, line, width);
        }
        rows.push(CollapsedPreviewRow {
            text: truncate_end_to_width(&format!("... +{} lines", preview.omitted_lines()), width),
            omission: true,
        });
        for line in preview.trailing_lines() {
            push_output(&mut rows, &mut truncated, line, width);
        }
    } else {
        for line in preview.leading_lines() {
            push_output(&mut rows, &mut truncated, line, width);
        }
    }

    CollapsedPreview {
        rows,
        expandable: preview.is_abridged() || preview.has_truncated_content() || truncated,
    }
}

fn normalize_execution_output(text: &str) -> Cow<'_, str> {
    if text.contains('\t') {
        Cow::Owned(text.replace('\t', "    "))
    } else {
        Cow::Borrowed(text)
    }
}

fn tool_status(message: &Message) -> String {
    if !message.tool_finished {
        "running".to_owned()
    } else if message.tool_is_error {
        "failed".to_owned()
    } else {
        "completed".to_owned()
    }
}

fn output_metrics(output_row_count: usize) -> (u16, u16) {
    let output_rows = output_row_count.min(MAX_EXPANDED_OUTPUT_ROWS as usize) as u16;
    let max_output_scroll = output_row_count
        .saturating_sub(MAX_EXPANDED_OUTPUT_ROWS as usize)
        .min(u16::MAX as usize) as u16;
    (output_rows, max_output_scroll)
}

fn card_summary(
    card: &ExecutionCardView<'_>,
    width: usize,
    expanded: bool,
    output_row_count: usize,
    expandable: bool,
    hover_fraction: f32,
) -> Vec<Span<'static>> {
    let details = if card.summary.trim().is_empty() {
        card.description.unwrap_or("")
    } else {
        card.summary
    };
    let status = if card.finished || card.streaming {
        card.status.clone()
    } else {
        "pending".to_owned()
    };
    let hint = if expanded {
        Some("click to collapse")
    } else if expandable {
        Some("click to expand")
    } else {
        None
    };
    let prefix = format!("  ◇ {} ", card.name);
    let output_hint = if output_row_count == 0 {
        String::new()
    } else {
        format!(" · {output_row_count} lines")
    };
    let hint_suffix = hint.map(|hint| format!(" · {hint}")).unwrap_or_default();
    let suffix = format!(" · {status}{output_hint}{hint_suffix}");
    let available = width.saturating_sub(prefix.width() + suffix.width());
    let details = truncate_end_to_width(details, available);
    let background = interpolate_color(BG_COLOR, Color::Rgb(8, 47, 73), hover_fraction);
    let marker_style = execution_style(
        BORDER_BRIGHT_COLOR,
        ACCENT_COLOR,
        background,
        hover_fraction,
    )
    .add_modifier(Modifier::BOLD);
    let detail_style = execution_style(
        TEXT_COLOR,
        Color::Rgb(186, 230, 253),
        background,
        hover_fraction,
    );
    let muted_style = execution_style(
        MUTED_TEXT_COLOR,
        Color::Rgb(125, 211, 252),
        background,
        hover_fraction,
    );
    let status_style = if card.is_error {
        execution_style(
            BORDER_BRIGHT_COLOR,
            ACCENT_COLOR,
            background,
            hover_fraction,
        )
        .add_modifier(Modifier::BOLD)
    } else {
        muted_style
    };

    vec![
        Span::styled(prefix, marker_style),
        Span::styled(details, detail_style),
        Span::styled(format!(" · {status}"), status_style),
        Span::styled(output_hint, muted_style),
        Span::styled(hint_suffix, muted_style),
    ]
}

fn execution_style(resting: Color, hovered: Color, background: Color, fraction: f32) -> Style {
    Style::default()
        .fg(interpolate_color(resting, hovered, fraction))
        .bg(background)
}

fn interpolate_color(resting: Color, hovered: Color, fraction: f32) -> Color {
    let fraction = fraction.clamp(0.0, 1.0);
    let (
        Color::Rgb(resting_red, resting_green, resting_blue),
        Color::Rgb(hovered_red, hovered_green, hovered_blue),
    ) = (resting, hovered)
    else {
        return hovered;
    };
    let interpolate_channel = |resting: u8, hovered: u8| {
        (resting as f32 + (hovered as f32 - resting as f32) * fraction).round() as u8
    };
    Color::Rgb(
        interpolate_channel(resting_red, hovered_red),
        interpolate_channel(resting_green, hovered_green),
        interpolate_channel(resting_blue, hovered_blue),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        agent::AgentEvent,
        subagent_transcript::{SubagentTranscript, SubagentTranscriptSnapshot},
        tasks::TaskStatus,
    };

    fn bash_card() -> ExecutionCardView<'static> {
        ExecutionCardView {
            id: ExecutionId::Tool("call-bash".to_owned()),
            name: "Bash",
            summary: "git remote update",
            description: None,
            status: "completed".to_owned(),
            preview: ExecutionOutputPreview::from_text("output"),
            finished: true,
            is_error: false,
            streaming: false,
        }
    }

    #[test]
    fn expanding_long_execution_output_keeps_the_card_height_stable() {
        let mut card = bash_card();
        let output = (1..=20)
            .map(|line| format!("line {line}"))
            .collect::<Vec<_>>()
            .join("\n");
        card.preview = ExecutionOutputPreview::from_text(&output);

        let collapsed = execution_card_lines(&card, None, 80, false, 0, 0.0);
        let expanded = execution_card_lines(&card, Some(&output), 80, true, 0, 0.0);

        assert_eq!(collapsed.preview_rows, 7);
        assert_eq!(expanded.output_rows, collapsed.preview_rows);
        assert_eq!(expanded.lines.len(), collapsed.lines.len());
        assert_eq!(expanded.max_output_scroll, 13);
    }

    #[test]
    fn execution_summary_has_no_border_or_disclosure_arrow() {
        let card = bash_card();
        let rendered =
            execution_card_lines(&card, Some("origin repository (fetch)"), 100, false, 0, 0.0)
                .lines
                .into_iter()
                .map(|line| {
                    line.spans
                        .into_iter()
                        .map(|span| span.content.into_owned())
                        .collect::<String>()
                })
                .collect::<String>();

        assert!(!rendered.contains('│'));
        assert!(!rendered.contains('╭'));
        assert!(!rendered.contains('╰'));
        assert!(!rendered.contains('›'));
        assert!(!rendered.contains('▼'));
        assert!(rendered.contains('◇'));
    }

    #[test]
    fn only_linked_subagent_tool_messages_project_to_execution_cards() {
        let mut app = App::test_empty();
        let mut linked =
            Message::tool_with_description("call-subagent", "Subagent", "inspect parser", None);
        linked.tool_finished = true;
        app.messages.push(linked.clone());
        app.subagent_transcripts.insert(
            "task-1".to_owned(),
            SubagentTranscript::from_snapshot(SubagentTranscriptSnapshot {
                task_id: "task-1".to_owned(),
                tool_call_id: "call-subagent".to_owned(),
                description: "inspect parser".to_owned(),
                prompt: "inspect parser behavior".to_owned(),
                messages: vec![Message::assistant("found the parser")],
                activity: None,
                status: TaskStatus::Completed,
                tool_use_count: 0,
            }),
        );

        let card = execution_card(&app, &app.messages[0]).expect("linked card");
        assert_eq!(card.id, ExecutionId::Task("task-1".to_owned()));
        assert!(
            app.execution_output_view(&card.id)
                .expect("subagent output")
                .contains("found the parser")
        );

        let mut unlinked_app = App::test_empty();
        unlinked_app.messages.push(linked);
        assert!(execution_card(&unlinked_app, &unlinked_app.messages[0]).is_none());
    }

    #[test]
    fn running_subagent_card_uses_live_activity_in_its_summary() {
        let mut app = App::test_empty();
        app.messages.push(Message::tool_with_description(
            "call-subagent",
            "Subagent",
            "inspect parser",
            None,
        ));
        let mut transcript = SubagentTranscript::from_snapshot(SubagentTranscriptSnapshot {
            task_id: "task-1".to_owned(),
            tool_call_id: "call-subagent".to_owned(),
            description: "inspect parser".to_owned(),
            prompt: "inspect parser behavior".to_owned(),
            messages: vec![Message::assistant("working")],
            activity: Some("Thinking".to_owned()),
            status: TaskStatus::Running,
            tool_use_count: 0,
        });
        transcript.apply(&AgentEvent::ToolStarted {
            id: "tool-1".to_owned(),
            name: "Grep".to_owned(),
            input_summary: "needle".to_owned(),
            input_description: None,
        });
        app.subagent_transcripts
            .insert("task-1".to_owned(), transcript);

        let card = execution_card(&app, &app.messages[0]).expect("running card");
        assert_eq!(card.summary, "Running Grep: needle");
        assert_eq!(card.status, "running");
    }

    #[test]
    fn collapsed_short_execution_output_shows_every_line_without_expand_hint() {
        let mut card = bash_card();
        card.preview = ExecutionOutputPreview::from_text("line 1\nline 2\nline 3");

        let rendered = execution_card_lines(&card, None, 80, false, 0, 0.0);
        let rendered_text = rendered
            .lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>();

        assert!(rendered_text.iter().any(|line| line == "    line 1"));
        assert!(rendered_text.iter().any(|line| line == "    line 2"));
        assert!(rendered_text.iter().any(|line| line == "    line 3"));
        assert!(!rendered_text.concat().contains("click to expand"));
        assert!(!rendered_text.concat().contains("preview up to"));
        assert!(rendered_text[1].ends_with(" · completed · 3 lines"));
        assert!(!rendered.expandable);
    }

    #[test]
    fn expanded_execution_output_tabs_are_projected_as_four_spaces() {
        let card = bash_card();

        let rendered = execution_card_lines(
            &card,
            Some("origin\thttps://github.com/xhhwyh/glint.git (fetch)"),
            100,
            true,
            0,
            0.0,
        );
        let output = rendered.lines[2]
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>();

        assert_eq!(
            output,
            "    origin    https://github.com/xhhwyh/glint.git (fetch)"
        );
        assert!(!output.contains('\t'));
    }

    #[test]
    fn collapsed_long_execution_output_shows_three_lines_from_each_end() {
        let mut card = bash_card();
        card.preview = ExecutionOutputPreview::from_text(
            "line 1\nline 2\nline 3\nline 4\nline 5\nline 6\nline 7\nline 8",
        );

        let rendered = execution_card_lines(&card, None, 80, false, 0, 0.0);
        let rendered_text = rendered
            .lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>();

        assert_eq!(
            &rendered_text[2..],
            [
                "    line 1",
                "    line 2",
                "    line 3",
                "    ... +2 lines",
                "    line 6",
                "    line 7",
                "    line 8",
            ]
        );
        assert!(rendered_text.concat().contains("click to expand"));
        assert!(!rendered_text.concat().contains("preview up to"));
        assert!(!rendered_text[1].contains(" ·  · "));
        assert!(rendered.expandable);
    }

    #[test]
    fn collapsed_short_output_remains_expandable_when_a_line_is_truncated() {
        let mut card = bash_card();
        card.preview = ExecutionOutputPreview::from_text(
            "a single terminal output line that is wider than the collapsed card",
        );

        let rendered = execution_card_lines(&card, None, 30, false, 0, 0.0);
        let rendered_text = rendered
            .lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .map(|span| span.content.as_ref())
            .collect::<String>();

        assert!(rendered_text.contains("click to expand"));
        assert!(rendered.expandable);
    }

    #[test]
    fn output_metrics_saturate_for_more_than_u16_maximum_wrapped_rows() {
        assert_eq!(
            output_metrics(usize::MAX),
            (MAX_EXPANDED_OUTPUT_ROWS, u16::MAX)
        );
    }

    #[test]
    fn expanded_output_offset_is_measured_from_the_bottom() {
        let card = bash_card();
        let output = (1..=20)
            .map(|line| format!("line {line}"))
            .collect::<Vec<_>>()
            .join("\n");
        let rendered = execution_card_lines(&card, Some(&output), 80, true, 3, 0.0)
            .lines
            .into_iter()
            .skip(2)
            .map(|line| {
                line.spans
                    .into_iter()
                    .map(|span| span.content.into_owned())
                    .collect::<String>()
            })
            .collect::<Vec<_>>();

        assert_eq!(rendered.first().map(String::as_str), Some("    line 11"));
        assert_eq!(rendered.last().map(String::as_str), Some("    line 17"));
    }

    #[test]
    fn hover_transition_brightens_the_summary_marker_without_changing_text_width() {
        let card = bash_card();
        let resting = execution_card_lines(&card, Some("output"), 80, false, 0, 0.0);
        let hovered = execution_card_lines(&card, Some("output"), 80, false, 0, 1.0);

        let resting_marker = &resting.lines[1].spans[0];
        let hovered_marker = &hovered.lines[1].spans[0];
        assert_eq!(
            resting_marker.content.width(),
            hovered_marker.content.width()
        );
        assert_ne!(resting_marker.style, hovered_marker.style);
    }
}
