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
use serde_json::json;

use crate::{
    agent::{
        AgentEvent,
        openai::OpenAiProvider,
        provider::{
            FinishReason, ModelMessage, ModelProvider, ModelRequest, ModelResponse, ToolCall,
            ToolResult,
        },
    },
    approval::{
        AgentControl, ApprovalDecision, ApprovalKind, ApprovalRequest, ConversationPermissions,
    },
    config::LlmConfig,
    context::{RuntimeContext, build_initial_messages},
    plugins::{HookEvent, HookRunner},
    progress::TodoUpdate,
    services::lsp::LspManager,
    services::tool_results::ToolResultBudget,
    settings::ProjectSettings,
    tasks::{SubagentSteering, TaskRequest},
    tools::{DynamicTool, ReadFileState, ToolRegistry},
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
    pub active_progress: Option<TodoUpdate>,
    pub current_user_message: String,
    pub tool_results_dir: PathBuf,
    pub task_requests: Sender<TaskRequest>,
    pub read_file_state: ReadFileState,
    pub lsp_manager: LspManager,
    pub dynamic_tools: Vec<Arc<dyn DynamicTool>>,
    pub hook_runner: HookRunner,
}

#[derive(Debug, PartialEq, Eq)]
pub struct AgentRunOutcome {
    pub final_message: String,
}

struct ToolExecutionState<'a> {
    project_settings: &'a mut ProjectSettings,
    conversation_permissions: &'a mut ConversationPermissions,
    tool_result_budget: &'a ToolResultBudget,
    approvals_enabled: bool,
    hook_runner: &'a HookRunner,
}

#[derive(Clone)]
struct RunModelTurnsConfig {
    initial_permissions: ConversationPermissions,
    approvals_enabled: bool,
}

pub fn spawn_agent_loop(
    input: AgentRunInput,
    tx: Sender<AgentEvent>,
    control_rx: Receiver<AgentControl>,
) {
    thread::spawn(move || {
        let mut provider = OpenAiProvider::new(input.llm.clone());
        let registry = main_tool_registry(&input);

        match run_agent_loop(
            input,
            &mut provider,
            &registry,
            &tx,
            &control_rx,
            None,
            true,
        ) {
            Ok(_) => {
                tx.send(AgentEvent::AssistantFinished).ok();
            }
            Err(error) => {
                tx.send(AgentEvent::Failed(format!("LLM error: {error:#}")))
                    .ok();
            }
        }
    });
}

pub fn spawn_subagent_loop(
    input: AgentRunInput,
    tx: Sender<AgentEvent>,
    result_tx: Sender<crate::tasks::SubagentOutcome>,
    control_rx: Receiver<AgentControl>,
    steering: Arc<SubagentSteering>,
) {
    thread::spawn(move || {
        let mut provider = OpenAiProvider::new(input.llm.clone());
        let registry = subagent_tool_registry(&input);
        let cwd = PathBuf::from(input.runtime_context.current_dir.clone());
        let result = crate::tools::with_tool_cwd(cwd, || {
            run_agent_loop(
                input,
                &mut provider,
                &registry,
                &tx,
                &control_rx,
                Some(steering.as_ref()),
                false,
            )
        });
        steering.close();

        match result {
            Ok(outcome) => {
                tx.send(AgentEvent::AssistantFinished).ok();
                result_tx
                    .send(crate::tasks::SubagentOutcome::completed(
                        outcome.final_message,
                    ))
                    .ok();
            }
            Err(error) => {
                if error.to_string() == "cancelled" {
                    tx.send(AgentEvent::AssistantFinished).ok();
                    result_tx
                        .send(crate::tasks::SubagentOutcome::cancelled(""))
                        .ok();
                    return;
                }
                let message = format!("LLM error: {error:#}");
                tx.send(AgentEvent::Failed(message.clone())).ok();
                result_tx
                    .send(crate::tasks::SubagentOutcome::failed(message, ""))
                    .ok();
            }
        }
    });
}

fn main_tool_registry(input: &AgentRunInput) -> ToolRegistry {
    ToolRegistry::with_task_requests(Some(input.task_requests.clone()))
        .with_lsp_manager(input.lsp_manager.clone())
        .with_read_file_state(input.read_file_state.clone())
        .with_dynamic_tools(input.dynamic_tools.clone())
}

fn subagent_tool_registry(input: &AgentRunInput) -> ToolRegistry {
    ToolRegistry::for_subagent(Some(input.task_requests.clone()))
        .with_lsp_manager(input.lsp_manager.clone())
        .with_read_file_state(input.read_file_state.clone())
}

fn run_agent_loop(
    input: AgentRunInput,
    provider: &mut impl ModelProvider,
    registry: &ToolRegistry,
    tx: &Sender<AgentEvent>,
    control_rx: &Receiver<AgentControl>,
    steering: Option<&SubagentSteering>,
    approvals_enabled: bool,
) -> Result<AgentRunOutcome> {
    input.hook_runner.run(
        HookEvent::AgentStart,
        json!({"cwd": input.runtime_context.current_dir}),
    )?;
    tx.send(AgentEvent::Started).ok();

    let mut messages = build_initial_messages(
        &input.system_prompt,
        &input.runtime_context,
        &input.conversation,
        input.active_progress.as_ref(),
        &input.current_user_message,
    );
    let tool_result_budget = ToolResultBudget::new(input.tool_results_dir);

    let outcome = run_model_turns(
        &mut messages,
        provider,
        registry,
        tx,
        control_rx,
        steering,
        &tool_result_budget,
        RunModelTurnsConfig {
            initial_permissions: input.conversation_permissions,
            approvals_enabled,
        },
        &input.hook_runner,
    );
    input
        .hook_runner
        .run(HookEvent::AgentEnd, json!({"success": outcome.is_ok()}))?;
    outcome
}

#[allow(clippy::too_many_arguments)]
fn run_model_turns(
    messages: &mut Vec<ModelMessage>,
    provider: &mut impl ModelProvider,
    registry: &ToolRegistry,
    tx: &Sender<AgentEvent>,
    control_rx: &Receiver<AgentControl>,
    steering: Option<&SubagentSteering>,
    tool_result_budget: &ToolResultBudget,
    config: RunModelTurnsConfig,
    hook_runner: &HookRunner,
) -> Result<AgentRunOutcome> {
    let mut tool_iterations = 0;
    let mut project_settings = ProjectSettings::load();
    let mut conversation_permissions = config.initial_permissions;

    loop {
        append_steering_messages(messages, drain_steering_messages(steering));
        if drain_control_messages(control_rx, tx, &mut conversation_permissions) {
            bail!("cancelled");
        }
        let mut on_delta = |delta: String| {
            tx.send(AgentEvent::AssistantDelta(delta)).ok();
        };
        hook_runner.run(
            HookEvent::BeforeModelCall,
            json!({"message_count": messages.len(), "tool_count": registry.specs().len()}),
        )?;
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
        hook_runner.run(
            HookEvent::AfterModelCall,
            json!({"tool_call_count": response.tool_calls.len(), "has_text": response.assistant_text.is_some()}),
        )?;

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
            let steering_messages = drain_steering_messages(steering);
            tool_iterations += 1;
            if tool_iterations > MAX_TOOL_ITERATIONS {
                bail!("maximum tool iterations exceeded");
            }

            let mut tool_state = ToolExecutionState {
                project_settings: &mut project_settings,
                conversation_permissions: &mut conversation_permissions,
                tool_result_budget,
                approvals_enabled: config.approvals_enabled,
                hook_runner,
            };
            append_tool_turn(
                messages,
                response,
                registry,
                tx,
                control_rx,
                &mut tool_state,
            )?;
            append_steering_messages(messages, steering_messages);
            continue;
        }

        if let Some(steering_messages) = finish_or_drain_steering(steering) {
            if response.finish_reason != FinishReason::Stop {
                return finish_without_tools(response);
            }
            messages.push(ModelMessage::assistant(
                response.assistant_text.filter(|text| !text.is_empty()),
                Vec::new(),
            ));
            append_steering_messages(messages, steering_messages);
            continue;
        }

        return finish_without_tools(response);
    }
}

fn drain_steering_messages(steering: Option<&SubagentSteering>) -> Vec<String> {
    steering.map(SubagentSteering::drain).unwrap_or_default()
}

fn finish_or_drain_steering(steering: Option<&SubagentSteering>) -> Option<Vec<String>> {
    steering.and_then(SubagentSteering::finish_or_drain)
}

fn append_steering_messages(messages: &mut Vec<ModelMessage>, steering: Vec<String>) {
    messages.extend(steering.into_iter().map(|message| {
        ModelMessage::user(format!(
            "<subagent-steering>\n{}\n</subagent-steering>",
            message.trim()
        ))
    }));
}

fn append_tool_turn(
    messages: &mut Vec<ModelMessage>,
    mut response: ModelResponse,
    registry: &ToolRegistry,
    tx: &Sender<AgentEvent>,
    control_rx: &Receiver<AgentControl>,
    state: &mut ToolExecutionState<'_>,
) -> Result<()> {
    for call in &mut response.tool_calls {
        apply_before_tool_hook(state.hook_runner, call)?;
    }
    let assistant_text = response.assistant_text.filter(|text| !text.is_empty());
    messages.push(ModelMessage::assistant(
        assistant_text,
        response.tool_calls.clone(),
    ));

    for batch in partition_tool_calls(registry, response.tool_calls, state) {
        if batch.concurrent {
            append_concurrent_tool_batch(messages, registry, tx, control_rx, state, batch.calls)?;
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
        state.approvals_enabled,
    )?;
    let tool_name = call.name.clone();
    let result = apply_after_tool_hook(state.hook_runner, &call, result);
    let result = state.tool_result_budget.apply(&tool_name, result);
    emit_todo_update_if_needed(tx, &call, &result);

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

fn emit_todo_update_if_needed(tx: &Sender<AgentEvent>, call: &ToolCall, result: &ToolResult) {
    if call.name != "TodoWrite" || result.is_error {
        return;
    }
    if let Ok(update) = TodoUpdate::from_tool_arguments(&call.arguments) {
        tx.send(AgentEvent::TodoUpdated(update)).ok();
    }
}

fn append_concurrent_tool_batch(
    messages: &mut Vec<ModelMessage>,
    registry: &ToolRegistry,
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
        let hook_runner = state.hook_runner.clone();
        let registry = registry.clone();
        thread::spawn(move || {
            let tool_name = call.name.clone();
            let mut is_cancelled = || cancelled.load(Ordering::Relaxed);
            let result = registry.execute_approved_with_cancel(&call, &mut is_cancelled);
            let result = apply_after_tool_hook(&hook_runner, &call, result);
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
        && (state
            .conversation_permissions
            .allowed_tools
            .contains(&call.name)
            || !registry.requires_approval(
                call,
                bash_prefix_allowed,
                state.conversation_permissions.edit_always_allowed,
            ))
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
    approvals_enabled: bool,
) -> Result<ToolResult> {
    if drain_control_messages(control_rx, tx, conversation_permissions) {
        bail!("cancelled");
    }
    let bash_prefix_allowed = bash_prefix_allowed(project_settings, &call);
    let requires_approval = !conversation_permissions.allowed_tools.contains(&call.name)
        && registry.requires_approval(
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
    if !approvals_enabled {
        return Ok(ToolResult {
            call_id: call.id,
            content: "Approval is unavailable inside a subagent for this tool call.".to_owned(),
            is_error: true,
        });
    }

    let request = ApprovalRequest {
        id: next_approval_id(),
        tool_name: call.name.clone(),
        command: summarize_tool_input(&call),
        explanation: approval_explanation(&call),
        kind: ApprovalKind::Tool,
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
                    allowed_tool: None,
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
                    allowed_tool: None,
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
                allowed_tool: None,
            })
            .ok();
            execute_approved_cancellable(registry, call, tx, control_rx, conversation_permissions)
        }
        ApprovalDecision::AllowConversationTool => {
            conversation_permissions
                .allowed_tools
                .insert(call.name.clone());
            tx.send(AgentEvent::ConversationPermissionChanged {
                edit_always_allowed: conversation_permissions.edit_always_allowed,
                allowed_tool: Some(call.name.clone()),
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
        "Edit" => "This Edit will modify a file and always needs approval unless allowed for this conversation.".to_owned(),
        _ => format!("{} needs approval before it can run.", call.name),
    }
}

fn apply_before_tool_hook(hook_runner: &HookRunner, call: &mut ToolCall) -> Result<()> {
    let outcome = hook_runner.run(
        HookEvent::BeforeToolCall,
        json!({
            "id": call.id,
            "name": call.name,
            "arguments": call.arguments,
        }),
    )?;
    if let Some(arguments) = outcome
        .replacement
        .and_then(|replacement| replacement.get("arguments").cloned())
    {
        if !arguments.is_object() {
            bail!("before_tool_call hook replacement.arguments must be an object");
        }
        call.arguments = arguments;
    }
    Ok(())
}

fn apply_after_tool_hook(
    hook_runner: &HookRunner,
    call: &ToolCall,
    mut result: ToolResult,
) -> ToolResult {
    let outcome = hook_runner.run(
        HookEvent::AfterToolCall,
        json!({
            "id": call.id,
            "name": call.name,
            "arguments": call.arguments,
            "content": result.content,
            "is_error": result.is_error,
        }),
    );
    match outcome {
        Ok(outcome) => {
            if let Some(replacement) = outcome.replacement {
                if let Some(content) = replacement.get("content").and_then(|value| value.as_str()) {
                    result.content = content.to_owned();
                }
                if let Some(is_error) = replacement
                    .get("is_error")
                    .and_then(|value| value.as_bool())
                {
                    result.is_error = is_error;
                }
            }
        }
        Err(error) => {
            result.content = format!("Tool result blocked by plugin hook: {error:#}");
            result.is_error = true;
        }
    }
    result
}

fn next_approval_id() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT_ID: AtomicU64 = AtomicU64::new(1);
    NEXT_ID.fetch_add(1, Ordering::Relaxed)
}

fn finish_without_tools(response: ModelResponse) -> Result<AgentRunOutcome> {
    let final_message = response.assistant_text.clone().unwrap_or_default();
    match response.finish_reason {
        FinishReason::Stop => Ok(AgentRunOutcome { final_message }),
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
    use std::{
        collections::VecDeque,
        sync::{Arc, mpsc},
    };

    use anyhow::Result;
    use serde_json::json;

    use super::*;
    use crate::{
        agent::provider::{ModelRole, ToolResult, ToolSpec},
        config::{LlmProviderConfig, LspConfig},
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

    struct SteeringProvider {
        steering: Arc<SubagentSteering>,
        requests: Vec<ModelRequest>,
    }

    impl ModelProvider for SteeringProvider {
        fn complete(&mut self, request: ModelRequest) -> Result<ModelResponse> {
            self.requests.push(request);
            if self.requests.len() == 1 {
                self.steering
                    .send("focus on the parser tests".to_owned())
                    .unwrap();
                Ok(final_response("initial answer"))
            } else {
                Ok(final_response("revised answer"))
            }
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
        let (task_requests, _task_rx) = mpsc::channel();
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
                    prompt_cache: Default::default(),
                }],
                temperature: 0.0,
                max_tokens: 100,
                context_window: Some(1000),
                api_key: "test-key".to_owned(),
                default_context_window: Some(1000),
                prompt_cache: Default::default(),
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
            active_progress: None,
            current_user_message: "hello".to_owned(),
            tool_results_dir: std::env::temp_dir(),
            task_requests,
            read_file_state: ReadFileState::new(),
            lsp_manager: LspManager::new(LspConfig::default(), PathBuf::from("/workspace")),
            dynamic_tools: Vec::new(),
            hook_runner: HookRunner::default(),
        }
    }

    #[test]
    fn query_registry_does_not_publish_terminal_run() {
        let names = ToolRegistry::with_task_requests(None)
            .specs()
            .into_iter()
            .map(|spec| spec.name)
            .collect::<Vec<_>>();

        assert!(!names.contains(&"TerminalRun".to_owned()));
    }

    struct FakeMcpTool;

    impl DynamicTool for FakeMcpTool {
        fn spec(&self) -> ToolSpec {
            ToolSpec {
                name: "mcp__test__echo".to_owned(),
                description: "Echoes a test MCP payload.".to_owned(),
                parameters: json!({"type":"object"}),
            }
        }

        fn execute(&self, call: &ToolCall, _is_cancelled: &mut dyn FnMut() -> bool) -> ToolResult {
            ToolResult {
                call_id: call.id.clone(),
                content: "dynamic tool result".to_owned(),
                is_error: false,
            }
        }
    }

    #[test]
    fn subagent_registry_excludes_dynamic_tools_while_main_keeps_them() {
        let mut run_input = input();
        run_input.dynamic_tools = vec![Arc::new(FakeMcpTool)];
        let call = ToolCall {
            id: "dynamic-tool".to_owned(),
            name: "mcp__test__echo".to_owned(),
            arguments: json!({}),
        };

        let main = main_tool_registry(&run_input);
        let subagent = subagent_tool_registry(&run_input);

        assert!(main.specs().iter().any(|spec| spec.name == call.name));
        assert!(!subagent.specs().iter().any(|spec| spec.name == call.name));
        assert_eq!(main.execute(&call).content, "dynamic tool result");
        let subagent_result = subagent.execute(&call);
        assert!(subagent_result.is_error);
        assert_eq!(
            subagent_result.content,
            "Tool 'mcp__test__echo' is not registered."
        );
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

    fn todo_response() -> ModelResponse {
        ModelResponse {
            assistant_text: None,
            tool_calls: vec![ToolCall {
                id: "todo-one".to_owned(),
                name: "TodoWrite".to_owned(),
                arguments: json!({
                    "todos": [
                        {
                            "content": "Run tests",
                            "active_form": "Running tests",
                            "status": "in_progress"
                        }
                    ]
                }),
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
            approvals_enabled: true,
            hook_runner: empty_hook_runner(),
        }
    }

    fn empty_hook_runner() -> &'static HookRunner {
        static RUNNER: std::sync::OnceLock<HookRunner> = std::sync::OnceLock::new();
        RUNNER.get_or_init(HookRunner::default)
    }

    #[test]
    fn final_assistant_response_ends_the_loop() {
        let (tx, rx) = mpsc::channel();
        let registry = ToolRegistry::new();
        let mut provider = FakeProvider::new(vec![final_response("done")]);

        let control_rx = control_rx();
        let outcome = run_agent_loop(
            input(),
            &mut provider,
            &registry,
            &tx,
            &control_rx,
            None,
            true,
        )
        .unwrap();

        let events: Vec<_> = rx.try_iter().collect();
        assert_eq!(provider.requests.len(), 1);
        assert_eq!(outcome.final_message, "done");
        assert!(matches!(events.first(), Some(AgentEvent::Started)));
        assert!(
            events
                .iter()
                .any(|event| matches!(event, AgentEvent::AssistantDelta(text) if text == "done"))
        );
    }

    #[test]
    fn steering_message_continues_a_finishing_subagent_turn() {
        let (tx, _rx) = mpsc::channel();
        let registry = ToolRegistry::new();
        let steering = Arc::new(SubagentSteering::default());
        let mut provider = SteeringProvider {
            steering: Arc::clone(&steering),
            requests: Vec::new(),
        };
        let control_rx = control_rx();

        let outcome = run_agent_loop(
            input(),
            &mut provider,
            &registry,
            &tx,
            &control_rx,
            Some(steering.as_ref()),
            false,
        )
        .unwrap();

        assert_eq!(outcome.final_message, "revised answer");
        assert_eq!(provider.requests.len(), 2);
        let second_request = &provider.requests[1];
        assert!(second_request.messages.iter().any(|message| {
            message.role == ModelRole::Assistant
                && message.content.as_deref() == Some("initial answer")
        }));
        assert!(second_request.messages.iter().any(|message| {
            message.role == ModelRole::User
                && message
                    .content
                    .as_deref()
                    .is_some_and(|content| content.contains("focus on the parser tests"))
        }));
    }

    #[test]
    fn tool_call_causes_second_model_request_with_tool_result() {
        let (tx, _rx) = mpsc::channel();
        let registry = ToolRegistry::new();
        let mut provider = FakeProvider::new(vec![tool_response("one"), final_response("done")]);

        let control_rx = control_rx();
        run_agent_loop(
            input(),
            &mut provider,
            &registry,
            &tx,
            &control_rx,
            None,
            true,
        )
        .unwrap();

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
    fn todo_write_emits_progress_update_event() {
        let (tx, rx) = mpsc::channel();
        let registry = ToolRegistry::new();
        let mut provider = FakeProvider::new(vec![todo_response(), final_response("done")]);

        let control_rx = control_rx();
        run_agent_loop(
            input(),
            &mut provider,
            &registry,
            &tx,
            &control_rx,
            None,
            true,
        )
        .unwrap();

        let events = rx.try_iter().collect::<Vec<_>>();
        assert!(events.iter().any(|event| {
            matches!(
                event,
                AgentEvent::TodoUpdated(update)
                    if update.active_label() == Some("Running tests")
            )
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
        let error = run_agent_loop(
            input(),
            &mut provider,
            &registry,
            &tx,
            &control_rx,
            None,
            true,
        )
        .unwrap_err();

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
    fn summarizes_shell_description_separately_from_command() {
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
