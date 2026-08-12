use std::time::{SystemTime, UNIX_EPOCH};

use ratatui::{
    style::{Modifier, Style},
    text::{Line, Span},
};
use unicode_width::UnicodeWidthStr;

use crate::tasks::TaskSnapshot;

use super::{
    format::duration_label,
    layout::truncate_end_to_width,
    theme::{ACCENT_COLOR, BORDER_BRIGHT_COLOR, MUTED_TEXT_COLOR, SOFT_TEXT_COLOR},
};

const RUNNING_ICONS: [&str; 4] = ["◐", "◓", "◑", "◒"];

pub(super) fn running_lines(
    tasks: &[TaskSnapshot],
    width: u16,
    animation_frame: usize,
) -> Vec<Line<'static>> {
    let running = tasks
        .iter()
        .filter(|task| task.status.is_running())
        .collect::<Vec<_>>();
    if running.is_empty() {
        return Vec::new();
    }

    let mut lines = vec![header_line(running.len())];
    for (index, task) in running.iter().enumerate() {
        lines.push(task_line(
            task,
            index + 1 == running.len(),
            width,
            animation_frame + index,
        ));
    }
    lines
}

fn header_line(running: usize) -> Line<'static> {
    Line::from(vec![
        Span::raw("  "),
        Span::styled(
            "Subagents",
            Style::default()
                .fg(ACCENT_COLOR)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!(" · {running} running"),
            Style::default().fg(MUTED_TEXT_COLOR),
        ),
    ])
}

fn task_line(
    task: &TaskSnapshot,
    is_last: bool,
    width: u16,
    animation_frame: usize,
) -> Line<'static> {
    let tree = if is_last { "└─" } else { "├─" };
    let icon = RUNNING_ICONS[animation_frame % RUNNING_ICONS.len()];
    let prefix = format!("  {tree} {icon} {}  ", task.id);
    let elapsed = duration_label(now_ms().saturating_sub(task.started_at_ms) / 1000);
    let tools = match task.tool_use_count {
        1 => "1 tool".to_owned(),
        count => format!("{count} tools"),
    };
    let full_stats = format!(" · {tools} · {elapsed}");
    let short_stats = format!(" · {elapsed}");
    let width = width as usize;
    let minimum_middle = 8;
    let stats = if prefix.width() + full_stats.width() + minimum_middle <= width {
        full_stats
    } else if prefix.width() + short_stats.width() + minimum_middle <= width {
        short_stats
    } else {
        String::new()
    };
    let middle_width = width.saturating_sub(prefix.width() + stats.width()).max(1);
    let description = compact_text(&task.description);
    let activity = compact_text(task.activity.as_deref().unwrap_or("Working"));
    let middle = truncate_end_to_width(&format!("{description} · {activity}"), middle_width);

    Line::from(vec![
        Span::raw("  "),
        Span::styled(format!("{tree} "), Style::default().fg(MUTED_TEXT_COLOR)),
        Span::styled(
            icon.to_owned(),
            Style::default()
                .fg(ACCENT_COLOR)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" "),
        Span::styled(
            task.id.clone(),
            Style::default()
                .fg(BORDER_BRIGHT_COLOR)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled(middle, Style::default().fg(SOFT_TEXT_COLOR)),
        Span::styled(stats, Style::default().fg(MUTED_TEXT_COLOR)),
    ])
}

fn compact_text(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tasks::{SubagentBackend, TaskKind, TaskStatus};

    fn task(id: &str, status: TaskStatus) -> TaskSnapshot {
        TaskSnapshot {
            id: id.to_owned(),
            kind: TaskKind::Subagent,
            status,
            description: "Inspect parser".to_owned(),
            backend: SubagentBackend::Codex,
            cwd: "/workspace".to_owned(),
            terminal_tab: Some(1),
            started_at_ms: now_ms().saturating_sub(12_000),
            ended_at_ms: None,
            summary: None,
            activity: Some("Grep · parser".to_owned()),
            tool_use_count: 2,
            result: None,
            error: None,
        }
    }

    fn text(lines: Vec<Line<'static>>) -> String {
        lines
            .into_iter()
            .map(|line| {
                line.spans
                    .into_iter()
                    .map(|span| span.content.into_owned())
                    .collect::<String>()
            })
            .collect::<Vec<String>>()
            .join("\n")
    }

    #[test]
    fn running_panel_shows_only_active_tasks_with_activity_and_stats() {
        let rendered = text(running_lines(
            &[
                task("a1", TaskStatus::Running),
                task("a2", TaskStatus::Completed),
            ],
            100,
            0,
        ));

        assert!(rendered.contains("Subagents · 1 running"));
        assert!(rendered.contains("◐ a1"));
        assert!(rendered.contains("Inspect parser · Grep · parser"));
        assert!(rendered.contains("2 tools · "));
        assert!(!rendered.contains("a2"));
    }

    #[test]
    fn running_icon_animates() {
        let task = task("a1", TaskStatus::Running);
        let frames = (0..4)
            .map(|frame| text(running_lines(std::slice::from_ref(&task), 80, frame)))
            .collect::<Vec<_>>();

        assert!(frames[0].contains("◐"));
        assert!(frames[1].contains("◓"));
        assert!(frames[2].contains("◑"));
        assert!(frames[3].contains("◒"));
    }

    #[test]
    fn multiple_running_tasks_use_tree_connectors() {
        let rendered = text(running_lines(
            &[
                task("a1", TaskStatus::Running),
                task("a2", TaskStatus::Running),
            ],
            100,
            0,
        ));

        assert!(rendered.contains("Subagents · 2 running"));
        assert!(rendered.contains("├─ ◐ a1"));
        assert!(rendered.contains("└─ ◓ a2"));
    }

    #[test]
    fn terminal_tasks_do_not_leave_an_empty_panel() {
        let tasks = [
            task("a1", TaskStatus::Completed),
            task("a2", TaskStatus::Failed),
            task("a3", TaskStatus::Cancelled),
        ];

        assert!(running_lines(&tasks, 80, 0).is_empty());
    }
}
