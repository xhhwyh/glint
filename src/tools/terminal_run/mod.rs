use std::{
    path::Path,
    sync::mpsc::{self, Sender},
    time::Duration,
};

use serde_json::Value;

use crate::{
    agent::provider::{ToolCall, ToolResult},
    terminal::{
        TERMINAL_RUN_DEFAULT_TIMEOUT_MS, TERMINAL_RUN_MAX_TIMEOUT_MS, TerminalRequest,
        TerminalRunResult,
    },
};

mod description;

use super::{
    ToolBehavior,
    bash::{
        bash_command_requires_approval, bash_requires_approval, contains_shell_control,
        dedicated_tool_replacement,
    },
    utils::{error, missing_arg, string_arg},
};

const DIRECT_OUTPUT_REPLACEMENT: &str =
    "Output text directly to the user instead of running echo or printf.";

const POLL_INTERVAL: Duration = Duration::from_millis(50);

pub(super) struct TerminalRunTool;

impl ToolBehavior for TerminalRunTool {
    fn name(&self) -> &'static str {
        "TerminalRun"
    }

    fn description(&self) -> &'static str {
        description::DESCRIPTION
    }

    fn required_args(&self) -> &'static [&'static str] {
        description::REQUIRED_ARGS
    }

    fn execute(&self, call: &ToolCall, _is_cancelled: &mut dyn FnMut() -> bool) -> ToolResult {
        error(
            call,
            "TerminalRun is unavailable without an agent terminal channel.".to_owned(),
        )
    }

    fn requires_approval(
        &self,
        call: &ToolCall,
        bash_prefix_allowed: bool,
        _edit_allowed: bool,
    ) -> bool {
        string_arg(call, "command")
            .is_none_or(|command| bash_command_requires_approval(command, bash_prefix_allowed))
    }

    fn input_summary(&self, call: &ToolCall) -> Option<String> {
        string_arg(call, "command").map(str::to_owned)
    }

    fn input_description(&self, call: &ToolCall) -> Option<String> {
        string_arg(call, "description").map(str::to_owned)
    }
}

pub(super) fn terminal_run(
    call: &ToolCall,
    terminal_requests: Option<&Sender<TerminalRequest>>,
    is_cancelled: &mut dyn FnMut() -> bool,
    approved: bool,
) -> ToolResult {
    let Some(command) = string_arg(call, "command") else {
        return missing_arg(call, "command");
    };
    let Some(description) = string_arg(call, "description") else {
        return missing_arg(call, "description");
    };

    if !approved && bash_requires_approval(command) {
        return error(
            call,
            format!("Approval required before running TerminalRun command: {command}"),
        );
    }

    if let Some(replacement) = terminal_run_replacement(command) {
        return error(
            call,
            format!(
                "TerminalRun command was not run because a dedicated tool should handle this action. {replacement}"
            ),
        );
    }

    let Some(terminal_requests) = terminal_requests else {
        return error(call, "agent terminal is unavailable".to_owned());
    };
    let timeout = terminal_timeout(call);
    let (response_tx, response_rx) = mpsc::channel();
    if let Err(err) = terminal_requests.send(TerminalRequest::Run {
        command: command.to_owned(),
        description: description.to_owned(),
        timeout,
        response: response_tx,
    }) {
        return error(call, format!("failed to request agent terminal: {err}"));
    }

    loop {
        if is_cancelled() {
            terminal_requests.send(TerminalRequest::CancelActive).ok();
            return error(call, "cancelled".to_owned());
        }

        match response_rx.recv_timeout(POLL_INTERVAL) {
            Ok(result) => return terminal_tool_result(call, result),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return error(call, "agent terminal response channel closed".to_owned());
            }
        }
    }
}

fn terminal_run_replacement(command: &str) -> Option<&'static str> {
    let replacement = dedicated_tool_replacement(command)?;
    if replacement == DIRECT_OUTPUT_REPLACEMENT && is_plain_terminal_output_command(command) {
        None
    } else {
        Some(replacement)
    }
}

fn is_plain_terminal_output_command(command: &str) -> bool {
    if contains_shell_control(command) {
        return false;
    }
    let Some(words) = shlex::split(command) else {
        return false;
    };
    words
        .first()
        .map(|word| program_name(word))
        .is_some_and(|program| matches!(program, "echo" | "printf"))
}

fn program_name(word: &str) -> &str {
    Path::new(word)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(word)
}

fn terminal_timeout(call: &ToolCall) -> Duration {
    let timeout_ms = call
        .arguments
        .get("timeout_ms")
        .and_then(Value::as_u64)
        .unwrap_or(TERMINAL_RUN_DEFAULT_TIMEOUT_MS)
        .clamp(1, TERMINAL_RUN_MAX_TIMEOUT_MS);
    Duration::from_millis(timeout_ms)
}

fn terminal_tool_result(call: &ToolCall, result: TerminalRunResult) -> ToolResult {
    let is_error = result.error.is_some() || result.timed_out || result.exit_code != Some(0);
    let exit_code = result
        .exit_code
        .map(|code| code.to_string())
        .unwrap_or_else(|| "unknown".to_owned());
    let mut content = format!(
        "command: {}\nexit_code: {}\ntimed_out: {}\noutput:\n{}",
        result.command, exit_code, result.timed_out, result.output
    );
    if let Some(error) = result.error {
        content.push_str("\nerror:\n");
        content.push_str(&error);
    }

    ToolResult {
        call_id: call.id.clone(),
        content,
        is_error,
    }
}
