use std::{
    path::{Path, PathBuf},
    sync::mpsc::{Receiver, Sender},
    thread,
};

use anyhow::{Context, Result, bail};

use crate::{
    approval::{AgentControl, ApprovalDecision, ApprovalRequest, ConversationPermissions},
    config::LlmConfig,
    message::Message,
    settings::ProjectSettings,
};

use super::{
    AgentEvent, RuntimeContext,
    context::build_initial_messages,
    openai::OpenAiProvider,
    provider::{FinishReason, ModelMessage, ModelProvider, ModelRequest, ModelResponse, ToolCall},
    tools::ToolRegistry,
};

const MAX_TOOL_ITERATIONS: usize = 8;

#[derive(Clone)]
pub struct AgentRunInput {
    pub llm: LlmConfig,
    pub system_prompt: String,
    pub runtime_context: RuntimeContext,
    pub conversation_permissions: ConversationPermissions,
    pub conversation: Vec<Message>,
    pub current_user_message: String,
}

pub fn spawn_agent_loop(
    input: AgentRunInput,
    tx: Sender<AgentEvent>,
    control_rx: Receiver<AgentControl>,
) {
    thread::spawn(move || {
        let mut provider = OpenAiProvider::new(input.llm.clone());
        let registry = ToolRegistry::new();

        match run_agent_loop(input, &mut provider, &registry, &tx, &control_rx) {
            Ok(()) => {
                tx.send(AgentEvent::AssistantFinished).ok();
            }
            Err(error) => {
                tx.send(AgentEvent::Failed(format!("LLM error: {error:#}")))
                    .ok();
            }
        }
    });
}

fn run_agent_loop(
    input: AgentRunInput,
    provider: &mut impl ModelProvider,
    registry: &ToolRegistry,
    tx: &Sender<AgentEvent>,
    control_rx: &Receiver<AgentControl>,
) -> Result<()> {
    tx.send(AgentEvent::Started).ok();

    let mut messages = build_initial_messages(
        &input.system_prompt,
        &input.runtime_context,
        &input.conversation,
        &input.current_user_message,
    );

    run_model_turns(
        &mut messages,
        provider,
        registry,
        tx,
        control_rx,
        input.conversation_permissions,
    )
}

fn run_model_turns(
    messages: &mut Vec<ModelMessage>,
    provider: &mut impl ModelProvider,
    registry: &ToolRegistry,
    tx: &Sender<AgentEvent>,
    control_rx: &Receiver<AgentControl>,
    initial_permissions: ConversationPermissions,
) -> Result<()> {
    let mut tool_iterations = 0;
    let mut project_settings = ProjectSettings::load();
    let mut conversation_permissions = initial_permissions;

    loop {
        let mut on_delta = |delta: String| {
            tx.send(AgentEvent::AssistantDelta(delta)).ok();
        };
        let response = provider
            .stream(
                ModelRequest {
                    messages: messages.clone(),
                    tools: registry.specs(),
                },
                &mut on_delta,
            )
            .context("model request failed")?;

        if let Some(usage) = response.usage {
            tx.send(AgentEvent::Usage(usage)).ok();
        }

        if !response.tool_calls.is_empty() {
            tool_iterations += 1;
            if tool_iterations > MAX_TOOL_ITERATIONS {
                bail!("maximum tool iterations exceeded");
            }

            append_tool_turn(
                messages,
                response,
                registry,
                tx,
                control_rx,
                &mut project_settings,
                &mut conversation_permissions,
            )?;
            continue;
        }

        return finish_without_tools(response);
    }
}

fn append_tool_turn(
    messages: &mut Vec<ModelMessage>,
    response: ModelResponse,
    registry: &ToolRegistry,
    tx: &Sender<AgentEvent>,
    control_rx: &Receiver<AgentControl>,
    project_settings: &mut ProjectSettings,
    conversation_permissions: &mut ConversationPermissions,
) -> Result<()> {
    let assistant_text = response.assistant_text.filter(|text| !text.is_empty());
    messages.push(ModelMessage::assistant(
        assistant_text,
        response.tool_calls.clone(),
    ));

    for call in response.tool_calls {
        tx.send(AgentEvent::ToolStarted {
            id: call.id.clone(),
            name: call.name.clone(),
            input_summary: summarize_tool_input(&call),
        })
        .ok();

        let result = execute_tool_with_approval(
            registry,
            call.clone(),
            tx,
            control_rx,
            project_settings,
            conversation_permissions,
        )?;

        tx.send(AgentEvent::ToolFinished {
            id: call.id.clone(),
            name: call.name,
            output_summary: summarize_tool_output(&result.content),
        })
        .ok();

        messages.push(ModelMessage::tool_result(&result));
    }
    Ok(())
}

fn execute_tool_with_approval(
    registry: &ToolRegistry,
    call: ToolCall,
    tx: &Sender<AgentEvent>,
    control_rx: &Receiver<AgentControl>,
    project_settings: &mut ProjectSettings,
    conversation_permissions: &mut ConversationPermissions,
) -> Result<super::provider::ToolResult> {
    drain_control_messages(control_rx, tx, conversation_permissions);
    let bash_prefix_allowed = call
        .arguments
        .get("command")
        .and_then(|value| value.as_str())
        .is_some_and(|command| project_settings.allows_bash(command));
    let requires_approval = registry.requires_approval(
        &call,
        bash_prefix_allowed,
        conversation_permissions.edit_always_allowed,
    );
    if !requires_approval {
        return Ok(registry.execute_approved(&call));
    }

    let request = ApprovalRequest {
        id: next_approval_id(),
        tool_name: call.name.clone(),
        command: summarize_tool_input(&call),
        explanation: approval_explanation(&call),
    };
    tx.send(AgentEvent::ToolApprovalRequested(request.clone()))
        .ok();

    loop {
        match control_rx.recv().context("approval channel closed")? {
            AgentControl::ApprovalDecision { id, decision } if id == request.id => {
                return handle_approval_decision(
                    registry,
                    call,
                    decision,
                    tx,
                    project_settings,
                    conversation_permissions,
                );
            }
            AgentControl::ClearConversationEditPermission => {
                conversation_permissions.edit_always_allowed = false;
                tx.send(AgentEvent::ConversationPermissionChanged {
                    edit_always_allowed: false,
                })
                .ok();
            }
            AgentControl::ApprovalDecision { .. } => {}
        }
    }
}

fn drain_control_messages(
    control_rx: &Receiver<AgentControl>,
    tx: &Sender<AgentEvent>,
    conversation_permissions: &mut ConversationPermissions,
) {
    while let Ok(message) = control_rx.try_recv() {
        match message {
            AgentControl::ClearConversationEditPermission => {
                conversation_permissions.edit_always_allowed = false;
                tx.send(AgentEvent::ConversationPermissionChanged {
                    edit_always_allowed: false,
                })
                .ok();
            }
            AgentControl::ApprovalDecision { .. } => {}
        }
    }
}

fn handle_approval_decision(
    registry: &ToolRegistry,
    call: ToolCall,
    decision: ApprovalDecision,
    tx: &Sender<AgentEvent>,
    project_settings: &mut ProjectSettings,
    conversation_permissions: &mut ConversationPermissions,
) -> Result<super::provider::ToolResult> {
    match decision {
        ApprovalDecision::AllowOnce => Ok(registry.execute_approved(&call)),
        ApprovalDecision::AllowProjectPrefix => {
            if let Some(command) = call
                .arguments
                .get("command")
                .and_then(|value| value.as_str())
            {
                project_settings.allow_bash_prefix(command)?;
            }
            Ok(registry.execute_approved(&call))
        }
        ApprovalDecision::AllowConversation => {
            conversation_permissions.edit_always_allowed = true;
            tx.send(AgentEvent::ConversationPermissionChanged {
                edit_always_allowed: true,
            })
            .ok();
            Ok(registry.execute_approved(&call))
        }
        ApprovalDecision::Deny { feedback } => Ok(super::provider::ToolResult {
            call_id: call.id,
            content: if feedback.is_empty() {
                "Denied by user.".to_owned()
            } else {
                format!("Denied by user. Feedback: {feedback}")
            },
            is_error: true,
        }),
    }
}

fn approval_explanation(call: &ToolCall) -> String {
    match call.name.as_str() {
        "Bash" => "This Bash command can modify project state and needs approval.".to_owned(),
        "Edit" => "This Edit will modify a file and always needs approval unless allowed for this conversation.".to_owned(),
        _ => format!("{} needs approval before it can run.", call.name),
    }
}

fn next_approval_id() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT_ID: AtomicU64 = AtomicU64::new(1);
    NEXT_ID.fetch_add(1, Ordering::Relaxed)
}

fn finish_without_tools(response: ModelResponse) -> Result<()> {
    match response.finish_reason {
        FinishReason::Stop => Ok(()),
        FinishReason::Length => bail!("model stopped because max_tokens was reached"),
        FinishReason::ToolCalls => {
            bail!("model stopped for tool calls without any tool call payload")
        }
        FinishReason::Other(reason) => bail!("model stopped for unsupported reason: {reason}"),
    }
}

fn summarize_tool_input(call: &ToolCall) -> String {
    let summary = match call.name.as_str() {
        "Read" => path_arg(call, "file_path"),
        "Glob" => glob_summary(call),
        "Grep" => string_arg(call, "pattern").map(str::to_owned),
        "Bash" => string_arg(call, "command").map(str::to_owned),
        "Edit" => path_arg(call, "file_path"),
        _ => None,
    };

    let summary = summary.unwrap_or_else(|| call.arguments.to_string());
    truncate_summary(&summary)
}

fn summarize_tool_output(output: &str) -> String {
    truncate_summary(output)
}

fn truncate_summary(output: &str) -> String {
    const MAX_SUMMARY_CHARS: usize = 120;

    if output.chars().count() <= MAX_SUMMARY_CHARS {
        return output.to_owned();
    }

    format!(
        "{}...",
        output.chars().take(MAX_SUMMARY_CHARS).collect::<String>()
    )
}

fn glob_summary(call: &ToolCall) -> Option<String> {
    let pattern = string_arg(call, "pattern")?;
    let Some(path) = string_arg(call, "path") else {
        return Some(pattern.to_owned());
    };

    let display_path = display_path(path);
    if display_path == "." {
        Some(pattern.to_owned())
    } else {
        Some(format!("{display_path} ｜ {pattern}"))
    }
}

fn path_arg(call: &ToolCall, name: &str) -> Option<String> {
    string_arg(call, name).map(display_path)
}

fn string_arg<'a>(call: &'a ToolCall, name: &str) -> Option<&'a str> {
    call.arguments.get(name).and_then(|value| value.as_str())
}

fn display_path(path: &str) -> String {
    let path = Path::new(path);
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    };
    let display_path = absolute.canonicalize().unwrap_or(absolute);

    display_path
        .strip_prefix(&cwd)
        .map(display_relative_path)
        .unwrap_or_else(|_| display_path.display().to_string())
}

fn display_relative_path(path: &Path) -> String {
    let display = path.display().to_string();
    if display.is_empty() {
        return ".".to_owned();
    }

    display
        .strip_prefix("./")
        .unwrap_or(&display)
        .trim_end_matches('/')
        .to_owned()
}

#[cfg(test)]
mod tests {
    use std::{collections::VecDeque, sync::mpsc};

    use anyhow::Result;
    use serde_json::json;

    use super::*;
    use crate::agent::provider::{ModelRole, ToolResult};

    struct FakeProvider {
        responses: VecDeque<ModelResponse>,
        requests: Vec<ModelRequest>,
    }

    impl FakeProvider {
        fn new(responses: Vec<ModelResponse>) -> Self {
            Self {
                responses: responses.into(),
                requests: Vec::new(),
            }
        }
    }

    impl ModelProvider for FakeProvider {
        fn complete(&mut self, request: ModelRequest) -> Result<ModelResponse> {
            self.requests.push(request);
            self.responses
                .pop_front()
                .ok_or_else(|| anyhow::anyhow!("no fake response queued"))
        }
    }

    struct RepeatingToolProvider {
        requests: Vec<ModelRequest>,
    }

    impl ModelProvider for RepeatingToolProvider {
        fn complete(&mut self, request: ModelRequest) -> Result<ModelResponse> {
            self.requests.push(request);
            Ok(tool_response("call"))
        }
    }

    fn input() -> AgentRunInput {
        AgentRunInput {
            llm: LlmConfig {
                base_url: "http://localhost".to_owned(),
                model: "test-model".to_owned(),
                temperature: 0.0,
                max_tokens: 100,
                context_window: Some(1000),
                api_key: "test-key".to_owned(),
            },
            system_prompt: "system".to_owned(),
            runtime_context: RuntimeContext {
                current_time: "unix_seconds=1".to_owned(),
                current_dir: "/workspace".to_owned(),
                shell: "/bin/zsh".to_owned(),
                app_name: "glint".to_owned(),
                app_version: "0.1.0".to_owned(),
                tool_mode: "available tools: Read, Glob, Grep, Bash, Edit".to_owned(),
            },
            conversation_permissions: ConversationPermissions::default(),
            conversation: Vec::new(),
            current_user_message: "hello".to_owned(),
        }
    }

    fn final_response(text: &str) -> ModelResponse {
        ModelResponse {
            assistant_text: Some(text.to_owned()),
            tool_calls: Vec::new(),
            finish_reason: FinishReason::Stop,
            usage: None,
        }
    }

    fn tool_response(id_suffix: &str) -> ModelResponse {
        ModelResponse {
            assistant_text: None,
            tool_calls: vec![ToolCall {
                id: format!("tool-{id_suffix}"),
                name: "read_file".to_owned(),
                arguments: json!({ "path": "Cargo.toml" }),
            }],
            finish_reason: FinishReason::ToolCalls,
            usage: None,
        }
    }

    fn control_rx() -> Receiver<AgentControl> {
        let (_tx, rx) = mpsc::channel();
        rx
    }

    #[test]
    fn final_assistant_response_ends_the_loop() {
        let (tx, rx) = mpsc::channel();
        let registry = ToolRegistry::new();
        let mut provider = FakeProvider::new(vec![final_response("done")]);

        let control_rx = control_rx();
        run_agent_loop(input(), &mut provider, &registry, &tx, &control_rx).unwrap();

        let events: Vec<_> = rx.try_iter().collect();
        assert_eq!(provider.requests.len(), 1);
        assert!(matches!(events.first(), Some(AgentEvent::Started)));
        assert!(
            events
                .iter()
                .any(|event| matches!(event, AgentEvent::AssistantDelta(text) if text == "done"))
        );
    }

    #[test]
    fn tool_call_causes_second_model_request_with_tool_result() {
        let (tx, _rx) = mpsc::channel();
        let registry = ToolRegistry::new();
        let mut provider = FakeProvider::new(vec![tool_response("one"), final_response("done")]);

        let control_rx = control_rx();
        run_agent_loop(input(), &mut provider, &registry, &tx, &control_rx).unwrap();

        assert_eq!(provider.requests.len(), 2);
        let second_request = &provider.requests[1];
        assert!(second_request.messages.iter().any(|message| {
            message.role == ModelRole::Tool
                && message.tool_call_id.as_deref() == Some("tool-one")
                && message
                    .content
                    .as_deref()
                    .is_some_and(|content| content.contains("not registered"))
        }));
    }

    #[test]
    fn unknown_tool_result_is_marked_as_error() {
        let registry = ToolRegistry::new();
        let result = registry.execute(&ToolCall {
            id: "tool-one".to_owned(),
            name: "missing_tool".to_owned(),
            arguments: json!({}),
        });

        assert_eq!(
            result,
            ToolResult {
                call_id: "tool-one".to_owned(),
                content: "Tool 'missing_tool' is not registered.".to_owned(),
                is_error: true,
            }
        );
    }

    #[test]
    fn max_tool_iterations_fail_cleanly() {
        let (tx, _rx) = mpsc::channel();
        let registry = ToolRegistry::new();
        let mut provider = RepeatingToolProvider {
            requests: Vec::new(),
        };

        let control_rx = control_rx();
        let error =
            run_agent_loop(input(), &mut provider, &registry, &tx, &control_rx).unwrap_err();

        assert_eq!(provider.requests.len(), MAX_TOOL_ITERATIONS + 1);
        assert!(format!("{error:#}").contains("maximum tool iterations exceeded"));
    }

    #[test]
    fn summarizes_tool_inputs_without_json() {
        assert_eq!(
            summarize_tool_input(&ToolCall {
                id: "read".to_owned(),
                name: "Read".to_owned(),
                arguments: json!({ "file_path": "src/app.rs" }),
            }),
            "src/app.rs"
        );
        assert_eq!(
            summarize_tool_input(&ToolCall {
                id: "glob".to_owned(),
                name: "Glob".to_owned(),
                arguments: json!({ "path": ".", "pattern": "**/*.rs" }),
            }),
            "**/*.rs"
        );
        assert_eq!(
            summarize_tool_input(&ToolCall {
                id: "bash".to_owned(),
                name: "Bash".to_owned(),
                arguments: json!({ "command": "cargo test" }),
            }),
            "cargo test"
        );
        assert_eq!(
            summarize_tool_input(&ToolCall {
                id: "edit".to_owned(),
                name: "Edit".to_owned(),
                arguments: json!({ "file_path": "src/main.rs" }),
            }),
            "src/main.rs"
        );
    }

    #[test]
    fn glob_summary_shows_path_and_pattern_when_path_is_not_cwd() {
        assert_eq!(
            summarize_tool_input(&ToolCall {
                id: "glob".to_owned(),
                name: "Glob".to_owned(),
                arguments: json!({ "path": "src", "pattern": "*.rs" }),
            }),
            "src ｜ *.rs"
        );
    }
}
