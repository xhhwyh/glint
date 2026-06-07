use ratatui::{
    Frame,
    layout::{Constraint, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
};

use crate::{app::App, message::Role};

pub fn render(frame: &mut Frame, app: &App) {
    let [header, transcript, input, status] = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(1),
        Constraint::Length(3),
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
        Paragraph::new(format!("> {}", app.input.value))
            .block(Block::default().borders(Borders::ALL).title("input")),
        input,
    );

    frame.render_widget(
        Paragraph::new("Enter send · ↑/↓ scroll · q/Ctrl+C quit")
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

            [
                Line::from(role),
                Line::from(message.content.clone()),
                Line::from(""),
            ]
        })
        .collect()
}
