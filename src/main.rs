mod agent;
mod app;
mod approval;
mod commands;
mod config;
mod context;
mod event;
mod execution;
mod input;
mod message;
mod plugins;
mod progress;
mod query;
mod runtime;
mod services;
mod settings;
mod subagent_transcript;
mod tasks;
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

fn draw_synchronized<W, F, E>(
    terminal: &mut Terminal<CrosstermBackend<W>>,
    render: F,
) -> io::Result<()>
where
    W: Write,
    F: FnOnce(&mut ratatui::Frame) -> Result<(), E>,
    E: Into<io::Error>,
{
    use crossterm::terminal::{BeginSynchronizedUpdate, EndSynchronizedUpdate};
    use crossterm::{ExecutableCommand, QueueableCommand};

    terminal.backend_mut().queue(BeginSynchronizedUpdate)?;
    let draw_result = terminal.try_draw(render).map(|_| ());
    let end_result = terminal
        .backend_mut()
        .execute(EndSynchronizedUpdate)
        .map(|_| ());

    match (draw_result, end_result) {
        (Err(draw_error), _) => Err(draw_error),
        (Ok(_), Err(end_error)) => Err(end_error),
        (Ok(()), Ok(())) => Ok(()),
    }
}

fn run(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>, config: Config) -> Result<()> {
    let mut app = App::new(config)?;

    while !app.should_quit {
        let size = terminal.size()?;
        app.update_tasks();
        let prepared_document = ui::prepare_document(&app, size.width, size.height);
        synchronize_layout_state(&mut app, &prepared_document);
        draw_synchronized(terminal, |frame| {
            ui::render_prepared_document(frame, &app, &prepared_document);
            io::Result::Ok(())
        })?;

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
                Event::Mouse(mouse) => {
                    let mouse = MouseAction::from(mouse);
                    if let Some(action) =
                        ui::extension_mouse_action(&app, mouse, size.width, size.height)
                    {
                        app.update(AppEvent::ExtensionMouse(action));
                    } else {
                        app.update(AppEvent::Mouse(mouse));
                    }
                }
                _ => {}
            }
        }

        app.update_agent_events();
        app.update_tasks();
    }

    Ok(())
}

fn synchronize_layout_state(app: &mut App, prepared_document: &ui::PreparedDocument) {
    let execution_metrics = prepared_document.execution_expansion_metrics(app);
    app.reconcile_execution_expansion_metrics(execution_metrics);
    app.set_document_viewport(
        prepared_document.document_viewport_height(),
        prepared_document.document_scroll_top(app),
    );
    let execution_hitboxes = prepared_document.execution_hitboxes(app);
    app.set_execution_hitboxes(execution_hitboxes);
    let (input_top_row, input_rows, input_content_width) = prepared_document.composer_hitbox();
    app.set_input_hitbox(input_top_row, input_rows, input_content_width);
    app.set_return_bottom_button_hitbox(prepared_document.return_bottom_button_hitbox(app));
    let (width, height) = prepared_document.size();
    app.set_mcp_detail_max_scroll(ui::mcp_detail_max_scroll(app, width, height));
    app.set_plugins_detail_max_scroll(ui::plugins_detail_max_scroll(app, width, height));
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

    #[derive(Clone, Default)]
    struct RecordingWriter(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

    impl RecordingWriter {
        fn output(&self) -> String {
            let bytes = self.0.lock().unwrap().clone();
            String::from_utf8_lossy(&bytes).into_owned()
        }
    }

    impl Write for RecordingWriter {
        fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buffer);
            Ok(buffer.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

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

    #[test]
    fn synchronized_draw_emits_begin_and_end_markers() {
        let writer = RecordingWriter::default();
        let backend = CrosstermBackend::new(writer.clone());
        let mut terminal = Terminal::with_options(
            backend,
            ratatui::TerminalOptions {
                viewport: ratatui::Viewport::Fixed(ratatui::layout::Rect::new(0, 0, 20, 4)),
            },
        )
        .unwrap();

        draw_synchronized(&mut terminal, |_frame| io::Result::Ok(())).unwrap();

        let output = writer.output();
        assert!(output.contains("\x1b[?2026h"));
        assert!(output.contains("\x1b[?2026l"));
    }

    #[test]
    fn synchronized_draw_emits_end_marker_when_render_fails() {
        let writer = RecordingWriter::default();
        let backend = CrosstermBackend::new(writer.clone());
        let mut terminal = Terminal::with_options(
            backend,
            ratatui::TerminalOptions {
                viewport: ratatui::Viewport::Fixed(ratatui::layout::Rect::new(0, 0, 20, 4)),
            },
        )
        .unwrap();

        let error = draw_synchronized(&mut terminal, |_frame| {
            io::Result::<()>::Err(io::Error::other("render failed"))
        })
        .unwrap_err();

        assert_eq!(error.to_string(), "render failed");
        let output = writer.output();
        assert!(output.contains("\x1b[?2026h"));
        assert!(output.contains("\x1b[?2026l"));
    }
}
