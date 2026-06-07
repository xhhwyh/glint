mod agent;
mod app;
mod event;
mod input;
mod message;
mod ui;

use std::{io, time::Duration};

use anyhow::Result;
use app::App;
use crossterm::{
    event::{self as term_event, Event, KeyEventKind},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use event::{AppEvent, KeyAction};
use ratatui::{Terminal, backend::CrosstermBackend};

fn main() -> Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout))?;

    let result = run(&mut terminal);

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    result
}

fn run(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) -> Result<()> {
    let mut app = App::default();

    while !app.should_quit {
        terminal.draw(|frame| ui::render(frame, &app))?;

        if term_event::poll(Duration::from_millis(40))?
            && let Event::Key(key) = term_event::read()?
            && key.kind == KeyEventKind::Press
        {
            app.update(AppEvent::Key(KeyAction::from(key)));
        }

        while let Ok(event) = app.agent_events.try_recv() {
            app.update(AppEvent::Agent(event));
        }
    }

    Ok(())
}
