use std::{
    collections::HashMap,
    ffi::OsStr,
    fs,
    io::{Read, Write},
    path::Path,
    sync::mpsc::{self, Receiver, Sender},
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};

use crate::message::{Message, Role};
use crate::tasks::{SubagentRequest, SubagentStartResponse};

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
const ALTERNATE_SCROLL_ENABLE: &[u8] = b"\x1b[?1007h";
const ALTERNATE_SCROLL_DISABLE: &[u8] = b"\x1b[?1007l";
const TITLE_PROCESS_PROBE_INTERVAL: Duration = Duration::from_millis(500);
const TITLE_PROCESS_START_GRACE: Duration = Duration::from_secs(2);

#[derive(Debug)]
pub enum TerminalRequest {
    Run {
        command: String,
        description: String,
        timeout: Duration,
        response: Sender<TerminalRunResult>,
    },
    StartSubagent {
        request: SubagentRequest,
        response: Sender<SubagentStartResponse>,
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

pub enum TerminalTab {
    Pty(Box<PtyTerminalTab>),
    Subagent(SubagentTerminalTab),
}

pub struct PtyTerminalTab {
    title: String,
    input_buffer: String,
    session_title: Option<SessionTitle>,
    pane: TerminalPane,
}

pub struct SubagentTerminalTab {
    title: String,
    status: TerminalStatus,
    messages: Vec<Message>,
    activity: Option<String>,
    scroll: usize,
}

struct SessionTitle {
    command: String,
    started_at: Instant,
    last_probe_at: Option<Instant>,
    seen_process: bool,
}

impl TerminalTab {
    pub fn new_agent() -> Result<Self> {
        Self::new_agent_in(None)
    }

    pub fn new_agent_in(cwd: Option<&Path>) -> Result<Self> {
        Ok(Self::Pty(Box::new(PtyTerminalTab {
            title: default_terminal_title(),
            input_buffer: String::new(),
            session_title: None,
            pane: TerminalPane::new_agent_in(cwd)?,
        })))
    }

    pub fn new_subagent(title: impl Into<String>) -> Self {
        Self::Subagent(SubagentTerminalTab {
            title: title.into(),
            status: TerminalStatus::Running {
                description: "subagent".to_owned(),
            },
            messages: Vec::new(),
            activity: None,
            scroll: 0,
        })
    }

    pub fn title(&self) -> &str {
        match self {
            Self::Pty(tab) => &tab.title,
            Self::Subagent(tab) => &tab.title,
        }
    }

    pub fn kind_label(&self) -> &'static str {
        match self {
            Self::Pty(_) => "term",
            Self::Subagent(_) => "subagent",
        }
    }

    pub fn is_pty(&self) -> bool {
        matches!(self, Self::Pty(_))
    }

    pub fn status(&self) -> TerminalStatus {
        match self {
            Self::Pty(tab) => tab.pane.status(),
            Self::Subagent(tab) => tab.status.clone(),
        }
    }

    pub fn is_running(&self) -> bool {
        match self {
            Self::Pty(tab) => tab.pane.is_running(),
            Self::Subagent(tab) => matches!(tab.status, TerminalStatus::Running { .. }),
        }
    }

    pub fn tick(&mut self) {
        if let Self::Pty(tab) = self {
            tab.pane.tick();
            tab.refresh_session_title();
        }
    }

    pub fn resize(&mut self, rows: u16, cols: u16) {
        if let Self::Pty(tab) = self {
            tab.pane.resize(rows, cols);
        }
    }

    pub fn cursor_position(&self) -> Option<(u16, u16)> {
        match self {
            Self::Pty(tab) => tab.pane.cursor_position(),
            Self::Subagent(_) => None,
        }
    }

    pub fn styled_screen_lines(&self, height: u16, width: u16) -> Vec<TerminalStyledLine> {
        match self {
            Self::Pty(tab) => tab.pane.styled_screen_lines(height, width),
            Self::Subagent(_) => Vec::new(),
        }
    }

    pub fn subagent_messages(&self) -> Option<&[Message]> {
        match self {
            Self::Subagent(tab) => Some(&tab.messages),
            Self::Pty(_) => None,
        }
    }

    pub fn subagent_activity(&self) -> Option<&str> {
        match self {
            Self::Subagent(tab) => tab.activity.as_deref(),
            Self::Pty(_) => None,
        }
    }

    pub fn append_subagent_message(&mut self, message: Message) {
        if let Self::Subagent(tab) = self {
            tab.messages.push(message);
            tab.scroll = 0;
        }
    }

    pub fn subagent_tool_message_mut(&mut self, id: &str) -> Option<&mut Message> {
        let Self::Subagent(tab) = self else {
            return None;
        };
        tab.messages.iter_mut().rev().find(|message| {
            message.role == Role::Tool && message.tool_call_id.as_deref() == Some(id)
        })
    }

    pub fn append_subagent_assistant_delta(&mut self, delta: &str) {
        let Self::Subagent(tab) = self else {
            return;
        };
        if !matches!(tab.messages.last(), Some(message) if message.role == Role::Assistant) {
            tab.messages.push(Message::assistant(""));
        }
        if let Some(message) = tab.messages.last_mut() {
            message.content.push_str(delta);
        }
        tab.scroll = 0;
    }

    pub fn remove_empty_subagent_assistant_tail(&mut self) {
        let Self::Subagent(tab) = self else {
            return;
        };
        if matches!(
            tab.messages.last(),
            Some(message) if message.role == Role::Assistant && message.content.is_empty()
        ) {
            tab.messages.pop();
        }
    }

    pub fn set_subagent_activity(&mut self, activity: Option<String>) {
        if let Self::Subagent(tab) = self {
            tab.activity = activity;
        }
    }

    pub fn finish_subagent(&mut self, status: TerminalStatus) {
        if let Self::Subagent(tab) = self {
            tab.status = status;
            tab.activity = None;
        }
    }

    pub fn subagent_scroll(&self) -> usize {
        match self {
            Self::Subagent(tab) => tab.scroll,
            Self::Pty(_) => 0,
        }
    }

    pub fn scroll_up(&mut self, lines: usize) {
        match self {
            Self::Pty(tab) => tab.pane.scroll_up(lines),
            Self::Subagent(tab) => {
                tab.scroll = tab.scroll.saturating_add(lines);
            }
        }
    }

    pub fn scroll_down(&mut self, lines: usize) {
        match self {
            Self::Pty(tab) => tab.pane.scroll_down(lines),
            Self::Subagent(tab) => {
                tab.scroll = tab.scroll.saturating_sub(lines);
            }
        }
    }

    pub fn write_mouse_scroll(&mut self, direction: TerminalMouseScroll) -> bool {
        match self {
            Self::Pty(tab) => tab.pane.write_mouse_scroll(direction),
            Self::Subagent(_) => false,
        }
    }

    pub fn write_input(&mut self, input: &[u8]) {
        if let Self::Pty(tab) = self {
            tab.record_user_input(input);
            tab.pane.write_input(input);
        }
    }

    pub fn cancel_active(&mut self) {
        if let Self::Pty(tab) = self {
            tab.pane.cancel_active();
        }
    }

    pub fn close(mut self) {
        if let Self::Pty(tab) = &mut self {
            tab.pane.close("terminal tab closed");
        }
    }

    pub fn run_noninteractive(
        &mut self,
        command: String,
        description: String,
        timeout: Duration,
        response: Sender<TerminalRunResult>,
    ) {
        match self {
            Self::Pty(tab) => {
                tab.update_title_for_command(&command);
                tab.pane
                    .run_noninteractive(command, description, timeout, response);
            }
            Self::Subagent(_) => {
                response
                    .send(TerminalRunResult::failed(
                        command,
                        "active tab is not a PTY terminal",
                    ))
                    .ok();
            }
        }
    }
}

impl PtyTerminalTab {
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
            self.title = title.clone();
            self.session_title = Some(SessionTitle {
                command: title,
                started_at: Instant::now(),
                last_probe_at: None,
                seen_process: false,
            });
        }
    }

    fn refresh_session_title(&mut self) {
        let Some(session) = &self.session_title else {
            return;
        };
        if session
            .last_probe_at
            .is_some_and(|last_probe_at| last_probe_at.elapsed() < TITLE_PROCESS_PROBE_INTERVAL)
        {
            return;
        }

        let command = session.command.clone();
        let running = self.pane.has_descendant_command(&command);
        let Some(session) = &mut self.session_title else {
            return;
        };
        session.last_probe_at = Some(Instant::now());
        if running {
            session.seen_process = true;
            return;
        }

        if session.seen_process || session.started_at.elapsed() >= TITLE_PROCESS_START_GRACE {
            self.title = default_terminal_title();
            self.session_title = None;
        }
    }
}

pub struct TerminalPane {
    parser: vt100::Parser,
    writer: Box<dyn Write + Send>,
    output_rx: Receiver<Vec<u8>>,
    active: Option<ActiveTerminalRun>,
    last_status: TerminalStatus,
    alternate_scroll: bool,
    escape_scan_tail: Vec<u8>,
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TerminalMouseScroll {
    Up,
    Down,
}

impl TerminalPane {
    pub fn new_agent_in(cwd: Option<&Path>) -> Result<Self> {
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
        if let Some(cwd) = cwd {
            command.cwd(cwd);
        } else if let Ok(cwd) = std::env::current_dir() {
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
            alternate_scroll: false,
            escape_scan_tail: Vec::new(),
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
            let output = strip_sentinel_lines(&active.raw_output, &active.id);
            active
                .response
                .send(TerminalRunResult {
                    command: active.command,
                    output: truncate_terminal_output(&output),
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
        terminal_cursor_position(self.parser.screen())
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

    pub fn write_mouse_scroll(&mut self, direction: TerminalMouseScroll) -> bool {
        let screen = self.parser.screen();
        if !(self.alternate_scroll && screen.alternate_screen()) {
            return false;
        }

        self.write_input(&terminal_alternate_scroll_input(direction));
        true
    }

    pub fn tick(&mut self) {
        while let Ok(chunk) = self.output_rx.try_recv() {
            self.update_alternate_scroll_mode(&chunk);
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

    fn has_descendant_command(&self, command: &str) -> bool {
        self.child.process_id().is_some_and(|root_pid| {
            process_tree_contains_command(root_pid, command, &proc_processes())
        })
    }

    fn finish(&mut self, mut result: TerminalRunResult) {
        if let Some(active) = self.active.take() {
            result.output = truncate_terminal_output(&result.output);
            self.last_status = if result.timed_out {
                TerminalStatus::TimedOut
            } else {
                TerminalStatus::Idle
            };
            active.response.send(result).ok();
        }
    }

    fn update_alternate_scroll_mode(&mut self, chunk: &[u8]) {
        update_alternate_scroll_state(
            &mut self.alternate_scroll,
            &mut self.escape_scan_tail,
            chunk,
        );
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

fn terminal_alternate_scroll_input(direction: TerminalMouseScroll) -> Vec<u8> {
    match direction {
        TerminalMouseScroll::Up => b"\x1b[A".to_vec(),
        TerminalMouseScroll::Down => b"\x1b[B".to_vec(),
    }
}

fn update_alternate_scroll_state(enabled: &mut bool, tail: &mut Vec<u8>, chunk: &[u8]) {
    if chunk.is_empty() {
        return;
    }

    let mut bytes = Vec::with_capacity(tail.len() + chunk.len());
    bytes.extend_from_slice(tail);
    bytes.extend_from_slice(chunk);

    for index in 0..bytes.len() {
        let remaining = &bytes[index..];
        if remaining.starts_with(ALTERNATE_SCROLL_ENABLE) {
            *enabled = true;
        } else if remaining.starts_with(ALTERNATE_SCROLL_DISABLE) {
            *enabled = false;
        }
    }

    let max_sequence_len = ALTERNATE_SCROLL_ENABLE
        .len()
        .max(ALTERNATE_SCROLL_DISABLE.len());
    let keep = bytes.len().min(max_sequence_len.saturating_sub(1));
    tail.clear();
    tail.extend_from_slice(&bytes[bytes.len() - keep..]);
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

fn terminal_cursor_position(screen: &vt100::Screen) -> Option<(u16, u16)> {
    if screen.hide_cursor() || screen.scrollback() > 0 {
        return None;
    }

    let (_, cols) = screen.size();
    let rows = screen.rows(0, cols).collect::<Vec<_>>();
    Some(remap_cursor_for_hidden_lines(
        &rows,
        screen.cursor_position(),
    ))
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ProcessSnapshot {
    pid: u32,
    ppid: u32,
    args: Vec<String>,
    comm: String,
}

fn proc_processes() -> Vec<ProcessSnapshot> {
    let Ok(entries) = fs::read_dir("/proc") else {
        return Vec::new();
    };

    entries
        .flatten()
        .filter_map(|entry| {
            let pid = entry.file_name().to_string_lossy().parse().ok()?;
            proc_process(pid)
        })
        .collect()
}

fn proc_process(pid: u32) -> Option<ProcessSnapshot> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let after_comm = stat.rsplit_once(") ")?.1;
    let mut fields = after_comm.split_whitespace();
    fields.next()?;
    let ppid = fields.next()?.parse().ok()?;

    let cmdline = fs::read(format!("/proc/{pid}/cmdline")).unwrap_or_default();
    let args = cmdline
        .split(|byte| *byte == 0)
        .filter(|arg| !arg.is_empty())
        .map(|arg| String::from_utf8_lossy(arg).into_owned())
        .collect::<Vec<_>>();

    let comm = fs::read_to_string(format!("/proc/{pid}/comm"))
        .unwrap_or_default()
        .trim()
        .to_owned();

    Some(ProcessSnapshot {
        pid,
        ppid,
        args,
        comm,
    })
}

fn process_tree_contains_command(
    root_pid: u32,
    command: &str,
    processes: &[ProcessSnapshot],
) -> bool {
    let parents = processes
        .iter()
        .map(|process| (process.pid, process.ppid))
        .collect::<HashMap<_, _>>();

    processes.iter().any(|process| {
        process_matches_command(process, command) && is_descendant(process.pid, root_pid, &parents)
    })
}

fn is_descendant(pid: u32, root_pid: u32, parents: &HashMap<u32, u32>) -> bool {
    let mut current = pid;
    while let Some(parent) = parents.get(&current).copied() {
        if parent == root_pid {
            return true;
        }
        if parent == 0 || parent == current {
            return false;
        }
        current = parent;
    }
    false
}

fn process_matches_command(process: &ProcessSnapshot, command: &str) -> bool {
    let Some(expected) = Path::new(command).file_name().and_then(OsStr::to_str) else {
        return false;
    };

    process.comm == expected
        || process.args.iter().any(|arg| {
            Path::new(arg)
                .file_name()
                .and_then(OsStr::to_str)
                .is_some_and(|name| command_name_matches(name, expected))
        })
}

fn command_name_matches(name: &str, expected: &str) -> bool {
    name == expected || name.strip_suffix(".js") == Some(expected)
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
    fn terminal_cursor_hides_while_viewing_scrollback() {
        let mut parser = vt100::Parser::new(3, 12, 10);
        for index in 0..5 {
            parser.process(format!("line-{index}\r\n").as_bytes());
        }

        parser.set_scrollback(1);

        assert_eq!(terminal_cursor_position(parser.screen()), None);
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
    fn process_tree_detects_wrapped_codex_command() {
        let processes = vec![
            ProcessSnapshot {
                pid: 10,
                ppid: 1,
                args: vec!["zsh".to_owned()],
                comm: "zsh".to_owned(),
            },
            ProcessSnapshot {
                pid: 11,
                ppid: 10,
                args: vec![
                    "node".to_owned(),
                    "/home/user/.nvm/versions/node/bin/codex.js".to_owned(),
                ],
                comm: "node".to_owned(),
            },
        ];

        assert!(process_tree_contains_command(10, "codex", &processes));
    }

    #[test]
    fn process_tree_ignores_same_command_outside_shell_tree() {
        let processes = vec![
            ProcessSnapshot {
                pid: 10,
                ppid: 1,
                args: vec!["zsh".to_owned()],
                comm: "zsh".to_owned(),
            },
            ProcessSnapshot {
                pid: 20,
                ppid: 1,
                args: vec!["codex".to_owned()],
                comm: "codex".to_owned(),
            },
        ];

        assert!(!process_tree_contains_command(10, "codex", &processes));
    }

    #[test]
    fn alternate_scroll_uses_arrow_keys() {
        assert_eq!(
            terminal_alternate_scroll_input(TerminalMouseScroll::Up),
            b"\x1b[A"
        );
        assert_eq!(
            terminal_alternate_scroll_input(TerminalMouseScroll::Down),
            b"\x1b[B"
        );
    }

    #[test]
    fn alternate_scroll_state_tracks_chunked_sequences() {
        let mut enabled = false;
        let mut tail = Vec::new();

        update_alternate_scroll_state(&mut enabled, &mut tail, b"\x1b[?10");
        assert!(!enabled);

        update_alternate_scroll_state(&mut enabled, &mut tail, b"07h");
        assert!(enabled);

        update_alternate_scroll_state(&mut enabled, &mut tail, b"ignored\x1b[?1007l");
        assert!(!enabled);
    }

    #[test]
    fn vt100_tracks_alternate_screen_for_scroll_forwarding() {
        let mut parser = vt100::Parser::new(2, 8, 0);

        parser.process(b"\x1b[?1049h");
        assert!(parser.screen().alternate_screen());

        parser.process(b"\x1b[?1049l");
        assert!(!parser.screen().alternate_screen());
    }

    #[test]
    fn top_scroll_region_rows_are_available_in_scrollback() {
        let mut parser = vt100::Parser::new(4, 12, 10);

        parser.process(b"\x1b[1;1Hhistory-one\x1b[2;1Hviewport");
        parser.process(b"\x1b[1;2r\x1b[2;1H\r\nhistory-two");
        parser.set_scrollback(1);

        assert_eq!(parser.screen().scrollback(), 1);
        let rows = parser.screen().rows(0, 12).collect::<Vec<_>>();
        assert!(rows[0].contains("history-one"), "{rows:?}");
    }

    #[test]
    fn large_scrollback_offset_does_not_panic() {
        let mut parser = vt100::Parser::new(3, 12, 10);
        for index in 0..8 {
            parser.process(format!("line-{index}\r\n").as_bytes());
        }

        parser.set_scrollback(8);

        let rows = parser.screen().rows(0, 12).collect::<Vec<_>>();
        assert_eq!(rows.len(), 3);
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
