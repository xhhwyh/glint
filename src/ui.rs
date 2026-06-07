use ratatui::{
    Frame,
    layout::{Constraint, Layout, Position},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
};

use crate::{app::App, message::Role};

pub fn render(frame: &mut Frame, app: &App) {
    let [header, transcript, input, status] = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(1),
        Constraint::Length(5),
        Constraint::Length(1),
    ])
    .areas(frame.area());

    frame.render_widget(
        Paragraph::new("Agent TUI").block(
            Block::default()
                .borders(Borders::ALL)
                .title(app.status.label()),
        ),
        header,
    );

    frame.render_widget(
        Paragraph::new(transcript_lines(app))
            .block(
                Block::default()
                    .borders(Borders::LEFT | Borders::RIGHT)
                    .title("conversation"),
            )
            .wrap(Wrap { trim: false })
            .scroll((app.scroll, 0)),
        transcript,
    );

    frame.render_widget(
        Paragraph::new(format!("> {}", app.input.value.replace('\n', "\n  ")))
            .block(Block::default().borders(Borders::ALL).title("input")),
        input,
    );

    let (cursor_x, cursor_y) = app.input.cursor_position();
    frame.set_cursor_position(Position::new(
        input.x + cursor_x + 1,
        input.y + cursor_y + 1,
    ));

    frame.render_widget(
        Paragraph::new("Enter send · Shift+Enter newline · arrows move/history · q/Ctrl+C quit")
            .style(Style::default().fg(Color::DarkGray)),
        status,
    );
}

fn transcript_lines(app: &App) -> Vec<Line<'static>> {
    app.messages
        .iter()
        .flat_map(|message| {
            let role = match message.role {
                Role::User => Span::styled(
                    "You",
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
                Role::Assistant => Span::styled(
                    "Assistant",
                    Style::default()
                        .fg(Color::Green)
                        .add_modifier(Modifier::BOLD),
                ),
                Role::System => Span::styled(
                    "System",
                    Style::default()
                        .fg(Color::DarkGray)
                        .add_modifier(Modifier::BOLD),
                ),
            };

            let mut lines = vec![Line::from(role)];
            lines.extend(
                message
                    .content
                    .lines()
                    .map(|line| Line::from(line.to_owned())),
            );
            lines.push(Line::from(""));
            lines
        })
        .collect()
}
