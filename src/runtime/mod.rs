use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{
        Arc,
        mpsc::{self, Receiver, Sender},
    },
    time::{Duration, Instant},
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
    subagent_transcript::SubagentTranscriptSnapshot,
    tasks::{
        self, SubagentOutcome, SubagentRequest, SubagentSteering, TaskManager, TaskRequest,
        TaskSnapshot, TaskWaitResponse,
    },
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
    pub subagent_transcripts: Vec<SubagentTranscriptSnapshot>,
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

pub enum SubagentRuntimeEvent {
    Agent {
        task_id: String,
        terminal_tab: usize,
        event: AgentEvent,
    },
    Finished {
        task_id: String,
        terminal_tab: usize,
        task: TaskSnapshot,
    },
}

struct RunningSubagent {
    task_id: String,
    terminal_tab: usize,
    events: Receiver<AgentEvent>,
    outcome: Receiver<SubagentOutcome>,
    control: Sender<AgentControl>,
    steering: Arc<SubagentSteering>,
}

struct PendingTaskWait {
    task_ids: Vec<String>,
    deadline: Instant,
    response: Sender<Result<TaskWaitResponse, String>>,
}

pub struct SessionRuntime {
    transcript: TranscriptStore,
    transcript_cwd: String,
    agent_tx: Sender<AgentEvent>,
    agent_events: Receiver<AgentEvent>,
    agent_control_tx: Option<mpsc::Sender<AgentControl>>,
    terminal_request_tx: Sender<TerminalRequest>,
    terminal_requests: Receiver<TerminalRequest>,
    task_request_tx: Sender<TaskRequest>,
    task_requests: Receiver<TaskRequest>,
    lsp_manager: LspManager,
    mcp_manager: McpManager,
    hook_runner: HookRunner,
    pending_mcp_elicitations: BTreeMap<u64, McpElicitation>,
    runtime_time_label: String,
    conversation_permissions: ConversationPermissions,
    read_file_state: ReadFileState,
    task_manager: TaskManager,
    running_subagents: Vec<RunningSubagent>,
    pending_task_waits: Vec<PendingTaskWait>,
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
        let (task_request_tx, task_requests) = mpsc::channel();
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
            task_request_tx,
            task_requests,
            lsp_manager,
            mcp_manager,
            hook_runner,
            pending_mcp_elicitations: BTreeMap::new(),
            runtime_time_label: crate::context::current_time_label(),
            conversation_permissions: ConversationPermissions::default(),
            read_file_state: ReadFileState::new(),
            task_manager: TaskManager::default(),
            running_subagents: Vec::new(),
            pending_task_waits: Vec::new(),
            progress_state,
            pending_prompt_after_compact: None,
            auto_compact_failures: 0,
        }
    }

    pub fn ui_messages(&self) -> Vec<Message> {
        self.transcript.ui_messages()
    }

    pub fn ui_subagent_transcripts(&self) -> Vec<SubagentTranscriptSnapshot> {
        self.transcript.ui_subagent_transcripts()
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
        let subagent_transcripts = transcript.ui_subagent_transcripts();
        self.transcript = transcript;
        self.reset_session_state();
        Ok(LoadedTranscript {
            messages,
            usage,
            subagent_transcripts,
        })
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
            subagent_transcripts: self.transcript.ui_subagent_transcripts(),
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

    pub fn try_recv_task_request(&self) -> Option<TaskRequest> {
        self.task_requests.try_recv().ok()
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

    pub fn task_request_sender(&self) -> Sender<TaskRequest> {
        self.task_request_tx.clone()
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

    #[allow(clippy::too_many_arguments)]
    pub fn attach_subagent_run(
        &mut self,
        task_id: String,
        terminal_tab: usize,
        events: Receiver<AgentEvent>,
        outcome: Receiver<SubagentOutcome>,
        control: Sender<AgentControl>,
        steering: Arc<SubagentSteering>,
    ) {
        self.running_subagents.push(RunningSubagent {
            task_id,
            terminal_tab,
            events,
            outcome,
            control,
            steering,
        });
    }

    pub fn poll_subagent_events(&mut self) -> Vec<SubagentRuntimeEvent> {
        let mut runtime_events = Vec::new();
        let mut finished = Vec::new();

        for (index, run) in self.running_subagents.iter().enumerate() {
            while let Ok(event) = run.events.try_recv() {
                if let Some((activity, tool_started)) = subagent_activity(&event) {
                    self.task_manager.update_subagent_activity(
                        &run.task_id,
                        activity,
                        tool_started,
                    );
                }
                runtime_events.push(SubagentRuntimeEvent::Agent {
                    task_id: run.task_id.clone(),
                    terminal_tab: run.terminal_tab,
                    event,
                });
            }
            match run.outcome.try_recv() {
                Ok(outcome) => finished.push((index, outcome)),
                Err(mpsc::TryRecvError::Empty) => {}
                Err(mpsc::TryRecvError::Disconnected) => finished.push((
                    index,
                    SubagentOutcome::failed("subagent result channel closed", ""),
                )),
            }
        }

        for (index, outcome) in finished.into_iter().rev() {
            let run = self.running_subagents.remove(index);
            if let Some(task) = self.finish_subagent_task(&run.task_id, outcome) {
                runtime_events.push(SubagentRuntimeEvent::Finished {
                    task_id: run.task_id,
                    terminal_tab: run.terminal_tab,
                    task,
                });
            }
        }
        self.flush_task_waits();
        runtime_events
    }

    pub fn register_task_wait(
        &mut self,
        task_ids: Vec<String>,
        timeout: Duration,
        response: Sender<Result<TaskWaitResponse, String>>,
    ) {
        if task_ids.is_empty() {
            response
                .send(Err("task_ids must contain at least one task".to_owned()))
                .ok();
            return;
        }
        let tasks = match self.task_manager.snapshots_for(&task_ids) {
            Ok(tasks) => tasks,
            Err(error) => {
                response.send(Err(error)).ok();
                return;
            }
        };
        if tasks.iter().all(|task| task.status.is_terminal()) {
            response
                .send(Ok(TaskWaitResponse {
                    tasks,
                    timed_out: false,
                }))
                .ok();
            return;
        }
        self.pending_task_waits.push(PendingTaskWait {
            task_ids,
            deadline: Instant::now() + timeout,
            response,
        });
    }

    pub fn send_task_message(
        &mut self,
        task_id: &str,
        message: String,
    ) -> Result<TaskSnapshot, String> {
        if message.trim().is_empty() {
            return Err("message must not be empty".to_owned());
        }
        let task = self
            .task_manager
            .snapshot(task_id)
            .ok_or_else(|| format!("unknown task: {task_id}"))?;
        if !task.status.is_running() {
            return Err(format!(
                "task {task_id} is {}; messages can only be sent to running tasks",
                task.status.label()
            ));
        }
        let run = self
            .running_subagents
            .iter()
            .find(|run| run.task_id == task_id)
            .ok_or_else(|| format!("task {task_id} has no active runtime"))?;
        run.steering
            .send(message)
            .map_err(|error| format!("task {task_id}: {error}"))?;
        Ok(task)
    }

    pub fn cancel_task(&mut self, task_id: &str) -> Result<TaskSnapshot, String> {
        let task = self
            .task_manager
            .snapshot(task_id)
            .ok_or_else(|| format!("unknown task: {task_id}"))?;
        if !task.status.is_running() {
            return Err(format!("task {task_id} is already {}", task.status.label()));
        }
        let run = self
            .running_subagents
            .iter()
            .find(|run| run.task_id == task_id)
            .ok_or_else(|| format!("task {task_id} has no active runtime"))?;
        run.control
            .send(AgentControl::Cancel)
            .map_err(|_| format!("task {task_id} is no longer running"))?;
        Ok(task)
    }

    fn flush_task_waits(&mut self) {
        let now = Instant::now();
        let mut index = 0;
        while index < self.pending_task_waits.len() {
            let waiter = &self.pending_task_waits[index];
            let tasks = self.task_manager.snapshots_for(&waiter.task_ids);
            let ready = tasks
                .as_ref()
                .is_ok_and(|tasks| tasks.iter().all(|task| task.status.is_terminal()));
            let timed_out = now >= waiter.deadline;
            if !ready && !timed_out {
                index += 1;
                continue;
            }
            let waiter = self.pending_task_waits.remove(index);
            let result = tasks.map(|tasks| TaskWaitResponse { tasks, timed_out });
            waiter.response.send(result).ok();
        }
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

    pub fn append_subagent_presentation(
        &mut self,
        snapshot: &SubagentTranscriptSnapshot,
    ) -> Result<()> {
        self.transcript.append_subagent_presentation(snapshot)
    }

    pub fn terminal_tab_has_running_task(&self, terminal_tab: usize) -> bool {
        self.task_manager
            .terminal_tab_has_running_task(terminal_tab)
    }

    pub fn handle_terminal_tab_closed(&mut self, closed_index: usize) {
        self.task_manager.handle_terminal_tab_closed(closed_index);
        for run in &mut self.running_subagents {
            if run.terminal_tab > closed_index {
                run.terminal_tab -= 1;
            }
        }
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
                task_requests: self.task_request_tx.clone(),
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

fn subagent_activity(event: &AgentEvent) -> Option<(String, bool)> {
    match event {
        AgentEvent::Started => Some(("Thinking".to_owned(), false)),
        AgentEvent::AssistantDelta(_) => Some(("Responding".to_owned(), false)),
        AgentEvent::ToolStarted {
            name,
            input_summary,
            ..
        } => Some((format!("{name} · {input_summary}"), true)),
        AgentEvent::ToolFinished {
            name,
            output_summary,
            ..
        } => Some((format!("Finished {name} · {output_summary}"), false)),
        AgentEvent::AssistantFinished => Some(("Finishing".to_owned(), false)),
        AgentEvent::Failed(error) => Some((format!("Failed · {error}"), false)),
        AgentEvent::AssistantTurn { .. }
        | AgentEvent::TodoUpdated(_)
        | AgentEvent::ToolApprovalRequested(_)
        | AgentEvent::ConversationPermissionChanged { .. }
        | AgentEvent::CompactStarted
        | AgentEvent::CompactFinished { .. }
        | AgentEvent::CompactFailed(_) => None,
    }
}

impl Drop for SessionRuntime {
    fn drop(&mut self) {
        for run in &self.running_subagents {
            run.control.send(AgentControl::Cancel).ok();
            run.steering.close();
        }
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
    use crate::tasks::{SubagentBackend, TaskRequest};

    fn runtime() -> SessionRuntime {
        SessionRuntime::test_empty(
            std::env::temp_dir().join(format!("glint-runtime-test-{}.jsonl", uuid::Uuid::new_v4())),
            "/workspace".to_owned(),
        )
    }

    fn subagent_request() -> SubagentRequest {
        SubagentRequest {
            task_id: "a1".to_owned(),
            tool_call_id: "call-subagent".to_owned(),
            description: "inspect parser".to_owned(),
            prompt: "look at parser".to_owned(),
            agent: None,
            backend: SubagentBackend::Codex,
            cwd: "/workspace".to_owned(),
        }
    }

    #[test]
    fn subagent_runtime_events_carry_task_id() {
        let mut runtime = runtime();
        let request = subagent_request();
        runtime.start_subagent_task(&request, 0).unwrap();
        let (event_tx, event_rx) = mpsc::channel();
        let (_outcome_tx, outcome_rx) = mpsc::channel();
        let (control_tx, _control_rx) = mpsc::channel();
        runtime.attach_subagent_run(
            request.task_id.clone(),
            0,
            event_rx,
            outcome_rx,
            control_tx,
            Arc::new(SubagentSteering::default()),
        );
        event_tx.send(AgentEvent::Started).unwrap();

        assert!(matches!(
            runtime.poll_subagent_events().as_slice(),
            [SubagentRuntimeEvent::Agent { task_id, .. }] if task_id == "a1"
        ));
    }

    #[test]
    fn task_request_channel_carries_list_requests() {
        let runtime = runtime();
        let sender = runtime.task_request_sender();
        let (response, receiver) = std::sync::mpsc::channel();

        sender.send(TaskRequest::List { response }).unwrap();

        assert!(matches!(
            runtime.try_recv_task_request(),
            Some(TaskRequest::List { .. })
        ));
        drop(receiver);
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
    fn runtime_controls_and_completes_a_running_subagent() {
        let mut runtime = runtime();
        let request = subagent_request();
        runtime.start_subagent_task(&request, 0).unwrap();
        let (event_tx, event_rx) = mpsc::channel();
        let (outcome_tx, outcome_rx) = mpsc::channel();
        let (control_tx, control_rx) = mpsc::channel();
        let steering = Arc::new(SubagentSteering::default());
        runtime.attach_subagent_run(
            request.task_id.clone(),
            0,
            event_rx,
            outcome_rx,
            control_tx,
            Arc::clone(&steering),
        );

        runtime
            .send_task_message("a1", "also inspect tests".to_owned())
            .unwrap();
        assert_eq!(steering.drain(), vec!["also inspect tests"]);
        event_tx
            .send(AgentEvent::ToolStarted {
                id: "tool-1".to_owned(),
                name: "Grep".to_owned(),
                input_summary: "parser".to_owned(),
                input_description: None,
            })
            .unwrap();
        runtime.poll_subagent_events();
        let snapshot = runtime.task_snapshots().remove(0);
        assert_eq!(snapshot.activity.as_deref(), Some("Grep · parser"));
        assert_eq!(snapshot.tool_use_count, 1);
        runtime.cancel_task("a1").unwrap();
        assert!(matches!(control_rx.recv().unwrap(), AgentControl::Cancel));

        outcome_tx
            .send(SubagentOutcome::cancelled("partial"))
            .unwrap();
        let events = runtime.poll_subagent_events();
        assert!(matches!(
            events.as_slice(),
            [SubagentRuntimeEvent::Finished { task_id, task, .. }]
                if task_id == "a1" && task.status == tasks::TaskStatus::Cancelled
        ));
    }

    #[test]
    fn task_waiter_receives_completed_result_after_runtime_poll() {
        let mut runtime = runtime();
        let request = subagent_request();
        runtime.start_subagent_task(&request, 0).unwrap();
        let (_event_tx, event_rx) = mpsc::channel();
        let (outcome_tx, outcome_rx) = mpsc::channel();
        let (control_tx, _control_rx) = mpsc::channel();
        runtime.attach_subagent_run(
            request.task_id.clone(),
            0,
            event_rx,
            outcome_rx,
            control_tx,
            Arc::new(SubagentSteering::default()),
        );
        let (wait_tx, wait_rx) = mpsc::channel();
        runtime.register_task_wait(vec!["a1".to_owned()], Duration::from_secs(1), wait_tx);
        assert!(wait_rx.try_recv().is_err());

        outcome_tx
            .send(SubagentOutcome::completed("final result"))
            .unwrap();
        runtime.poll_subagent_events();

        let waited = wait_rx.recv().unwrap().unwrap();
        assert!(!waited.timed_out);
        assert_eq!(waited.tasks[0].result.as_deref(), Some("final result"));
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
