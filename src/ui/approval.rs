use ratatui::{
    style::{Modifier, Style},
    text::Line,
};
use unicode_width::UnicodeWidthStr;

use crate::{
    app::App,
    approval::{ApprovalChoice, ApprovalFocus},
};

use super::{
    layout::{box_body_styled, box_bottom, box_top, wrap_text},
    theme::*,
};

pub(super) fn approval_lines(app: &App, width: u16) -> Vec<Line<'static>> {
    let Some(approval) = &app.approval else {
        return Vec::new();
    };

    let mut lines = vec![box_top(
        if approval.request.is_mcp_elicitation() {
            "MCP INTERACTION / INPUT REQUIRED"
        } else {
            "SECURITY CHECK / APPROVAL REQUIRED"
        },
        width,
    )];
    lines.push(box_body_styled("", width, Style::default()));
    lines.push(box_body_styled(
        if approval.request.is_mcp_elicitation() {
            "REQUEST"
        } else {
            "COMMAND"
        },
        width,
        Style::default()
            .fg(ACCENT_COLOR)
            .add_modifier(Modifier::BOLD),
    ));
    lines.extend(
        wrap_text(&approval.request.command, width.saturating_sub(6))
            .into_iter()
            .map(|row| {
                box_body_styled(
                    &if approval.request.is_mcp_elicitation() {
                        row
                    } else {
                        format!("$ {row}")
                    },
                    width,
                    Style::default().fg(TEXT_COLOR).add_modifier(Modifier::BOLD),
                )
            }),
    );
    lines.push(box_body_styled("", width, Style::default()));
    lines.push(box_body_styled(
        "REASON",
        width,
        Style::default()
            .fg(ACCENT_COLOR)
            .add_modifier(Modifier::BOLD),
    ));
    lines.extend(
        wrap_text(&approval.request.explanation, width.saturating_sub(6))
            .into_iter()
            .map(|row| box_body_styled(&row, width, Style::default().fg(MUTED_TEXT_COLOR))),
    );
    lines.push(box_body_styled("", width, Style::default()));

    let choices = if approval.request.is_mcp_elicitation() {
        vec![ApprovalChoice::Yes, ApprovalChoice::No]
    } else {
        vec![
            ApprovalChoice::Yes,
            ApprovalChoice::Always,
            ApprovalChoice::No,
        ]
    };
    for choice in choices {
        let label = match choice {
            ApprovalChoice::Yes => "allow once",
            ApprovalChoice::Always => approval.always_label(),
            ApprovalChoice::No => "deny",
        };
        let selected = approval.selected == choice;
        let style = if selected {
            Style::default()
                .fg(ACCENT_COLOR)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(SOFT_TEXT_COLOR)
        };
        lines.push(box_body_styled(
            &format!("{} {label}", if selected { "❯" } else { " " }),
            width,
            style,
        ));
        if (approval.request.is_mcp_elicitation() && choice == ApprovalChoice::Yes)
            || (!approval.request.is_mcp_elicitation() && choice == ApprovalChoice::No)
        {
            let label = approval.feedback_label();
            let feedback = if approval.feedback.value.is_empty() {
                format!("{label}: ")
            } else {
                format!("{label}: {}", approval.feedback.value)
            };
            let style = if approval.focus == ApprovalFocus::Feedback {
                Style::default().fg(TEXT_COLOR)
            } else {
                Style::default().fg(MUTED_TEXT_COLOR)
            };
            lines.push(box_body_styled(&format!("  {feedback}"), width, style));
        }
    }

    lines.push(box_body_styled("", width, Style::default()));
    lines.push(box_bottom(width));
    lines
}

pub(super) fn approval_feedback_cursor_x(app: &App, width: u16) -> u16 {
    let Some(approval) = &app.approval else {
        return 0;
    };
    let prefix_width = format!("  {}: ", approval.feedback_label()).width();
    let value_width = approval.feedback.value[..approval.feedback.cursor].width();
    (prefix_width + value_width + 2).min(width.saturating_sub(1) as usize) as u16
}
