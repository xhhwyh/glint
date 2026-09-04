mod agent;
mod app;
mod approval;
mod cli;
mod commands;
mod config;
#[allow(dead_code)]
mod configuration;
mod context;
#[allow(dead_code)]
mod credentials;
mod event;
mod execution;
#[cfg(test)]
mod http_proxy_tests;
mod input;
mod message;
#[allow(dead_code)]
mod paths;
#[allow(dead_code)]
mod persistence;
mod plugins;
mod progress;
#[allow(dead_code)]
mod provider_catalog;
mod query;
mod runtime;
mod services;
mod settings;
#[allow(dead_code)]
mod setup;
mod subagent_transcript;
mod tasks;
mod tools;
mod transcript;
mod ui;

use std::{
    io::{self, IsTerminal, Write},
    time::Duration,
};

use anyhow::{Context, Result, bail};
use app::{App, ExecutionRepaintRequest};
use clap::Parser;
use cli::Cli;
use config::Config;
use configuration::ConfigurationManager;
use crossterm::{
    event::{
        self as term_event, DisableMouseCapture, EnableMouseCapture, Event, KeyEventKind,
        KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
    },
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use event::{AppEvent, KeyAction, KeyInput, MouseAction};
use provider_catalog::ProviderCatalog;
use ratatui::{Terminal, backend::CrosstermBackend};
use setup::{SetupEffect, SetupOutcome, SetupState, apply_setup_effect};

const MAX_TERMINAL_EVENTS_PER_FRAME: usize = 64;

fn main() -> Result<()> {
    let _cli = Cli::parse();
    let workspace = std::env::current_dir().context("failed to resolve current directory")?;
    let paths = paths::GlintPaths::discover()?;
    let mut configuration = ConfigurationManager::discover(paths, &workspace)?;
    configuration.repair_selection()?;
    let choice = bootstrap_choice(
        !configuration.available_providers()?.is_empty(),
        io::stdin().is_terminal(),
        io::stdout().is_terminal(),
    )?;
    let catalog = ProviderCatalog::embedded()?;
    let mut stdout = io::stdout();
    let mut lifecycle = TerminalLifecycle::enter(&mut stdout)?;
    let mut cleanup_stdout = io::stdout();
    let mut cleanup_control = CrosstermTerminalControl {
        writer: &mut cleanup_stdout,
    };
    let mut terminal = construct_terminal(
        &mut lifecycle,
        || Terminal::new(CrosstermBackend::new(stdout)),
        &mut cleanup_control,
    )?;

    let result = (|| -> Result<()> {
        match choice {
            BootstrapChoice::Chat => {
                let config = configuration.build_runtime()?;
                run(&mut terminal, config, configuration)
            }
            BootstrapChoice::Setup => {
                let initial_state = if configuration
                    .provider_statuses()?
                    .iter()
                    .any(|status| status.configured)
                {
                    SetupState::provider_list(&catalog, &configuration)?
                } else {
                    SetupState::welcome(&catalog)
                };
                match run_setup(&mut terminal, &mut configuration, initial_state, &catalog)? {
                    SetupOutcome::StartGlint => {
                        let config = configuration.build_runtime()?;
                        run(&mut terminal, config, configuration)
                    }
                    SetupOutcome::Exit => Ok(()),
                }
            }
        }
    })();

    let restore_result = lifecycle.restore(terminal.backend_mut());
    let cursor_result = terminal.show_cursor();
    combine_terminal_result(result, first_io_error(restore_result, cursor_result))
}

#[derive(Default)]
struct TerminalLifecycle {
    raw_mode: bool,
    alternate_screen: bool,
    mouse_capture: bool,
    keyboard_enhancement: bool,
}

impl TerminalLifecycle {
    fn enter<W: Write>(writer: &mut W) -> Result<Self> {
        let mut control = CrosstermTerminalControl { writer };
        Self::enter_with(&mut control).map_err(Into::into)
    }

    fn enter_with<C: TerminalControl>(control: &mut C) -> io::Result<Self> {
        let mut lifecycle = Self::default();
        let enter_result = (|| -> io::Result<()> {
            control.enable_raw_mode()?;
            lifecycle.raw_mode = true;
            lifecycle.alternate_screen = true;
            control.enter_alternate_screen()?;
            lifecycle.mouse_capture = true;
            control.enable_mouse_capture()?;
            lifecycle.keyboard_enhancement = true;
            control.push_keyboard_enhancement()?;
            Ok(())
        })();

        match enter_result {
            Ok(()) => Ok(lifecycle),
            Err(error) => {
                let _ = lifecycle.restore_with(control);
                Err(error)
            }
        }
    }

    fn restore<W: Write>(&mut self, writer: &mut W) -> io::Result<()> {
        let mut control = CrosstermTerminalControl { writer };
        self.restore_with(&mut control)
    }

    fn restore_with<C: TerminalControl>(&mut self, control: &mut C) -> io::Result<()> {
        let mut first_error = None;
        if self.raw_mode {
            record_io_error(&mut first_error, control.disable_raw_mode());
            self.raw_mode = false;
        }
        if self.keyboard_enhancement {
            record_io_error(&mut first_error, control.pop_keyboard_enhancement());
            self.keyboard_enhancement = false;
        }
        if self.mouse_capture {
            record_io_error(&mut first_error, control.disable_mouse_capture());
            self.mouse_capture = false;
        }
        if self.alternate_screen {
            record_io_error(&mut first_error, control.leave_alternate_screen());
            self.alternate_screen = false;
        }
        first_error.map_or(Ok(()), Err)
    }

    #[cfg(test)]
    fn is_restored(&self) -> bool {
        !self.raw_mode
            && !self.alternate_screen
            && !self.mouse_capture
            && !self.keyboard_enhancement
    }
}

trait TerminalControl {
    fn enable_raw_mode(&mut self) -> io::Result<()>;
    fn disable_raw_mode(&mut self) -> io::Result<()>;
    fn enter_alternate_screen(&mut self) -> io::Result<()>;
    fn leave_alternate_screen(&mut self) -> io::Result<()>;
    fn enable_mouse_capture(&mut self) -> io::Result<()>;
    fn disable_mouse_capture(&mut self) -> io::Result<()>;
    fn push_keyboard_enhancement(&mut self) -> io::Result<()>;
    fn pop_keyboard_enhancement(&mut self) -> io::Result<()>;
}

struct CrosstermTerminalControl<'a, W> {
    writer: &'a mut W,
}

impl<W: Write> TerminalControl for CrosstermTerminalControl<'_, W> {
    fn enable_raw_mode(&mut self) -> io::Result<()> {
        enable_raw_mode()
    }

    fn disable_raw_mode(&mut self) -> io::Result<()> {
        disable_raw_mode()
    }

    fn enter_alternate_screen(&mut self) -> io::Result<()> {
        execute!(self.writer, EnterAlternateScreen)
    }

    fn leave_alternate_screen(&mut self) -> io::Result<()> {
        execute!(self.writer, LeaveAlternateScreen)
    }

    fn enable_mouse_capture(&mut self) -> io::Result<()> {
        execute!(self.writer, EnableMouseCapture)
    }

    fn disable_mouse_capture(&mut self) -> io::Result<()> {
        execute!(self.writer, DisableMouseCapture)
    }

    fn push_keyboard_enhancement(&mut self) -> io::Result<()> {
        execute!(
            self.writer,
            PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
        )
    }

    fn pop_keyboard_enhancement(&mut self) -> io::Result<()> {
        execute!(self.writer, PopKeyboardEnhancementFlags)
    }
}

fn construct_terminal<T, F, C>(
    lifecycle: &mut TerminalLifecycle,
    construct: F,
    cleanup: &mut C,
) -> Result<T>
where
    F: FnOnce() -> io::Result<T>,
    C: TerminalControl,
{
    match construct() {
        Ok(terminal) => Ok(terminal),
        Err(error) => combine_terminal_result(Err(error.into()), lifecycle.restore_with(cleanup)),
    }
}

fn record_io_error(slot: &mut Option<io::Error>, result: io::Result<()>) {
    if let Err(error) = result
        && slot.is_none()
    {
        *slot = Some(error);
    }
}

fn first_io_error(left: io::Result<()>, right: io::Result<()>) -> io::Result<()> {
    left.err().or_else(|| right.err()).map_or(Ok(()), Err)
}

fn combine_terminal_result<T>(result: Result<T>, cleanup: io::Result<()>) -> Result<T> {
    match (result, cleanup) {
        (Err(error), _) => Err(error),
        (Ok(_), Err(error)) => Err(error.into()),
        (Ok(value), Ok(())) => Ok(value),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BootstrapChoice {
    Setup,
    Chat,
}

fn bootstrap_choice(
    has_available_models: bool,
    stdin_is_terminal: bool,
    stdout_is_terminal: bool,
) -> Result<BootstrapChoice> {
    if has_available_models {
        return Ok(BootstrapChoice::Chat);
    }
    if stdin_is_terminal && stdout_is_terminal {
        return Ok(BootstrapChoice::Setup);
    }
    bail!("no model is configured; run `glint` in an interactive terminal to add one")
}

fn run_setup(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    manager: &mut ConfigurationManager,
    mut state: SetupState,
    catalog: &ProviderCatalog,
) -> Result<SetupOutcome> {
    loop {
        terminal.draw(|frame| ui::setup::render(frame, &state, catalog))?;
        let Event::Key(key) = term_event::read()? else {
            continue;
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }
        let input = KeyInput::from(key);
        match setup_step(&mut state, input.action) {
            SetupStep::Continue => {}
            SetupStep::Exit(outcome) => return Ok(outcome),
            SetupStep::Effect(effect) => {
                if let Some(outcome) = apply_setup_effect(manager, &mut state, effect)? {
                    return Ok(outcome);
                }
            }
        }
    }
}

enum SetupStep {
    Continue,
    Effect(SetupEffect),
    Exit(SetupOutcome),
}

fn setup_step(state: &mut SetupState, action: KeyAction) -> SetupStep {
    if matches!(action, KeyAction::Quit | KeyAction::ForceQuit) {
        return SetupStep::Exit(SetupOutcome::Exit);
    }
    state
        .update(action)
        .map_or(SetupStep::Continue, SetupStep::Effect)
}

#[cfg(test)]
fn draw_synchronized<W, F, E>(
    terminal: &mut Terminal<CrosstermBackend<W>>,
    render: F,
) -> io::Result<()>
where
    W: Write,
    F: FnMut(&mut ratatui::Frame) -> Result<(), E>,
    E: Into<io::Error>,
{
    draw_synchronized_with_repaint(terminal, TerminalRepaint::Diff, render)
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum TerminalRepaint {
    Diff,
    Full,
}

fn draw_synchronized_with_repaint<W, F, E>(
    terminal: &mut Terminal<CrosstermBackend<W>>,
    repaint: TerminalRepaint,
    mut render: F,
) -> io::Result<()>
where
    W: Write,
    F: FnMut(&mut ratatui::Frame) -> Result<(), E>,
    E: Into<io::Error>,
{
    use crossterm::terminal::{BeginSynchronizedUpdate, EndSynchronizedUpdate};
    use crossterm::{ExecutableCommand, QueueableCommand};

    terminal.backend_mut().queue(BeginSynchronizedUpdate)?;
    let draw_result = match repaint {
        TerminalRepaint::Diff => terminal
            .try_draw(|frame| render(frame).map_err(Into::into))
            .map(|_| ()),
        TerminalRepaint::Full => terminal.clear().and_then(|()| {
            terminal
                .try_draw(|frame| render(frame).map_err(Into::into))
                .map(|_| ())
        }),
    };
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

fn run(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    config: Config,
    configuration: ConfigurationManager,
) -> Result<()> {
    let mut app = App::new(config, configuration)?;

    while !app.should_quit {
        let size = terminal.size()?;
        app.update_tasks();
        let prepared_document = ui::prepare_document(&app, size.width, size.height);
        synchronize_layout_state(&mut app, &prepared_document);
        let repaint = take_terminal_repaint(&mut app);
        draw_synchronized_with_repaint(terminal, repaint, |frame| {
            ui::render_prepared_document(frame, &app, &prepared_document);
            io::Result::Ok(())
        })?;

        if term_event::poll(Duration::from_millis(40))? {
            let events = terminal_event_batch(term_event::read()?, || {
                if term_event::poll(Duration::ZERO)? {
                    term_event::read().map(Some)
                } else {
                    Ok(None)
                }
            })?;
            for event in events {
                handle_terminal_event(terminal, &mut app, event, size.width, size.height);
            }
        }

        app.update_agent_events();
        app.update_tasks();
    }

    Ok(())
}

fn terminal_event_batch<T>(
    first: T,
    mut read_queued: impl FnMut() -> io::Result<Option<T>>,
) -> io::Result<Vec<T>> {
    let mut events = Vec::with_capacity(MAX_TERMINAL_EVENTS_PER_FRAME);
    events.push(first);
    while events.len() < MAX_TERMINAL_EVENTS_PER_FRAME {
        let Some(event) = read_queued()? else {
            break;
        };
        events.push(event);
    }
    Ok(events)
}

fn handle_terminal_event(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut App,
    event: Event,
    width: u16,
    height: u16,
) {
    match event {
        Event::Key(key) if key.kind == KeyEventKind::Press => {
            let input = KeyInput::from(key);
            if input.action == KeyAction::Quit {
                if let Some(text) = app.selected_input_text() {
                    match copy_selection_to_clipboard(terminal, &text) {
                        Ok(()) => app.finish_input_selection_copy(),
                        Err(error) => app.fail_selection_copy(&format!("{error:#}")),
                    }
                } else if let Some(text) = ui::selected_text(app, width) {
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
            if let Some(action) = ui::extension_mouse_action(app, mouse, width, height) {
                app.update(AppEvent::ExtensionMouse(action));
            } else {
                app.update(AppEvent::Mouse(mouse));
            }
        }
        _ => {}
    }
}

fn take_terminal_repaint(app: &mut App) -> TerminalRepaint {
    match app.take_execution_repaint_request() {
        Some(ExecutionRepaintRequest::Full) => TerminalRepaint::Full,
        None => TerminalRepaint::Diff,
    }
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
    use std::collections::VecDeque;

    #[test]
    fn unconfigured_interactive_startup_enters_setup() {
        assert_eq!(
            bootstrap_choice(false, true, true).unwrap(),
            BootstrapChoice::Setup
        );
    }

    #[test]
    fn configured_startup_bypasses_setup() {
        assert_eq!(
            bootstrap_choice(true, false, false).unwrap(),
            BootstrapChoice::Chat
        );
    }

    #[test]
    fn unconfigured_startup_requires_both_terminal_streams() {
        for (stdin_is_terminal, stdout_is_terminal) in
            [(false, false), (false, true), (true, false)]
        {
            let error = bootstrap_choice(false, stdin_is_terminal, stdout_is_terminal).unwrap_err();

            assert!(
                error
                    .to_string()
                    .contains("run `glint` in an interactive terminal to add one")
            );
        }
    }

    #[test]
    fn setup_quit_actions_exit_the_setup_loop() {
        let catalog = ProviderCatalog::embedded().unwrap();
        let mut state = SetupState::welcome(&catalog);

        assert!(matches!(
            setup_step(&mut state, KeyAction::Quit),
            SetupStep::Exit(SetupOutcome::Exit)
        ));
        assert!(matches!(
            setup_step(&mut state, KeyAction::ForceQuit),
            SetupStep::Exit(SetupOutcome::Exit)
        ));
        assert!(matches!(
            state.screen,
            crate::setup::SetupScreen::Welcome(_)
        ));
    }

    #[derive(Clone, Default)]
    struct RecordingWriter(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

    impl RecordingWriter {
        fn output(&self) -> String {
            let bytes = self.0.lock().unwrap().clone();
            String::from_utf8_lossy(&bytes).into_owned()
        }

        fn clear(&self) {
            self.0.lock().unwrap().clear();
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
    fn terminal_lifecycle_restores_every_control_in_reverse_order_and_is_idempotent() {
        let mut control = FakeTerminalControl::default();
        let mut lifecycle = TerminalLifecycle::enter_with(&mut control).unwrap();

        lifecycle.restore_with(&mut control).unwrap();
        assert_eq!(
            control.operations,
            vec![
                TerminalOperation::EnableRaw,
                TerminalOperation::EnterAlternate,
                TerminalOperation::EnableMouse,
                TerminalOperation::PushKeyboard,
                TerminalOperation::DisableRaw,
                TerminalOperation::PopKeyboard,
                TerminalOperation::DisableMouse,
                TerminalOperation::LeaveAlternate,
            ]
        );
        assert!(lifecycle.is_restored());

        lifecycle.restore_with(&mut control).unwrap();
        assert_eq!(control.operations.len(), 8);
    }

    #[test]
    fn terminal_lifecycle_continues_cleanup_after_a_raw_mode_failure() {
        let mut startup = FakeTerminalControl::default();
        let mut lifecycle = TerminalLifecycle::enter_with(&mut startup).unwrap();
        let mut cleanup = FakeTerminalControl::failing(TerminalOperation::DisableRaw);

        assert!(lifecycle.restore_with(&mut cleanup).is_err());
        assert_eq!(
            cleanup.operations,
            vec![
                TerminalOperation::DisableRaw,
                TerminalOperation::PopKeyboard,
                TerminalOperation::DisableMouse,
                TerminalOperation::LeaveAlternate,
            ]
        );
        assert!(lifecycle.is_restored());
    }

    #[test]
    fn terminal_lifecycle_rolls_back_an_interrupted_enter_sequence() {
        let mut control = FakeTerminalControl::failing(TerminalOperation::PushKeyboard);

        assert!(TerminalLifecycle::enter_with(&mut control).is_err());
        assert_eq!(
            control.operations,
            vec![
                TerminalOperation::EnableRaw,
                TerminalOperation::EnterAlternate,
                TerminalOperation::EnableMouse,
                TerminalOperation::PushKeyboard,
                TerminalOperation::DisableRaw,
                TerminalOperation::PopKeyboard,
                TerminalOperation::DisableMouse,
                TerminalOperation::LeaveAlternate,
            ]
        );
    }

    #[test]
    fn terminal_constructor_failure_restores_the_entered_lifecycle() {
        let mut startup = FakeTerminalControl::default();
        let mut lifecycle = TerminalLifecycle::enter_with(&mut startup).unwrap();
        let mut cleanup = FakeTerminalControl::default();

        let result: Result<()> = construct_terminal(
            &mut lifecycle,
            || Err(io::Error::other("terminal creation failed")),
            &mut cleanup,
        );

        assert!(result.is_err());
        assert_eq!(
            cleanup.operations,
            vec![
                TerminalOperation::DisableRaw,
                TerminalOperation::PopKeyboard,
                TerminalOperation::DisableMouse,
                TerminalOperation::LeaveAlternate,
            ]
        );
        assert!(lifecycle.is_restored());
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum TerminalOperation {
        EnableRaw,
        DisableRaw,
        EnterAlternate,
        LeaveAlternate,
        EnableMouse,
        DisableMouse,
        PushKeyboard,
        PopKeyboard,
    }

    #[derive(Default)]
    struct FakeTerminalControl {
        operations: Vec<TerminalOperation>,
        failure: Option<TerminalOperation>,
    }

    impl FakeTerminalControl {
        fn failing(operation: TerminalOperation) -> Self {
            Self {
                operations: Vec::new(),
                failure: Some(operation),
            }
        }

        fn record(&mut self, operation: TerminalOperation) -> io::Result<()> {
            self.operations.push(operation);
            if self.failure == Some(operation) {
                Err(io::Error::other("injected terminal control failure"))
            } else {
                Ok(())
            }
        }
    }

    impl TerminalControl for FakeTerminalControl {
        fn enable_raw_mode(&mut self) -> io::Result<()> {
            self.record(TerminalOperation::EnableRaw)
        }

        fn disable_raw_mode(&mut self) -> io::Result<()> {
            self.record(TerminalOperation::DisableRaw)
        }

        fn enter_alternate_screen(&mut self) -> io::Result<()> {
            self.record(TerminalOperation::EnterAlternate)
        }

        fn leave_alternate_screen(&mut self) -> io::Result<()> {
            self.record(TerminalOperation::LeaveAlternate)
        }

        fn enable_mouse_capture(&mut self) -> io::Result<()> {
            self.record(TerminalOperation::EnableMouse)
        }

        fn disable_mouse_capture(&mut self) -> io::Result<()> {
            self.record(TerminalOperation::DisableMouse)
        }

        fn push_keyboard_enhancement(&mut self) -> io::Result<()> {
            self.record(TerminalOperation::PushKeyboard)
        }

        fn pop_keyboard_enhancement(&mut self) -> io::Result<()> {
            self.record(TerminalOperation::PopKeyboard)
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
    fn terminal_event_batch_preserves_fifo_order() {
        let mut queued = VecDeque::from([2, 3, 4]);

        let batch = terminal_event_batch(1, || Ok(queued.pop_front())).unwrap();

        assert_eq!(batch, [1, 2, 3, 4]);
    }

    #[test]
    fn terminal_event_batch_is_bounded_per_frame() {
        let mut queued = VecDeque::from_iter(1..=MAX_TERMINAL_EVENTS_PER_FRAME + 4);

        let batch = terminal_event_batch(0, || Ok(queued.pop_front())).unwrap();

        assert_eq!(batch.len(), MAX_TERMINAL_EVENTS_PER_FRAME);
        assert_eq!(batch[0], 0);
        assert_eq!(batch[MAX_TERMINAL_EVENTS_PER_FRAME - 1], 63);
        assert_eq!(queued.front(), Some(&64));
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

    #[test]
    fn full_repaint_reemits_unchanged_content() {
        let writer = RecordingWriter::default();
        let backend = CrosstermBackend::new(writer.clone());
        let mut terminal = Terminal::with_options(
            backend,
            ratatui::TerminalOptions {
                viewport: ratatui::Viewport::Fixed(ratatui::layout::Rect::new(0, 0, 20, 4)),
            },
        )
        .unwrap();
        let render = |frame: &mut ratatui::Frame| {
            frame.render_widget(
                ratatui::widgets::Paragraph::new("unchanged"),
                ratatui::layout::Rect::new(0, 1, 20, 1),
            );
            io::Result::Ok(())
        };

        draw_synchronized(&mut terminal, render).unwrap();
        writer.clear();
        draw_synchronized_with_repaint(&mut terminal, TerminalRepaint::Full, render).unwrap();

        let output = writer.output();
        assert!(
            output.contains("unchanged"),
            "full repaint did not re-emit unchanged content"
        );
        assert!(output.contains("\x1b[?2026h"));
        assert!(output.contains("\x1b[?2026l"));
    }

    #[test]
    fn execution_output_scroll_requests_a_full_terminal_repaint() {
        let mut app = App::test_empty();
        let id = crate::execution::ExecutionId::Tool("call-1".to_owned());
        app.set_execution_hitboxes(vec![crate::execution::ExecutionHitbox {
            id: id.clone(),
            region: crate::execution::ExecutionRegion::Output,
            start_row: 2,
            end_row: 5,
            start_column: 0,
            end_column: 80,
            expandable: true,
            expansion_rows: 3,
            max_output_scroll: 12,
        }]);
        app.toggle_execution(id.clone(), 3);
        assert_eq!(take_terminal_repaint(&mut app), TerminalRepaint::Full);

        app.scroll_execution(&id, 3);

        assert_eq!(take_terminal_repaint(&mut app), TerminalRepaint::Full);

        app.scroll_execution(&id, -3);

        assert_eq!(take_terminal_repaint(&mut app), TerminalRepaint::Full);
    }
}
