use std::{
    io::{Read, Write},
    sync::mpsc::{self, Receiver, Sender},
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};

const TERMINAL_ROWS: u16 = 12;
const TERMINAL_COLS: u16 = 120;
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

pub struct TerminalPane {
    name: String,
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
            name: "agent".to_owned(),
            parser: vt100::Parser::new(TERMINAL_ROWS, TERMINAL_COLS, 0),
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

    pub fn name(&self) -> &str {
        &self.name
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
        (!self.parser.screen().hide_cursor()).then(|| self.parser.screen().cursor_position())
    }

    pub fn screen_lines(&self, height: u16, width: u16) -> Vec<String> {
        let screen = self.parser.screen();
        let rows = screen.rows(0, screen.size().0);
        let mut lines = rows
            .into_iter()
            .map(|row| row.trim_end().to_owned())
            .collect::<Vec<_>>();

        let height = height as usize;
        let width = width as usize;
        if lines.len() > height {
            lines = lines[lines.len() - height..].to_vec();
        }
        lines
            .into_iter()
            .map(|line| truncate_chars(&line, width))
            .collect()
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
        .filter(|line| {
            let trimmed = line.trim();
            !(trimmed.starts_with(&sentinel)
                || (trimmed.contains("printf") && trimmed.contains(&sentinel_token)))
        })
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

fn truncate_chars(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_owned();
    }
    value.chars().take(max_chars).collect()
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
    fn terminal_output_truncates_with_head_and_tail() {
        let output = "a".repeat(4_500) + &"b".repeat(8_500);

        let truncated = truncate_terminal_output(&output);

        assert!(truncated.starts_with(&"a".repeat(4_000)));
        assert!(truncated.ends_with(&"b".repeat(8_000)));
        assert!(truncated.contains("terminal-output-truncated"));
    }
}
