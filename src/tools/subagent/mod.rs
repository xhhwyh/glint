use std::{
    path::PathBuf,
    sync::mpsc::{self, Sender},
    time::Duration,
};

use crate::{
    agent::provider::{ToolCall, ToolResult},
    tasks::{SubagentBackend, SubagentRequest, TaskRequest, next_task_id},
};

mod description;

use super::{
    ToolBehavior,
    utils::{current_tool_dir, error, missing_arg, ok, string_arg},
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
        error(call, "Subagent requires the task runtime.".to_owned())
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

pub(super) fn subagent(call: &ToolCall, task_requests: Option<&Sender<TaskRequest>>) -> ToolResult {
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
    let Some(task_requests) = task_requests else {
        return error(call, "Subagent task runtime is unavailable.".to_owned());
    };

    let task_id = next_task_id();
    let request = SubagentRequest {
        task_id: task_id.clone(),
        tool_call_id: call.id.clone(),
        description: description.to_owned(),
        prompt: prompt.to_owned(),
        agent: string_arg(call, "agent").map(str::to_owned),
        backend,
        cwd,
    };
    let task_id = request.task_id.clone();
    let (response_tx, response_rx) = mpsc::channel();
    if task_requests
        .send(TaskRequest::StartSubagent {
            request,
            response: response_tx,
        })
        .is_err()
    {
        return error(call, "Subagent task runtime is unavailable.".to_owned());
    }

    match response_rx.recv_timeout(START_RESPONSE_TIMEOUT) {
        Ok(response) => {
            if let Some(error_message) = response.error {
                return error(call, error_message);
            }
            ok(
                call,
                format!(
                    "Started Codex subagent {}. Use TaskWait for its result, TaskSend to refine it, or TaskCancel to stop it.",
                    response.task_id,
                ),
            )
        }
        Err(mpsc::RecvTimeoutError::Timeout) => error(
            call,
            format!("Timed out starting Codex subagent {task_id}."),
        ),
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            error(call, "Subagent task runtime stopped.".to_owned())
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
                current_tool_dir()?.join(path)
            }
        }
        None => current_tool_dir()?,
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
    use std::fs;

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
    fn starts_subagent_via_task_request() {
        let (task_tx, task_rx) = mpsc::channel();
        let workspace = std::env::temp_dir().join(format!(
            "glint-subagent-request-workspace-{}",
            uuid::Uuid::new_v4()
        ));
        let cwd = workspace.join("child");
        fs::create_dir_all(&cwd).unwrap();
        let call = ToolCall {
            id: "call".to_owned(),
            name: "Subagent".to_owned(),
            arguments: json!({
                "description": "inspect parser",
                "prompt": "look at parser",
                "cwd": "child"
            }),
        };
        let expected_cwd = cwd.canonicalize().unwrap();
        let worker = std::thread::spawn(move || {
            let TaskRequest::StartSubagent { request, response } = task_rx.recv().expect("request")
            else {
                panic!("expected start subagent request");
            };
            assert_eq!(request.description, "inspect parser");
            assert_eq!(request.prompt, "look at parser");
            assert_eq!(request.tool_call_id, "call");
            assert_eq!(PathBuf::from(&request.cwd), expected_cwd);
            response
                .send(crate::tasks::SubagentStartResponse::started(
                    request.task_id,
                ))
                .unwrap();
        });

        let context = crate::tools::ToolContext::new(workspace.clone(), workspace.clone());
        let result = crate::tools::with_tool_context(context, || subagent(&call, Some(&task_tx)));
        worker.join().unwrap();

        assert!(!result.is_error);
        assert!(result.content.contains("Started Codex subagent"));
        assert!(result.content.contains("Use TaskWait for its result"));
        fs::remove_dir_all(workspace).ok();
    }

    #[test]
    fn relative_subagent_cwd_uses_the_scoped_tool_workspace() {
        let workspace =
            std::env::temp_dir().join(format!("glint-subagent-workspace-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(workspace.join("child")).unwrap();
        let call = ToolCall {
            id: "call".to_owned(),
            name: "Subagent".to_owned(),
            arguments: json!({
                "description": "inspect child",
                "prompt": "look at child",
                "cwd": "child"
            }),
        };

        let context = crate::tools::ToolContext::new(workspace.clone(), workspace.clone());
        let cwd = crate::tools::with_tool_context(context, || subagent_cwd(&call)).unwrap();

        assert_eq!(
            PathBuf::from(cwd),
            workspace.join("child").canonicalize().unwrap()
        );
        fs::remove_dir_all(workspace).ok();
    }
}
