mod agent;
mod app;
mod approval;
mod commands;
mod config;
mod context;
mod event;
mod input;
mod message;
mod query;
mod services;
mod settings;
mod terminal;
mod tools;
mod transcript;
mod ui;

use std::{io, time::Duration};

use anyhow::Result;
use app::App;
use config::Config;
use crossterm::{
    event::{
        self as term_event, DisableMouseCapture, EnableMouseCapture, Event, KeyEventKind,
        KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
    },
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use event::{AppEvent, KeyInput, MouseAction};
use ratatui::{Terminal, backend::CrosstermBackend};

fn main() -> Result<()> {
    let config = Config::load()?;

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(
        stdout,
        EnterAlternateScreen,
        EnableMouseCapture,
        PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
    )?;
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout))?;

    let result = run(&mut terminal, config);

    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        PopKeyboardEnhancementFlags,
        DisableMouseCapture,
        LeaveAlternateScreen
    )?;
    terminal.show_cursor()?;
    result
}

fn run(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>, config: Config) -> Result<()> {
    let mut app = App::new(config)?;

    while !app.should_quit {
        let size = terminal.size()?;
        app.resize_terminal(ui::terminal_height(size.height), size.width);
        app.update_terminal();
        terminal.draw(|frame| ui::render(frame, &app))?;

        if term_event::poll(Duration::from_millis(40))? {
            match term_event::read()? {
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    app.update(AppEvent::Key(KeyInput::from(key)));
                }
                Event::Mouse(mouse) => app.update(AppEvent::Mouse(MouseAction::from(mouse))),
                _ => {}
            }
        }

        while let Ok(event) = app.agent_events.try_recv() {
            app.update(AppEvent::Agent(event));
        }
        app.update_terminal();
    }

    Ok(())
}
