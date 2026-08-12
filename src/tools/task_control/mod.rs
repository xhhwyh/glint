use std::{
    sync::mpsc::{self, Sender},
    time::{Duration, Instant},
};

use serde_json::{Value, json};

use crate::{
    agent::provider::{ToolCall, ToolResult},
    tasks::{TaskKind, TaskSnapshot, TaskWaitResponse},
    terminal::TerminalRequest,
};

use super::{
    ToolBehavior,
    utils::{error, missing_arg, ok, string_arg},
};

mod description;

const CONTROL_RESPONSE_TIMEOUT: Duration = Duration::from_secs(5);
const RESPONSE_POLL_INTERVAL: Duration = Duration::from_millis(100);
const DEFAULT_WAIT_TIMEOUT_MS: u64 = 30_000;
pub const MAX_WAIT_TIMEOUT_MS: u64 = 300_000;

pub(super) struct TaskListTool;
pub(super) struct TaskWaitTool;
pub(super) struct TaskSendTool;
pub(super) struct TaskCancelTool;

impl ToolBehavior for TaskListTool {
    fn name(&self) -> &'static str {
        "TaskList"
    }

    fn description(&self) -> &'static str {
        description::LIST_DESCRIPTION
    }

    fn required_args(&self) -> &'static [&'static str] {
        description::NO_REQUIRED_ARGS
    }

    fn execute(&self, call: &ToolCall, _is_cancelled: &mut dyn FnMut() -> bool) -> ToolResult {
        error(call, "TaskList requires the task runtime.".to_owned())
    }

    fn requires_approval(&self, _call: &ToolCall, _bash: bool, _edit: bool) -> bool {
        false
    }

    fn is_concurrency_safe(&self, _call: &ToolCall) -> bool {
        true
    }
}

impl ToolBehavior for TaskWaitTool {
    fn name(&self) -> &'static str {
        "TaskWait"
    }

    fn description(&self) -> &'static str {
        description::WAIT_DESCRIPTION
    }

    fn required_args(&self) -> &'static [&'static str] {
        description::WAIT_REQUIRED_ARGS
    }

    fn execute(&self, call: &ToolCall, _is_cancelled: &mut dyn FnMut() -> bool) -> ToolResult {
        error(call, "TaskWait requires the task runtime.".to_owned())
    }

    fn requires_approval(&self, _call: &ToolCall, _bash: bool, _edit: bool) -> bool {
        false
    }

    fn is_concurrency_safe(&self, _call: &ToolCall) -> bool {
        true
    }

    fn input_summary(&self, call: &ToolCall) -> Option<String> {
        task_ids(call).ok().map(|ids| ids.join(", "))
    }
}

impl ToolBehavior for TaskSendTool {
    fn name(&self) -> &'static str {
        "TaskSend"
    }

    fn description(&self) -> &'static str {
        description::SEND_DESCRIPTION
    }

    fn required_args(&self) -> &'static [&'static str] {
        description::SEND_REQUIRED_ARGS
    }

    fn execute(&self, call: &ToolCall, _is_cancelled: &mut dyn FnMut() -> bool) -> ToolResult {
        error(call, "TaskSend requires the task runtime.".to_owned())
    }

    fn requires_approval(&self, _call: &ToolCall, _bash: bool, _edit: bool) -> bool {
        false
    }

    fn is_concurrency_safe(&self, _call: &ToolCall) -> bool {
        true
    }

    fn input_summary(&self, call: &ToolCall) -> Option<String> {
        string_arg(call, "task_id").map(str::to_owned)
    }

    fn input_description(&self, call: &ToolCall) -> Option<String> {
        string_arg(call, "message").map(str::to_owned)
    }
}

impl ToolBehavior for TaskCancelTool {
    fn name(&self) -> &'static str {
        "TaskCancel"
    }

    fn description(&self) -> &'static str {
        description::CANCEL_DESCRIPTION
    }

    fn required_args(&self) -> &'static [&'static str] {
        description::CANCEL_REQUIRED_ARGS
    }

    fn execute(&self, call: &ToolCall, _is_cancelled: &mut dyn FnMut() -> bool) -> ToolResult {
        error(call, "TaskCancel requires the task runtime.".to_owned())
    }

    fn requires_approval(&self, _call: &ToolCall, _bash: bool, _edit: bool) -> bool {
        false
    }

    fn is_concurrency_safe(&self, _call: &ToolCall) -> bool {
        true
    }

    fn input_summary(&self, call: &ToolCall) -> Option<String> {
        string_arg(call, "task_id").map(str::to_owned)
    }
}

pub(super) fn execute(
    call: &ToolCall,
    terminal_requests: Option<&Sender<TerminalRequest>>,
    is_cancelled: &mut dyn FnMut() -> bool,
) -> ToolResult {
    let Some(terminal_requests) = terminal_requests else {
        return error(call, "Task runtime is unavailable.".to_owned());
    };
    match call.name.as_str() {
        "TaskList" => list_tasks(call, terminal_requests, is_cancelled),
        "TaskWait" => wait_tasks(call, terminal_requests, is_cancelled),
        "TaskSend" => send_task_message(call, terminal_requests, is_cancelled),
        "TaskCancel" => cancel_task(call, terminal_requests, is_cancelled),
        _ => error(call, format!("Unknown task control tool: {}", call.name)),
    }
}

fn list_tasks(
    call: &ToolCall,
    terminal_requests: &Sender<TerminalRequest>,
    is_cancelled: &mut dyn FnMut() -> bool,
) -> ToolResult {
    let (response, receiver) = mpsc::channel();
    if terminal_requests
        .send(TerminalRequest::ListTasks { response })
        .is_err()
    {
        return error(call, "Task runtime is unavailable.".to_owned());
    }
    match receive_until(&receiver, CONTROL_RESPONSE_TIMEOUT, is_cancelled) {
        Ok(tasks) => ok(call, format_tasks(&tasks, false)),
        Err(message) => error(call, message),
    }
}

fn wait_tasks(
    call: &ToolCall,
    terminal_requests: &Sender<TerminalRequest>,
    is_cancelled: &mut dyn FnMut() -> bool,
) -> ToolResult {
    let task_ids = match task_ids(call) {
        Ok(task_ids) => task_ids,
        Err(message) => return error(call, message),
    };
    let timeout_ms = match wait_timeout_ms(call) {
        Ok(timeout_ms) => timeout_ms,
        Err(message) => return error(call, message),
    };
    let timeout = Duration::from_millis(timeout_ms);
    let (response, receiver) = mpsc::channel();
    if terminal_requests
        .send(TerminalRequest::WaitTasks {
            task_ids,
            timeout,
            response,
        })
        .is_err()
    {
        return error(call, "Task runtime is unavailable.".to_owned());
    }
    match receive_until(&receiver, timeout + CONTROL_RESPONSE_TIMEOUT, is_cancelled) {
        Ok(Ok(TaskWaitResponse { tasks, timed_out })) => ok(call, format_tasks(&tasks, timed_out)),
        Ok(Err(message)) | Err(message) => error(call, message),
    }
}

fn send_task_message(
    call: &ToolCall,
    terminal_requests: &Sender<TerminalRequest>,
    is_cancelled: &mut dyn FnMut() -> bool,
) -> ToolResult {
    let Some(task_id) = string_arg(call, "task_id") else {
        return missing_arg(call, "task_id");
    };
    let Some(message) = string_arg(call, "message") else {
        return missing_arg(call, "message");
    };
    if message.trim().is_empty() {
        return error(call, "message must not be empty".to_owned());
    }
    let (response, receiver) = mpsc::channel();
    if terminal_requests
        .send(TerminalRequest::SendTaskMessage {
            task_id: task_id.to_owned(),
            message: message.to_owned(),
            response,
        })
        .is_err()
    {
        return error(call, "Task runtime is unavailable.".to_owned());
    }
    match receive_until(&receiver, CONTROL_RESPONSE_TIMEOUT, is_cancelled) {
        Ok(Ok(task)) => ok(
            call,
            format!("Message accepted by running task {}.", task.id),
        ),
        Ok(Err(message)) | Err(message) => error(call, message),
    }
}

fn cancel_task(
    call: &ToolCall,
    terminal_requests: &Sender<TerminalRequest>,
    is_cancelled: &mut dyn FnMut() -> bool,
) -> ToolResult {
    let Some(task_id) = string_arg(call, "task_id") else {
        return missing_arg(call, "task_id");
    };
    let (response, receiver) = mpsc::channel();
    if terminal_requests
        .send(TerminalRequest::CancelTask {
            task_id: task_id.to_owned(),
            response,
        })
        .is_err()
    {
        return error(call, "Task runtime is unavailable.".to_owned());
    }
    match receive_until(&receiver, CONTROL_RESPONSE_TIMEOUT, is_cancelled) {
        Ok(Ok(task)) => ok(
            call,
            format!("Cancellation requested for task {}.", task.id),
        ),
        Ok(Err(message)) | Err(message) => error(call, message),
    }
}

fn receive_until<T>(
    receiver: &mpsc::Receiver<T>,
    timeout: Duration,
    is_cancelled: &mut dyn FnMut() -> bool,
) -> Result<T, String> {
    let deadline = Instant::now() + timeout;
    loop {
        if is_cancelled() {
            return Err("Task control call cancelled.".to_owned());
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err("Timed out waiting for the task runtime.".to_owned());
        }
        match receiver.recv_timeout(remaining.min(RESPONSE_POLL_INTERVAL)) {
            Ok(response) => return Ok(response),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err("Task runtime stopped.".to_owned());
            }
        }
    }
}

fn task_ids(call: &ToolCall) -> Result<Vec<String>, String> {
    let Some(values) = call.arguments.get("task_ids").and_then(Value::as_array) else {
        return Err("Missing required argument: task_ids".to_owned());
    };
    let mut task_ids = Vec::new();
    for value in values {
        let Some(task_id) = value.as_str() else {
            return Err("task_ids must be an array of strings".to_owned());
        };
        if task_id.trim().is_empty() {
            return Err("task_ids must not contain empty values".to_owned());
        }
        if !task_ids.iter().any(|existing| existing == task_id) {
            task_ids.push(task_id.to_owned());
        }
    }
    if task_ids.is_empty() {
        return Err("task_ids must contain at least one task".to_owned());
    }
    Ok(task_ids)
}

fn wait_timeout_ms(call: &ToolCall) -> Result<u64, String> {
    let timeout_ms = call
        .arguments
        .get("timeout_ms")
        .and_then(Value::as_u64)
        .unwrap_or(DEFAULT_WAIT_TIMEOUT_MS);
    if timeout_ms == 0 || timeout_ms > MAX_WAIT_TIMEOUT_MS {
        return Err(format!(
            "timeout_ms must be between 1 and {MAX_WAIT_TIMEOUT_MS}"
        ));
    }
    Ok(timeout_ms)
}

fn format_tasks(tasks: &[TaskSnapshot], timed_out: bool) -> String {
    json!({
        "timed_out": timed_out,
        "tasks": tasks.iter().map(task_json).collect::<Vec<_>>()
    })
    .to_string()
}

fn task_json(task: &TaskSnapshot) -> Value {
    json!({
        "id": task.id,
        "kind": match task.kind { TaskKind::Subagent => "subagent" },
        "status": task.status.label(),
        "description": task.description,
        "backend": task.backend.label(),
        "cwd": task.cwd,
        "terminal_tab": task.terminal_tab.map(|tab| tab + 1),
        "started_at_ms": task.started_at_ms,
        "ended_at_ms": task.ended_at_ms,
        "summary": task.summary,
        "activity": task.activity,
        "tool_use_count": task.tool_use_count,
        "result": task.result,
        "error": task.error,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tasks::{SubagentBackend, TaskStatus};

    fn call(name: &str, arguments: Value) -> ToolCall {
        ToolCall {
            id: "call".to_owned(),
            name: name.to_owned(),
            arguments,
        }
    }

    fn snapshot() -> TaskSnapshot {
        TaskSnapshot {
            id: "a1".to_owned(),
            kind: TaskKind::Subagent,
            status: TaskStatus::Completed,
            description: "inspect parser".to_owned(),
            backend: SubagentBackend::Codex,
            cwd: "/workspace".to_owned(),
            terminal_tab: Some(1),
            started_at_ms: 1,
            ended_at_ms: Some(2),
            summary: Some("completed".to_owned()),
            activity: Some("completed".to_owned()),
            tool_use_count: 2,
            result: Some("done".to_owned()),
            error: None,
        }
    }

    #[test]
    fn task_wait_returns_structured_result() {
        let (terminal_tx, terminal_rx) = mpsc::channel();
        let tool_call = call("TaskWait", json!({"task_ids": ["a1"], "timeout_ms": 1000}));
        let worker_call = tool_call.clone();
        let worker =
            std::thread::spawn(move || execute(&worker_call, Some(&terminal_tx), &mut || false));

        let TerminalRequest::WaitTasks {
            task_ids,
            timeout,
            response,
        } = terminal_rx.recv().unwrap()
        else {
            panic!("expected task wait request");
        };
        assert_eq!(task_ids, vec!["a1"]);
        assert_eq!(timeout, Duration::from_secs(1));
        response
            .send(Ok(TaskWaitResponse {
                tasks: vec![snapshot()],
                timed_out: false,
            }))
            .unwrap();

        let result = worker.join().unwrap();
        assert!(!result.is_error);
        let payload: Value = serde_json::from_str(&result.content).unwrap();
        assert_eq!(payload["tasks"][0]["status"], "completed");
        assert_eq!(payload["tasks"][0]["result"], "done");
        assert_eq!(payload["tasks"][0]["tool_use_count"], 2);
    }

    #[test]
    fn task_wait_rejects_empty_task_ids() {
        let tool_call = call("TaskWait", json!({"task_ids": []}));
        let (terminal_tx, _terminal_rx) = mpsc::channel();

        let result = execute(&tool_call, Some(&terminal_tx), &mut || false);

        assert!(result.is_error);
        assert!(result.content.contains("at least one task"));
    }
}
