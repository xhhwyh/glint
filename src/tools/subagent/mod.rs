use std::{
    path::PathBuf,
    sync::mpsc::{self, Sender},
    time::Duration,
};

use crate::{
    agent::provider::{ToolCall, ToolResult},
    tasks::{SubagentBackend, SubagentRequest, next_task_id},
    terminal::TerminalRequest,
};

mod description;

use super::{
    ToolBehavior,
    utils::{error, missing_arg, ok, string_arg},
};

const START_RESPONSE_TIMEOUT: Duration = Duration::from_secs(5);

pub(super) struct SubagentTool;

impl ToolBehavior for SubagentTool {
    fn name(&self) -> &'static str {
        "Subagent"
    }

    fn description(&self) -> &'static str {
        description::DESCRIPTION
    }

    fn required_args(&self) -> &'static [&'static str] {
        description::REQUIRED_ARGS
    }

    fn execute(&self, call: &ToolCall, _is_cancelled: &mut dyn FnMut() -> bool) -> ToolResult {
        error(call, "Subagent requires a terminal runtime.".to_owned())
    }

    fn requires_approval(
        &self,
        _call: &ToolCall,
        _bash_prefix_allowed: bool,
        _edit_allowed: bool,
    ) -> bool {
        false
    }

    fn input_summary(&self, call: &ToolCall) -> Option<String> {
        string_arg(call, "description").map(str::to_owned)
    }

    fn input_description(&self, call: &ToolCall) -> Option<String> {
        string_arg(call, "prompt").map(str::to_owned)
    }
}

pub(super) fn subagent(
    call: &ToolCall,
    terminal_requests: Option<&Sender<TerminalRequest>>,
) -> ToolResult {
    let Some(description) = string_arg(call, "description") else {
        return missing_arg(call, "description");
    };
    let Some(prompt) = string_arg(call, "prompt") else {
        return missing_arg(call, "prompt");
    };
    let backend = match SubagentBackend::parse(string_arg(call, "backend")) {
        Ok(backend) => backend,
        Err(message) => return error(call, message),
    };
    let cwd = match subagent_cwd(call) {
        Ok(cwd) => cwd,
        Err(message) => return error(call, message),
    };
    let Some(terminal_requests) = terminal_requests else {
        return error(call, "Subagent terminal runtime is unavailable.".to_owned());
    };

    let task_id = next_task_id();
    let request = SubagentRequest {
        task_id: task_id.clone(),
        description: description.to_owned(),
        prompt: prompt.to_owned(),
        backend,
        cwd,
    };
    let task_id = request.task_id.clone();
    let (response_tx, response_rx) = mpsc::channel();
    if terminal_requests
        .send(TerminalRequest::StartSubagent {
            request,
            response: response_tx,
        })
        .is_err()
    {
        return error(call, "Subagent terminal runtime is unavailable.".to_owned());
    }

    match response_rx.recv_timeout(START_RESPONSE_TIMEOUT) {
        Ok(response) => {
            if let Some(error_message) = response.error {
                return error(call, error_message);
            }
            ok(
                call,
                format!(
                    "Started Codex subagent {} in terminal tab {}. Its final result will be added to the model context when it completes.",
                    response.task_id,
                    response.terminal_tab.map(|tab| tab + 1).unwrap_or_default()
                ),
            )
        }
        Err(mpsc::RecvTimeoutError::Timeout) => error(
            call,
            format!("Timed out starting Codex subagent {task_id}."),
        ),
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            error(call, "Subagent terminal runtime stopped.".to_owned())
        }
    }
}

fn subagent_cwd(call: &ToolCall) -> Result<String, String> {
    let cwd = match string_arg(call, "cwd") {
        Some(cwd) if cwd.trim().starts_with('~') => {
            return Err(
                "cwd must not use ~; use an absolute or current-directory-relative path".to_owned(),
            );
        }
        Some(cwd) => {
            let path = PathBuf::from(cwd);
            if path.is_absolute() {
                path
            } else {
                std::env::current_dir()
                    .map_err(|error| format!("failed to resolve current directory: {error}"))?
                    .join(path)
            }
        }
        None => std::env::current_dir()
            .map_err(|error| format!("failed to resolve current directory: {error}"))?,
    };
    let cwd = cwd
        .canonicalize()
        .map_err(|error| format!("invalid cwd: {error}"))?;
    if !cwd.is_dir() {
        return Err("cwd must be an existing directory".to_owned());
    }
    Ok(cwd.display().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn rejects_unknown_backend() {
        let call = ToolCall {
            id: "call".to_owned(),
            name: "Subagent".to_owned(),
            arguments: json!({
                "description": "inspect parser",
                "prompt": "look at parser",
                "backend": "custom"
            }),
        };

        let result = subagent(&call, None);

        assert!(result.is_error);
        assert!(result.content.contains("unsupported subagent backend"));
    }

    #[test]
    fn rejects_tilde_cwd() {
        let call = ToolCall {
            id: "call".to_owned(),
            name: "Subagent".to_owned(),
            arguments: json!({
                "description": "inspect parser",
                "prompt": "look at parser",
                "cwd": "~/project"
            }),
        };

        let result = subagent(&call, None);

        assert!(result.is_error);
        assert!(result.content.contains("cwd must not use ~"));
    }

    #[test]
    fn starts_subagent_via_terminal_request() {
        let (terminal_tx, terminal_rx) = mpsc::channel();
        let cwd = std::env::current_dir().unwrap();
        let call = ToolCall {
            id: "call".to_owned(),
            name: "Subagent".to_owned(),
            arguments: json!({
                "description": "inspect parser",
                "prompt": "look at parser",
                "cwd": cwd
            }),
        };
        let worker = std::thread::spawn(move || {
            let TerminalRequest::StartSubagent { request, response } =
                terminal_rx.recv().expect("request")
            else {
                panic!("expected start subagent request");
            };
            assert_eq!(request.description, "inspect parser");
            assert_eq!(request.prompt, "look at parser");
            response
                .send(crate::tasks::SubagentStartResponse::started(
                    request.task_id,
                    0,
                ))
                .unwrap();
        });

        let result = subagent(&call, Some(&terminal_tx));
        worker.join().unwrap();

        assert!(!result.is_error);
        assert!(result.content.contains("Started Codex subagent"));
        assert!(result.content.contains("terminal tab 1"));
    }
}
