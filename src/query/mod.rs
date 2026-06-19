use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, Sender},
    },
    thread,
    time::Duration,
};

use anyhow::{Context, Result, bail};

use crate::{
    agent::{
        AgentEvent,
        openai::OpenAiProvider,
        provider::{
            FinishReason, ModelMessage, ModelProvider, ModelRequest, ModelResponse, ToolCall,
            ToolResult,
        },
    },
    approval::{AgentControl, ApprovalDecision, ApprovalRequest, ConversationPermissions},
    config::LlmConfig,
    context::{RuntimeContext, build_initial_messages},
    services::tool_results::ToolResultBudget,
    settings::ProjectSettings,
    terminal::TerminalRequest,
    tools::{ReadFileState, ShellToolMode, ToolRegistry},
};

const MAX_TOOL_ITERATIONS: usize = 8;
const TOOL_BATCH_POLL_INTERVAL: Duration = Duration::from_millis(50);

#[derive(Clone)]
pub struct AgentRunInput {
    pub llm: LlmConfig,
    pub system_prompt: String,
    pub runtime_context: RuntimeContext,
    pub conversation_permissions: ConversationPermissions,
    pub conversation: Vec<ModelMessage>,
    pub current_user_message: String,
    pub tool_results_dir: PathBuf,
    pub terminal_requests: Sender<TerminalRequest>,
    pub shell_tool_mode: ShellToolMode,
    pub read_file_state: ReadFileState,
}

struct ToolExecutionState<'a> {
    project_settings: &'a mut ProjectSettings,
    conversation_permissions: &'a mut ConversationPermissions,
    tool_result_budget: &'a ToolResultBudget,
}

pub fn spawn_agent_loop(
    input: AgentRunInput,
    tx: Sender<AgentEvent>,
    control_rx: Receiver<AgentControl>,
) {
    thread::spawn(move || {
        let mut provider = OpenAiProvider::new(input.llm.clone());
        let registry = ToolRegistry::with_shell_tool(
            input.shell_tool_mode,
            Some(input.terminal_requests.clone()),
        )
        .with_read_file_state(input.read_file_state.clone());

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
    let tool_result_budget = ToolResultBudget::new(input.tool_results_dir);

    run_model_turns(
        &mut messages,
        provider,
        registry,
        tx,
        control_rx,
        input.conversation_permissions,
        &tool_result_budget,
    )
}

fn run_model_turns(
    messages: &mut Vec<ModelMessage>,
    provider: &mut impl ModelProvider,
    registry: &ToolRegistry,
    tx: &Sender<AgentEvent>,
    control_rx: &Receiver<AgentControl>,
    initial_permissions: ConversationPermissions,
    tool_result_budget: &ToolResultBudget,
) -> Result<()> {
    let mut tool_iterations = 0;
    let mut project_settings = ProjectSettings::load();
    let mut conversation_permissions = initial_permissions;

    loop {
        if drain_control_messages(control_rx, tx, &mut conversation_permissions) {
            bail!("cancelled");
        }
        let mut on_delta = |delta: String| {
            tx.send(AgentEvent::AssistantDelta(delta)).ok();
        };
        let response = provider
            .stream(
                ModelRequest {
                    messages: messages.clone(),
                    tools: registry.specs(),
                    max_tokens: None,
                },
                &mut on_delta,
            )
            .context("model request failed")?;

        if drain_control_messages(control_rx, tx, &mut conversation_permissions) {
            bail!("cancelled");
        }

        let normalized_tool_calls = response
            .tool_calls
            .iter()
            .map(|call| registry.normalize_for_context(call))
            .collect();
        let response = ModelResponse {
            tool_calls: normalized_tool_calls,
            ..response
        };

        tx.send(AgentEvent::AssistantTurn {
            usage: response.usage,
            finish_reason: response.finish_reason.clone(),
            tool_calls: response.tool_calls.clone(),
        })
        .ok();

        if !response.tool_calls.is_empty() {
            tool_iterations += 1;
            if tool_iterations > MAX_TOOL_ITERATIONS {
                bail!("maximum tool iterations exceeded");
            }

            let mut tool_state = ToolExecutionState {
                project_settings: &mut project_settings,
                conversation_permissions: &mut conversation_permissions,
                tool_result_budget,
            };
            append_tool_turn(
                messages,
                response,
                registry,
                tx,
                control_rx,
                &mut tool_state,
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
    state: &mut ToolExecutionState<'_>,
) -> Result<()> {
    let assistant_text = response.assistant_text.filter(|text| !text.is_empty());
    messages.push(ModelMessage::assistant(
        assistant_text,
        response.tool_calls.clone(),
    ));

    for batch in partition_tool_calls(registry, response.tool_calls, state) {
        if batch.concurrent {
            append_concurrent_tool_batch(messages, tx, control_rx, state, batch.calls)?;
        } else {
            for call in batch.calls {
                append_serial_tool_call(messages, registry, tx, control_rx, state, call)?;
            }
        }
    }
    Ok(())
}

fn append_serial_tool_call(
    messages: &mut Vec<ModelMessage>,
    registry: &ToolRegistry,
    tx: &Sender<AgentEvent>,
    control_rx: &Receiver<AgentControl>,
    state: &mut ToolExecutionState<'_>,
    call: ToolCall,
) -> Result<()> {
    if drain_control_messages(control_rx, tx, state.conversation_permissions) {
        bail!("cancelled");
    }
    tx.send(AgentEvent::ToolStarted {
        id: call.id.clone(),
        name: call.name.clone(),
        input_summary: summarize_tool_input(&call),
        input_description: summarize_tool_description(&call),
    })
    .ok();

    let result = execute_tool_with_approval(
        registry,
        call.clone(),
        tx,
        control_rx,
        state.project_settings,
        state.conversation_permissions,
    )?;
    let tool_name = call.name.clone();
    let result = state.tool_result_budget.apply(&tool_name, result);

    tx.send(AgentEvent::ToolFinished {
        id: call.id,
        name: tool_name,
        output: result.content.clone(),
        is_error: result.is_error,
        output_summary: summarize_tool_output(&result.content),
    })
    .ok();

    messages.push(ModelMessage::tool_result(&result));
    Ok(())
}

fn append_concurrent_tool_batch(
    messages: &mut Vec<ModelMessage>,
    tx: &Sender<AgentEvent>,
    control_rx: &Receiver<AgentControl>,
    state: &mut ToolExecutionState<'_>,
    calls: Vec<ToolCall>,
) -> Result<()> {
    let call_count = calls.len();
    let cancelled = Arc::new(AtomicBool::new(false));
    let (result_tx, result_rx) = mpsc::channel();

    for (index, call) in calls.into_iter().enumerate() {
        tx.send(AgentEvent::ToolStarted {
            id: call.id.clone(),
            name: call.name.clone(),
            input_summary: summarize_tool_input(&call),
            input_description: summarize_tool_description(&call),
        })
        .ok();

        let result_tx = result_tx.clone();
        let cancelled = Arc::clone(&cancelled);
        let tool_result_budget = state.tool_result_budget.clone();
        thread::spawn(move || {
            let registry = ToolRegistry::new();
            let tool_name = call.name.clone();
            let mut is_cancelled = || cancelled.load(Ordering::Relaxed);
            let result = registry.execute_approved_with_cancel(&call, &mut is_cancelled);
            let result = tool_result_budget.apply(&tool_name, result);
            result_tx
                .send(CompletedTool {
                    index,
                    call,
                    result,
                })
                .ok();
        });
    }
    drop(result_tx);

    let mut completed = (0..call_count).map(|_| None).collect::<Vec<_>>();
    let mut remaining = call_count;
    let mut saw_cancel = false;

    while remaining > 0 {
        if drain_control_messages(control_rx, tx, state.conversation_permissions) {
            cancelled.store(true, Ordering::Relaxed);
            saw_cancel = true;
        }

        match result_rx.recv_timeout(TOOL_BATCH_POLL_INTERVAL) {
            Ok(completed_tool) => {
                remaining -= 1;
                tx.send(AgentEvent::ToolFinished {
                    id: completed_tool.call.id.clone(),
                    name: completed_tool.call.name.clone(),
                    output: completed_tool.result.content.clone(),
                    is_error: completed_tool.result.is_error,
                    output_summary: summarize_tool_output(&completed_tool.result.content),
                })
                .ok();
                let index = completed_tool.index;
                completed[index] = Some(completed_tool);
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                bail!("tool worker channel closed before all tools completed");
            }
        }
    }

    if saw_cancel || drain_control_messages(control_rx, tx, state.conversation_permissions) {
        cancelled.store(true, Ordering::Relaxed);
        bail!("cancelled");
    }

    for completed_tool in completed.into_iter().flatten() {
        messages.push(ModelMessage::tool_result(&completed_tool.result));
    }
    Ok(())
}

struct CompletedTool {
    index: usize,
    call: ToolCall,
    result: ToolResult,
}

struct ToolBatch {
    concurrent: bool,
    calls: Vec<ToolCall>,
}

fn partition_tool_calls(
    registry: &ToolRegistry,
    calls: Vec<ToolCall>,
    state: &ToolExecutionState<'_>,
) -> Vec<ToolBatch> {
    let mut batches: Vec<ToolBatch> = Vec::new();

    for call in calls {
        let concurrent = can_run_concurrently(registry, &call, state);
        if concurrent && batches.last().is_some_and(|batch| batch.concurrent) {
            batches
                .last_mut()
                .expect("last batch exists")
                .calls
                .push(call);
        } else {
            batches.push(ToolBatch {
                concurrent,
                calls: vec![call],
            });
        }
    }

    batches
}

fn can_run_concurrently(
    registry: &ToolRegistry,
    call: &ToolCall,
    state: &ToolExecutionState<'_>,
) -> bool {
    let bash_prefix_allowed = bash_prefix_allowed(state.project_settings, call);
    registry.is_concurrency_safe(call)
        && !registry.requires_approval(
            call,
            bash_prefix_allowed,
            state.conversation_permissions.edit_always_allowed,
        )
}

fn bash_prefix_allowed(project_settings: &ProjectSettings, call: &ToolCall) -> bool {
    call.arguments
        .get("command")
        .and_then(|value| value.as_str())
        .is_some_and(|command| project_settings.allows_bash(command))
}

fn execute_tool_with_approval(
    registry: &ToolRegistry,
    call: ToolCall,
    tx: &Sender<AgentEvent>,
    control_rx: &Receiver<AgentControl>,
    project_settings: &mut ProjectSettings,
    conversation_permissions: &mut ConversationPermissions,
) -> Result<ToolResult> {
    if drain_control_messages(control_rx, tx, conversation_permissions) {
        bail!("cancelled");
    }
    let bash_prefix_allowed = bash_prefix_allowed(project_settings, &call);
    let requires_approval = registry.requires_approval(
        &call,
        bash_prefix_allowed,
        conversation_permissions.edit_always_allowed,
    );
    if !requires_approval {
        return execute_approved_cancellable(
            registry,
            call,
            tx,
            control_rx,
            conversation_permissions,
        );
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
                    control_rx,
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
            AgentControl::Cancel => bail!("cancelled"),
            AgentControl::ApprovalDecision { .. } => {}
        }
    }
}

fn drain_control_messages(
    control_rx: &Receiver<AgentControl>,
    tx: &Sender<AgentEvent>,
    conversation_permissions: &mut ConversationPermissions,
) -> bool {
    let mut cancelled = false;
    while let Ok(message) = control_rx.try_recv() {
        match message {
            AgentControl::ClearConversationEditPermission => {
                conversation_permissions.edit_always_allowed = false;
                tx.send(AgentEvent::ConversationPermissionChanged {
                    edit_always_allowed: false,
                })
                .ok();
            }
            AgentControl::Cancel => {
                cancelled = true;
            }
            AgentControl::ApprovalDecision { .. } => {}
        }
    }
    cancelled
}

fn handle_approval_decision(
    registry: &ToolRegistry,
    call: ToolCall,
    decision: ApprovalDecision,
    tx: &Sender<AgentEvent>,
    control_rx: &Receiver<AgentControl>,
    project_settings: &mut ProjectSettings,
    conversation_permissions: &mut ConversationPermissions,
) -> Result<ToolResult> {
    match decision {
        ApprovalDecision::AllowOnce => {
            execute_approved_cancellable(registry, call, tx, control_rx, conversation_permissions)
        }
        ApprovalDecision::AllowProjectPrefix => {
            if let Some(command) = call
                .arguments
                .get("command")
                .and_then(|value| value.as_str())
            {
                project_settings.allow_bash_prefix(command)?;
            }
            execute_approved_cancellable(registry, call, tx, control_rx, conversation_permissions)
        }
        ApprovalDecision::AllowConversation => {
            conversation_permissions.edit_always_allowed = true;
            tx.send(AgentEvent::ConversationPermissionChanged {
                edit_always_allowed: true,
            })
            .ok();
            execute_approved_cancellable(registry, call, tx, control_rx, conversation_permissions)
        }
        ApprovalDecision::Deny { feedback } => Ok(ToolResult {
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

fn execute_approved_cancellable(
    registry: &ToolRegistry,
    call: ToolCall,
    tx: &Sender<AgentEvent>,
    control_rx: &Receiver<AgentControl>,
    conversation_permissions: &mut ConversationPermissions,
) -> Result<ToolResult> {
    let mut cancelled = false;
    let result = {
        let mut is_cancelled = || {
            if !cancelled {
                cancelled = drain_control_messages(control_rx, tx, conversation_permissions);
            }
            cancelled
        };
        registry.execute_approved_with_cancel(&call, &mut is_cancelled)
    };

    if cancelled || drain_control_messages(control_rx, tx, conversation_permissions) {
        bail!("cancelled");
    }

    Ok(result)
}

fn approval_explanation(call: &ToolCall) -> String {
    match call.name.as_str() {
        "Bash" => "This Bash command can modify project state and needs approval.".to_owned(),
        "TerminalRun" => {
            "This terminal command can modify project state and needs approval.".to_owned()
        }
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
    ToolRegistry::new().input_summary(call)
}

fn summarize_tool_description(call: &ToolCall) -> Option<String> {
    ToolRegistry::new().input_description(call)
}

fn summarize_tool_output(output: &str) -> String {
    ToolRegistry::new().output_summary(output)
}

#[cfg(test)]
mod tests {
    use std::{collections::VecDeque, sync::mpsc};

    use anyhow::Result;
    use serde_json::json;

    use super::*;
    use crate::{
        agent::provider::{ModelRole, ToolResult},
        config::LlmProviderConfig,
        settings::{ProjectPermissions, ProjectSettings},
    };

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
        let (terminal_requests, _terminal_rx) = mpsc::channel();
        AgentRunInput {
            llm: LlmConfig {
                provider: "test".to_owned(),
                base_url: "http://localhost".to_owned(),
                model: "test-model".to_owned(),
                providers: vec![LlmProviderConfig {
                    name: "test".to_owned(),
                    base_url: "http://localhost".to_owned(),
                    models: vec!["test-model".to_owned()],
                    model_context_windows: Default::default(),
                    api_key_env: "TEST_API_KEY".to_owned(),
                }],
                temperature: 0.0,
                max_tokens: 100,
                context_window: Some(1000),
                api_key: "test-key".to_owned(),
                default_context_window: Some(1000),
            },
            system_prompt: "system".to_owned(),
            runtime_context: RuntimeContext {
                current_time: "unix_seconds=1".to_owned(),
                current_dir: "/workspace".to_owned(),
                shell: "/bin/zsh".to_owned(),
                app_name: "glint".to_owned(),
                app_version: "0.1.0".to_owned(),
                tool_mode: "available tools: Read, Glob, Grep, LSP, Bash, Edit".to_owned(),
            },
            conversation_permissions: ConversationPermissions::default(),
            conversation: Vec::new(),
            current_user_message: "hello".to_owned(),
            tool_results_dir: std::env::temp_dir(),
            terminal_requests,
            shell_tool_mode: ShellToolMode::Bash,
            read_file_state: ReadFileState::new(),
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

    fn tool_state<'a>(
        project_settings: &'a mut ProjectSettings,
        conversation_permissions: &'a mut ConversationPermissions,
        tool_result_budget: &'a ToolResultBudget,
    ) -> ToolExecutionState<'a> {
        ToolExecutionState {
            project_settings,
            conversation_permissions,
            tool_result_budget,
        }
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
    fn partitions_consecutive_read_only_tools_for_concurrent_execution() {
        let registry = ToolRegistry::new();
        let mut project_settings = ProjectSettings {
            root: std::env::current_dir().unwrap(),
            permissions: ProjectPermissions::default(),
        };
        let mut conversation_permissions = ConversationPermissions::default();
        let budget = ToolResultBudget::new(std::env::temp_dir());
        let state = tool_state(
            &mut project_settings,
            &mut conversation_permissions,
            &budget,
        );
        let batches = partition_tool_calls(
            &registry,
            vec![
                ToolCall {
                    id: "read".to_owned(),
                    name: "Read".to_owned(),
                    arguments: json!({ "file_path": "Cargo.toml" }),
                },
                ToolCall {
                    id: "grep".to_owned(),
                    name: "Grep".to_owned(),
                    arguments: json!({ "pattern": "glint", "path": "Cargo.toml" }),
                },
                ToolCall {
                    id: "bash".to_owned(),
                    name: "Bash".to_owned(),
                    arguments: json!({ "command": "git status --short" }),
                },
                ToolCall {
                    id: "glob".to_owned(),
                    name: "Glob".to_owned(),
                    arguments: json!({ "pattern": "src/*.rs" }),
                },
            ],
            &state,
        );

        assert_eq!(batches.len(), 3);
        assert!(batches[0].concurrent);
        assert_eq!(batches[0].calls.len(), 2);
        assert!(!batches[1].concurrent);
        assert_eq!(batches[1].calls[0].name, "Bash");
        assert!(batches[2].concurrent);
        assert_eq!(batches[2].calls[0].name, "Glob");
    }

    #[test]
    fn protected_read_tool_runs_serially() {
        let registry = ToolRegistry::new();
        let mut project_settings = ProjectSettings {
            root: std::env::current_dir().unwrap(),
            permissions: ProjectPermissions::default(),
        };
        let mut conversation_permissions = ConversationPermissions::default();
        let budget = ToolResultBudget::new(std::env::temp_dir());
        let state = tool_state(
            &mut project_settings,
            &mut conversation_permissions,
            &budget,
        );
        let batches = partition_tool_calls(
            &registry,
            vec![ToolCall {
                id: "read".to_owned(),
                name: "Read".to_owned(),
                arguments: json!({ "file_path": ".glint/settings.local.json" }),
            }],
            &state,
        );

        assert_eq!(batches.len(), 1);
        assert!(!batches[0].concurrent);
    }

    #[test]
    fn concurrent_tool_results_are_added_to_model_context_in_call_order() {
        let registry = ToolRegistry::new();
        let (tx, _rx) = mpsc::channel();
        let control_rx = control_rx();
        let mut project_settings = ProjectSettings {
            root: std::env::current_dir().unwrap(),
            permissions: ProjectPermissions::default(),
        };
        let mut conversation_permissions = ConversationPermissions::default();
        let budget = ToolResultBudget::new(std::env::temp_dir());
        let mut state = tool_state(
            &mut project_settings,
            &mut conversation_permissions,
            &budget,
        );
        let mut messages = Vec::new();

        append_tool_turn(
            &mut messages,
            ModelResponse {
                assistant_text: None,
                tool_calls: vec![
                    ToolCall {
                        id: "read-cargo".to_owned(),
                        name: "Read".to_owned(),
                        arguments: json!({ "file_path": "Cargo.toml" }),
                    },
                    ToolCall {
                        id: "read-main".to_owned(),
                        name: "Read".to_owned(),
                        arguments: json!({ "file_path": "src/main.rs" }),
                    },
                ],
                finish_reason: FinishReason::ToolCalls,
                usage: None,
            },
            &registry,
            &tx,
            &control_rx,
            &mut state,
        )
        .unwrap();

        let tool_ids = messages
            .iter()
            .filter(|message| message.role == ModelRole::Tool)
            .map(|message| message.tool_call_id.as_deref())
            .collect::<Vec<_>>();
        assert_eq!(tool_ids, [Some("read-cargo"), Some("read-main")]);
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
                id: "terminal".to_owned(),
                name: "TerminalRun".to_owned(),
                arguments: json!({
                    "command": "cargo test --lib",
                    "description": "Run library tests"
                }),
            }),
            "cargo test --lib"
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
    fn summarizes_shell_description_separately_from_command() {
        assert_eq!(
            summarize_tool_description(&ToolCall {
                id: "terminal".to_owned(),
                name: "TerminalRun".to_owned(),
                arguments: json!({
                    "command": "cargo test --lib",
                    "description": "Run library tests"
                }),
            }),
            Some("Run library tests".to_owned())
        );
        assert_eq!(
            summarize_tool_description(&ToolCall {
                id: "read".to_owned(),
                name: "Read".to_owned(),
                arguments: json!({ "file_path": "src/main.rs" }),
            }),
            None
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
