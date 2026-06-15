use std::{
    env, fs,
    io::{BufRead, BufReader, Read},
    process::{Command, Stdio},
    sync::mpsc::{self, Receiver, Sender},
    thread,
    time::{Duration, Instant},
};

use crate::agent::provider::{ToolCall, ToolResult};

mod description;

use super::{
    ToolBehavior,
    utils::{
        command_error_message, error, glob_summary, kill_child_tree, missing_arg, ok,
        prepare_killable_command, program_in_path, resolve_tool_path, string_arg, usize_arg,
    },
};

pub(super) const GLOB_DEFAULT_LIMIT: usize = 100;
pub(super) const GLOB_MAX_LIMIT: usize = 100;
pub(super) const GLOB_DEFAULT_TIMEOUT_SECONDS: u64 = 20;
pub(super) const GLOB_WSL_TIMEOUT_SECONDS: u64 = 60;
pub(super) const GLOB_TIMEOUT_ENV: &str = "GLINT_GLOB_TIMEOUT_SECONDS";
const POLL_INTERVAL: Duration = Duration::from_millis(50);
const DEFAULT_GLOB_EXCLUDES: &[&str] = &[
    "!.git/**",
    "!target/**",
    "!.worktree/**",
    "!node_modules/**",
    "!dist/**",
    "!build/**",
    "!vendor/**",
    "!.venv/**",
];

pub(super) struct GlobTool;

impl ToolBehavior for GlobTool {
    fn name(&self) -> &'static str {
        "Glob"
    }

    fn description(&self) -> &'static str {
        description::DESCRIPTION
    }

    fn required_args(&self) -> &'static [&'static str] {
        description::REQUIRED_ARGS
    }

    fn execute(&self, call: &ToolCall, is_cancelled: &mut dyn FnMut() -> bool) -> ToolResult {
        glob(call, is_cancelled)
    }

    fn is_concurrency_safe(&self, _call: &ToolCall) -> bool {
        true
    }

    fn input_summary(&self, call: &ToolCall) -> Option<String> {
        glob_summary(call)
    }
}

fn glob(call: &ToolCall, is_cancelled: &mut dyn FnMut() -> bool) -> ToolResult {
    let Some(pattern) = string_arg(call, "pattern") else {
        return missing_arg(call, "pattern");
    };

    let path = string_arg(call, "path").unwrap_or(".");
    let path = match resolve_tool_path(path) {
        Ok(path) => path,
        Err(message) => return error(call, message),
    };
    if !program_in_path("rg") {
        return missing_ripgrep(call);
    }

    let mut command = Command::new("rg");
    command
        .args(["--files", "--glob", pattern, "--sort=modified"])
        .args(
            DEFAULT_GLOB_EXCLUDES
                .iter()
                .flat_map(|pattern| ["--glob", *pattern]),
        )
        .arg(path);

    match run_limited_glob_command(&mut command, is_cancelled, glob_limit(call), glob_timeout()) {
        Ok(output) => ok(call, format_glob_output(output)),
        Err(message) => error(call, message),
    }
}

fn run_limited_glob_command(
    command: &mut Command,
    is_cancelled: &mut dyn FnMut() -> bool,
    limit: usize,
    timeout: Duration,
) -> Result<GlobSearchOutput, String> {
    prepare_killable_command(command);
    let mut child = match command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(err) => return Err(format!("failed to run command: {err}")),
    };

    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let reader_count = usize::from(stdout.is_some()) + usize::from(stderr.is_some());
    let (tx, rx) = mpsc::channel();
    let stdout_reader = stdout
        .map(|stdout| spawn_line_reader(stdout, OutputStream::Stdout, tx.clone(), Some(limit + 1)));
    let stderr_reader =
        stderr.map(|stderr| spawn_line_reader(stderr, OutputStream::Stderr, tx, None));
    let started = Instant::now();
    let mut stdout_lines = Vec::new();
    let mut stderr_lines = Vec::new();
    let mut done_readers = 0;

    loop {
        if drain_output_events(
            &rx,
            &mut stdout_lines,
            &mut stderr_lines,
            &mut done_readers,
            limit,
        ) {
            kill_child_tree(&mut child);
            wait_for_reader(stdout_reader);
            wait_for_reader(stderr_reader);
            drain_output_events(
                &rx,
                &mut stdout_lines,
                &mut stderr_lines,
                &mut done_readers,
                limit,
            );
            return Ok(GlobSearchOutput {
                files: stdout_lines,
                truncated: true,
                timed_out: false,
            });
        }

        if is_cancelled() {
            kill_child_tree(&mut child);
            wait_for_reader(stdout_reader);
            wait_for_reader(stderr_reader);
            return Err("cancelled".to_owned());
        }

        if started.elapsed() >= timeout {
            kill_child_tree(&mut child);
            wait_for_reader(stdout_reader);
            wait_for_reader(stderr_reader);
            let reached_limit = drain_output_events(
                &rx,
                &mut stdout_lines,
                &mut stderr_lines,
                &mut done_readers,
                limit,
            );
            if stdout_lines.is_empty() {
                return Err(format!(
                    "Ripgrep search timed out after {} seconds. The search may have matched files but did not complete in time. Try searching a more specific path or pattern.",
                    timeout.as_secs()
                ));
            }
            return Ok(GlobSearchOutput {
                files: stdout_lines,
                truncated: reached_limit,
                timed_out: true,
            });
        }

        match child.try_wait() {
            Ok(Some(status)) => {
                let mut reached_limit = false;
                while done_readers < reader_count {
                    match rx.recv_timeout(POLL_INTERVAL) {
                        Ok(event) => {
                            reached_limit |= record_output_event(
                                event,
                                &mut stdout_lines,
                                &mut stderr_lines,
                                &mut done_readers,
                                limit,
                            );
                        }
                        Err(mpsc::RecvTimeoutError::Timeout) => {}
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    }
                }
                wait_for_reader(stdout_reader);
                wait_for_reader(stderr_reader);
                if reached_limit {
                    return Ok(GlobSearchOutput {
                        files: stdout_lines,
                        truncated: true,
                        timed_out: false,
                    });
                }
                if status.success() || status.code() == Some(1) {
                    return Ok(GlobSearchOutput {
                        files: stdout_lines,
                        truncated: false,
                        timed_out: false,
                    });
                }
                return Err(command_error_message(status, stderr_lines));
            }
            Ok(None) => thread::sleep(POLL_INTERVAL),
            Err(err) => return Err(format!("failed to wait for command: {err}")),
        }
    }
}

pub(super) struct GlobSearchOutput {
    pub(super) files: Vec<String>,
    pub(super) truncated: bool,
    pub(super) timed_out: bool,
}

#[derive(Clone, Copy)]
pub(super) enum OutputStream {
    Stdout,
    Stderr,
}

pub(super) enum OutputEvent {
    Line(OutputStream, String),
    Done,
}

fn spawn_line_reader<R: Read + Send + 'static>(
    reader: R,
    stream: OutputStream,
    tx: Sender<OutputEvent>,
    line_limit: Option<usize>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        for (line_count, line) in BufReader::new(reader).lines().enumerate() {
            if line_limit.is_some_and(|limit| line_count >= limit) {
                break;
            }
            let Ok(line) = line else {
                break;
            };
            if tx.send(OutputEvent::Line(stream, line)).is_err() {
                return;
            }
        }
        tx.send(OutputEvent::Done).ok();
    })
}

fn drain_output_events(
    rx: &Receiver<OutputEvent>,
    stdout_lines: &mut Vec<String>,
    stderr_lines: &mut Vec<String>,
    done_readers: &mut usize,
    limit: usize,
) -> bool {
    let mut reached_limit = false;
    while let Ok(event) = rx.try_recv() {
        reached_limit |=
            record_output_event(event, stdout_lines, stderr_lines, done_readers, limit);
    }
    reached_limit
}

pub(super) fn record_output_event(
    event: OutputEvent,
    stdout_lines: &mut Vec<String>,
    stderr_lines: &mut Vec<String>,
    done_readers: &mut usize,
    limit: usize,
) -> bool {
    match event {
        OutputEvent::Line(OutputStream::Stdout, line) => {
            if stdout_lines.len() < limit {
                stdout_lines.push(line);
                false
            } else {
                true
            }
        }
        OutputEvent::Line(OutputStream::Stderr, line) => {
            stderr_lines.push(line);
            false
        }
        OutputEvent::Done => {
            *done_readers += 1;
            false
        }
    }
}

fn wait_for_reader(reader: Option<thread::JoinHandle<()>>) {
    if let Some(reader) = reader {
        reader.join().ok();
    }
}

pub(super) fn format_glob_output(output: GlobSearchOutput) -> String {
    if output.files.is_empty() {
        return "No files found".to_owned();
    }

    let mut content = output.files.join("\n");
    if output.truncated {
        content
            .push_str("\n(Results are truncated. Consider using a more specific path or pattern.)");
    }
    if output.timed_out {
        content.push_str(
            "\n(Search timed out before completing. Consider using a more specific path or pattern.)",
        );
    }
    content
}

pub(super) fn missing_ripgrep(call: &ToolCall) -> ToolResult {
    error(
        call,
        "Missing dependency: ripgrep (`rg`) is required for Glob but was not found in PATH."
            .to_owned(),
    )
}

pub(super) fn glob_limit(call: &ToolCall) -> usize {
    usize_arg(call, "limit")
        .unwrap_or(GLOB_DEFAULT_LIMIT)
        .clamp(1, GLOB_MAX_LIMIT)
}

fn glob_timeout() -> Duration {
    let env_timeout = env::var(GLOB_TIMEOUT_ENV).ok();
    glob_timeout_from(env_timeout.as_deref(), running_on_wsl())
}

pub(super) fn glob_timeout_from(env_timeout: Option<&str>, running_on_wsl: bool) -> Duration {
    env_timeout
        .and_then(parse_glob_timeout_override)
        .unwrap_or_else(|| Duration::from_secs(default_glob_timeout_seconds(running_on_wsl)))
}

fn parse_glob_timeout_override(value: &str) -> Option<Duration> {
    let seconds = value.trim().parse::<u64>().ok()?;
    (seconds > 0).then(|| Duration::from_secs(seconds))
}

fn default_glob_timeout_seconds(running_on_wsl: bool) -> u64 {
    if running_on_wsl {
        GLOB_WSL_TIMEOUT_SECONDS
    } else {
        GLOB_DEFAULT_TIMEOUT_SECONDS
    }
}

fn running_on_wsl() -> bool {
    env::var_os("WSL_DISTRO_NAME").is_some()
        || fs::read_to_string("/proc/version").is_ok_and(|content| {
            let content = content.to_ascii_lowercase();
            content.contains("microsoft") || content.contains("wsl")
        })
}
