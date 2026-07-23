use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{
        Arc,
        mpsc::{self, Receiver, Sender},
    },
};

use anyhow::Result;
use serde_json::json;

use crate::{
    agent::{
        self, AgentEvent, AgentRunInput, CompactRunInput, RuntimeContext, TokenUsage,
        provider::{FinishReason, ToolCall},
    },
    approval::{
        AgentControl, ApprovalDecision, ApprovalKind, ApprovalRequest, ConversationPermissions,
    },
    config::{LlmConfig, LspConfig},
    message::Message,
    plugins::{HookEvent, HookRunner, PluginHook},
    progress::{ProgressState, TodoUpdate},
    services::{
        lsp::LspManager,
        mcp::{McpConfig, McpElicitation, McpElicitationRequest, McpManager, McpServerStatus},
    },
    tasks::{self, SubagentOutcome, SubagentRequest, TaskManager, TaskSnapshot},
    terminal::TerminalRequest,
    tools::{DynamicTool, ReadFileState, ShellToolMode},
    transcript::{
        AssistantTranscript, CompactTrigger, TranscriptSessionSummary, TranscriptStore,
        WorkspaceUsageStats,
    },
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ConversationUsage {
    pub last_usage: Option<TokenUsage>,
    pub total_prompt_tokens: u64,
    pub total_completion_tokens: u64,
    pub total_tokens: u64,
    pub total_cached_prompt_tokens: u64,
}

impl ConversationUsage {
    pub fn record(self, usage: TokenUsage) -> Self {
        Self {
            last_usage: Some(usage),
            total_prompt_tokens: self.total_prompt_tokens + usage.prompt_tokens,
            total_completion_tokens: self.total_completion_tokens + usage.completion_tokens,
            total_tokens: self.total_tokens + usage.total_tokens,
            total_cached_prompt_tokens: self.total_cached_prompt_tokens
                + usage.cached_prompt_tokens.unwrap_or(0),
        }
    }

    pub fn cache_percent(self) -> Option<u8> {
        let usage = self.last_usage?;
        let cached_prompt_tokens = usage.cached_prompt_tokens?;
        if usage.prompt_tokens == 0 {
            return None;
        }

        Some(percent(cached_prompt_tokens, usage.prompt_tokens))
    }
}

#[derive(Clone)]
pub struct StartPromptConfig {
    pub llm: LlmConfig,
    pub system_prompt: String,
    pub runtime_current_dir: String,
    pub shell_tool_mode: ShellToolMode,
}

pub enum RuntimeCommand {
    StartManualCompact {
        llm: LlmConfig,
        pre_prompt_tokens: Option<u64>,
    },
    SubmitPrompt {
        prompt: String,
        config: StartPromptConfig,
        pre_prompt_tokens: Option<u64>,
    },
    StartPrompt {
        prompt: String,
        config: StartPromptConfig,
    },
    ApprovalDecision {
        id: u64,
        decision: ApprovalDecision,
        input: Option<serde_json::Value>,
    },
    ClearConversationEditPermission,
    CancelCurrentTurn {
        compacting: bool,
    },
}

pub enum RuntimeEvent {
    NoMessagesToCompact,
    CompactStarted {
        automatic: bool,
    },
    PromptStarted {
        prompt: String,
        released_progress: Option<TodoUpdate>,
    },
    PermissionChanged,
    Cancelled {
        was_compacting: bool,
    },
    Blocked {
        message: String,
    },
}

pub struct LoadedTranscript {
    pub messages: Vec<Message>,
    pub usage: ConversationUsage,
}

pub struct CompactFinished {
    pub messages: Vec<Message>,
    pub pending_prompt: Option<String>,
    pub automatic: bool,
}

pub struct CompactFailed {
    pub pending_prompt: Option<String>,
    pub automatic: bool,
}

pub struct AssistantRecord {
    pub content: String,
    pub provider: String,
    pub model: String,
    pub tool_calls: Vec<ToolCall>,
    pub usage: Option<TokenUsage>,
    pub finish_reason: FinishReason,
    pub error: Option<String>,
}

pub struct SessionRuntime {
    transcript: TranscriptStore,
    transcript_cwd: String,
    agent_tx: Sender<AgentEvent>,
    agent_events: Receiver<AgentEvent>,
    agent_control_tx: Option<mpsc::Sender<AgentControl>>,
    terminal_request_tx: Sender<TerminalRequest>,
    terminal_requests: Receiver<TerminalRequest>,
    lsp_manager: LspManager,
    mcp_manager: McpManager,
    hook_runner: HookRunner,
    pending_mcp_elicitations: BTreeMap<u64, McpElicitation>,
    runtime_time_label: String,
    conversation_permissions: ConversationPermissions,
    read_file_state: ReadFileState,
    task_manager: TaskManager,
    progress_state: ProgressState,
    pending_prompt_after_compact: Option<String>,
    auto_compact_failures: u8,
}

impl SessionRuntime {
    pub fn create_new(
        cwd: String,
        lsp_config: LspConfig,
        mcp_config: McpConfig,
        hooks: Vec<PluginHook>,
    ) -> Result<Self> {
        TranscriptStore::prune_archive_older_than_in_background(30);
        let transcript = TranscriptStore::create_new(&cwd)?;
        let runtime = Self::from_transcript(transcript, cwd.clone(), lsp_config, mcp_config, hooks);
        runtime
            .hook_runner
            .run(HookEvent::SessionStart, json!({"cwd": cwd}))?;
        Ok(runtime)
    }

    fn from_transcript(
        transcript: TranscriptStore,
        transcript_cwd: String,
        lsp_config: LspConfig,
        mcp_config: McpConfig,
        hooks: Vec<PluginHook>,
    ) -> Self {
        let (agent_tx, agent_events) = mpsc::channel();
        let (terminal_request_tx, terminal_requests) = mpsc::channel();
        let lsp_manager = LspManager::new(lsp_config, PathBuf::from(&transcript_cwd));
        let mcp_manager = McpManager::new(mcp_config, PathBuf::from(&transcript_cwd));
        let hook_runner = HookRunner::new(hooks);
        let progress_state = transcript.progress_state();
        Self {
            transcript,
            transcript_cwd,
            agent_tx,
            agent_events,
            agent_control_tx: None,
            terminal_request_tx,
            terminal_requests,
            lsp_manager,
            mcp_manager,
            hook_runner,
            pending_mcp_elicitations: BTreeMap::new(),
            runtime_time_label: crate::context::current_time_label(),
            conversation_permissions: ConversationPermissions::default(),
            read_file_state: ReadFileState::new(),
            task_manager: TaskManager::default(),
            progress_state,
            pending_prompt_after_compact: None,
            auto_compact_failures: 0,
        }
    }

    pub fn ui_messages(&self) -> Vec<Message> {
        self.transcript.ui_messages()
    }

    pub fn usage(&self) -> ConversationUsage {
        usage_from_transcript(&self.transcript)
    }

    #[cfg(test)]
    pub fn model_history(&self) -> Vec<crate::agent::provider::ModelMessage> {
        self.transcript.model_history()
    }

    pub fn conversation_permissions(&self) -> ConversationPermissions {
        self.conversation_permissions.clone()
    }

    pub fn mcp_status_text(&self) -> String {
        self.mcp_manager.status_text()
    }

    pub fn mcp_statuses(&self) -> Vec<McpServerStatus> {
        self.mcp_manager.statuses()
    }

    pub fn reload_extensions(&mut self, lsp: LspConfig, mcp: McpConfig, hooks: Vec<PluginHook>) {
        self.decline_pending_elicitations();
        self.lsp_manager.shutdown();
        self.mcp_manager.shutdown();
        let root = PathBuf::from(&self.transcript_cwd);
        self.lsp_manager = LspManager::new(lsp, root.clone());
        self.mcp_manager = McpManager::new(mcp, root);
        self.hook_runner = HookRunner::new(hooks);
    }

    pub fn reload_mcp(&mut self, mcp: McpConfig) {
        self.decline_pending_elicitations();
        self.mcp_manager.shutdown();
        self.mcp_manager = McpManager::new_background(mcp, PathBuf::from(&self.transcript_cwd));
    }

    pub fn reconnect_mcp(&self, server: &str) -> Result<()> {
        self.mcp_manager.reconnect(server)
    }

    pub fn begin_mcp_oauth(&self, server: &str) -> Result<String> {
        self.mcp_manager.begin_oauth(server)
    }

    pub fn complete_mcp_oauth(&self, server: &str, callback_url: &str) -> Result<()> {
        self.mcp_manager.complete_oauth(server, callback_url)
    }

    pub fn logout_mcp_oauth(&self, server: &str) -> Result<()> {
        self.mcp_manager.logout_oauth(server)
    }

    pub fn has_pending_prompt_after_compact(&self) -> bool {
        self.pending_prompt_after_compact.is_some()
    }

    #[cfg(test)]
    pub fn auto_compact_failures(&self) -> u8 {
        self.auto_compact_failures
    }

    pub fn sessions(&self) -> Result<Vec<TranscriptSessionSummary>> {
        TranscriptStore::sessions(&self.transcript_cwd)
    }

    pub fn workspace_usage_stats(&self) -> Result<WorkspaceUsageStats> {
        TranscriptStore::workspace_usage_stats(&self.transcript_cwd)
    }

    pub fn load_path(&mut self, path: PathBuf) -> Result<LoadedTranscript> {
        let transcript = TranscriptStore::load_path(path)?;
        let messages = transcript.ui_messages();
        let usage = usage_from_transcript(&transcript);
        self.transcript = transcript;
        self.reset_session_state();
        Ok(LoadedTranscript { messages, usage })
    }

    pub fn create_new_session(&mut self) -> Result<LoadedTranscript> {
        let transcript = self.transcript.create_new_sibling()?;
        self.transcript = transcript;
        self.reset_session_state();
        Ok(self.loaded_transcript())
    }

    pub fn archive_current_session(&mut self) -> Result<LoadedTranscript> {
        let transcript = self.transcript.create_new_sibling()?;
        self.transcript.archive_current()?;
        self.transcript = transcript;
        self.reset_session_state();
        Ok(self.loaded_transcript())
    }

    pub fn delete_current_session(&mut self) -> Result<LoadedTranscript> {
        let transcript = self.transcript.create_new_sibling()?;
        self.transcript.delete_current()?;
        self.transcript = transcript;
        self.reset_session_state();
        Ok(self.loaded_transcript())
    }

    fn loaded_transcript(&self) -> LoadedTranscript {
        LoadedTranscript {
            messages: self.transcript.ui_messages(),
            usage: usage_from_transcript(&self.transcript),
        }
    }

    pub fn clear_context(&mut self) -> Result<LoadedTranscript> {
        self.transcript.append_clear_boundary()?;
        self.reset_session_state();
        Ok(self.loaded_transcript())
    }

    pub fn try_recv_agent_event(&self) -> Option<AgentEvent> {
        self.agent_events.try_recv().ok()
    }

    pub fn try_recv_terminal_request(&self) -> Option<TerminalRequest> {
        self.terminal_requests.try_recv().ok()
    }

    pub fn try_recv_mcp_elicitation(&mut self) -> Option<ApprovalRequest> {
        let elicitation = self.mcp_manager.try_recv_elicitation()?;
        let id = elicitation.id;
        let (command, explanation, input_schema) = match &elicitation.request {
            McpElicitationRequest::Form { message, schema } => (
                message.clone(),
                format!(
                    "An MCP server is requesting structured input matching this schema:\n{}",
                    serde_json::to_string_pretty(schema).unwrap_or_else(|_| schema.to_string())
                ),
                Some(schema.clone()),
            ),
            McpElicitationRequest::Url {
                message,
                url,
                elicitation_id,
            } => (
                message.clone(),
                format!("Complete MCP interaction `{elicitation_id}` at {url}"),
                None,
            ),
        };
        self.pending_mcp_elicitations.insert(id, elicitation);
        Some(ApprovalRequest {
            id,
            tool_name: "McpElicitation".to_owned(),
            command,
            explanation,
            kind: ApprovalKind::McpElicitation { input_schema },
        })
    }

    pub fn task_snapshots(&self) -> Vec<TaskSnapshot> {
        self.task_manager.snapshots()
    }

    pub fn pinned_progress(&self) -> Option<&TodoUpdate> {
        self.progress_state.pinned()
    }

    pub fn terminal_request_sender(&self) -> Sender<TerminalRequest> {
        self.terminal_request_tx.clone()
    }

    pub fn lsp_manager(&self) -> LspManager {
        self.lsp_manager.clone()
    }

    pub fn dynamic_tools(&self) -> Vec<Arc<dyn DynamicTool>> {
        self.mcp_manager.dynamic_tools()
    }

    pub fn hook_runner(&self) -> HookRunner {
        self.hook_runner.clone()
    }

    pub fn read_file_state(&self) -> ReadFileState {
        self.read_file_state.clone()
    }

    pub fn tool_results_dir(&self) -> PathBuf {
        self.transcript.tool_results_dir()
    }

    pub fn start_subagent_task(
        &mut self,
        request: &SubagentRequest,
        terminal_tab: usize,
    ) -> Result<TaskSnapshot, String> {
        let task = self.task_manager.start_subagent(request, terminal_tab)?;
        self.transcript
            .append_subagent_started(
                task.id.clone(),
                task.description.clone(),
                task.backend.label().to_owned(),
                task.cwd.clone(),
            )
            .ok();
        Ok(task)
    }

    pub fn finish_subagent_task(
        &mut self,
        task_id: &str,
        outcome: SubagentOutcome,
    ) -> Option<TaskSnapshot> {
        let task = self.task_manager.finish_subagent(task_id, outcome)?;
        self.transcript
            .append_subagent_finished(
                task.id.clone(),
                task.status.label().to_owned(),
                task.summary.clone(),
                task.error.clone(),
            )
            .ok();
        self.transcript
            .append_hidden_user(tasks::task_model_context_message(&task))
            .ok();
        Some(task)
    }

    pub fn terminal_tab_has_running_task(&self, terminal_tab: usize) -> bool {
        self.task_manager
            .terminal_tab_has_running_task(terminal_tab)
    }

    pub fn handle_terminal_tab_closed(&mut self, closed_index: usize) {
        self.task_manager.handle_terminal_tab_closed(closed_index);
    }

    pub fn handle_command(&mut self, command: RuntimeCommand) -> RuntimeEvent {
        match command {
            RuntimeCommand::StartManualCompact {
                llm,
                pre_prompt_tokens,
            } => self.start_manual_compact(llm, pre_prompt_tokens),
            RuntimeCommand::SubmitPrompt {
                prompt,
                config,
                pre_prompt_tokens,
            } => self.submit_prompt_or_auto_compact(prompt, config, pre_prompt_tokens),
            RuntimeCommand::StartPrompt { prompt, config } => self.start_prompt(prompt, config),
            RuntimeCommand::ApprovalDecision {
                id,
                decision,
                input,
            } => self.submit_approval_decision(id, decision, input),
            RuntimeCommand::ClearConversationEditPermission => {
                self.clear_conversation_edit_permission()
            }
            RuntimeCommand::CancelCurrentTurn { compacting } => {
                self.cancel_current_turn(compacting)
            }
        }
    }

    pub fn record_local_exchange(
        &mut self,
        user: String,
        assistant: String,
        provider: String,
        model: String,
    ) {
        self.transcript.append_user(user).ok();
        self.record_assistant(AssistantRecord {
            content: assistant,
            provider,
            model,
            tool_calls: Vec::new(),
            usage: None,
            finish_reason: FinishReason::Stop,
            error: None,
        });
    }

    pub fn record_assistant(&mut self, record: AssistantRecord) {
        self.transcript
            .append_assistant(AssistantTranscript {
                content: record.content,
                provider: record.provider,
                model: record.model,
                tool_calls: record.tool_calls,
                usage: record.usage,
                finish_reason: record.finish_reason,
                error: record.error,
            })
            .ok();
    }

    pub fn record_tool(&mut self, call_id: String, content: String, is_error: bool) {
        self.transcript.append_tool(call_id, content, is_error).ok();
    }

    pub fn apply_todo_update(&mut self, update: TodoUpdate) {
        self.progress_state.apply_update(update.clone());
        self.transcript.append_todo_update(update).ok();
    }

    pub fn complete_turn(&mut self) {
        self.transcript.complete_turn().ok();
        self.progress_state.mark_completed_for_release();
        self.agent_control_tx = None;
    }

    pub fn abort_turn(&mut self, reason: String) {
        self.transcript.abort_turn(reason).ok();
        self.agent_control_tx = None;
    }

    pub fn finish_compact(
        &mut self,
        summary: String,
        pre_prompt_tokens: Option<u64>,
    ) -> CompactFinished {
        self.hook_runner
            .run(
                HookEvent::AfterCompact,
                json!({"summary": summary, "success": true}),
            )
            .ok();
        let pending_prompt = self.pending_prompt_after_compact.take();
        let automatic = pending_prompt.is_some();
        let trigger = if automatic {
            CompactTrigger::Auto
        } else {
            CompactTrigger::Manual
        };
        self.transcript
            .append_compact_boundary(trigger, summary, pre_prompt_tokens)
            .ok();
        self.agent_control_tx = None;
        if automatic {
            self.auto_compact_failures = 0;
        }
        CompactFinished {
            messages: self.transcript.ui_messages(),
            pending_prompt,
            automatic,
        }
    }

    pub fn fail_compact(&mut self) -> CompactFailed {
        self.hook_runner
            .run(HookEvent::AfterCompact, json!({"success": false}))
            .ok();
        let pending_prompt = self.pending_prompt_after_compact.take();
        let automatic = pending_prompt.is_some();
        self.agent_control_tx = None;
        if automatic {
            self.auto_compact_failures = self.auto_compact_failures.saturating_add(1);
        }
        CompactFailed {
            pending_prompt,
            automatic,
        }
    }

    pub fn sync_conversation_permission(
        &mut self,
        edit_always_allowed: bool,
        allowed_tool: Option<String>,
    ) {
        self.conversation_permissions.edit_always_allowed = edit_always_allowed;
        if let Some(tool) = allowed_tool {
            self.conversation_permissions.allowed_tools.insert(tool);
        }
    }

    fn start_manual_compact(
        &mut self,
        llm: LlmConfig,
        pre_prompt_tokens: Option<u64>,
    ) -> RuntimeEvent {
        self.pending_prompt_after_compact = None;
        self.reset_agent_channel();
        let conversation = self.transcript.model_history();
        if conversation.is_empty() {
            return RuntimeEvent::NoMessagesToCompact;
        }
        if let Err(error) = self
            .hook_runner
            .run(HookEvent::BeforeCompact, json!({"automatic": false}))
        {
            return RuntimeEvent::Blocked {
                message: format!("Compaction blocked by plugin hook: {error:#}"),
            };
        }

        self.agent_control_tx = None;
        agent::spawn_compact_loop(
            CompactRunInput {
                llm,
                conversation,
                pre_prompt_tokens,
            },
            self.agent_tx.clone(),
        );
        RuntimeEvent::CompactStarted { automatic: false }
    }

    fn submit_prompt_or_auto_compact(
        &mut self,
        prompt: String,
        config: StartPromptConfig,
        pre_prompt_tokens: Option<u64>,
    ) -> RuntimeEvent {
        if agent::should_auto_compact(&config.llm, pre_prompt_tokens, self.auto_compact_failures) {
            let conversation = self.transcript.model_history();
            if !conversation.is_empty() {
                if let Err(error) = self
                    .hook_runner
                    .run(HookEvent::BeforeCompact, json!({"automatic": true}))
                {
                    return RuntimeEvent::Blocked {
                        message: format!("Compaction blocked by plugin hook: {error:#}"),
                    };
                }
                self.reset_agent_channel();
                self.pending_prompt_after_compact = Some(prompt);
                self.agent_control_tx = None;
                agent::spawn_compact_loop(
                    CompactRunInput {
                        llm: config.llm,
                        conversation,
                        pre_prompt_tokens,
                    },
                    self.agent_tx.clone(),
                );
                return RuntimeEvent::CompactStarted { automatic: true };
            }
        }

        self.start_prompt(prompt, config)
    }

    fn start_prompt(&mut self, prompt: String, config: StartPromptConfig) -> RuntimeEvent {
        let prompt = match self
            .hook_runner
            .run(HookEvent::PromptSubmit, json!({"prompt": prompt}))
        {
            Ok(outcome) => outcome
                .replacement
                .and_then(|replacement| {
                    replacement
                        .get("prompt")
                        .and_then(|value| value.as_str())
                        .map(str::to_owned)
                })
                .unwrap_or(prompt),
            Err(error) => {
                return RuntimeEvent::Blocked {
                    message: format!("Prompt blocked by plugin hook: {error:#}"),
                };
            }
        };
        let released_progress = self.release_completed_progress();
        self.reset_agent_channel();
        let conversation = self.transcript.model_history();
        self.transcript
            .start_turn(
                self.transcript_cwd.clone(),
                config.llm.provider.clone(),
                config.llm.model.clone(),
            )
            .ok();
        self.transcript.append_user(prompt.clone()).ok();

        let (control_tx, control_rx) = mpsc::channel();
        self.agent_control_tx = Some(control_tx);

        agent::spawn_agent_loop(
            AgentRunInput {
                llm: config.llm.clone(),
                system_prompt: config.system_prompt,
                runtime_context: RuntimeContext::with_time(
                    self.runtime_time_label.clone(),
                    config.runtime_current_dir,
                    config.shell_tool_mode,
                ),
                conversation_permissions: self.conversation_permissions.clone(),
                conversation,
                active_progress: self.progress_state.pinned().cloned(),
                current_user_message: prompt.clone(),
                tool_results_dir: self.transcript.tool_results_dir(),
                terminal_requests: self.terminal_request_tx.clone(),
                shell_tool_mode: config.shell_tool_mode,
                read_file_state: self.read_file_state.clone(),
                lsp_manager: self.lsp_manager.clone(),
                dynamic_tools: self.mcp_manager.dynamic_tools(),
                hook_runner: self.hook_runner.clone(),
            },
            self.agent_tx.clone(),
            control_rx,
        );

        RuntimeEvent::PromptStarted {
            prompt,
            released_progress,
        }
    }

    fn release_completed_progress(&mut self) -> Option<TodoUpdate> {
        let update = self.progress_state.release_completed()?;
        self.transcript
            .append_progress_snapshot(update.clone())
            .ok();
        Some(update)
    }

    fn submit_approval_decision(
        &mut self,
        id: u64,
        decision: ApprovalDecision,
        input: Option<serde_json::Value>,
    ) -> RuntimeEvent {
        if let Some(elicitation) = self.pending_mcp_elicitations.remove(&id) {
            elicitation.respond(!matches!(decision, ApprovalDecision::Deny { .. }), input);
            return RuntimeEvent::PermissionChanged;
        }
        if decision == ApprovalDecision::AllowConversation {
            self.conversation_permissions.edit_always_allowed = true;
        }
        if let Some(tx) = &self.agent_control_tx {
            tx.send(AgentControl::ApprovalDecision { id, decision })
                .ok();
        }
        RuntimeEvent::PermissionChanged
    }

    fn clear_conversation_edit_permission(&mut self) -> RuntimeEvent {
        self.conversation_permissions.edit_always_allowed = false;
        if let Some(tx) = &self.agent_control_tx {
            tx.send(AgentControl::ClearConversationEditPermission).ok();
        }
        RuntimeEvent::PermissionChanged
    }

    fn cancel_current_turn(&mut self, compacting: bool) -> RuntimeEvent {
        if let Some(tx) = &self.agent_control_tx {
            tx.send(AgentControl::Cancel).ok();
        }
        self.reset_agent_channel();
        if !compacting {
            self.transcript.abort_turn("cancelled".to_owned()).ok();
        }
        self.agent_control_tx = None;
        self.pending_prompt_after_compact = None;
        self.decline_pending_elicitations();
        RuntimeEvent::Cancelled {
            was_compacting: compacting,
        }
    }

    fn reset_agent_channel(&mut self) {
        let (agent_tx, agent_events) = mpsc::channel();
        self.agent_tx = agent_tx;
        self.agent_events = agent_events;
    }

    fn reset_session_state(&mut self) {
        self.reset_agent_channel();
        self.agent_control_tx = None;
        self.runtime_time_label = crate::context::current_time_label();
        self.conversation_permissions = ConversationPermissions::default();
        self.read_file_state.clear();
        self.progress_state = self.transcript.progress_state();
        self.pending_prompt_after_compact = None;
        self.auto_compact_failures = 0;
        self.decline_pending_elicitations();
    }

    fn decline_pending_elicitations(&mut self) {
        for (_, elicitation) in std::mem::take(&mut self.pending_mcp_elicitations) {
            elicitation.respond(false, None);
        }
    }

    #[cfg(test)]
    pub(crate) fn test_empty(path: PathBuf, transcript_cwd: String) -> Self {
        Self::from_transcript(
            TranscriptStore::test_empty(path),
            transcript_cwd,
            LspConfig::default(),
            McpConfig::default(),
            Vec::new(),
        )
    }

    #[cfg(test)]
    pub(crate) fn transcript_mut(&mut self) -> &mut TranscriptStore {
        &mut self.transcript
    }

    #[cfg(test)]
    pub(crate) fn set_pending_prompt_after_compact(&mut self, prompt: Option<String>) {
        self.pending_prompt_after_compact = prompt;
    }

    #[cfg(test)]
    pub(crate) fn set_auto_compact_failures(&mut self, failures: u8) {
        self.auto_compact_failures = failures;
    }
}

impl Drop for SessionRuntime {
    fn drop(&mut self) {
        self.lsp_manager.shutdown();
        self.mcp_manager.shutdown();
        self.hook_runner
            .run(HookEvent::SessionEnd, json!({"cwd": self.transcript_cwd}))
            .ok();
    }
}

fn usage_from_transcript(transcript: &TranscriptStore) -> ConversationUsage {
    transcript
        .token_usages()
        .fold(ConversationUsage::default(), |usage, item| {
            usage.record(item)
        })
}

fn percent(value: u64, total: u64) -> u8 {
    (value.saturating_mul(100) / total).min(100) as u8
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::progress::TodoUpdate;
    use crate::tasks::SubagentBackend;

    fn runtime() -> SessionRuntime {
        SessionRuntime::test_empty(
            std::env::temp_dir().join(format!("glint-runtime-test-{}.jsonl", uuid::Uuid::new_v4())),
            "/workspace".to_owned(),
        )
    }

    fn subagent_request() -> SubagentRequest {
        SubagentRequest {
            task_id: "a1".to_owned(),
            description: "inspect parser".to_owned(),
            prompt: "look at parser".to_owned(),
            agent: None,
            backend: SubagentBackend::Codex,
            cwd: "/workspace".to_owned(),
        }
    }

    #[test]
    fn subagent_outcome_is_model_visible_but_not_ui_visible() {
        let mut runtime = runtime();
        let request = subagent_request();
        runtime.start_subagent_task(&request, 0).unwrap();
        runtime
            .finish_subagent_task("a1", SubagentOutcome::completed("done"))
            .unwrap();

        assert!(runtime.ui_messages().is_empty());
        let history = runtime.model_history();
        assert_eq!(history.len(), 1);
        let content = history[0].content.as_deref().unwrap_or_default();
        assert!(content.contains("<subagent-outcome>"));
        assert!(content.contains("<result>done</result>"));
    }

    #[test]
    fn completed_progress_releases_on_next_prompt_boundary() {
        let mut runtime = runtime();
        let update = TodoUpdate::from_tool_arguments(&serde_json::json!({
            "todos": [
                {"content": "Inspect", "active_form": "Inspecting", "status": "completed"}
            ]
        }))
        .unwrap();

        runtime.apply_todo_update(update.clone());
        runtime.complete_turn();
        assert_eq!(runtime.pinned_progress(), Some(&update));

        let released = runtime.release_completed_progress();

        assert_eq!(released, Some(update));
        assert!(runtime.pinned_progress().is_none());
        assert!(
            runtime
                .ui_messages()
                .iter()
                .any(|message| message.role == crate::message::Role::Progress)
        );
    }
}
