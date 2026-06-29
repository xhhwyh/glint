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
mod runtime;
mod services;
mod settings;
mod terminal;
mod tools;
mod transcript;
mod ui;

use std::{
    io::{self, Write},
    time::Duration,
};

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
use event::{AppEvent, KeyAction, KeyInput, MouseAction};
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
        let terminal_height = ui::terminal_height_for_app(&app, size.height);
        let document_height = size.height.saturating_sub(terminal_height);
        let terminal_top_row = size.height.saturating_sub(terminal_height);
        app.set_terminal_top_row(terminal_top_row);
        if terminal_height > 0 {
            app.resize_terminal(
                terminal_height.saturating_sub(2),
                ui::terminal_content_width(size.width),
            );
        }
        app.update_terminal();
        app.set_document_viewport(
            ui::document_viewport_height(&app, size.width, document_height),
            ui::document_scroll_top(&app, size.width, document_height),
        );
        let (input_top_row, input_rows, input_content_width) =
            ui::composer_hitbox(&app, size.width, document_height);
        app.set_input_hitbox(input_top_row, input_rows, input_content_width);
        app.set_return_bottom_button_hitbox(ui::return_bottom_button_hitbox(
            &app,
            size.width,
            document_height,
        ));
        app.set_terminal_tab_hitbox(ui::terminal_tab_hitbox(
            &app,
            terminal_top_row,
            size.width,
            terminal_height,
        ));
        terminal.draw(|frame| ui::render(frame, &app))?;

        if term_event::poll(Duration::from_millis(40))? {
            match term_event::read()? {
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    let input = KeyInput::from(key);
                    if input.action == KeyAction::Quit {
                        if let Some(text) = app.selected_input_text() {
                            match copy_selection_to_clipboard(terminal, &text) {
                                Ok(()) => app.finish_input_selection_copy(),
                                Err(error) => app.fail_selection_copy(&format!("{error:#}")),
                            }
                        } else if let Some(text) = ui::selected_text(&app, size.width) {
                            match copy_selection_to_clipboard(terminal, &text) {
                                Ok(()) => app.finish_selection_copy(),
                                Err(error) => app.fail_selection_copy(&format!("{error:#}")),
                            }
                        } else {
                            app.request_quit();
                        }
                    } else if input.action == KeyAction::Cut {
                        if let Some(text) = app.selected_input_text() {
                            match copy_selection_to_clipboard(terminal, &text) {
                                Ok(()) => app.finish_input_selection_cut(),
                                Err(error) => app.fail_selection_copy(&format!("{error:#}")),
                            }
                        } else {
                            app.update(AppEvent::Key(input));
                        }
                    } else {
                        app.update(AppEvent::Key(input));
                    }
                }
                Event::Mouse(mouse) => app.update(AppEvent::Mouse(MouseAction::from(mouse))),
                _ => {}
            }
        }

        app.update_agent_events();
        app.update_terminal();
    }

    Ok(())
}

fn copy_selection_to_clipboard(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    text: &str,
) -> Result<()> {
    terminal
        .backend_mut()
        .write_all(osc52_sequence(text).as_bytes())?;
    terminal.backend_mut().flush()?;
    Ok(())
}

fn osc52_sequence(text: &str) -> String {
    format!("\x1b]52;c;{}\x07", base64_encode(text.as_bytes()))
}

fn base64_encode(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

    let mut encoded = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0];
        let b1 = chunk.get(1).copied().unwrap_or(0);
        let b2 = chunk.get(2).copied().unwrap_or(0);

        encoded.push(TABLE[(b0 >> 2) as usize] as char);
        encoded.push(TABLE[(((b0 & 0b0000_0011) << 4) | (b1 >> 4)) as usize] as char);
        if chunk.len() > 1 {
            encoded.push(TABLE[(((b1 & 0b0000_1111) << 2) | (b2 >> 6)) as usize] as char);
        } else {
            encoded.push('=');
        }
        if chunk.len() > 2 {
            encoded.push(TABLE[(b2 & 0b0011_1111) as usize] as char);
        } else {
            encoded.push('=');
        }
    }

    encoded
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_encodes_clipboard_payloads() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"hi"), "aGk=");
        assert_eq!(base64_encode(b"hello"), "aGVsbG8=");
    }

    #[test]
    fn osc52_sequence_wraps_base64_payload() {
        assert_eq!(osc52_sequence("hi"), "\x1b]52;c;aGk=\x07");
    }
}
