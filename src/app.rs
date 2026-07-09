use std::{
    path::{Path, PathBuf},
    sync::mpsc::Receiver,
    time::{Duration, Instant},
};

use anyhow::Result;

use crate::{
    agent::{
        AgentEvent, AgentRunInput, AgentStatus, TokenUsage,
        provider::{FinishReason, ToolCall},
        spawn_subagent_loop,
    },
    approval::{ApprovalFocus, ApprovalPrompt},
    commands::{SlashCommand, SlashCommandKind, matching_slash_commands},
    config::Config,
    event::{AppEvent, KeyAction, KeyInput, MouseAction},
    input::InputState,
    message::{Message, Role},
    runtime::{
        AssistantRecord, ConversationUsage, LoadedTranscript, RuntimeCommand, RuntimeEvent,
        SessionRuntime, StartPromptConfig,
    },
    tasks::{self, SubagentOutcome, SubagentRequest, SubagentStartResponse, TaskSnapshot},
    terminal::{
        TerminalMouseScroll, TerminalRequest, TerminalRunResult, TerminalStatus, TerminalTab,
    },
    tools::ShellToolMode,
    transcript::{TranscriptSessionSummary, WorkspaceUsageStats},
};

#[cfg(test)]
use crate::config::{LlmConfig, LlmProviderConfig, LspConfig, ModelCatalog};

pub struct App {
    pub should_quit: bool,
    pub messages: Vec<Message>,
    pub input: InputState,
    pub status: AgentStatus,
    pub scroll: u16,
    pub usage: ConversationUsage,
    pub slash_command_selection: usize,
    pub model_picker: Option<ModelPicker>,
    pub resume_picker: Option<ResumePicker>,
    pub status_view: Option<StatusView>,
    pub config: Config,
    pub current_dir: String,
    pub agent_activity: Option<String>,
    pub run_notice: Option<String>,
    pub approval: Option<ApprovalPrompt>,
    pub terminal_tabs: Vec<TerminalTab>,
    pub active_terminal_tab: usize,
    pub terminal_init_error: Option<String>,
    pub terminal_visible: bool,
    pub terminal_focused: bool,
    pub terminal_tab_switcher: Option<TerminalTabSwitcher>,
    pub text_selection: Option<TextSelection>,
    input_selection: Option<InputSelection>,
    terminal_top_row: u16,
    terminal_body_rows: u16,
    terminal_content_column: u16,
    terminal_content_width: u16,
    document_top_row: u16,
    document_height: u16,
    input_body_top_row: u16,
    input_body_rows: u16,
    input_content_width: u16,
    return_bottom_button: Option<ReturnBottomButton>,
    terminal_tab_hitbox: Option<TerminalTabHitbox>,
    pending_subagent_results: Vec<PendingSubagentRun>,
    turn_started_at: Option<Instant>,
    last_turn_duration: Option<Duration>,
    runtime: SessionRuntime,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TerminalTabSwitcher {
    pub candidate: usize,
    pub window_start: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelPicker {
    pub stage: ModelPickerStage,
    pub selected_provider: usize,
    pub selected_model: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModelPickerStage {
    Provider,
    Model,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResumePicker {
    pub sessions: Vec<TranscriptSessionSummary>,
    pub selected: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StatusView {
    pub tab: StatusTab,
    pub stats: WorkspaceUsageStats,
    pub error: Option<String>,
    pub selected_task: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StatusTab {
    General,
    Usage,
    Tasks,
    Stat,
}

struct PendingSubagentRun {
    task_id: String,
    terminal_tab: usize,
    events: Receiver<AgentEvent>,
    outcome: Receiver<SubagentOutcome>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct TextPosition {
    pub row: u16,
    pub column: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TextSelection {
    pub anchor: TextPosition,
    pub focus: TextPosition,
    pub dragging: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InputSelection {
    anchor: usize,
    focus: usize,
    dragging: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ReturnBottomButton {
    row: u16,
    start_column: u16,
    end_column: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct TerminalTabHitbox {
    start_row: u16,
    end_row: u16,
    start_column: u16,
    end_column: u16,
    first_tab: usize,
    tab_count: usize,
}

const TERMINAL_SWITCHER_CARD_WIDTH: u16 = 28;
const TERMINAL_SWITCHER_CARD_GAP: u16 = 1;

impl TextSelection {
    fn new(position: TextPosition) -> Self {
        Self {
            anchor: position,
            focus: position,
            dragging: true,
        }
    }

    pub fn ordered(self) -> Option<(TextPosition, TextPosition)> {
        if self.anchor == self.focus {
            return None;
        }
        if self.anchor <= self.focus {
            Some((self.anchor, self.focus))
        } else {
            Some((self.focus, self.anchor))
        }
    }
}

impl InputSelection {
    fn new(position: usize) -> Self {
        Self {
            anchor: position,
            focus: position,
            dragging: true,
        }
    }

    fn ordered(self) -> Option<(usize, usize)> {
        if self.anchor == self.focus {
            return None;
        }
        if self.anchor < self.focus {
            Some((self.anchor, self.focus))
        } else {
            Some((self.focus, self.anchor))
        }
    }
}

impl StatusTab {
    fn previous(self) -> Self {
        match self {
            Self::General => Self::Stat,
            Self::Usage => Self::General,
            Self::Tasks => Self::Usage,
            Self::Stat => Self::Tasks,
        }
    }

    fn next(self) -> Self {
        match self {
            Self::General => Self::Usage,
            Self::Usage => Self::Tasks,
            Self::Tasks => Self::Stat,
            Self::Stat => Self::General,
        }
    }
}

impl App {
    pub fn new(config: Config) -> Result<Self> {
        let current_dir = current_dir_label();
        let transcript_cwd = std::env::current_dir()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|_| current_dir.clone());
        let runtime = SessionRuntime::create_new(transcript_cwd, config.lsp.clone())?;
        let messages = runtime.ui_messages();
        let usage = runtime.usage();
        Ok(Self {
            should_quit: false,
            messages,
            input: InputState::default(),
            status: AgentStatus::Idle,
            scroll: 0,
            usage,
            slash_command_selection: 0,
            model_picker: None,
            resume_picker: None,
            status_view: None,
            config,
            current_dir,
            agent_activity: None,
            run_notice: None,
            approval: None,
            terminal_tabs: Vec::new(),
            active_terminal_tab: 0,
            terminal_init_error: None,
            terminal_visible: false,
            terminal_focused: false,
            terminal_tab_switcher: None,
            text_selection: None,
            input_selection: None,
            terminal_top_row: 0,
            terminal_body_rows: 0,
            terminal_content_column: 0,
            terminal_content_width: 1,
            document_top_row: 0,
            document_height: 0,
            input_body_top_row: 0,
            input_body_rows: 0,
            input_content_width: 1,
            return_bottom_button: None,
            terminal_tab_hitbox: None,
            pending_subagent_results: Vec::new(),
            turn_started_at: None,
            last_turn_duration: None,
            runtime,
        })
    }

    #[cfg(test)]
    pub(crate) fn test_empty() -> Self {
        Self {
            should_quit: false,
            messages: Vec::new(),
            input: InputState::default(),
            status: AgentStatus::Idle,
            scroll: 0,
            usage: ConversationUsage::default(),
            slash_command_selection: 0,
            model_picker: None,
            resume_picker: None,
            status_view: None,
            config: Config {
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
                model_catalog: ModelCatalog::default(),
                lsp: LspConfig::default(),
                system_prompt: "system".to_owned(),
            },
            current_dir: "/workspace".to_owned(),
            agent_activity: None,
            run_notice: None,
            approval: None,
            terminal_tabs: Vec::new(),
            active_terminal_tab: 0,
            terminal_init_error: None,
            terminal_visible: false,
            terminal_focused: false,
            terminal_tab_switcher: None,
            text_selection: None,
            input_selection: None,
            terminal_top_row: 0,
            terminal_body_rows: 0,
            terminal_content_column: 0,
            terminal_content_width: 1,
            document_top_row: 0,
            document_height: 0,
            input_body_top_row: 0,
            input_body_rows: 0,
            input_content_width: 1,
            return_bottom_button: None,
            terminal_tab_hitbox: None,
            pending_subagent_results: Vec::new(),
            turn_started_at: None,
            last_turn_duration: None,
            runtime: SessionRuntime::test_empty(
                std::env::temp_dir().join(format!("glint-app-test-{}.jsonl", uuid::Uuid::new_v4())),
                "/workspace".to_owned(),
            ),
        }
    }

    pub fn update(&mut self, event: AppEvent) {
        match event {
            AppEvent::Key(key) => self.update_key(key),
            AppEvent::Mouse(mouse) => self.update_mouse(mouse),
            AppEvent::Agent(event) => self.update_agent(event),
        }
    }

    pub fn update_agent_events(&mut self) {
        while let Some(event) = self.runtime.try_recv_agent_event() {
            self.update(AppEvent::Agent(event));
        }
    }

    pub fn processing_elapsed(&self) -> Option<Duration> {
        self.turn_started_at.map(|started_at| started_at.elapsed())
    }

    pub fn last_turn_duration(&self) -> Option<Duration> {
        self.last_turn_duration
    }

    pub fn edit_always_allowed(&self) -> bool {
        self.runtime.conversation_permissions().edit_always_allowed
    }

    pub fn task_snapshots(&self) -> Vec<TaskSnapshot> {
        self.runtime.task_snapshots()
    }

    pub fn update_terminal(&mut self) {
        for tab in &mut self.terminal_tabs {
            tab.tick();
        }

        while let Some(request) = self.runtime.try_recv_terminal_request() {
            match request {
                TerminalRequest::Run {
                    command,
                    description,
                    timeout,
                    response,
                } => {
                    if let Some(tab_index) = self.terminal_run_tab_index() {
                        if let Some(tab) = self.terminal_tabs.get_mut(tab_index) {
                            tab.run_noninteractive(command, description, timeout, response);
                        }
                    } else {
                        response
                            .send(TerminalRunResult::failed(
                                command,
                                self.terminal_init_error
                                    .clone()
                                    .unwrap_or_else(|| "agent terminal is unavailable".to_owned()),
                            ))
                            .ok();
                    }
                }
                TerminalRequest::StartSubagent { request, response } => {
                    self.start_subagent_terminal(request, response);
                }
                TerminalRequest::CancelActive => {
                    for tab in &mut self.terminal_tabs {
                        tab.cancel_active();
                    }
                }
            }
        }

        for tab in &mut self.terminal_tabs {
            tab.tick();
        }
        self.drain_subagent_results();
    }

    fn terminal_run_tab_index(&mut self) -> Option<usize> {
        if self
            .terminal_tabs
            .get(self.active_terminal_tab)
            .is_some_and(TerminalTab::is_pty)
        {
            return Some(self.active_terminal_tab);
        }
        if let Some(index) = self.terminal_tabs.iter().position(TerminalTab::is_pty) {
            return Some(index);
        }
        match TerminalTab::new_agent() {
            Ok(tab) => {
                self.terminal_tabs.push(tab);
                self.terminal_init_error = None;
                Some(self.terminal_tabs.len() - 1)
            }
            Err(error) => {
                self.terminal_init_error = Some(format!("{error:#}"));
                None
            }
        }
    }

    fn start_subagent_terminal(
        &mut self,
        request: SubagentRequest,
        response: std::sync::mpsc::Sender<SubagentStartResponse>,
    ) {
        let terminal_tab = self.terminal_tabs.len();
        let task = match self.runtime.start_subagent_task(&request, terminal_tab) {
            Ok(task) => task,
            Err(error) => {
                response
                    .send(SubagentStartResponse::failed(request.task_id, error))
                    .ok();
                return;
            }
        };

        let mut tab = TerminalTab::new_subagent(subagent_terminal_title(&task));
        tab.append_subagent_message(Message::user(request.prompt.clone()));
        self.terminal_tabs.push(tab);
        self.active_terminal_tab = terminal_tab;
        self.terminal_visible = true;
        self.terminal_focused = false;
        self.run_notice = Some(format!("Started Codex subagent {}.", task.id));
        self.messages
            .push(Message::assistant(tasks::task_started_message(&task)));

        let (event_tx, event_rx) = std::sync::mpsc::channel();
        let (outcome_tx, outcome_rx) = std::sync::mpsc::channel();
        let config = self.start_prompt_config();
        spawn_subagent_loop(
            AgentRunInput {
                llm: config.llm,
                system_prompt: config.system_prompt,
                runtime_context: crate::context::RuntimeContext::subagent_with_time(
                    crate::context::current_time_label(),
                    request.cwd.clone(),
                ),
                conversation_permissions: Default::default(),
                conversation: Vec::new(),
                current_user_message: request.prompt,
                tool_results_dir: self.runtime.tool_results_dir(),
                terminal_requests: self.runtime.terminal_request_sender(),
                shell_tool_mode: ShellToolMode::TerminalRun,
                read_file_state: self.runtime.read_file_state(),
                lsp_manager: self.runtime.lsp_manager(),
            },
            event_tx,
            outcome_tx,
        );
        self.pending_subagent_results.push(PendingSubagentRun {
            task_id: task.id.clone(),
            terminal_tab,
            events: event_rx,
            outcome: outcome_rx,
        });
        response
            .send(SubagentStartResponse::started(task.id, terminal_tab))
            .ok();
    }

    fn drain_subagent_results(&mut self) {
        let mut index = 0;
        while index < self.pending_subagent_results.len() {
            let terminal_tab = self.pending_subagent_results[index].terminal_tab;
            while let Ok(event) = self.pending_subagent_results[index].events.try_recv() {
                self.update_subagent_tab_event(terminal_tab, event);
            }

            match self.pending_subagent_results[index].outcome.try_recv() {
                Ok(outcome) => {
                    let pending = self.pending_subagent_results.remove(index);
                    self.finish_subagent_result(pending.task_id, pending.terminal_tab, outcome);
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    index += 1;
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    let pending = self.pending_subagent_results.remove(index);
                    self.finish_subagent_result(
                        pending.task_id,
                        pending.terminal_tab,
                        SubagentOutcome::failed("subagent result channel closed", ""),
                    );
                }
            }
        }
    }

    fn update_subagent_tab_event(&mut self, terminal_tab: usize, event: AgentEvent) {
        let Some(tab) = self.terminal_tabs.get_mut(terminal_tab) else {
            return;
        };
        match event {
            AgentEvent::Started => {
                tab.set_subagent_activity(Some("Thinking".to_owned()));
                tab.append_subagent_message(Message::assistant(""));
            }
            AgentEvent::AssistantDelta(delta) => {
                tab.set_subagent_activity(None);
                tab.append_subagent_assistant_delta(&delta);
            }
            AgentEvent::AssistantTurn { .. } => {}
            AgentEvent::ToolStarted {
                id,
                name,
                input_summary,
                input_description,
            } => {
                tab.set_subagent_activity(Some(format!("Running {name}: {input_summary}")));
                tab.remove_empty_subagent_assistant_tail();
                tab.append_subagent_message(Message::tool_with_description(
                    id,
                    name,
                    input_summary,
                    input_description,
                ));
            }
            AgentEvent::ToolFinished {
                id,
                name,
                output,
                output_summary,
                ..
            } => {
                tab.set_subagent_activity(Some(format!("Finished {name}: {output_summary}")));
                if name == "Read" {
                    if let Some(message) = tab.subagent_tool_message_mut(&id) {
                        message.tool_finished = true;
                    }
                    return;
                }
                if let Some(message) = tab.subagent_tool_message_mut(&id) {
                    message.content = output;
                    message.tool_finished = true;
                }
            }
            AgentEvent::ToolApprovalRequested(request) => {
                tab.set_subagent_activity(Some(format!(
                    "Approval unavailable: {}",
                    request.command
                )));
            }
            AgentEvent::ConversationPermissionChanged { .. } => {}
            AgentEvent::CompactStarted
            | AgentEvent::CompactFinished { .. }
            | AgentEvent::CompactFailed(_) => {}
            AgentEvent::AssistantFinished => {
                tab.set_subagent_activity(None);
            }
            AgentEvent::Failed(error) => {
                tab.set_subagent_activity(None);
                tab.remove_empty_subagent_assistant_tail();
                tab.append_subagent_assistant_delta(&error);
                tab.finish_subagent(TerminalStatus::Error(error));
            }
        }
    }

    fn finish_subagent_result(
        &mut self,
        task_id: String,
        terminal_tab: usize,
        outcome: SubagentOutcome,
    ) {
        let Some(task) = self.runtime.finish_subagent_task(&task_id, outcome) else {
            return;
        };
        if let Some(tab) = self.terminal_tabs.get_mut(terminal_tab) {
            let status = match task.status {
                tasks::TaskStatus::Completed => TerminalStatus::Idle,
                tasks::TaskStatus::Cancelled => TerminalStatus::TimedOut,
                tasks::TaskStatus::Failed => TerminalStatus::Error(
                    task.error
                        .clone()
                        .unwrap_or_else(|| "subagent failed".to_owned()),
                ),
                tasks::TaskStatus::Queued | tasks::TaskStatus::Running => TerminalStatus::Running {
                    description: format!("Codex subagent {}", task.id),
                },
            };
            tab.finish_subagent(status);
        }
        self.run_notice = Some(tasks::task_finished_message(&task));
    }

    pub fn resize_terminal(&mut self, rows: u16, cols: u16) {
        for tab in &mut self.terminal_tabs {
            tab.resize(rows, cols);
        }
    }

    pub fn set_terminal_top_row(&mut self, row: u16) {
        self.terminal_top_row = row;
    }

    pub fn set_terminal_content_geometry(
        &mut self,
        body_rows: u16,
        content_column: u16,
        content_width: u16,
    ) {
        self.terminal_body_rows = body_rows;
        self.terminal_content_column = content_column;
        self.terminal_content_width = content_width.max(1);
    }

    pub fn set_document_viewport(&mut self, height: u16, top_row: u16) {
        self.document_height = height;
        self.document_top_row = top_row;
    }

    pub fn set_input_hitbox(&mut self, top_row: u16, rows: u16, content_width: u16) {
        self.input_body_top_row = top_row;
        self.input_body_rows = rows;
        self.input_content_width = content_width.max(1);
    }

    pub fn set_return_bottom_button_hitbox(&mut self, hitbox: Option<(u16, u16, u16)>) {
        self.return_bottom_button =
            hitbox.map(|(row, start_column, end_column)| ReturnBottomButton {
                row,
                start_column,
                end_column,
            });
    }

    pub fn set_terminal_tab_hitbox(&mut self, hitbox: Option<(u16, u16, u16, u16, usize, usize)>) {
        self.terminal_tab_hitbox = hitbox.map(
            |(start_row, end_row, start_column, end_column, first_tab, tab_count)| {
                TerminalTabHitbox {
                    start_row,
                    end_row,
                    start_column,
                    end_column,
                    first_tab,
                    tab_count,
                }
            },
        );
    }

    pub fn input_selection_range(&self) -> Option<(usize, usize)> {
        let (start, end) = self.input_selection?.ordered()?;
        (end <= self.input.value.len()
            && self.input.value.is_char_boundary(start)
            && self.input.value.is_char_boundary(end))
        .then_some((start, end))
    }

    pub fn selected_input_text(&self) -> Option<String> {
        if !self.input_mouse_enabled() {
            return None;
        }
        let (start, end) = self.input_selection_range()?;
        self.input.value.get(start..end).map(str::to_owned)
    }

    pub fn finish_input_selection_copy(&mut self) {
        self.input_selection = None;
        self.run_notice = None;
    }

    pub fn finish_input_selection_cut(&mut self) {
        self.delete_input_selection();
        self.run_notice = None;
    }

    pub fn finish_selection_copy(&mut self) {
        self.text_selection = None;
        self.run_notice = None;
    }

    pub fn fail_selection_copy(&mut self, error: &str) {
        self.run_notice = Some(format!("Failed to copy selection: {error}"));
    }

    pub fn request_quit(&mut self) {
        self.should_quit = true;
    }

    pub fn should_defer_key_to_terminal(&self, key: &KeyInput) -> bool {
        self.terminal_focused && key.terminal_input.is_some()
    }

    pub fn active_terminal_tab(&self) -> Option<&TerminalTab> {
        self.terminal_tabs.get(self.active_terminal_tab)
    }

    fn active_terminal_tab_mut(&mut self) -> Option<&mut TerminalTab> {
        self.terminal_tabs.get_mut(self.active_terminal_tab)
    }

    fn update_key(&mut self, key: KeyInput) {
        let action = key.action;
        if action == KeyAction::ForceQuit {
            self.should_quit = true;
            return;
        }
        if action == KeyAction::NewTerminalTab {
            self.new_terminal_tab();
            return;
        }
        if action == KeyAction::CloseTerminalTab {
            self.close_terminal_tab();
            return;
        }
        if let KeyAction::SelectTerminalTab(index) = action {
            self.select_terminal_tab(index);
            return;
        }
        if action == KeyAction::ToggleTerminalFocus && self.terminal_visible {
            self.terminal_focused = !self.terminal_focused;
            if !self.terminal_focused {
                self.terminal_tab_switcher = None;
            }
            return;
        }
        if self.terminal_focused && self.handle_terminal_switcher_key(action) {
            return;
        }
        if self.terminal_focused
            && self.terminal_tab_switcher.is_none()
            && action == KeyAction::CtrlDown
            && !self.terminal_tabs.is_empty()
        {
            self.open_terminal_tab_switcher();
            return;
        }
        if self.terminal_focused && self.write_terminal_key(&key) {
            return;
        }
        if action == KeyAction::Quit {
            if self.status == AgentStatus::Idle {
                self.should_quit = true;
            } else {
                self.cancel_current_turn();
            }
            return;
        }
        if action == KeyAction::CancelConversationPermission {
            self.clear_conversation_edit_permission();
            return;
        }
        if self.approval.is_some() {
            self.update_approval_key(action);
            return;
        }
        if self.resume_picker.is_some() {
            self.update_resume_picker_key(action);
            return;
        }
        if self.status_view.is_some() {
            self.update_status_view_key(action);
            return;
        }
        if self.model_picker.is_some() {
            self.update_model_picker_key(action);
            return;
        }
        if self.slash_menu_visible()
            && matches!(action, KeyAction::Submit | KeyAction::Up | KeyAction::Down)
        {
            self.update_slash_menu_key(action);
            return;
        }

        match action {
            KeyAction::Quit => self.should_quit = true,
            KeyAction::ForceQuit => self.should_quit = true,
            KeyAction::Submit if self.status == AgentStatus::Idle => self.submit(),
            KeyAction::Newline if self.status == AgentStatus::Idle => {
                self.replace_input_selection_or_insert("\n")
            }
            KeyAction::Char(char) if self.status == AgentStatus::Idle => {
                self.replace_input_selection_or_insert(&char.to_string())
            }
            KeyAction::Backspace if self.status == AgentStatus::Idle => {
                if !self.delete_input_selection() {
                    self.input.backspace();
                }
            }
            KeyAction::Delete if self.status == AgentStatus::Idle => {
                if !self.delete_input_selection() {
                    self.input.delete_forward();
                }
            }
            KeyAction::Left if self.status == AgentStatus::Idle => {
                self.input_selection = None;
                self.input.move_left();
            }
            KeyAction::Right if self.status == AgentStatus::Idle => {
                self.input_selection = None;
                self.input.move_right();
            }
            KeyAction::Up if self.status == AgentStatus::Idle => {
                self.input_selection = None;
                self.input.move_up();
            }
            KeyAction::Down if self.status == AgentStatus::Idle => {
                self.input_selection = None;
                self.input.move_down();
            }
            KeyAction::Up => self.scroll = self.scroll.saturating_add(1),
            KeyAction::Down => self.scroll = self.scroll.saturating_sub(1),
            KeyAction::None
            | KeyAction::Submit
            | KeyAction::Newline
            | KeyAction::Char(_)
            | KeyAction::Backspace
            | KeyAction::Delete
            | KeyAction::Cut
            | KeyAction::Left
            | KeyAction::Right
            | KeyAction::CtrlUp
            | KeyAction::CtrlDown
            | KeyAction::Tab
            | KeyAction::Escape
            | KeyAction::ToggleTerminalFocus
            | KeyAction::NewTerminalTab
            | KeyAction::CloseTerminalTab
            | KeyAction::SelectTerminalTab(_)
            | KeyAction::CancelConversationPermission => {}
        }
        self.clamp_slash_command_selection();
    }

    fn replace_input_selection_or_insert(&mut self, replacement: &str) {
        if let Some((start, end)) = self.input_selection_range() {
            self.input.replace_range(start, end, replacement);
            self.input_selection = None;
        } else {
            for character in replacement.chars() {
                self.input.push(character);
            }
        }
    }

    fn delete_input_selection(&mut self) -> bool {
        let Some((start, end)) = self.input_selection_range() else {
            self.input_selection = None;
            return false;
        };
        self.input.delete_range(start, end);
        self.input_selection = None;
        true
    }

    fn write_terminal_key(&mut self, key: &KeyInput) -> bool {
        let Some(tab) = self.active_terminal_tab_mut() else {
            return false;
        };
        if tab.is_running() {
            if key.terminal_input.as_deref() == Some(&[0x03][..]) {
                tab.cancel_active();
            }
            return true;
        }

        if let Some(input) = key.terminal_input.as_deref() {
            tab.write_input(input);
            return true;
        }
        false
    }

    fn handle_terminal_switcher_key(&mut self, action: KeyAction) -> bool {
        if self.terminal_tab_switcher.is_none() {
            return false;
        }
        match action {
            KeyAction::Left => self.move_terminal_tab_switcher(-1),
            KeyAction::Right => self.move_terminal_tab_switcher(1),
            KeyAction::CtrlUp | KeyAction::Submit => self.confirm_terminal_tab_switcher(),
            KeyAction::Up | KeyAction::Down | KeyAction::CtrlDown => {}
            KeyAction::Escape => self.terminal_tab_switcher = None,
            _ => return false,
        }
        true
    }

    fn open_terminal_tab_switcher(&mut self) {
        let len = self.terminal_tabs.len();
        if len == 0 {
            return;
        }
        let candidate = self.active_terminal_tab.min(len - 1);
        let visible = self.visible_terminal_switcher_cards();
        self.terminal_tab_switcher = Some(TerminalTabSwitcher {
            candidate,
            window_start: terminal_switcher_open_window_start(candidate, len, visible),
        });
    }

    fn move_terminal_tab_switcher(&mut self, direction: isize) {
        let len = self.terminal_tabs.len();
        let visible = self.visible_terminal_switcher_cards();
        let Some(switcher) = self.terminal_tab_switcher.as_mut() else {
            return;
        };
        switcher.candidate = move_index(switcher.candidate, direction, len);
        switcher.window_start =
            terminal_switcher_window_start(switcher.window_start, switcher.candidate, len, visible);
    }

    fn confirm_terminal_tab_switcher(&mut self) {
        let Some(switcher) = self.terminal_tab_switcher.take() else {
            return;
        };
        self.select_terminal_tab(switcher.candidate);
    }

    fn visible_terminal_switcher_cards(&self) -> usize {
        terminal_switcher_visible_cards_for_width(
            self.terminal_content_width.saturating_add(4),
            self.terminal_tabs.len(),
        )
        .max(1)
    }

    pub fn slash_query(&self) -> Option<&str> {
        if self.status != AgentStatus::Idle
            || self.model_picker.is_some()
            || self.resume_picker.is_some()
            || self.status_view.is_some()
        {
            return None;
        }
        let value = self.input.value.as_str();
        let query = value.strip_prefix('/')?;
        if query.contains(char::is_whitespace) {
            return None;
        }
        Some(query)
    }

    pub fn slash_command_matches(&self) -> Vec<SlashCommand> {
        let Some(query) = self.slash_query() else {
            return Vec::new();
        };
        matching_slash_commands(query)
    }

    pub fn slash_menu_visible(&self) -> bool {
        self.slash_query().is_some()
    }

    fn update_slash_menu_key(&mut self, key: KeyAction) {
        let matches = self.slash_command_matches();
        match key {
            KeyAction::Submit => {
                if let Some(command) = matches.get(
                    self.slash_command_selection
                        .min(matches.len().saturating_sub(1)),
                ) {
                    self.run_slash_command(*command);
                } else {
                    self.submit_unknown_slash_command();
                }
            }
            KeyAction::Up if !matches.is_empty() => {
                self.slash_command_selection = if self.slash_command_selection == 0 {
                    matches.len() - 1
                } else {
                    self.slash_command_selection - 1
                };
            }
            KeyAction::Down if !matches.is_empty() => {
                self.slash_command_selection = (self.slash_command_selection + 1) % matches.len();
            }
            _ => {}
        }
    }

    fn run_slash_command(&mut self, command: SlashCommand) {
        match command.kind {
            SlashCommandKind::New => self.run_new_session(),
            SlashCommandKind::Clear => self.run_clear_context(),
            SlashCommandKind::Archive => self.run_archive_session(),
            SlashCommandKind::Delete => self.run_delete_session(),
            SlashCommandKind::Status => self.open_status_view(),
            SlashCommandKind::Compact => self.run_compact(),
            SlashCommandKind::Model => self.open_model_picker(),
            SlashCommandKind::Resume => self.open_resume_picker(),
            SlashCommandKind::Terminal => self.toggle_terminal(),
        }
    }

    fn submit_unknown_slash_command(&mut self) {
        let command = self.input.take_trimmed();
        self.input_selection = None;
        if command.is_empty() {
            return;
        }
        self.messages.push(Message::user(command.clone()));
        let response = format!("Unknown slash command `{command}`");
        self.record_local_exchange(command, response.clone());
        self.messages.push(Message::assistant(response));
        self.scroll = 0;
    }

    fn open_model_picker(&mut self) {
        self.input.set("/model");
        let selected_provider = self
            .config
            .llm
            .providers
            .iter()
            .position(|provider| provider.name == self.config.llm.provider)
            .unwrap_or(0);
        let selected_model = self
            .config
            .llm
            .providers
            .get(selected_provider)
            .and_then(|provider| {
                provider
                    .models
                    .iter()
                    .position(|model| model == &self.config.llm.model)
            })
            .unwrap_or(0);
        self.model_picker = Some(ModelPicker {
            stage: ModelPickerStage::Provider,
            selected_provider,
            selected_model,
        });
    }

    fn update_model_picker_key(&mut self, key: KeyAction) {
        match key {
            KeyAction::Submit => self.confirm_model_picker(),
            KeyAction::Backspace => self.back_out_of_model_picker(),
            KeyAction::Up => self.move_model_picker(-1),
            KeyAction::Down => self.move_model_picker(1),
            _ => {}
        }
    }

    fn confirm_model_picker(&mut self) {
        let Some(stage) = self.model_picker.as_ref().map(|picker| picker.stage) else {
            return;
        };

        match stage {
            ModelPickerStage::Provider => {
                let selected_provider = self
                    .model_picker
                    .as_ref()
                    .map(|picker| picker.selected_provider)
                    .unwrap_or(0);
                let selected_model = self
                    .config
                    .llm
                    .providers
                    .get(selected_provider)
                    .and_then(|provider| {
                        provider
                            .models
                            .iter()
                            .position(|model| model == &self.config.llm.model)
                    })
                    .unwrap_or(0);
                if let Some(picker) = self.model_picker.as_mut() {
                    picker.stage = ModelPickerStage::Model;
                    picker.selected_model = selected_model;
                }
            }
            ModelPickerStage::Model => self.switch_selected_model(),
        }
    }

    fn switch_selected_model(&mut self) {
        let Some(picker) = self.model_picker.take() else {
            return;
        };
        let Some((provider_name, model_name)) = self
            .config
            .llm
            .providers
            .get(picker.selected_provider)
            .and_then(|provider| {
                provider
                    .models
                    .get(picker.selected_model)
                    .map(|model| (provider.name.clone(), model.clone()))
            })
        else {
            return;
        };

        let command = self.input.take_trimmed();
        let command = if command.is_empty() {
            "/model".to_owned()
        } else {
            command
        };
        self.messages.push(Message::user(command.clone()));

        let result =
            match self
                .config
                .llm
                .switch_model(&provider_name, &model_name, |api_key_env| {
                    std::env::var(api_key_env).ok()
                }) {
                Ok(()) => format!("Switch model to `{model_name}` provided by `{provider_name}`"),
                Err(error) => format!("Failed to switch model: {error:#}"),
            };
        self.record_local_exchange(command, result.clone());
        self.messages.push(Message::assistant(result));
        self.scroll = 0;
    }

    fn back_out_of_model_picker(&mut self) {
        let Some(picker) = self.model_picker.as_mut() else {
            return;
        };

        if picker.stage == ModelPickerStage::Model {
            picker.stage = ModelPickerStage::Provider;
        } else {
            self.model_picker = None;
            self.input.set("");
        }
    }

    fn move_model_picker(&mut self, direction: isize) {
        let Some(picker) = self.model_picker.as_mut() else {
            return;
        };

        match picker.stage {
            ModelPickerStage::Provider => {
                picker.selected_provider = move_index(
                    picker.selected_provider,
                    direction,
                    self.config.llm.providers.len(),
                );
                picker.selected_model = 0;
            }
            ModelPickerStage::Model => {
                let model_count = self
                    .config
                    .llm
                    .providers
                    .get(picker.selected_provider)
                    .map(|provider| provider.models.len())
                    .unwrap_or(0);
                picker.selected_model = move_index(picker.selected_model, direction, model_count);
            }
        }
    }

    fn open_resume_picker(&mut self) {
        self.input.set("/resume");
        let sessions = self.runtime.sessions().unwrap_or_default();
        self.resume_picker = Some(ResumePicker {
            sessions,
            selected: 0,
        });
    }

    fn update_resume_picker_key(&mut self, key: KeyAction) {
        match key {
            KeyAction::Submit => self.confirm_resume_picker(),
            KeyAction::Escape => self.close_resume_picker(),
            KeyAction::Up | KeyAction::Left => self.move_resume_picker(-1),
            KeyAction::Down | KeyAction::Right => self.move_resume_picker(1),
            _ => {}
        }
    }

    fn confirm_resume_picker(&mut self) {
        let Some(path) = self
            .resume_picker
            .as_ref()
            .and_then(|picker| picker.sessions.get(picker.selected))
            .map(|session| session.path.clone())
        else {
            self.close_resume_picker();
            return;
        };
        let Ok(loaded) = self.runtime.load_path(path) else {
            self.close_resume_picker();
            return;
        };
        self.messages = loaded.messages;
        self.usage = loaded.usage;
        self.scroll = 0;
        self.close_resume_picker();
    }

    fn close_resume_picker(&mut self) {
        self.resume_picker = None;
        self.input.set("");
    }

    fn move_resume_picker(&mut self, direction: isize) {
        let Some(picker) = self.resume_picker.as_mut() else {
            return;
        };
        picker.selected = move_index(picker.selected, direction, picker.sessions.len());
    }

    fn open_status_view(&mut self) {
        self.input.set("/status");
        let (stats, error) = match self.runtime.workspace_usage_stats() {
            Ok(stats) => (stats, None),
            Err(error) => (WorkspaceUsageStats::default(), Some(format!("{error:#}"))),
        };
        self.status_view = Some(StatusView {
            tab: StatusTab::General,
            stats,
            error,
            selected_task: 0,
        });
    }

    fn update_status_view_key(&mut self, key: KeyAction) {
        match key {
            KeyAction::Escape => self.close_status_view(),
            KeyAction::Left => self.move_status_view(-1),
            KeyAction::Right | KeyAction::Tab => self.move_status_view(1),
            KeyAction::Up => self.move_status_task_selection(-1),
            KeyAction::Down => self.move_status_task_selection(1),
            KeyAction::Submit => self.open_selected_status_task_terminal(),
            _ => {}
        }
    }

    fn close_status_view(&mut self) {
        self.status_view = None;
        self.input.set("");
    }

    fn move_status_view(&mut self, direction: isize) {
        let Some(view) = self.status_view.as_mut() else {
            return;
        };
        view.tab = if direction < 0 {
            view.tab.previous()
        } else {
            view.tab.next()
        };
    }

    fn move_status_task_selection(&mut self, direction: isize) {
        let task_count = self.runtime.task_snapshots().len();
        let Some(view) = self.status_view.as_mut() else {
            return;
        };
        if view.tab != StatusTab::Tasks {
            return;
        }
        view.selected_task = move_index(view.selected_task, direction, task_count);
    }

    fn open_selected_status_task_terminal(&mut self) {
        let Some(view) = self.status_view.as_ref() else {
            return;
        };
        if view.tab != StatusTab::Tasks {
            return;
        }
        let Some(task) = self
            .runtime
            .task_snapshots()
            .get(view.selected_task)
            .cloned()
        else {
            return;
        };
        let Some(tab) = task.terminal_tab else {
            self.run_notice = Some(format!("Task {} has no terminal tab.", task.id));
            return;
        };
        if tab >= self.terminal_tabs.len() {
            self.run_notice = Some(format!("Task {} terminal tab is unavailable.", task.id));
            return;
        }
        self.status_view = None;
        self.input.set("");
        self.select_terminal_tab(tab);
    }

    fn apply_loaded_transcript(&mut self, loaded: LoadedTranscript) {
        self.messages = loaded.messages;
        self.usage = loaded.usage;
        self.scroll = 0;
        self.approval = None;
    }

    fn clamp_slash_command_selection(&mut self) {
        let matches = self.slash_command_matches();
        if matches.is_empty() {
            self.slash_command_selection = 0;
        } else {
            self.slash_command_selection = self.slash_command_selection.min(matches.len() - 1);
        }
    }

    fn update_approval_key(&mut self, key: KeyAction) {
        let Some(approval) = self.approval.as_mut() else {
            return;
        };

        match key {
            KeyAction::Submit => self.confirm_approval(),
            KeyAction::Up => approval.move_up(),
            KeyAction::Down => approval.move_down(),
            KeyAction::Tab => approval.focus_feedback(),
            KeyAction::Char(char) if approval.focus == ApprovalFocus::Feedback => {
                approval.feedback.push(char)
            }
            KeyAction::Backspace if approval.focus == ApprovalFocus::Feedback => {
                approval.feedback.backspace()
            }
            KeyAction::Left if approval.focus == ApprovalFocus::Feedback => {
                approval.feedback.move_left()
            }
            KeyAction::Right if approval.focus == ApprovalFocus::Feedback => {
                approval.feedback.move_right()
            }
            _ => {}
        }
    }

    fn confirm_approval(&mut self) {
        let Some(approval) = self.approval.take() else {
            return;
        };
        let decision = approval.decision();
        self.runtime
            .handle_command(RuntimeCommand::ApprovalDecision {
                id: approval.request.id,
                decision,
            });
        self.status = AgentStatus::Responding;
    }

    fn clear_conversation_edit_permission(&mut self) {
        self.runtime
            .handle_command(RuntimeCommand::ClearConversationEditPermission);
    }

    fn update_mouse(&mut self, mouse: MouseAction) {
        match mouse {
            MouseAction::LeftDown { column, row } if self.mouse_over_return_bottom(column, row) => {
                self.scroll = 0;
                self.text_selection = None;
                self.input_selection = None;
                self.terminal_focused = false;
            }
            MouseAction::LeftDown { column, row } if !self.mouse_over_terminal_area(row) => {
                self.terminal_focused = false;
                if let Some(byte_index) = self.input_byte_index_from_mouse(column, row) {
                    self.input.cursor = byte_index;
                    self.input_selection = Some(InputSelection::new(byte_index));
                    self.text_selection = None;
                } else {
                    self.input_selection = None;
                    self.text_selection =
                        self.document_position(column, row).map(TextSelection::new);
                }
            }
            MouseAction::LeftDown { column, row } => {
                self.text_selection = None;
                self.input_selection = None;
                self.terminal_focused = true;
                if let Some(index) = self.terminal_tab_at(column, row) {
                    self.terminal_tab_switcher = None;
                    self.select_terminal_tab(index);
                }
            }
            MouseAction::LeftDrag { column, row } => {
                if self
                    .input_selection
                    .is_some_and(|selection| selection.dragging)
                {
                    if let Some(byte_index) = self.clamped_input_byte_index_from_mouse(column, row)
                        && let Some(selection) = &mut self.input_selection
                    {
                        selection.focus = byte_index;
                        self.input.cursor = byte_index;
                    }
                } else if let Some(position) = self.clamped_document_position(column, row)
                    && let Some(selection) = &mut self.text_selection
                    && selection.dragging
                {
                    selection.focus = position;
                }
            }
            MouseAction::LeftUp { column, row } => {
                if self
                    .input_selection
                    .is_some_and(|selection| selection.dragging)
                {
                    if let Some(byte_index) = self.clamped_input_byte_index_from_mouse(column, row)
                        && let Some(selection) = &mut self.input_selection
                    {
                        selection.focus = byte_index;
                        selection.dragging = false;
                        self.input.cursor = byte_index;
                    }
                    if self.input_selection_range().is_none() {
                        self.input_selection = None;
                    }
                } else if let Some(position) = self.clamped_document_position(column, row)
                    && let Some(selection) = &mut self.text_selection
                    && selection.dragging
                {
                    selection.focus = position;
                    selection.dragging = false;
                }
            }
            MouseAction::ScrollUp { column, row }
                if self.mouse_over_terminal_switcher(column, row) =>
            {
                self.move_terminal_tab_switcher(-1);
            }
            MouseAction::ScrollDown { column, row }
                if self.mouse_over_terminal_switcher(column, row) =>
            {
                self.move_terminal_tab_switcher(1);
            }
            MouseAction::ScrollUp { column, row } if self.mouse_over_terminal(row) => {
                if self.write_terminal_mouse_scroll(column, row, TerminalMouseScroll::Up) {
                    return;
                }
                if let Some(tab) = self.active_terminal_tab_mut() {
                    tab.scroll_up(3);
                }
            }
            MouseAction::ScrollDown { column, row } if self.mouse_over_terminal(row) => {
                if self.write_terminal_mouse_scroll(column, row, TerminalMouseScroll::Down) {
                    return;
                }
                if let Some(tab) = self.active_terminal_tab_mut() {
                    tab.scroll_down(3);
                }
            }
            MouseAction::ScrollUp { .. } => self.scroll = self.scroll.saturating_add(3),
            MouseAction::ScrollDown { .. } => self.scroll = self.scroll.saturating_sub(3),
            MouseAction::None => {}
        }
    }

    fn document_position(&self, column: u16, row: u16) -> Option<TextPosition> {
        if self.document_height == 0 || row >= self.document_height {
            return None;
        }
        Some(TextPosition {
            row: self.document_top_row.saturating_add(row),
            column,
        })
    }

    fn clamped_document_position(&self, column: u16, row: u16) -> Option<TextPosition> {
        if self.document_height == 0 {
            return None;
        }
        Some(TextPosition {
            row: self
                .document_top_row
                .saturating_add(row.min(self.document_height.saturating_sub(1))),
            column,
        })
    }

    fn input_byte_index_from_mouse(&self, column: u16, row: u16) -> Option<usize> {
        if !self.input_mouse_enabled() {
            return None;
        }
        let input_row = row.checked_sub(self.input_body_top_row)?;
        if input_row >= self.input_body_rows {
            return None;
        }

        let input_column = column.saturating_sub(4) as usize;
        Some(self.input.visual_position_byte_index(
            input_row as usize,
            input_column,
            self.input_content_width as usize,
        ))
    }

    fn clamped_input_byte_index_from_mouse(&self, column: u16, row: u16) -> Option<usize> {
        if !self.input_mouse_enabled() || self.input_body_rows == 0 {
            return None;
        }

        let first_input_row = self.input_body_top_row;
        let last_input_row = first_input_row.saturating_add(self.input_body_rows - 1);
        let clamped_row = row.clamp(first_input_row, last_input_row);
        let input_column = if row < first_input_row {
            0
        } else if row > last_input_row {
            self.input_content_width as usize
        } else {
            column.saturating_sub(4) as usize
        };

        Some(self.input.visual_position_byte_index(
            (clamped_row - first_input_row) as usize,
            input_column,
            self.input_content_width as usize,
        ))
    }

    fn input_mouse_enabled(&self) -> bool {
        self.status == AgentStatus::Idle
            && self.approval.is_none()
            && self.model_picker.is_none()
            && self.resume_picker.is_none()
            && self.status_view.is_none()
    }

    fn mouse_over_return_bottom(&self, column: u16, row: u16) -> bool {
        self.return_bottom_button.is_some_and(|button| {
            row == button.row && column >= button.start_column && column < button.end_column
        })
    }

    fn mouse_over_terminal(&self, row: u16) -> bool {
        self.mouse_over_terminal_area(row) && !self.terminal_tabs.is_empty()
    }

    fn mouse_over_terminal_area(&self, row: u16) -> bool {
        self.terminal_visible && row >= self.terminal_top_row
    }

    fn terminal_tab_at(&self, column: u16, row: u16) -> Option<usize> {
        let hitbox = self.terminal_tab_hitbox?;
        if row < hitbox.start_row
            || row >= hitbox.end_row
            || column < hitbox.start_column
            || column >= hitbox.end_column
        {
            return None;
        }

        let relative_column = column - hitbox.start_column;
        let slot_width = TERMINAL_SWITCHER_CARD_WIDTH + TERMINAL_SWITCHER_CARD_GAP;
        let slot = relative_column / slot_width;
        let in_slot = relative_column % slot_width;
        if slot as usize >= hitbox.tab_count || in_slot >= TERMINAL_SWITCHER_CARD_WIDTH {
            return None;
        }
        let index = hitbox.first_tab + slot as usize;
        (index < self.terminal_tabs.len()).then_some(index)
    }

    fn mouse_over_terminal_switcher(&self, column: u16, row: u16) -> bool {
        self.terminal_tab_switcher.is_some()
            && self.terminal_tab_hitbox.is_some_and(|hitbox| {
                row >= hitbox.start_row
                    && row < hitbox.end_row
                    && column >= hitbox.start_column
                    && column < hitbox.end_column
            })
    }

    fn write_terminal_mouse_scroll(
        &mut self,
        column: u16,
        row: u16,
        direction: TerminalMouseScroll,
    ) -> bool {
        if !self.mouse_over_terminal_body(column, row) {
            return false;
        }

        let Some(tab) = self.active_terminal_tab_mut() else {
            return false;
        };
        tab.write_mouse_scroll(direction)
    }

    fn mouse_over_terminal_body(&self, column: u16, row: u16) -> bool {
        let body_top_row = self.terminal_top_row.saturating_add(1);
        let body_bottom_row = body_top_row.saturating_add(self.terminal_body_rows);
        self.terminal_body_rows > 0
            && row >= body_top_row
            && row < body_bottom_row
            && column >= self.terminal_content_column
    }

    fn toggle_terminal(&mut self) {
        let _command = self.input.take_trimmed();
        if self.terminal_visible {
            if self.terminal_tabs.iter().any(TerminalTab::is_running) {
                self.run_notice = Some("Terminal is running; cannot hide it.".to_owned());
                return;
            }
            self.terminal_visible = false;
            self.terminal_focused = false;
            self.terminal_tab_switcher = None;
            self.run_notice = Some("Terminal hidden. Bash is active.".to_owned());
            return;
        }

        if self.terminal_tabs.is_empty() && !self.create_terminal_tab() {
            return;
        }

        self.terminal_visible = true;
        self.terminal_focused = true;
        self.terminal_tab_switcher = None;
        self.run_notice = Some("Terminal visible. TerminalRun is active.".to_owned());
    }

    fn new_terminal_tab(&mut self) {
        if self.create_terminal_tab() {
            self.terminal_visible = true;
            self.run_notice = None;
        }
    }

    fn close_terminal_tab(&mut self) {
        if !self.terminal_visible || self.terminal_tabs.is_empty() {
            return;
        }
        self.terminal_tab_switcher = None;

        let index = self.active_terminal_tab.min(self.terminal_tabs.len() - 1);
        if self.runtime.terminal_tab_has_running_task(index) {
            self.run_notice = Some("Codex subagent is running; cannot close this tab.".to_owned());
            return;
        }
        let tab = self.terminal_tabs.remove(index);
        tab.close();
        self.runtime.handle_terminal_tab_closed(index);

        if self.terminal_tabs.is_empty() {
            self.active_terminal_tab = 0;
            self.terminal_visible = false;
            self.terminal_focused = false;
            self.run_notice = None;
            return;
        }

        self.active_terminal_tab = index.min(self.terminal_tabs.len() - 1);
        self.run_notice = None;
    }

    fn create_terminal_tab(&mut self) -> bool {
        match TerminalTab::new_agent() {
            Ok(tab) => {
                self.terminal_tabs.push(tab);
                self.active_terminal_tab = self.terminal_tabs.len().saturating_sub(1);
                self.terminal_tab_switcher = None;
                self.terminal_init_error = None;
                true
            }
            Err(error) => {
                self.terminal_init_error = Some(format!("{error:#}"));
                self.run_notice = Some("Failed to start terminal.".to_owned());
                false
            }
        }
    }

    fn select_terminal_tab(&mut self, index: usize) {
        if index >= self.terminal_tabs.len() {
            return;
        }
        self.active_terminal_tab = index;
        self.terminal_visible = true;
        self.terminal_tab_switcher = None;
        self.run_notice = None;
    }

    fn run_new_session(&mut self) {
        let _command = self.input.take_trimmed();
        match self.runtime.create_new_session() {
            Ok(loaded) => {
                self.apply_loaded_transcript(loaded);
                self.run_notice = Some("Started new session.".to_owned());
            }
            Err(error) => {
                self.run_notice = Some(format!("Failed to start new session: {error:#}"));
            }
        }
    }

    fn run_clear_context(&mut self) {
        let _command = self.input.take_trimmed();
        match self.runtime.clear_context() {
            Ok(loaded) => {
                self.apply_loaded_transcript(loaded);
                self.run_notice = Some("Cleared conversation context.".to_owned());
            }
            Err(error) => {
                self.run_notice = Some(format!("Failed to clear context: {error:#}"));
            }
        }
    }

    fn run_archive_session(&mut self) {
        let _command = self.input.take_trimmed();
        match self.runtime.archive_current_session() {
            Ok(loaded) => {
                self.apply_loaded_transcript(loaded);
                self.run_notice = Some("Archived conversation.".to_owned());
            }
            Err(error) => {
                self.run_notice = Some(format!("Failed to archive conversation: {error:#}"));
            }
        }
    }

    fn run_delete_session(&mut self) {
        let _command = self.input.take_trimmed();
        match self.runtime.delete_current_session() {
            Ok(loaded) => {
                self.apply_loaded_transcript(loaded);
                self.run_notice = Some("Deleted conversation.".to_owned());
            }
            Err(error) => {
                self.run_notice = Some(format!("Failed to delete conversation: {error:#}"));
            }
        }
    }

    fn run_compact(&mut self) {
        let _command = self.input.take_trimmed();
        self.run_notice = None;
        let event = self
            .runtime
            .handle_command(RuntimeCommand::StartManualCompact {
                llm: self.config.llm.clone(),
                pre_prompt_tokens: self.usage.last_usage.map(|usage| usage.prompt_tokens),
            });
        self.apply_runtime_event(event);
    }

    fn submit(&mut self) {
        let prompt = self.input.take_trimmed();
        self.input_selection = None;
        if prompt.is_empty() {
            return;
        }
        self.run_notice = None;
        let event = self.runtime.handle_command(RuntimeCommand::SubmitPrompt {
            prompt,
            config: self.start_prompt_config(),
            pre_prompt_tokens: self.usage.last_usage.map(|usage| usage.prompt_tokens),
        });
        self.apply_runtime_event(event);
    }

    fn submit_prompt(&mut self, prompt: String, clear_notice: bool) {
        if clear_notice {
            self.run_notice = None;
        }
        let event = self.runtime.handle_command(RuntimeCommand::StartPrompt {
            prompt,
            config: self.start_prompt_config(),
        });
        self.apply_runtime_event(event);
    }

    fn start_prompt_config(&self) -> StartPromptConfig {
        let runtime_current_dir = std::env::current_dir()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|_| self.current_dir.clone());

        StartPromptConfig {
            llm: self.config.llm.clone(),
            system_prompt: self.config.system_prompt.clone(),
            runtime_current_dir,
            shell_tool_mode: self.shell_tool_mode(),
        }
    }

    fn apply_runtime_event(&mut self, event: RuntimeEvent) {
        match event {
            RuntimeEvent::NoMessagesToCompact => {
                self.run_notice = Some("No messages to compact.".to_owned());
            }
            RuntimeEvent::CompactStarted { automatic } => {
                self.status = AgentStatus::Compacting;
                self.start_turn_timer();
                self.agent_activity = Some(if automatic {
                    "Auto-compacting conversation".to_owned()
                } else {
                    "Compacting conversation".to_owned()
                });
                self.scroll = 0;
            }
            RuntimeEvent::PromptStarted { prompt } => {
                self.messages.push(Message::user(prompt));
                self.status = AgentStatus::Thinking;
                self.start_turn_timer();
                self.scroll = 0;
                self.agent_activity = None;
            }
            RuntimeEvent::PermissionChanged => {}
            RuntimeEvent::Cancelled { was_compacting } => {
                self.status = AgentStatus::Idle;
                self.finish_turn_timer();
                self.agent_activity = None;
                self.approval = None;
                self.remove_empty_assistant_tail();
                self.run_notice = Some(if was_compacting {
                    "Stopped compact.".to_owned()
                } else {
                    "Stopped this turn.".to_owned()
                });
            }
        }
    }

    fn update_agent(&mut self, event: AgentEvent) {
        match event {
            AgentEvent::CompactStarted => {
                self.status = AgentStatus::Compacting;
                self.agent_activity = Some(if self.runtime.has_pending_prompt_after_compact() {
                    "Auto-compacting conversation".to_owned()
                } else {
                    "Compacting conversation".to_owned()
                });
            }
            AgentEvent::CompactFinished {
                summary,
                pre_prompt_tokens,
            } => {
                if let Some(prompt) = self.finish_compact(summary, pre_prompt_tokens) {
                    self.submit_prompt(prompt, false);
                }
            }
            AgentEvent::CompactFailed(error) => {
                if let Some(prompt) = self.fail_compact(error) {
                    self.submit_prompt(prompt, false);
                }
            }
            AgentEvent::Started => {
                self.status = AgentStatus::Responding;
                self.agent_activity = None;
                self.messages.push(Message::assistant(""));
            }
            AgentEvent::AssistantDelta(delta) => {
                self.agent_activity = None;
                self.append_assistant_delta(&delta);
            }
            AgentEvent::AssistantTurn {
                usage,
                finish_reason,
                tool_calls,
            } => {
                if let Some(usage) = usage {
                    self.usage = self.usage.record(usage);
                }
                self.record_assistant(
                    self.current_assistant_content(),
                    tool_calls,
                    usage,
                    finish_reason,
                    None,
                );
            }
            AgentEvent::ToolStarted {
                id,
                name,
                input_summary,
                input_description,
            } => {
                self.agent_activity = Some(format!("Running {name}: {input_summary}"));
                self.remove_empty_assistant_tail();
                if name == "Read" && self.merge_read_tool(&input_summary) {
                    return;
                }
                self.messages.push(Message::tool_with_description(
                    id,
                    name,
                    input_summary,
                    input_description,
                ));
            }
            AgentEvent::ToolFinished {
                id,
                name,
                output,
                is_error,
                output_summary,
            } => {
                self.agent_activity = Some(format!("Finished {name}: {output_summary}"));
                self.runtime
                    .record_tool(id.clone(), output.clone(), is_error);
                if name == "Read" {
                    if let Some(message) = self.find_tool_message(&id) {
                        message.tool_finished = true;
                    }
                    return;
                }

                if let Some(message) = self.messages.iter_mut().rev().find(|message| {
                    message.role == Role::Tool && message.tool_call_id.as_deref() == Some(&id)
                }) {
                    message.content = output;
                    message.tool_finished = true;
                }
            }
            AgentEvent::ToolApprovalRequested(request) => {
                self.status = AgentStatus::AwaitingApproval;
                self.agent_activity = Some(format!("Approval needed: {}", request.command));
                self.approval = Some(ApprovalPrompt::new(request));
            }
            AgentEvent::ConversationPermissionChanged {
                edit_always_allowed,
            } => {
                self.runtime
                    .sync_conversation_permission(edit_always_allowed);
            }
            AgentEvent::AssistantFinished => {
                self.runtime.complete_turn();
                self.status = AgentStatus::Idle;
                self.finish_turn_timer();
                self.agent_activity = None;
                self.approval = None;
            }
            AgentEvent::Failed(error) => {
                self.append_assistant_delta(&error);
                self.record_assistant(
                    self.current_assistant_content(),
                    Vec::new(),
                    None,
                    FinishReason::Other("error".to_owned()),
                    Some(error.clone()),
                );
                self.runtime.abort_turn(error);
                self.status = AgentStatus::Idle;
                self.finish_turn_timer();
                self.agent_activity = None;
                self.approval = None;
            }
        }
    }

    fn finish_compact(
        &mut self,
        summary: String,
        pre_prompt_tokens: Option<u64>,
    ) -> Option<String> {
        let finished = self.runtime.finish_compact(summary, pre_prompt_tokens);
        self.messages = finished.messages;
        self.status = AgentStatus::Idle;
        self.finish_turn_timer();
        self.agent_activity = None;
        self.approval = None;
        if finished.automatic {
            self.run_notice = Some("Auto-compacted conversation.".to_owned());
        } else {
            self.run_notice = Some("Compacted conversation.".to_owned());
        }
        finished.pending_prompt
    }

    fn fail_compact(&mut self, error: String) -> Option<String> {
        let failed = self.runtime.fail_compact();
        self.status = AgentStatus::Idle;
        self.finish_turn_timer();
        self.agent_activity = None;
        self.approval = None;
        if failed.automatic {
            self.run_notice = Some("Auto-compact failed; continuing without compact.".to_owned());
        } else {
            self.run_notice = Some(format!("Compact failed: {error}"));
        }
        failed.pending_prompt
    }

    fn append_assistant_delta(&mut self, delta: &str) {
        if !matches!(self.messages.last(), Some(message) if message.role == Role::Assistant) {
            self.messages.push(Message::assistant(""));
        }

        if let Some(message) = self.messages.last_mut() {
            message.content.push_str(delta);
        }
    }

    fn current_assistant_content(&self) -> String {
        self.messages
            .last()
            .filter(|message| message.role == Role::Assistant)
            .map(|message| message.content.clone())
            .unwrap_or_default()
    }

    fn record_local_exchange(&mut self, user: String, assistant: String) {
        self.runtime.record_local_exchange(
            user,
            assistant,
            self.config.llm.provider.clone(),
            self.config.llm.model.clone(),
        );
    }

    fn record_assistant(
        &mut self,
        content: String,
        tool_calls: Vec<ToolCall>,
        usage: Option<TokenUsage>,
        finish_reason: FinishReason,
        error: Option<String>,
    ) {
        self.runtime.record_assistant(AssistantRecord {
            content,
            provider: self.config.llm.provider.clone(),
            model: self.config.llm.model.clone(),
            tool_calls,
            usage,
            finish_reason,
            error,
        });
    }

    fn cancel_current_turn(&mut self) {
        let was_compacting = self.status == AgentStatus::Compacting;
        let event = self
            .runtime
            .handle_command(RuntimeCommand::CancelCurrentTurn {
                compacting: was_compacting,
            });
        self.apply_runtime_event(event);
    }

    fn start_turn_timer(&mut self) {
        self.turn_started_at = Some(Instant::now());
        self.last_turn_duration = None;
    }

    fn finish_turn_timer(&mut self) {
        self.last_turn_duration = self.turn_started_at.map(|started_at| started_at.elapsed());
        self.turn_started_at = None;
    }

    fn shell_tool_mode(&self) -> ShellToolMode {
        if self.terminal_visible {
            ShellToolMode::TerminalRun
        } else {
            ShellToolMode::Bash
        }
    }

    fn remove_empty_assistant_tail(&mut self) {
        if matches!(
            self.messages.last(),
            Some(message) if message.role == Role::Assistant && message.content.is_empty()
        ) {
            self.messages.pop();
        }
    }

    fn merge_read_tool(&mut self, input_summary: &str) -> bool {
        let Some(message) = self.messages.last_mut() else {
            return false;
        };
        if message.role != Role::Tool || message.tool_name.as_deref() != Some("Read") {
            return false;
        }

        if let Some(input) = message.tool_input.as_mut() {
            input.push_str(" ｜ ");
            input.push_str(input_summary);
        }
        true
    }

    fn find_tool_message(&mut self, id: &str) -> Option<&mut Message> {
        self.messages.iter_mut().rev().find(|message| {
            message.role == Role::Tool && message.tool_call_id.as_deref() == Some(id)
        })
    }
}

fn move_index(index: usize, direction: isize, len: usize) -> usize {
    if len == 0 {
        return 0;
    }
    index.saturating_add_signed(direction).min(len - 1)
}

fn terminal_switcher_visible_cards_for_width(width: u16, tab_count: usize) -> usize {
    if tab_count == 0 {
        return 0;
    }
    let inner_width = width.saturating_sub(4).max(TERMINAL_SWITCHER_CARD_WIDTH);
    let slot = TERMINAL_SWITCHER_CARD_WIDTH + TERMINAL_SWITCHER_CARD_GAP;
    let visible = ((inner_width + TERMINAL_SWITCHER_CARD_GAP) / slot).max(1) as usize;
    visible.min(tab_count)
}

fn terminal_switcher_open_window_start(candidate: usize, len: usize, visible: usize) -> usize {
    let visible = visible.max(1);
    if len <= visible {
        return 0;
    }
    candidate.saturating_sub(1).min(len - visible)
}

fn terminal_switcher_window_start(
    current_start: usize,
    candidate: usize,
    len: usize,
    visible: usize,
) -> usize {
    let visible = visible.max(1);
    if len <= visible {
        return 0;
    }
    let max_start = len - visible;
    if candidate < current_start {
        candidate.min(max_start)
    } else if candidate >= current_start.saturating_add(visible) {
        candidate
            .saturating_add(1)
            .saturating_sub(visible)
            .min(max_start)
    } else {
        current_start.min(max_start)
    }
}

fn subagent_terminal_title(task: &TaskSnapshot) -> String {
    const MAX_DESCRIPTION_CHARS: usize = 18;
    let description = if task.description.chars().count() <= MAX_DESCRIPTION_CHARS {
        task.description.clone()
    } else {
        let mut value = task
            .description
            .chars()
            .take(MAX_DESCRIPTION_CHARS.saturating_sub(3))
            .collect::<String>();
        value.push_str("...");
        value
    };
    format!("codex {} {}", task.id, description)
}

fn current_dir_label() -> String {
    std::env::current_dir()
        .map(|path| home_relative_path(&path))
        .unwrap_or_else(|_| "?".to_owned())
}

fn home_relative_path(path: &Path) -> String {
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
        return path.display().to_string();
    };

    if path == home {
        return "~".to_owned();
    }

    path.strip_prefix(&home)
        .ok()
        .filter(|relative| !relative.as_os_str().is_empty())
        .map(|relative| format!("~/{}", relative.display()))
        .unwrap_or_else(|| path.display().to_string())
}

#[cfg(test)]
mod tests {
    use crate::{agent::should_auto_compact, commands::SLASH_COMMANDS};

    use super::*;

    fn app() -> App {
        App::test_empty()
    }

    fn add_test_tabs(app: &mut App, count: usize) {
        for index in 0..count {
            app.terminal_tabs
                .push(TerminalTab::new_subagent(format!("tab {}", index + 1)));
        }
    }

    #[test]
    fn slash_commands_include_compact() {
        let names = SLASH_COMMANDS
            .iter()
            .map(|command| command.name)
            .collect::<Vec<_>>();

        assert!(names.contains(&"/new"));
        assert!(names.contains(&"/clear"));
        assert!(names.contains(&"/archive"));
        assert!(names.contains(&"/delete"));
        assert!(names.contains(&"/status"));
        assert!(names.contains(&"/compact"));
        assert!(names.contains(&"/terminal"));
    }

    #[test]
    fn mouse_drag_updates_text_selection_in_document_coordinates() {
        let mut app = app();
        app.set_document_viewport(20, 10);

        app.update(AppEvent::Mouse(MouseAction::LeftDown { column: 2, row: 3 }));
        app.update(AppEvent::Mouse(MouseAction::LeftDrag { column: 8, row: 5 }));
        app.update(AppEvent::Mouse(MouseAction::LeftUp { column: 9, row: 5 }));

        let selection = app.text_selection.expect("selection should be active");
        assert_eq!(selection.anchor, TextPosition { row: 13, column: 2 });
        assert_eq!(selection.focus, TextPosition { row: 15, column: 9 });
        assert!(!selection.dragging);
    }

    #[test]
    fn mouse_down_outside_document_does_not_start_selection() {
        let mut app = app();
        app.set_document_viewport(4, 0);

        app.update(AppEvent::Mouse(MouseAction::LeftDown { column: 2, row: 5 }));

        assert!(app.text_selection.is_none());
    }

    #[test]
    fn mouse_down_in_document_area_focuses_chat() {
        let mut app = app();
        app.terminal_visible = true;
        app.terminal_focused = true;
        app.set_terminal_top_row(10);
        app.set_document_viewport(10, 0);

        app.update(AppEvent::Mouse(MouseAction::LeftDown { column: 2, row: 3 }));

        assert!(!app.terminal_focused);
        assert!(app.text_selection.is_some());
    }

    #[test]
    fn mouse_down_in_terminal_area_focuses_terminal() {
        let mut app = app();
        app.terminal_visible = true;
        app.terminal_focused = false;
        app.text_selection = Some(TextSelection {
            anchor: TextPosition { row: 1, column: 1 },
            focus: TextPosition { row: 2, column: 2 },
            dragging: false,
        });
        app.input_selection = Some(InputSelection {
            anchor: 1,
            focus: 3,
            dragging: false,
        });
        app.set_terminal_top_row(10);
        app.set_document_viewport(10, 0);

        app.update(AppEvent::Mouse(MouseAction::LeftDown {
            column: 2,
            row: 11,
        }));

        assert!(app.terminal_focused);
        assert!(app.text_selection.is_none());
        assert!(app.input_selection.is_none());
    }

    #[test]
    fn mouse_down_on_terminal_tab_selects_that_tab() {
        let mut app = app();
        app.terminal_visible = true;
        add_test_tabs(&mut app, 4);
        app.active_terminal_tab = 0;
        app.terminal_tab_switcher = Some(TerminalTabSwitcher {
            candidate: 1,
            window_start: 1,
        });
        app.set_terminal_top_row(10);
        app.set_terminal_tab_hitbox(Some((11, 12, 2, 89, 1, 3)));

        app.update(AppEvent::Mouse(MouseAction::LeftDown {
            column: 31,
            row: 11,
        }));

        assert!(app.terminal_focused);
        assert_eq!(app.active_terminal_tab, 2);
        assert!(app.terminal_tab_switcher.is_none());
        assert!(app.run_notice.is_none());
    }

    #[test]
    fn mouse_down_in_terminal_content_does_not_switch_tabs() {
        let mut app = app();
        app.terminal_visible = true;
        add_test_tabs(&mut app, 4);
        app.active_terminal_tab = 1;
        app.set_terminal_top_row(10);
        app.set_terminal_tab_hitbox(Some((11, 12, 2, 89, 1, 3)));

        app.update(AppEvent::Mouse(MouseAction::LeftDown {
            column: 10,
            row: 12,
        }));

        assert!(app.terminal_focused);
        assert_eq!(app.active_terminal_tab, 1);
        assert!(app.run_notice.is_none());
    }

    #[test]
    fn ctrl_down_opens_terminal_tab_switcher_when_terminal_focused() {
        let mut app = app();
        app.terminal_visible = true;
        app.terminal_focused = true;
        add_test_tabs(&mut app, 4);
        app.active_terminal_tab = 1;
        app.set_terminal_content_geometry(5, 2, 116);

        app.update(AppEvent::Key(KeyInput {
            action: KeyAction::CtrlDown,
            terminal_input: Some(b"\x1b[B".to_vec()),
        }));

        assert_eq!(
            app.terminal_tab_switcher,
            Some(TerminalTabSwitcher {
                candidate: 1,
                window_start: 0,
            })
        );
    }

    #[test]
    fn switcher_right_moves_candidate_and_window() {
        let mut app = app();
        app.terminal_visible = true;
        app.terminal_focused = true;
        add_test_tabs(&mut app, 5);
        app.terminal_tab_switcher = Some(TerminalTabSwitcher {
            candidate: 1,
            window_start: 0,
        });
        app.set_terminal_content_geometry(5, 2, 58);

        app.update(AppEvent::Key(KeyInput {
            action: KeyAction::Right,
            terminal_input: Some(b"\x1b[C".to_vec()),
        }));
        app.update(AppEvent::Key(KeyInput {
            action: KeyAction::Right,
            terminal_input: Some(b"\x1b[C".to_vec()),
        }));

        assert_eq!(
            app.terminal_tab_switcher,
            Some(TerminalTabSwitcher {
                candidate: 3,
                window_start: 2,
            })
        );
    }

    #[test]
    fn switcher_enter_confirms_and_escape_cancels() {
        let mut app = app();
        app.terminal_visible = true;
        app.terminal_focused = true;
        add_test_tabs(&mut app, 4);
        app.active_terminal_tab = 0;
        app.terminal_tab_switcher = Some(TerminalTabSwitcher {
            candidate: 2,
            window_start: 1,
        });

        app.update(AppEvent::Key(KeyInput {
            action: KeyAction::Submit,
            terminal_input: Some(b"\r".to_vec()),
        }));

        assert_eq!(app.active_terminal_tab, 2);
        assert!(app.terminal_tab_switcher.is_none());

        app.terminal_tab_switcher = Some(TerminalTabSwitcher {
            candidate: 3,
            window_start: 2,
        });
        app.update(AppEvent::Key(KeyInput {
            action: KeyAction::Escape,
            terminal_input: Some(b"\x1b".to_vec()),
        }));

        assert_eq!(app.active_terminal_tab, 2);
        assert!(app.terminal_tab_switcher.is_none());
    }

    #[test]
    fn switcher_up_confirms_candidate() {
        let mut app = app();
        app.terminal_visible = true;
        app.terminal_focused = true;
        add_test_tabs(&mut app, 3);
        app.active_terminal_tab = 0;
        app.terminal_tab_switcher = Some(TerminalTabSwitcher {
            candidate: 1,
            window_start: 0,
        });

        app.update(AppEvent::Key(KeyInput {
            action: KeyAction::CtrlUp,
            terminal_input: Some(b"\x1b[A".to_vec()),
        }));

        assert_eq!(app.active_terminal_tab, 1);
        assert!(app.terminal_tab_switcher.is_none());
    }

    #[test]
    fn switcher_plain_down_is_consumed_without_moving_candidate() {
        let mut app = app();
        app.terminal_visible = true;
        app.terminal_focused = true;
        add_test_tabs(&mut app, 4);
        app.terminal_tab_switcher = Some(TerminalTabSwitcher {
            candidate: 1,
            window_start: 0,
        });
        app.set_terminal_content_geometry(5, 2, 58);

        app.update(AppEvent::Key(KeyInput {
            action: KeyAction::Down,
            terminal_input: Some(b"\x1b[B".to_vec()),
        }));

        assert_eq!(
            app.terminal_tab_switcher,
            Some(TerminalTabSwitcher {
                candidate: 1,
                window_start: 0,
            })
        );
    }

    #[test]
    fn mouse_wheel_over_switcher_moves_candidate_horizontally() {
        let mut app = app();
        app.terminal_visible = true;
        app.terminal_focused = true;
        add_test_tabs(&mut app, 5);
        app.terminal_tab_switcher = Some(TerminalTabSwitcher {
            candidate: 1,
            window_start: 0,
        });
        app.set_terminal_top_row(10);
        app.set_terminal_content_geometry(5, 2, 58);
        app.set_terminal_tab_hitbox(Some((11, 12, 2, 60, 0, 2)));

        app.update(AppEvent::Mouse(MouseAction::ScrollDown {
            column: 4,
            row: 11,
        }));

        assert_eq!(
            app.terminal_tab_switcher,
            Some(TerminalTabSwitcher {
                candidate: 2,
                window_start: 1,
            })
        );
    }

    #[test]
    fn terminal_focus_defers_ctrl_c_to_terminal_input() {
        let mut app = app();
        app.terminal_focused = true;
        let input = KeyInput {
            action: KeyAction::Quit,
            terminal_input: Some(vec![0x03]),
        };

        assert!(app.should_defer_key_to_terminal(&input));

        app.terminal_focused = false;
        assert!(!app.should_defer_key_to_terminal(&input));
    }

    #[test]
    fn terminal_body_hit_test_uses_content_area() {
        let mut app = app();
        app.set_terminal_top_row(10);
        app.set_terminal_content_geometry(5, 14, 80);

        assert!(app.mouse_over_terminal_body(17, 12));
        assert!(!app.mouse_over_terminal_body(13, 12));
        assert!(!app.mouse_over_terminal_body(17, 10));
        assert!(!app.mouse_over_terminal_body(17, 16));
    }

    #[test]
    fn mouse_down_in_input_moves_cursor_without_starting_selection() {
        let mut app = app();
        app.input.set("hello");
        app.terminal_focused = true;
        app.set_document_viewport(20, 0);
        app.set_input_hitbox(3, 1, 80);

        app.update(AppEvent::Mouse(MouseAction::LeftDown { column: 7, row: 3 }));

        assert_eq!(app.input.cursor, 3);
        assert!(!app.terminal_focused);
        assert!(app.text_selection.is_none());
    }

    #[test]
    fn mouse_down_in_wrapped_input_moves_cursor_to_visual_row() {
        let mut app = app();
        app.input.set("abcdef");
        app.set_document_viewport(20, 0);
        app.set_input_hitbox(3, 2, 3);

        app.update(AppEvent::Mouse(MouseAction::LeftDown { column: 5, row: 4 }));

        assert_eq!(app.input.cursor, 4);
        assert!(app.text_selection.is_none());
    }

    #[test]
    fn mouse_drag_in_input_selects_text() {
        let mut app = app();
        app.input.set("hello");
        app.set_document_viewport(20, 0);
        app.set_input_hitbox(3, 1, 80);

        app.update(AppEvent::Mouse(MouseAction::LeftDown { column: 5, row: 3 }));
        app.update(AppEvent::Mouse(MouseAction::LeftDrag { column: 8, row: 3 }));
        app.update(AppEvent::Mouse(MouseAction::LeftUp { column: 8, row: 3 }));

        assert_eq!(app.selected_input_text().as_deref(), Some("ell"));
        assert_eq!(app.input.cursor, 4);
        assert!(app.text_selection.is_none());
    }

    #[test]
    fn delete_key_removes_input_selection() {
        let mut app = app();
        app.input.set("hello");
        app.input_selection = Some(InputSelection {
            anchor: 1,
            focus: 4,
            dragging: false,
        });

        app.update(AppEvent::Key(KeyInput {
            action: KeyAction::Delete,
            terminal_input: None,
        }));

        assert_eq!(app.input.value, "ho");
        assert_eq!(app.input.cursor, 1);
        assert!(app.input_selection.is_none());
    }

    #[test]
    fn typed_character_replaces_input_selection() {
        let mut app = app();
        app.input.set("hello");
        app.input_selection = Some(InputSelection {
            anchor: 1,
            focus: 4,
            dragging: false,
        });

        app.update(AppEvent::Key(KeyInput {
            action: KeyAction::Char('i'),
            terminal_input: None,
        }));

        assert_eq!(app.input.value, "hio");
        assert_eq!(app.input.cursor, 2);
        assert!(app.input_selection.is_none());
    }

    #[test]
    fn cut_completion_deletes_input_selection() {
        let mut app = app();
        app.input.set("hello");
        app.input_selection = Some(InputSelection {
            anchor: 1,
            focus: 4,
            dragging: false,
        });

        app.finish_input_selection_cut();

        assert_eq!(app.input.value, "ho");
        assert!(app.run_notice.is_none());
    }

    #[test]
    fn mouse_down_on_return_bottom_button_scrolls_to_bottom() {
        let mut app = app();
        app.scroll = 12;
        app.text_selection = Some(TextSelection {
            anchor: TextPosition { row: 2, column: 1 },
            focus: TextPosition { row: 3, column: 4 },
            dragging: false,
        });
        app.set_return_bottom_button_hitbox(Some((8, 20, 30)));

        app.update(AppEvent::Mouse(MouseAction::LeftDown {
            column: 24,
            row: 8,
        }));

        assert_eq!(app.scroll, 0);
        assert!(app.text_selection.is_none());
    }

    #[test]
    fn app_starts_with_terminal_hidden_and_uncreated() {
        let app = app();

        assert!(!app.terminal_visible);
        assert!(app.terminal_tabs.is_empty());
    }

    #[test]
    fn terminal_command_shows_and_focuses_terminal() {
        let mut app = app();
        add_test_tabs(&mut app, 1);
        app.terminal_focused = false;
        app.terminal_tab_switcher = Some(TerminalTabSwitcher {
            candidate: 0,
            window_start: 0,
        });
        app.input.set("/terminal");

        let command = SLASH_COMMANDS
            .iter()
            .find(|command| command.name == "/terminal")
            .copied()
            .unwrap();
        app.run_slash_command(command);

        assert!(app.terminal_visible);
        assert!(app.terminal_focused);
        assert!(app.terminal_tab_switcher.is_none());
        assert!(app.input.value.is_empty());
    }

    #[test]
    fn compact_command_on_empty_history_does_not_record_user_message() {
        let mut app = app();
        app.input.set("/compact");

        let command = SLASH_COMMANDS
            .iter()
            .find(|command| command.name == "/compact")
            .copied()
            .unwrap();
        app.run_slash_command(command);

        assert_eq!(app.status, AgentStatus::Idle);
        assert_eq!(app.messages.len(), 0);
        assert_eq!(app.runtime.model_history().len(), 0);
        assert_eq!(app.run_notice.as_deref(), Some("No messages to compact."));
    }

    #[test]
    fn new_command_starts_empty_session() {
        let mut app = app();
        app.runtime
            .transcript_mut()
            .append_user("old user".to_owned())
            .unwrap();
        app.messages = app.runtime.ui_messages();
        app.input.set("/new");

        let command = SLASH_COMMANDS
            .iter()
            .find(|command| command.name == "/new")
            .copied()
            .unwrap();
        app.run_slash_command(command);

        assert!(app.messages.is_empty());
        assert!(app.runtime.model_history().is_empty());
        assert_eq!(app.usage, ConversationUsage::default());
        assert_eq!(app.run_notice.as_deref(), Some("Started new session."));
    }

    #[test]
    fn clear_command_clears_current_context() {
        let mut app = app();
        app.runtime
            .transcript_mut()
            .append_user("old user".to_owned())
            .unwrap();
        app.messages = app.runtime.ui_messages();
        app.usage = ConversationUsage::default().record(TokenUsage {
            prompt_tokens: 100,
            completion_tokens: 25,
            total_tokens: 125,
            cached_prompt_tokens: None,
        });
        app.input.set("/clear");

        let command = SLASH_COMMANDS
            .iter()
            .find(|command| command.name == "/clear")
            .copied()
            .unwrap();
        app.run_slash_command(command);

        assert_eq!(app.messages.len(), 1);
        assert_eq!(app.messages[0].content, "Cleared conversation context");
        assert!(app.runtime.model_history().is_empty());
        assert_eq!(app.usage, ConversationUsage::default());
        assert_eq!(
            app.run_notice.as_deref(),
            Some("Cleared conversation context.")
        );
    }

    #[test]
    fn delete_command_deletes_current_session_and_starts_empty_session() {
        let mut app = app();
        app.runtime
            .transcript_mut()
            .append_user("old user".to_owned())
            .unwrap();
        app.messages = app.runtime.ui_messages();
        app.usage = ConversationUsage::default().record(TokenUsage {
            prompt_tokens: 100,
            completion_tokens: 25,
            total_tokens: 125,
            cached_prompt_tokens: None,
        });
        app.input.set("/delete");

        let command = SLASH_COMMANDS
            .iter()
            .find(|command| command.name == "/delete")
            .copied()
            .unwrap();
        app.run_slash_command(command);

        assert!(app.messages.is_empty());
        assert!(app.runtime.model_history().is_empty());
        assert_eq!(app.usage, ConversationUsage::default());
        assert_eq!(app.run_notice.as_deref(), Some("Deleted conversation."));
    }

    #[test]
    fn status_command_opens_status_view() {
        let mut app = app();
        app.input.set("/status");

        let command = SLASH_COMMANDS
            .iter()
            .find(|command| command.name == "/status")
            .copied()
            .unwrap();
        app.run_slash_command(command);

        assert!(app.status_view.is_some());
        assert_eq!(
            app.status_view.as_ref().map(|view| view.tab),
            Some(StatusTab::General)
        );
        assert_eq!(app.input.value, "/status");
    }

    #[test]
    fn status_view_switches_tabs_and_closes() {
        let mut app = app();
        app.status_view = Some(StatusView {
            tab: StatusTab::General,
            stats: WorkspaceUsageStats::default(),
            error: None,
            selected_task: 0,
        });
        app.input.set("/status");

        app.update_status_view_key(KeyAction::Right);
        assert_eq!(
            app.status_view.as_ref().map(|view| view.tab),
            Some(StatusTab::Usage)
        );

        app.update_status_view_key(KeyAction::Left);
        assert_eq!(
            app.status_view.as_ref().map(|view| view.tab),
            Some(StatusTab::General)
        );

        app.update_status_view_key(KeyAction::Escape);
        assert!(app.status_view.is_none());
        assert_eq!(app.input.value, "");
    }

    #[test]
    fn slash_menu_navigation_wraps_at_edges() {
        let mut app = app();
        app.input.set("/");

        app.update_slash_menu_key(KeyAction::Up);
        assert_eq!(app.slash_command_selection, SLASH_COMMANDS.len() - 1);

        app.update_slash_menu_key(KeyAction::Down);
        assert_eq!(app.slash_command_selection, 0);
    }

    #[test]
    fn compact_finished_records_boundary_and_notice() {
        let mut app = app();
        app.runtime
            .transcript_mut()
            .append_user("old user".to_owned())
            .unwrap();
        app.messages = app.runtime.ui_messages();
        app.status = AgentStatus::Compacting;
        app.turn_started_at = Some(Instant::now());

        let pending_prompt = app.finish_compact("important summary".to_owned(), Some(20));

        let history = app.runtime.model_history();
        assert_eq!(pending_prompt, None);
        assert_eq!(app.status, AgentStatus::Idle);
        assert_eq!(app.run_notice.as_deref(), Some("Compacted conversation."));
        assert!(
            app.messages
                .iter()
                .any(|message| message.content == "Compacted conversation")
        );
        assert_eq!(history.len(), 1);
        assert!(
            history[0]
                .content
                .as_deref()
                .is_some_and(|content| content.contains("important summary"))
        );
    }

    #[test]
    fn assistant_finished_records_last_turn_duration() {
        let mut app = app();
        app.status = AgentStatus::Responding;
        app.turn_started_at = Some(Instant::now() - Duration::from_secs(65));

        app.update_agent(AgentEvent::AssistantFinished);

        assert_eq!(app.status, AgentStatus::Idle);
        assert!(app.processing_elapsed().is_none());
        assert!(
            app.last_turn_duration()
                .is_some_and(|duration| duration >= Duration::from_secs(65))
        );

        app.start_turn_timer();

        assert!(app.last_turn_duration().is_none());
    }

    #[test]
    fn auto_compact_finished_records_auto_boundary_and_returns_pending_prompt() {
        let mut app = app();
        app.runtime
            .transcript_mut()
            .append_user("old user".to_owned())
            .unwrap();
        app.runtime
            .set_pending_prompt_after_compact(Some("new prompt".to_owned()));
        app.runtime.set_auto_compact_failures(2);
        app.status = AgentStatus::Compacting;

        let pending_prompt = app.finish_compact("auto summary".to_owned(), Some(900));

        assert_eq!(pending_prompt.as_deref(), Some("new prompt"));
        assert_eq!(app.runtime.auto_compact_failures(), 0);
        assert_eq!(
            app.run_notice.as_deref(),
            Some("Auto-compacted conversation.")
        );
        assert!(
            app.runtime
                .model_history()
                .first()
                .and_then(|message| message.content.as_deref())
                .is_some_and(|content| content.contains("auto summary"))
        );
    }

    #[test]
    fn auto_compact_failure_returns_pending_prompt_and_increments_failures() {
        let mut app = app();
        app.runtime
            .set_pending_prompt_after_compact(Some("new prompt".to_owned()));
        app.runtime.set_auto_compact_failures(2);
        app.status = AgentStatus::Compacting;

        let pending_prompt = app.fail_compact("network error".to_owned());

        assert_eq!(pending_prompt.as_deref(), Some("new prompt"));
        assert_eq!(app.runtime.auto_compact_failures(), 3);
        assert_eq!(
            app.run_notice.as_deref(),
            Some("Auto-compact failed; continuing without compact.")
        );
        assert_eq!(app.status, AgentStatus::Idle);
    }

    #[test]
    fn should_auto_compact_uses_latest_prompt_usage_and_failure_limit() {
        let mut app = app();
        app.config.llm.context_window = Some(1_000_000);
        app.config.llm.max_tokens = 8_196;
        app.usage = ConversationUsage::default().record(TokenUsage {
            prompt_tokens: 978_804,
            completion_tokens: 10,
            total_tokens: 978_814,
            cached_prompt_tokens: None,
        });

        assert!(should_auto_compact(
            &app.config.llm,
            app.usage.last_usage.map(|usage| usage.prompt_tokens),
            app.runtime.auto_compact_failures()
        ));

        app.runtime.set_auto_compact_failures(3);
        assert!(!should_auto_compact(
            &app.config.llm,
            app.usage.last_usage.map(|usage| usage.prompt_tokens),
            app.runtime.auto_compact_failures()
        ));
    }

    #[test]
    fn records_latest_usage_and_accumulates_totals() {
        let usage = ConversationUsage::default()
            .record(TokenUsage {
                prompt_tokens: 100,
                completion_tokens: 25,
                total_tokens: 125,
                cached_prompt_tokens: Some(40),
            })
            .record(TokenUsage {
                prompt_tokens: 200,
                completion_tokens: 50,
                total_tokens: 250,
                cached_prompt_tokens: Some(100),
            });

        assert_eq!(usage.total_prompt_tokens, 300);
        assert_eq!(usage.total_completion_tokens, 75);
        assert_eq!(usage.total_tokens, 375);
        assert_eq!(usage.last_usage.map(|last| last.prompt_tokens), Some(200));
    }

    #[test]
    fn computes_cache_percent_when_cached_tokens_are_reported() {
        let usage = ConversationUsage::default().record(TokenUsage {
            prompt_tokens: 300,
            completion_tokens: 25,
            total_tokens: 325,
            cached_prompt_tokens: Some(140),
        });

        assert_eq!(usage.cache_percent(), Some(46));
    }

    #[test]
    fn returns_no_cache_percent_without_cache_data_or_prompt_tokens() {
        let missing_cache = ConversationUsage::default().record(TokenUsage {
            prompt_tokens: 300,
            completion_tokens: 25,
            total_tokens: 325,
            cached_prompt_tokens: None,
        });
        let zero_prompt = ConversationUsage::default().record(TokenUsage {
            prompt_tokens: 0,
            completion_tokens: 25,
            total_tokens: 25,
            cached_prompt_tokens: Some(10),
        });

        assert_eq!(missing_cache.cache_percent(), None);
        assert_eq!(zero_prompt.cache_percent(), None);
    }

    #[test]
    fn caps_cache_percent_at_one_hundred() {
        let usage = ConversationUsage::default().record(TokenUsage {
            prompt_tokens: 100,
            completion_tokens: 25,
            total_tokens: 125,
            cached_prompt_tokens: Some(140),
        });

        assert_eq!(usage.cache_percent(), Some(100));
    }

    #[test]
    fn current_dir_label_uses_home_prefix() {
        let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
            return;
        };

        assert_eq!(home_relative_path(&home), "~");
        assert_eq!(
            home_relative_path(&home.join("projects/glint")),
            "~/projects/glint"
        );
    }
}
