use std::{
    path::PathBuf,
    sync::mpsc::{self, Receiver, Sender},
};

use anyhow::Result;

use crate::{
    agent::{
        self, AgentEvent, AgentRunInput, CompactRunInput, RuntimeContext, TokenUsage,
        provider::{FinishReason, ToolCall},
    },
    approval::{AgentControl, ApprovalDecision, ConversationPermissions},
    config::{LlmConfig, LspConfig},
    message::Message,
    services::lsp::LspManager,
    tasks::{self, SubagentOutcome, SubagentRequest, TaskManager, TaskSnapshot},
    terminal::TerminalRequest,
    tools::{ReadFileState, ShellToolMode},
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
    },
    ClearConversationEditPermission,
    CancelCurrentTurn {
        compacting: bool,
    },
}

pub enum RuntimeEvent {
    NoMessagesToCompact,
    CompactStarted { automatic: bool },
    PromptStarted { prompt: String },
    PermissionChanged,
    Cancelled { was_compacting: bool },
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
    runtime_time_label: String,
    conversation_permissions: ConversationPermissions,
    read_file_state: ReadFileState,
    task_manager: TaskManager,
    pending_prompt_after_compact: Option<String>,
    auto_compact_failures: u8,
}

impl SessionRuntime {
    pub fn create_new(cwd: String, lsp_config: LspConfig) -> Result<Self> {
        TranscriptStore::prune_archive_older_than_in_background(30);
        let transcript = TranscriptStore::create_new(&cwd)?;
        Ok(Self::from_transcript(transcript, cwd, lsp_config))
    }

    fn from_transcript(
        transcript: TranscriptStore,
        transcript_cwd: String,
        lsp_config: LspConfig,
    ) -> Self {
        let (agent_tx, agent_events) = mpsc::channel();
        let (terminal_request_tx, terminal_requests) = mpsc::channel();
        let lsp_manager = LspManager::new(lsp_config, PathBuf::from(&transcript_cwd));
        Self {
            transcript,
            transcript_cwd,
            agent_tx,
            agent_events,
            agent_control_tx: None,
            terminal_request_tx,
            terminal_requests,
            lsp_manager,
            runtime_time_label: crate::context::current_time_label(),
            conversation_permissions: ConversationPermissions::default(),
            read_file_state: ReadFileState::new(),
            task_manager: TaskManager::default(),
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
        self.conversation_permissions
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

    pub fn task_snapshots(&self) -> Vec<TaskSnapshot> {
        self.task_manager.snapshots()
    }

    pub fn terminal_request_sender(&self) -> Sender<TerminalRequest> {
        self.terminal_request_tx.clone()
    }

    pub fn lsp_manager(&self) -> LspManager {
        self.lsp_manager.clone()
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
            RuntimeCommand::ApprovalDecision { id, decision } => {
                self.submit_approval_decision(id, decision)
            }
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

    pub fn complete_turn(&mut self) {
        self.transcript.complete_turn().ok();
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

    pub fn sync_conversation_permission(&mut self, edit_always_allowed: bool) {
        self.conversation_permissions.edit_always_allowed = edit_always_allowed;
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
                conversation_permissions: self.conversation_permissions,
                conversation,
                current_user_message: prompt.clone(),
                tool_results_dir: self.transcript.tool_results_dir(),
                terminal_requests: self.terminal_request_tx.clone(),
                shell_tool_mode: config.shell_tool_mode,
                read_file_state: self.read_file_state.clone(),
                lsp_manager: self.lsp_manager.clone(),
            },
            self.agent_tx.clone(),
            control_rx,
        );

        RuntimeEvent::PromptStarted { prompt }
    }

    fn submit_approval_decision(&mut self, id: u64, decision: ApprovalDecision) -> RuntimeEvent {
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
        self.pending_prompt_after_compact = None;
        self.auto_compact_failures = 0;
    }

    #[cfg(test)]
    pub(crate) fn test_empty(path: PathBuf, transcript_cwd: String) -> Self {
        Self::from_transcript(
            TranscriptStore::test_empty(path),
            transcript_cwd,
            LspConfig::default(),
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
}
