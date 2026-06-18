use std::{
    io::{Read, Write},
    path::Path,
    sync::mpsc::{self, Receiver, Sender},
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};

const TERMINAL_ROWS: u16 = 12;
const TERMINAL_COLS: u16 = 120;
const TERMINAL_SCROLLBACK_LINES: usize = 10_000;
const DONE_PREFIX: &str = "__GLINT_DONE_";
const TIMEOUT_GRACE: Duration = Duration::from_millis(800);
pub const TERMINAL_RUN_DEFAULT_TIMEOUT_MS: u64 = 120_000;
pub const TERMINAL_RUN_MAX_TIMEOUT_MS: u64 = 600_000;
pub const TERMINAL_RUN_OUTPUT_MAX_CHARS: usize = 12_000;
const TERMINAL_RUN_OUTPUT_HEAD_CHARS: usize = 4_000;
const TERMINAL_RUN_OUTPUT_TAIL_CHARS: usize = 8_000;

#[derive(Debug)]
pub enum TerminalRequest {
    Run {
        command: String,
        description: String,
        timeout: Duration,
        response: Sender<TerminalRunResult>,
    },
    CancelActive,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TerminalRunResult {
    pub command: String,
    pub output: String,
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    pub error: Option<String>,
}

impl TerminalRunResult {
    pub fn busy(command: String) -> Self {
        Self {
            command,
            output: String::new(),
            exit_code: None,
            timed_out: false,
            error: Some("agent terminal is busy".to_owned()),
        }
    }

    pub fn failed(command: String, error: impl Into<String>) -> Self {
        Self {
            command,
            output: String::new(),
            exit_code: None,
            timed_out: false,
            error: Some(error.into()),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TerminalColor {
    #[default]
    Default,
    Indexed(u8),
    Rgb(u8, u8, u8),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TerminalCellStyle {
    pub fg: TerminalColor,
    pub bg: TerminalColor,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub inverse: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TerminalStyledSpan {
    pub text: String,
    pub style: TerminalCellStyle,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TerminalStyledLine {
    pub spans: Vec<TerminalStyledSpan>,
}

impl TerminalStyledLine {
    pub fn plain(text: impl Into<String>) -> Self {
        Self {
            spans: vec![TerminalStyledSpan {
                text: text.into(),
                style: TerminalCellStyle::default(),
            }],
        }
    }
}

pub struct TerminalTab {
    title: String,
    input_buffer: String,
    pane: TerminalPane,
}

impl TerminalTab {
    pub fn new_agent() -> Result<Self> {
        Ok(Self {
            title: default_terminal_title(),
            input_buffer: String::new(),
            pane: TerminalPane::new_agent()?,
        })
    }

    pub fn title(&self) -> &str {
        &self.title
    }

    pub fn status(&self) -> TerminalStatus {
        self.pane.status()
    }

    pub fn is_running(&self) -> bool {
        self.pane.is_running()
    }

    pub fn tick(&mut self) {
        self.pane.tick();
    }

    pub fn resize(&mut self, rows: u16, cols: u16) {
        self.pane.resize(rows, cols);
    }

    pub fn cursor_position(&self) -> Option<(u16, u16)> {
        self.pane.cursor_position()
    }

    pub fn styled_screen_lines(&self, height: u16, width: u16) -> Vec<TerminalStyledLine> {
        self.pane.styled_screen_lines(height, width)
    }

    pub fn scroll_up(&mut self, lines: usize) {
        self.pane.scroll_up(lines);
    }

    pub fn scroll_down(&mut self, lines: usize) {
        self.pane.scroll_down(lines);
    }

    pub fn write_input(&mut self, input: &[u8]) {
        self.record_user_input(input);
        self.pane.write_input(input);
    }

    pub fn cancel_active(&mut self) {
        self.pane.cancel_active();
    }

    pub fn close(mut self) {
        self.pane.close("terminal tab closed");
    }

    pub fn run_noninteractive(
        &mut self,
        command: String,
        description: String,
        timeout: Duration,
        response: Sender<TerminalRunResult>,
    ) {
        self.update_title_for_command(&command);
        self.pane
            .run_noninteractive(command, description, timeout, response);
    }

    fn record_user_input(&mut self, input: &[u8]) {
        if input == b"\r" || input == b"\n" {
            let command = std::mem::take(&mut self.input_buffer);
            self.update_title_for_command(&command);
            return;
        }

        if input == [0x7f] {
            self.input_buffer.pop();
            return;
        }

        if input == [0x03] {
            self.input_buffer.clear();
            return;
        }

        let Ok(text) = std::str::from_utf8(input) else {
            return;
        };
        if text.contains('\x1b') || text.contains('\r') || text.contains('\n') {
            return;
        }
        self.input_buffer.push_str(text);
    }

    fn update_title_for_command(&mut self, command: &str) {
        if let Some(title) = terminal_title_for_command(command) {
            self.title = title;
        }
    }
}

pub struct TerminalPane {
    parser: vt100::Parser,
    writer: Box<dyn Write + Send>,
    output_rx: Receiver<Vec<u8>>,
    active: Option<ActiveTerminalRun>,
    last_status: TerminalStatus,
    rows: u16,
    cols: u16,
    child: Box<dyn Child + Send + Sync>,
    _pty: Box<dyn MasterPty + Send>,
}

struct ActiveTerminalRun {
    id: String,
    command: String,
    description: String,
    timeout: Duration,
    started_at: Instant,
    interrupted_at: Option<Instant>,
    raw_output: String,
    response: Sender<TerminalRunResult>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TerminalStatus {
    Idle,
    Running { description: String },
    TimedOut,
    Error(String),
}

impl TerminalPane {
    pub fn new_agent() -> Result<Self> {
        let pty_system = native_pty_system();
        let pty = pty_system
            .openpty(PtySize {
                rows: TERMINAL_ROWS,
                cols: TERMINAL_COLS,
                pixel_width: 0,
                pixel_height: 0,
            })
            .context("failed to create agent terminal PTY")?;

        let shell = std::env::var("SHELL").unwrap_or_else(|_| "sh".to_owned());
        let mut command = CommandBuilder::new(shell);
        if let Ok(cwd) = std::env::current_dir() {
            command.cwd(cwd);
        }
        command.env("TERM", "xterm-256color");
        command.env("COLORTERM", "truecolor");
        let child = pty
            .slave
            .spawn_command(command)
            .context("failed to start agent terminal shell")?;

        let mut reader = pty
            .master
            .try_clone_reader()
            .context("failed to clone agent terminal reader")?;
        let writer = pty
            .master
            .take_writer()
            .context("failed to open agent terminal writer")?;
        let (output_tx, output_rx) = mpsc::channel();
        thread::spawn(move || {
            let mut buf = [0; 8192];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(read) => {
                        if output_tx.send(buf[..read].to_vec()).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        });

        Ok(Self {
            parser: vt100::Parser::new(TERMINAL_ROWS, TERMINAL_COLS, TERMINAL_SCROLLBACK_LINES),
            writer,
            output_rx,
            active: None,
            last_status: TerminalStatus::Idle,
            rows: TERMINAL_ROWS,
            cols: TERMINAL_COLS,
            child,
            _pty: pty.master,
        })
    }

    pub fn status(&self) -> TerminalStatus {
        self.active
            .as_ref()
            .map(|active| TerminalStatus::Running {
                description: active.description.clone(),
            })
            .unwrap_or_else(|| self.last_status.clone())
    }

    pub fn is_running(&self) -> bool {
        self.active.is_some()
    }

    pub fn write_input(&mut self, input: &[u8]) {
        if input.is_empty() {
            return;
        }
        self.writer.write_all(input).ok();
        self.writer.flush().ok();
    }

    pub fn cancel_active(&mut self) {
        let should_interrupt = self
            .active
            .as_ref()
            .is_some_and(|active| active.interrupted_at.is_none());
        if !should_interrupt {
            return;
        }

        self.write_input(&[0x03]);
        if let Some(active) = &mut self.active {
            active.interrupted_at = Some(Instant::now());
        }
        self.last_status = TerminalStatus::TimedOut;
    }

    pub fn close(&mut self, reason: impl Into<String>) {
        self.tick();
        let reason = reason.into();
        if let Some(active) = self.active.take() {
            active
                .response
                .send(TerminalRunResult {
                    command: active.command,
                    output: truncate_terminal_output(&strip_sentinel_lines(
                        &active.raw_output,
                        &active.id,
                    )),
                    exit_code: None,
                    timed_out: false,
                    error: Some(reason.clone()),
                })
                .ok();
        }
        self.last_status = TerminalStatus::Error(reason);
    }

    pub fn resize(&mut self, rows: u16, cols: u16) {
        let rows = rows.max(1);
        let cols = cols.max(1);
        if rows == self.rows && cols == self.cols {
            return;
        }

        self.rows = rows;
        self.cols = cols;
        self.parser.set_size(rows, cols);
        if let Err(error) = self._pty.resize(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        }) {
            self.last_status = TerminalStatus::Error(format!("resize failed: {error:#}"));
        }
    }

    pub fn cursor_position(&self) -> Option<(u16, u16)> {
        let screen = self.parser.screen();
        if screen.hide_cursor() {
            return None;
        }

        let (_, cols) = screen.size();
        let rows = screen.rows(0, cols).collect::<Vec<_>>();
        Some(remap_cursor_for_hidden_lines(
            &rows,
            screen.cursor_position(),
        ))
    }

    pub fn styled_screen_lines(&self, height: u16, width: u16) -> Vec<TerminalStyledLine> {
        let screen = self.parser.screen();
        let (_, cols) = screen.size();
        let row_text = screen.rows(0, cols).collect::<Vec<_>>();
        let mut lines = row_text
            .iter()
            .enumerate()
            .filter(|(_, text)| !is_internal_terminal_line(text))
            .map(|(row, _)| styled_screen_row(screen, row as u16, width))
            .collect::<Vec<_>>();

        let height = height as usize;
        if lines.len() > height {
            lines = lines[lines.len() - height..].to_vec();
        }
        lines
    }

    pub fn scroll_up(&mut self, lines: usize) {
        let current = self.parser.screen().scrollback();
        self.parser.set_scrollback(current.saturating_add(lines));
    }

    pub fn scroll_down(&mut self, lines: usize) {
        let current = self.parser.screen().scrollback();
        self.parser.set_scrollback(current.saturating_sub(lines));
    }

    pub fn tick(&mut self) {
        while let Ok(chunk) = self.output_rx.try_recv() {
            self.parser.process(&chunk);
            if let Some(active) = &mut self.active {
                active.raw_output.push_str(&String::from_utf8_lossy(&chunk));
            }
        }

        if let Some(active) = &self.active
            && let Some((exit_code, output)) =
                parse_completed_output(&active.raw_output, &active.id)
        {
            self.finish(TerminalRunResult {
                command: active.command.clone(),
                output,
                exit_code: Some(exit_code),
                timed_out: false,
                error: None,
            });
            return;
        }

        let Some(active) = &self.active else {
            return;
        };
        if active.started_at.elapsed() <= active.timeout {
            return;
        }

        if let Some(interrupted_at) = active.interrupted_at {
            if interrupted_at.elapsed() >= TIMEOUT_GRACE {
                let output = strip_sentinel_lines(&active.raw_output, &active.id);
                let result = TerminalRunResult {
                    command: active.command.clone(),
                    output,
                    exit_code: None,
                    timed_out: true,
                    error: Some("terminal command timed out".to_owned()),
                };
                self.finish(result);
            }
        } else {
            self.write_input(&[0x03]);
            if let Some(active) = &mut self.active {
                active.interrupted_at = Some(Instant::now());
            }
            self.last_status = TerminalStatus::TimedOut;
        }
    }

    pub fn run_noninteractive(
        &mut self,
        command: String,
        description: String,
        timeout: Duration,
        response: Sender<TerminalRunResult>,
    ) {
        self.tick();
        if self.active.is_some() {
            response.send(TerminalRunResult::busy(command)).ok();
            return;
        }

        let id = uuid::Uuid::new_v4().simple().to_string();
        let input = terminal_run_input(&command, &id);
        match self.writer.write_all(input.as_bytes()) {
            Ok(()) => {
                self.writer.flush().ok();
                self.last_status = TerminalStatus::Idle;
                self.active = Some(ActiveTerminalRun {
                    id,
                    command,
                    description,
                    timeout,
                    started_at: Instant::now(),
                    interrupted_at: None,
                    raw_output: String::new(),
                    response,
                });
            }
            Err(err) => {
                response
                    .send(TerminalRunResult::failed(
                        command,
                        format!("failed to write to agent terminal: {err}"),
                    ))
                    .ok();
                self.last_status = TerminalStatus::Error("write failed".to_owned());
            }
        }
    }

    fn finish(&mut self, mut result: TerminalRunResult) {
        result.output = truncate_terminal_output(&result.output);
        if let Some(active) = self.active.take() {
            self.last_status = if result.timed_out {
                TerminalStatus::TimedOut
            } else {
                TerminalStatus::Idle
            };
            active.response.send(result).ok();
        }
    }
}

impl Drop for TerminalPane {
    fn drop(&mut self) {
        self.child.kill().ok();
    }
}

fn default_terminal_title() -> String {
    std::env::var("SHELL")
        .ok()
        .and_then(|shell| {
            Path::new(&shell)
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
        })
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "sh".to_owned())
}

pub fn terminal_title_for_command(command: &str) -> Option<String> {
    let recognized = [
        "ssh", "codex", "claude", "bash", "zsh", "fish", "sh", "nu", "pwsh",
    ];
    let mut skip_options = false;

    for token in command.split_whitespace() {
        let token = clean_command_token(token);
        if token.is_empty() {
            continue;
        }
        if matches!(token, "sudo" | "env" | "command" | "exec") {
            skip_options = matches!(token, "sudo" | "env");
            continue;
        }
        if skip_options && token.starts_with('-') {
            continue;
        }
        skip_options = false;
        if is_env_assignment(token) {
            continue;
        }

        let command = Path::new(token)
            .file_name()
            .map(|name| name.to_string_lossy())
            .unwrap_or_else(|| token.into());
        return recognized
            .contains(&command.as_ref())
            .then(|| command.into_owned());
    }

    None
}

fn clean_command_token(token: &str) -> &str {
    token
        .trim_start_matches(['(', '{'])
        .trim_end_matches([';', '&', '|', ')', '}'])
        .trim_matches(['\'', '"'])
}

fn is_env_assignment(token: &str) -> bool {
    let Some((name, _)) = token.split_once('=') else {
        return false;
    };
    !name.is_empty()
        && name
            .chars()
            .all(|char| char.is_ascii_alphanumeric() || char == '_')
}

pub fn terminal_run_input(command: &str, id: &str) -> String {
    format!("{command}\nprintf '\\n%s:%s\\n' '{DONE_PREFIX}{id}__' \"$?\"\n")
}

pub fn parse_completed_output(raw: &str, id: &str) -> Option<(i32, String)> {
    let sentinel = format!("{DONE_PREFIX}{id}__:");
    for line in normalized_lines(raw) {
        let trimmed = line.trim();
        let Some(exit_code) = trimmed.strip_prefix(&sentinel) else {
            continue;
        };
        let exit_code = exit_code.trim().parse().ok()?;
        return Some((exit_code, strip_sentinel_lines(raw, id)));
    }
    None
}

pub fn strip_sentinel_lines(raw: &str, id: &str) -> String {
    let sentinel = format!("{DONE_PREFIX}{id}__:");
    let sentinel_token = format!("{DONE_PREFIX}{id}__");
    normalized_lines(raw)
        .into_iter()
        .filter(|line| !is_internal_result_line(line, &sentinel, &sentinel_token))
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_owned()
}

pub fn truncate_terminal_output(output: &str) -> String {
    let char_count = output.chars().count();
    if char_count <= TERMINAL_RUN_OUTPUT_MAX_CHARS {
        return output.to_owned();
    }

    let head = output
        .chars()
        .take(TERMINAL_RUN_OUTPUT_HEAD_CHARS)
        .collect::<String>();
    let tail = output
        .chars()
        .skip(char_count.saturating_sub(TERMINAL_RUN_OUTPUT_TAIL_CHARS))
        .collect::<String>();
    format!(
        "{head}\n\n<terminal-output-truncated omitted_chars=\"{}\">\n\n{tail}",
        char_count - TERMINAL_RUN_OUTPUT_HEAD_CHARS - TERMINAL_RUN_OUTPUT_TAIL_CHARS
    )
}

fn normalized_lines(raw: &str) -> Vec<String> {
    raw.replace("\r\n", "\n")
        .replace('\r', "\n")
        .lines()
        .map(strip_ansi)
        .collect()
}

fn strip_ansi(line: &str) -> String {
    let mut output = String::new();
    let mut chars = line.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\x1b' && chars.peek() == Some(&'[') {
            chars.next();
            for next in chars.by_ref() {
                if ('@'..='~').contains(&next) {
                    break;
                }
            }
            continue;
        }
        output.push(ch);
    }
    output
}

fn is_internal_terminal_line(line: &str) -> bool {
    let trimmed = line.trim();
    trimmed.contains(DONE_PREFIX) || is_internal_done_printf_line(trimmed)
}

fn remap_cursor_for_hidden_lines(rows: &[String], cursor: (u16, u16)) -> (u16, u16) {
    let hidden_before_cursor = rows
        .iter()
        .take(cursor.0 as usize)
        .filter(|line| is_internal_terminal_line(line))
        .count();

    (
        cursor.0.saturating_sub(hidden_before_cursor as u16),
        cursor.1,
    )
}

fn styled_screen_row(screen: &vt100::Screen, row: u16, width: u16) -> TerminalStyledLine {
    let mut spans = Vec::new();
    let mut active: Option<TerminalStyledSpan> = None;

    for col in 0..width {
        let Some(cell) = screen.cell(row, col) else {
            push_terminal_cell(
                &mut active,
                &mut spans,
                " ".to_owned(),
                TerminalCellStyle::default(),
            );
            continue;
        };

        if cell.is_wide_continuation() {
            continue;
        }

        let text = if cell.has_contents() {
            cell.contents()
        } else {
            " ".to_owned()
        };
        push_terminal_cell(&mut active, &mut spans, text, terminal_cell_style(cell));
    }

    if let Some(span) = active {
        spans.push(span);
    }
    TerminalStyledLine { spans }
}

fn push_terminal_cell(
    active: &mut Option<TerminalStyledSpan>,
    spans: &mut Vec<TerminalStyledSpan>,
    text: String,
    style: TerminalCellStyle,
) {
    if let Some(span) = active.as_mut()
        && span.style == style
    {
        span.text.push_str(&text);
        return;
    }

    if let Some(span) = active.take() {
        spans.push(span);
    }
    *active = Some(TerminalStyledSpan { text, style });
}

fn terminal_cell_style(cell: &vt100::Cell) -> TerminalCellStyle {
    TerminalCellStyle {
        fg: terminal_color(cell.fgcolor()),
        bg: terminal_color(cell.bgcolor()),
        bold: cell.bold(),
        italic: cell.italic(),
        underline: cell.underline(),
        inverse: cell.inverse(),
    }
}

fn terminal_color(color: vt100::Color) -> TerminalColor {
    match color {
        vt100::Color::Default => TerminalColor::Default,
        vt100::Color::Idx(index) => TerminalColor::Indexed(index),
        vt100::Color::Rgb(red, green, blue) => TerminalColor::Rgb(red, green, blue),
    }
}

fn is_internal_result_line(line: &str, sentinel: &str, sentinel_token: &str) -> bool {
    let trimmed = line.trim();
    trimmed.starts_with(sentinel)
        || is_internal_terminal_line(trimmed)
        || (trimmed.contains("printf") && trimmed.contains(sentinel_token))
}

fn is_internal_done_printf_line(trimmed: &str) -> bool {
    trimmed.contains("printf '\\n%s:%s\\n'")
        || trimmed.contains("printf '\\n%s")
        || trimmed.contains("printf '") && trimmed.contains("%s:%s") && trimmed.contains("\\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sentinel_parser_extracts_exit_code_and_removes_done_line() {
        let raw = "echo ok\r\nok\r\n__GLINT_DONE_abc__:7\r\n";

        let parsed = parse_completed_output(raw, "abc").expect("sentinel should parse");

        assert_eq!(parsed.0, 7);
        assert_eq!(parsed.1, "echo ok\nok");
    }

    #[test]
    fn sentinel_parser_ignores_echoed_printf_command() {
        let raw = "printf '\\n%s:%s\\n' '__GLINT_DONE_abc__' \"$?\"\r\n__GLINT_DONE_abc__:0\r\n";

        let parsed = parse_completed_output(raw, "abc").expect("sentinel should parse");

        assert_eq!(parsed.0, 0);
        assert_eq!(parsed.1, "");
    }

    #[test]
    fn terminal_screen_lines_hide_internal_done_protocol() {
        let lines = vec![
            "echo glint-terminal-test".to_owned(),
            "glint-terminal-test".to_owned(),
            "printf '\\n%s:%s\\n' '__GLINT_DONE_abc__' \"$?\"".to_owned(),
            "__GLINT_DONE_abc__:0".to_owned(),
            "$ ".to_owned(),
        ];
        let visible = lines
            .into_iter()
            .map(|row| row.trim_end().to_owned())
            .filter(|line| !is_internal_terminal_line(line))
            .collect::<Vec<_>>();

        assert_eq!(
            visible,
            ["echo glint-terminal-test", "glint-terminal-test", "$"]
        );
    }

    #[test]
    fn terminal_cursor_remaps_after_hidden_protocol_lines() {
        let lines = vec![
            "echo glint-terminal-test".to_owned(),
            "glint-terminal-test".to_owned(),
            "printf '\\n%s:%s\\n' '__GLINT_DONE_abc__' \"$?\"".to_owned(),
            "__GLINT_DONE_abc__:0".to_owned(),
            "$ ".to_owned(),
        ];

        assert_eq!(remap_cursor_for_hidden_lines(&lines, (4, 2)), (2, 2));
    }

    #[test]
    fn styled_terminal_rows_preserve_ansi_attrs() {
        let mut parser = vt100::Parser::new(2, 32, 0);
        parser.process(b"\x1b[31;1mRED\x1b[0m plain \x1b[38;2;1;2;3mRGB");

        let line = styled_screen_row(parser.screen(), 0, 32);

        assert!(line.spans.iter().any(|span| {
            span.text.contains("RED")
                && span.style.fg == TerminalColor::Indexed(1)
                && span.style.bold
        }));
        assert!(line.spans.iter().any(|span| {
            span.text.contains("RGB") && span.style.fg == TerminalColor::Rgb(1, 2, 3)
        }));
    }

    #[test]
    fn terminal_title_detects_shell_and_session_commands() {
        assert_eq!(
            terminal_title_for_command("ssh host").as_deref(),
            Some("ssh")
        );
        assert_eq!(
            terminal_title_for_command("codex").as_deref(),
            Some("codex")
        );
        assert_eq!(
            terminal_title_for_command("sudo -E claude").as_deref(),
            Some("claude")
        );
        assert_eq!(
            terminal_title_for_command("FOO=bar /bin/zsh").as_deref(),
            Some("zsh")
        );
        assert_eq!(terminal_title_for_command("echo hello"), None);
    }

    #[test]
    fn terminal_output_truncates_with_head_and_tail() {
        let output = "a".repeat(4_500) + &"b".repeat(8_500);

        let truncated = truncate_terminal_output(&output);

        assert!(truncated.starts_with(&"a".repeat(4_000)));
        assert!(truncated.ends_with(&"b".repeat(8_000)));
        assert!(truncated.contains("terminal-output-truncated"));
    }
}
