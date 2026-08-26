use std::{
    path::{Path, PathBuf},
    sync::{Arc, mpsc::Receiver},
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};

use crate::{
    agent::{
        AgentEvent, AgentRunInput, AgentStatus, TokenUsage,
        provider::{FinishReason, ToolCall},
        spawn_subagent_loop,
    },
    approval::{AgentControl, ApprovalFocus, ApprovalPrompt},
    commands::{SlashCommand, SlashCommandKind, matching_slash_commands},
    config::Config,
    event::{
        AppEvent, ExtensionMouseAction, KeyAction, KeyInput, McpMouseAction, MouseAction,
        PluginsMouseAction, PluginsMouseTab, ResumeMouseAction,
    },
    input::InputState,
    message::{Message, Role},
    plugins::{PluginManager, PluginMutationResult},
    progress::TodoUpdate,
    runtime::{
        AssistantRecord, ConversationUsage, LoadedTranscript, RuntimeCommand, RuntimeEvent,
        SessionRuntime, StartPromptConfig, SubagentRuntimeEvent,
    },
    services::mcp::{
        McpApprovalPolicy, McpConfig, McpOAuthConfig, McpServerConfig, McpTransportConfig,
        persist_mcp_server,
    },
    tasks::{
        self, SubagentRequest, SubagentStartResponse, SubagentSteering, TaskRequest, TaskSnapshot,
    },
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
    pub mcp_view: Option<McpView>,
    pub plugins_view: Option<PluginsView>,
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
    pending_plugin_operation: Option<PendingPluginOperation>,
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

#[derive(Debug)]
pub struct McpView {
    pub selected: usize,
    pub detail_scroll: usize,
    pub detail_max_scroll: usize,
    pub focus: McpFocus,
    pub screen: McpScreen,
    pub notice: Option<McpNotice>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum McpFocus {
    Servers,
    Details,
}

#[derive(Debug)]
pub enum McpScreen {
    Browse,
    Details,
    Add(Box<McpAddForm>),
    OAuth {
        server: String,
        authorization_url: String,
        callback: InputState,
    },
    ConfirmLogout {
        server: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct McpNotice {
    pub message: String,
    pub failed: bool,
}

#[derive(Debug)]
pub struct McpAddForm {
    pub transport: McpAddTransport,
    pub focus: usize,
    pub name: InputState,
    pub command: InputState,
    pub arguments: InputState,
    pub working_directory: InputState,
    pub environment_variables: InputState,
    pub url: InputState,
    pub bearer_token_env: InputState,
    pub redirect_uri: InputState,
    pub scopes: InputState,
    pub approval: McpApprovalPolicy,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum McpAddTransport {
    Stdio,
    StreamableHttp,
    OAuth,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum McpAddField {
    Transport,
    Name,
    Command,
    Arguments,
    WorkingDirectory,
    EnvironmentVariables,
    Url,
    BearerTokenEnv,
    RedirectUri,
    Scopes,
    Approval,
}

impl Default for McpAddForm {
    fn default() -> Self {
        Self {
            transport: McpAddTransport::Stdio,
            focus: 0,
            name: InputState::default(),
            command: InputState::default(),
            arguments: InputState::default(),
            working_directory: InputState::default(),
            environment_variables: InputState::default(),
            url: InputState::default(),
            bearer_token_env: InputState::default(),
            redirect_uri: InputState::default(),
            scopes: InputState::default(),
            approval: McpApprovalPolicy::Prompt,
        }
    }
}

impl McpAddForm {
    pub fn fields(&self) -> &'static [McpAddField] {
        match self.transport {
            McpAddTransport::Stdio => &[
                McpAddField::Transport,
                McpAddField::Name,
                McpAddField::Command,
                McpAddField::Arguments,
                McpAddField::WorkingDirectory,
                McpAddField::EnvironmentVariables,
                McpAddField::Approval,
            ],
            McpAddTransport::StreamableHttp => &[
                McpAddField::Transport,
                McpAddField::Name,
                McpAddField::Url,
                McpAddField::BearerTokenEnv,
                McpAddField::Approval,
            ],
            McpAddTransport::OAuth => &[
                McpAddField::Transport,
                McpAddField::Name,
                McpAddField::Url,
                McpAddField::RedirectUri,
                McpAddField::Scopes,
                McpAddField::Approval,
            ],
        }
    }

    pub fn selected_field(&self) -> McpAddField {
        self.fields()[self.focus.min(self.fields().len() - 1)]
    }

    fn selected_input_mut(&mut self) -> Option<&mut InputState> {
        match self.selected_field() {
            McpAddField::Name => Some(&mut self.name),
            McpAddField::Command => Some(&mut self.command),
            McpAddField::Arguments => Some(&mut self.arguments),
            McpAddField::WorkingDirectory => Some(&mut self.working_directory),
            McpAddField::EnvironmentVariables => Some(&mut self.environment_variables),
            McpAddField::Url => Some(&mut self.url),
            McpAddField::BearerTokenEnv => Some(&mut self.bearer_token_env),
            McpAddField::RedirectUri => Some(&mut self.redirect_uri),
            McpAddField::Scopes => Some(&mut self.scopes),
            McpAddField::Transport | McpAddField::Approval => None,
        }
    }
}

impl McpAddTransport {
    fn previous(self) -> Self {
        match self {
            Self::Stdio => Self::OAuth,
            Self::StreamableHttp => Self::Stdio,
            Self::OAuth => Self::StreamableHttp,
        }
    }

    fn next(self) -> Self {
        match self {
            Self::Stdio => Self::StreamableHttp,
            Self::StreamableHttp => Self::OAuth,
            Self::OAuth => Self::Stdio,
        }
    }
}

#[derive(Debug)]
pub struct PluginsView {
    pub tab: PluginsTab,
    pub screen: PluginsScreen,
    pub selected_installed: usize,
    pub selected_marketplace: usize,
    pub detail_scroll: usize,
    pub detail_max_scroll: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PluginsTab {
    Installed,
    Marketplaces,
}

#[derive(Debug)]
pub enum PluginsScreen {
    Browse,
    InstalledDetail(usize),
    MarketplacePluginDetail(usize),
    AddMarketplace(InputState),
    Operation(PluginOperationView),
}

#[derive(Debug)]
pub struct PluginOperationView {
    pub title: String,
    pub subject: String,
    pub log: Vec<String>,
    pub uses_git: bool,
    pub finished: bool,
    pub failed: bool,
}

enum PluginUiMutation {
    AddMarketplace(String),
    Install(String),
    Uninstall(String),
}

enum PluginOperationEvent {
    Progress(String),
    Finished(Box<Result<PluginMutationResult, String>>),
}

struct PendingPluginOperation {
    events: Receiver<PluginOperationEvent>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MarketplaceSelection {
    Add,
    Marketplace(usize),
    Plugin(usize),
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

impl PluginsTab {
    fn previous(self) -> Self {
        match self {
            Self::Installed => Self::Marketplaces,
            Self::Marketplaces => Self::Installed,
        }
    }

    fn next(self) -> Self {
        match self {
            Self::Installed => Self::Marketplaces,
            Self::Marketplaces => Self::Installed,
        }
    }
}

impl App {
    pub fn new(config: Config) -> Result<Self> {
        let current_dir = current_dir_label();
        let transcript_cwd = std::env::current_dir()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|_| current_dir.clone());
        let runtime = SessionRuntime::create_new(
            transcript_cwd,
            config.lsp.clone(),
            config.mcp.clone(),
            config.extensions.hooks.clone(),
        )?;
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
            mcp_view: None,
            plugins_view: None,
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
            pending_plugin_operation: None,
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
            mcp_view: None,
            plugins_view: None,
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
                mcp: Default::default(),
                extensions: Default::default(),
                system_prompt: "system".to_owned(),
                plugins: Default::default(),
                base_lsp: LspConfig::default(),
                base_mcp: Default::default(),
                base_system_prompt: "system".to_owned(),
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
            pending_plugin_operation: None,
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
            AppEvent::ExtensionMouse(mouse) => self.update_extension_mouse(mouse),
            AppEvent::Agent(event) => self.update_agent(event),
        }
    }

    pub fn update_agent_events(&mut self) {
        while let Some(event) = self.runtime.try_recv_agent_event() {
            self.update(AppEvent::Agent(event));
        }
        self.drain_plugin_operation();
        if self.approval.is_none()
            && let Some(request) = self.runtime.try_recv_mcp_elicitation()
        {
            self.status = AgentStatus::AwaitingApproval;
            self.agent_activity = Some("MCP server is requesting input".to_owned());
            self.approval = Some(ApprovalPrompt::new(request));
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

    #[cfg(test)]
    pub(crate) fn test_start_subagent_task(&mut self, request: &SubagentRequest) {
        self.runtime.start_subagent_task(request, 0).unwrap();
    }

    pub fn pinned_progress(&self) -> Option<&TodoUpdate> {
        self.runtime.pinned_progress()
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
                TerminalRequest::CancelActive => {
                    for tab in &mut self.terminal_tabs {
                        tab.cancel_active();
                    }
                }
            }
        }

        while let Some(request) = self.runtime.try_recv_task_request() {
            match request {
                TaskRequest::StartSubagent { request, response } => {
                    self.start_subagent_terminal(request, response);
                }
                TaskRequest::List { response } => {
                    response.send(self.runtime.task_snapshots()).ok();
                }
                TaskRequest::Wait {
                    task_ids,
                    timeout,
                    response,
                } => {
                    self.runtime.register_task_wait(task_ids, timeout, response);
                }
                TaskRequest::Send {
                    task_id,
                    message,
                    response,
                } => {
                    let result = self.runtime.send_task_message(&task_id, message.clone());
                    if let Ok(task) = &result {
                        if let Some(tab) = task
                            .terminal_tab
                            .and_then(|terminal_tab| self.terminal_tabs.get_mut(terminal_tab))
                        {
                            tab.append_subagent_message(Message::user(message));
                        }
                        self.run_notice = Some(format!("Sent a message to task {}.", task.id));
                    }
                    response.send(result).ok();
                }
                TaskRequest::Cancel { task_id, response } => {
                    let result = self.runtime.cancel_task(&task_id);
                    if result.is_ok() {
                        self.run_notice = Some(format!("Cancelling task {task_id}."));
                    }
                    response.send(result).ok();
                }
            }
        }

        for tab in &mut self.terminal_tabs {
            tab.tick();
        }
        self.drain_subagent_events();
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
        mut request: SubagentRequest,
        response: std::sync::mpsc::Sender<SubagentStartResponse>,
    ) {
        if let Some(agent) = request.agent.as_deref() {
            match self.config.extensions.agent_prompt(agent, &request.prompt) {
                Ok(prompt) => request.prompt = prompt,
                Err(error) => {
                    response
                        .send(SubagentStartResponse::failed(
                            request.task_id,
                            format!("{error:#}"),
                        ))
                        .ok();
                    return;
                }
            }
        }
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
        let (control_tx, control_rx) = std::sync::mpsc::channel::<AgentControl>();
        let steering = Arc::new(SubagentSteering::default());
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
                active_progress: None,
                current_user_message: request.prompt,
                tool_results_dir: self.runtime.tool_results_dir(),
                terminal_requests: self.runtime.terminal_request_sender(),
                task_requests: self.runtime.task_request_sender(),
                shell_tool_mode: ShellToolMode::TerminalRun,
                read_file_state: self.runtime.read_file_state(),
                lsp_manager: self.runtime.lsp_manager(),
                dynamic_tools: self.runtime.dynamic_tools(),
                hook_runner: self.runtime.hook_runner(),
            },
            event_tx,
            outcome_tx,
            control_rx,
            Arc::clone(&steering),
        );
        self.runtime.attach_subagent_run(
            task.id.clone(),
            terminal_tab,
            event_rx,
            outcome_rx,
            control_tx,
            steering,
        );
        response
            .send(SubagentStartResponse::started(task.id, terminal_tab))
            .ok();
    }

    fn drain_subagent_events(&mut self) {
        for event in self.runtime.poll_subagent_events() {
            match event {
                SubagentRuntimeEvent::Agent {
                    terminal_tab,
                    event,
                } => self.update_subagent_tab_event(terminal_tab, event),
                SubagentRuntimeEvent::Finished { terminal_tab, task } => {
                    self.finish_subagent_result(terminal_tab, task);
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
            AgentEvent::TodoUpdated(_) => {}
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

    fn finish_subagent_result(&mut self, terminal_tab: usize, task: TaskSnapshot) {
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
        if action == KeyAction::Quit && self.pending_plugin_operation.is_some() {
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
        if self.mcp_view.is_some() {
            self.update_mcp_view_key(action);
            return;
        }
        if self.plugins_view.is_some() {
            self.update_plugins_view_key(action);
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
            || self.mcp_view.is_some()
            || self.plugins_view.is_some()
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
        matching_slash_commands(query, &self.config.extensions.commands)
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
                    self.run_slash_command(command.clone());
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

    fn run_slash_command(&mut self, command: impl Into<SlashCommand>) {
        let command = command.into();
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
            SlashCommandKind::Mcp => self.open_mcp_view(),
            SlashCommandKind::Plugins => self.open_plugins_view(),
            SlashCommandKind::PluginPrompt(index) => self.run_plugin_prompt(index),
            SlashCommandKind::ReloadPlugins => {
                let result = PluginManager::refresh(
                    &self.config.plugins,
                    self.config.base_mcp.clone(),
                    self.config.base_lsp.clone(),
                    &plugin_command_cwd(),
                );
                self.apply_plugin_mutation("/reload-plugins", result);
            }
        }
    }

    fn run_plugin_prompt(&mut self, index: usize) {
        let Some(command) = self.config.extensions.commands.get(index) else {
            return;
        };
        let prompt = command.expand("");
        self.input.set("");
        self.submit_prompt(prompt, true);
    }

    fn show_local_command(&mut self, command: &str, response: String) {
        self.input.set("");
        self.messages.push(Message::user(command));
        self.record_local_exchange(command.to_owned(), response.clone());
        self.messages.push(Message::assistant(response));
        self.scroll = 0;
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

    fn open_mcp_view(&mut self) {
        self.input.set("/mcp");
        self.mcp_view = Some(McpView {
            selected: usize::from(!self.runtime.mcp_statuses().is_empty()),
            detail_scroll: 0,
            detail_max_scroll: 0,
            focus: McpFocus::Servers,
            screen: McpScreen::Browse,
            notice: None,
        });
    }

    fn update_mcp_view_key(&mut self, key: KeyAction) {
        if matches!(
            self.mcp_view.as_ref().map(|view| &view.screen),
            Some(McpScreen::Add(_))
        ) {
            self.update_mcp_add_key(key);
            return;
        }
        if matches!(
            self.mcp_view.as_ref().map(|view| &view.screen),
            Some(McpScreen::OAuth { .. })
        ) {
            self.update_mcp_oauth_key(key);
            return;
        }
        if matches!(
            self.mcp_view.as_ref().map(|view| &view.screen),
            Some(McpScreen::ConfirmLogout { .. })
        ) {
            self.update_mcp_logout_key(key);
            return;
        }

        let details = matches!(
            self.mcp_view.as_ref().map(|view| &view.screen),
            Some(McpScreen::Details)
        );
        match key {
            KeyAction::Escape if details => self.show_mcp_browse(),
            KeyAction::Escape => self.close_mcp_view(),
            KeyAction::Submit if details => self.show_mcp_browse(),
            KeyAction::Submit => self.open_selected_mcp_item(),
            KeyAction::Tab if !details => self.toggle_mcp_focus(),
            KeyAction::Left if !details => self.set_mcp_focus(McpFocus::Servers),
            KeyAction::Right if !details => self.set_mcp_focus(McpFocus::Details),
            KeyAction::Up => self.move_or_scroll_mcp(-1),
            KeyAction::Down => self.move_or_scroll_mcp(1),
            KeyAction::Char('r' | 'R') => self.reconnect_selected_mcp(),
            KeyAction::Char('a' | 'A') => self.authorize_selected_mcp(),
            KeyAction::Char('l' | 'L') => self.request_mcp_logout(),
            _ => {}
        }
    }

    fn update_mcp_oauth_key(&mut self, key: KeyAction) {
        match key {
            KeyAction::Escape => {
                self.show_mcp_browse();
                return;
            }
            KeyAction::Submit => {
                self.complete_mcp_oauth();
                return;
            }
            _ => {}
        }

        let Some(McpScreen::OAuth { callback, .. }) =
            self.mcp_view.as_mut().map(|view| &mut view.screen)
        else {
            return;
        };
        match key {
            KeyAction::Char(char) => callback.push(char),
            KeyAction::Backspace => callback.backspace(),
            KeyAction::Delete => callback.delete_forward(),
            KeyAction::Left => callback.move_left(),
            KeyAction::Right => callback.move_right(),
            _ => {}
        }
    }

    fn update_mcp_add_key(&mut self, key: KeyAction) {
        if !matches!(key, KeyAction::Escape | KeyAction::Submit)
            && let Some(view) = self.mcp_view.as_mut()
        {
            view.notice = None;
        }
        match key {
            KeyAction::Escape => {
                self.show_mcp_browse();
                return;
            }
            KeyAction::Submit => {
                self.add_mcp_server();
                return;
            }
            KeyAction::Up => {
                if let Some(McpScreen::Add(form)) =
                    self.mcp_view.as_mut().map(|view| &mut view.screen)
                {
                    form.focus = form.focus.saturating_sub(1);
                }
                return;
            }
            KeyAction::Down | KeyAction::Tab => {
                if let Some(McpScreen::Add(form)) =
                    self.mcp_view.as_mut().map(|view| &mut view.screen)
                {
                    form.focus = (form.focus + 1).min(form.fields().len() - 1);
                }
                return;
            }
            _ => {}
        }

        let Some(McpScreen::Add(form)) = self.mcp_view.as_mut().map(|view| &mut view.screen) else {
            return;
        };
        match (form.selected_field(), key) {
            (McpAddField::Transport, KeyAction::Left) => {
                form.transport = form.transport.previous();
                form.focus = 0;
            }
            (McpAddField::Transport, KeyAction::Right | KeyAction::Char(' ')) => {
                form.transport = form.transport.next();
                form.focus = 0;
            }
            (McpAddField::Approval, KeyAction::Left) => {
                form.approval = previous_mcp_approval(form.approval)
            }
            (McpAddField::Approval, KeyAction::Right | KeyAction::Char(' ')) => {
                form.approval = next_mcp_approval(form.approval)
            }
            (_, KeyAction::Char(char)) => {
                if let Some(input) = form.selected_input_mut() {
                    input.push(char);
                }
            }
            (_, KeyAction::Backspace) => {
                if let Some(input) = form.selected_input_mut() {
                    input.backspace();
                }
            }
            (_, KeyAction::Delete) => {
                if let Some(input) = form.selected_input_mut() {
                    input.delete_forward();
                }
            }
            (_, KeyAction::Left) => {
                if let Some(input) = form.selected_input_mut() {
                    input.move_left();
                }
            }
            (_, KeyAction::Right) => {
                if let Some(input) = form.selected_input_mut() {
                    input.move_right();
                }
            }
            _ => {}
        }
    }

    fn update_mcp_logout_key(&mut self, key: KeyAction) {
        match key {
            KeyAction::Submit | KeyAction::Char('y' | 'Y') => self.confirm_mcp_logout(),
            KeyAction::Escape | KeyAction::Char('n' | 'N') => self.show_mcp_browse(),
            _ => {}
        }
    }

    fn close_mcp_view(&mut self) {
        self.mcp_view = None;
        self.input.set("");
    }

    fn show_mcp_browse(&mut self) {
        if let Some(view) = self.mcp_view.as_mut() {
            view.screen = McpScreen::Browse;
            view.detail_scroll = 0;
        }
    }

    fn toggle_mcp_focus(&mut self) {
        let Some(view) = self.mcp_view.as_mut() else {
            return;
        };
        view.focus = match view.focus {
            McpFocus::Servers => McpFocus::Details,
            McpFocus::Details => McpFocus::Servers,
        };
    }

    fn open_selected_mcp_item(&mut self) {
        let selected = self
            .mcp_view
            .as_ref()
            .map(|view| view.selected)
            .unwrap_or_default();
        if selected == 0 {
            if let Some(view) = self.mcp_view.as_mut() {
                view.screen = McpScreen::Add(Box::default());
                view.notice = None;
            }
        } else if self.runtime.mcp_statuses().get(selected - 1).is_some()
            && let Some(view) = self.mcp_view.as_mut()
        {
            view.screen = McpScreen::Details;
            view.focus = McpFocus::Details;
            view.detail_scroll = 0;
            view.notice = None;
        }
    }

    fn set_mcp_focus(&mut self, focus: McpFocus) {
        if let Some(view) = self.mcp_view.as_mut() {
            view.focus = focus;
        }
    }

    fn move_or_scroll_mcp(&mut self, direction: isize) {
        let row_count = self.runtime.mcp_statuses().len() + 1;
        let Some(view) = self.mcp_view.as_mut() else {
            return;
        };
        let selecting = matches!(view.screen, McpScreen::Browse) && view.focus == McpFocus::Servers;
        if selecting {
            view.selected = move_index(view.selected, direction, row_count);
            view.detail_scroll = 0;
            view.notice = None;
        } else {
            view.detail_scroll = view
                .detail_scroll
                .saturating_add_signed(direction)
                .min(view.detail_max_scroll);
        }
    }

    fn selected_mcp_server(&self) -> Option<String> {
        let view = self.mcp_view.as_ref()?;
        let index = view.selected.checked_sub(1)?;
        self.runtime
            .mcp_statuses()
            .get(index)
            .map(|status| status.name.clone())
    }

    fn add_mcp_server(&mut self) {
        self.add_mcp_server_at(&plugin_command_cwd().join("config.yaml"));
    }

    fn add_mcp_server_at(&mut self, config_path: &Path) {
        let result = self
            .mcp_view
            .as_ref()
            .and_then(|view| match &view.screen {
                McpScreen::Add(form) => Some(mcp_server_from_form(form)),
                _ => None,
            })
            .unwrap_or_else(|| bail!("MCP add form is not open"))
            .and_then(|(name, server)| {
                if self.config.mcp.servers.contains_key(&name) {
                    bail!("MCP server '{name}' already exists");
                }
                persist_mcp_server(config_path, &name, &server)?;
                Ok((name, server))
            });

        match result {
            Ok((name, server)) => {
                self.config
                    .base_mcp
                    .servers
                    .insert(name.clone(), server.clone());
                self.config.mcp.servers.insert(name.clone(), server);
                self.runtime.reload_mcp(self.config.mcp.clone());
                let selected = self
                    .runtime
                    .mcp_statuses()
                    .iter()
                    .position(|status| status.name == name)
                    .map(|index| index + 1)
                    .unwrap_or_default();
                if let Some(view) = self.mcp_view.as_mut() {
                    view.selected = selected;
                    view.detail_scroll = 0;
                    view.focus = McpFocus::Servers;
                    view.screen = McpScreen::Browse;
                }
                self.set_mcp_notice(
                    format!("Added MCP server '{name}' and saved it to config.yaml."),
                    false,
                );
            }
            Err(error) => self.set_mcp_notice(format!("Could not add MCP server: {error:#}"), true),
        }
    }

    fn reconnect_selected_mcp(&mut self) {
        let Some(server) = self.selected_mcp_server() else {
            return;
        };
        if self
            .config
            .mcp
            .servers
            .get(&server)
            .is_some_and(|config| !config.enabled)
        {
            self.set_mcp_notice(
                "This server is disabled by configuration; enable it in config.yaml or its plugin.",
                true,
            );
            return;
        }
        match self.runtime.reconnect_mcp(&server) {
            Ok(()) => self.set_mcp_notice(format!("Reconnected MCP server '{server}'."), false),
            Err(error) => {
                self.set_mcp_notice(format!("Could not reconnect '{server}': {error:#}"), true)
            }
        }
    }

    fn authorize_selected_mcp(&mut self) {
        let Some(server) = self.selected_mcp_server() else {
            return;
        };
        match self.runtime.begin_mcp_oauth(&server) {
            Ok(authorization_url) => {
                if let Some(view) = self.mcp_view.as_mut() {
                    view.screen = McpScreen::OAuth {
                        server,
                        authorization_url,
                        callback: InputState::default(),
                    };
                    view.notice = None;
                }
            }
            Err(error) => {
                self.set_mcp_notice(format!("Could not authorize '{server}': {error:#}"), true)
            }
        }
    }

    fn complete_mcp_oauth(&mut self) {
        let Some((server, callback_url)) = self.mcp_view.as_ref().and_then(|view| {
            if let McpScreen::OAuth {
                server, callback, ..
            } = &view.screen
            {
                Some((server.clone(), callback.value.trim().to_owned()))
            } else {
                None
            }
        }) else {
            return;
        };
        if callback_url.is_empty() {
            self.set_mcp_notice("Paste the complete callback URL before continuing.", true);
            return;
        }
        match self.runtime.complete_mcp_oauth(&server, &callback_url) {
            Ok(()) => {
                self.show_mcp_browse();
                self.set_mcp_notice(format!("Authorized MCP server '{server}'."), false);
            }
            Err(error) => self.set_mcp_notice(
                format!("Could not complete authorization for '{server}': {error:#}"),
                true,
            ),
        }
    }

    fn request_mcp_logout(&mut self) {
        let Some(server) = self.selected_mcp_server() else {
            return;
        };
        if let Some(view) = self.mcp_view.as_mut() {
            view.screen = McpScreen::ConfirmLogout { server };
            view.notice = None;
        }
    }

    fn confirm_mcp_logout(&mut self) {
        let Some(server) = self.mcp_view.as_ref().and_then(|view| {
            if let McpScreen::ConfirmLogout { server } = &view.screen {
                Some(server.clone())
            } else {
                None
            }
        }) else {
            return;
        };
        match self.runtime.logout_mcp_oauth(&server) {
            Ok(()) => {
                self.show_mcp_browse();
                self.set_mcp_notice(format!("Logged out of MCP server '{server}'."), false);
            }
            Err(error) => {
                self.show_mcp_browse();
                self.set_mcp_notice(format!("Could not log out of '{server}': {error:#}"), true);
            }
        }
    }

    fn set_mcp_notice(&mut self, message: impl Into<String>, failed: bool) {
        if let Some(view) = self.mcp_view.as_mut() {
            view.notice = Some(McpNotice {
                message: message.into(),
                failed,
            });
        }
    }

    pub(crate) fn mcp_statuses(&self) -> Vec<crate::services::mcp::McpServerStatus> {
        self.runtime.mcp_statuses()
    }

    pub fn set_mcp_detail_max_scroll(&mut self, max_scroll: usize) {
        if let Some(view) = self.mcp_view.as_mut() {
            view.detail_max_scroll = max_scroll;
            view.detail_scroll = view.detail_scroll.min(max_scroll);
        }
    }

    pub fn set_plugins_detail_max_scroll(&mut self, max_scroll: usize) {
        if let Some(view) = self.plugins_view.as_mut() {
            view.detail_max_scroll = max_scroll;
            view.detail_scroll = view.detail_scroll.min(max_scroll);
        }
    }

    #[cfg(test)]
    pub(crate) fn reload_mcp_for_test(&mut self, config: crate::services::mcp::McpConfig) {
        self.config.mcp = config.clone();
        self.config.base_mcp = config.clone();
        self.runtime
            .reload_extensions(Default::default(), config, Vec::new());
    }

    fn open_plugins_view(&mut self) {
        self.input.set("/plugins");
        self.plugins_view = Some(PluginsView {
            tab: PluginsTab::Installed,
            screen: PluginsScreen::Browse,
            selected_installed: 0,
            selected_marketplace: 0,
            detail_scroll: 0,
            detail_max_scroll: 0,
        });
    }

    fn update_plugins_view_key(&mut self, key: KeyAction) {
        if matches!(
            self.plugins_view.as_ref().map(|view| &view.screen),
            Some(PluginsScreen::AddMarketplace(_))
        ) {
            self.update_marketplace_input_key(key);
            return;
        }

        if matches!(
            self.plugins_view.as_ref().map(|view| &view.screen),
            Some(PluginsScreen::Operation(_))
        ) {
            if key == KeyAction::Escape || key == KeyAction::Submit {
                let finished = self
                    .plugins_view
                    .as_ref()
                    .and_then(|view| match &view.screen {
                        PluginsScreen::Operation(operation) => Some(operation.finished),
                        _ => None,
                    })
                    .unwrap_or(false);
                if finished && let Some(view) = self.plugins_view.as_mut() {
                    view.screen = PluginsScreen::Browse;
                    view.detail_scroll = 0;
                }
            }
            return;
        }

        if !matches!(
            self.plugins_view.as_ref().map(|view| &view.screen),
            Some(PluginsScreen::Browse)
        ) {
            match key {
                KeyAction::Escape | KeyAction::Submit => {
                    if let Some(view) = self.plugins_view.as_mut() {
                        view.screen = PluginsScreen::Browse;
                        view.detail_scroll = 0;
                    }
                }
                KeyAction::Char(' ') => self.toggle_selected_plugin(),
                _ => {}
            }
            return;
        }

        match key {
            KeyAction::Escape => self.close_plugins_view(),
            KeyAction::Left => self.move_plugins_tab(-1),
            KeyAction::Right | KeyAction::Tab => self.move_plugins_tab(1),
            KeyAction::Up => self.move_plugins_selection(-1),
            KeyAction::Down => self.move_plugins_selection(1),
            KeyAction::Submit => self.open_selected_plugin_item(),
            KeyAction::Char(' ') => self.toggle_selected_plugin(),
            _ => {}
        }
    }

    fn update_marketplace_input_key(&mut self, key: KeyAction) {
        let source = match key {
            KeyAction::Submit => {
                self.plugins_view
                    .as_mut()
                    .and_then(|view| match &mut view.screen {
                        PluginsScreen::AddMarketplace(input) => Some(input.take_trimmed()),
                        _ => None,
                    })
            }
            KeyAction::Escape => {
                if let Some(view) = self.plugins_view.as_mut() {
                    view.screen = PluginsScreen::Browse;
                    view.detail_scroll = 0;
                }
                return;
            }
            KeyAction::Char(character) => {
                if let Some(PluginsScreen::AddMarketplace(input)) =
                    self.plugins_view.as_mut().map(|view| &mut view.screen)
                {
                    input.push(character);
                }
                return;
            }
            KeyAction::Backspace => {
                if let Some(PluginsScreen::AddMarketplace(input)) =
                    self.plugins_view.as_mut().map(|view| &mut view.screen)
                {
                    input.backspace();
                }
                return;
            }
            KeyAction::Delete => {
                if let Some(PluginsScreen::AddMarketplace(input)) =
                    self.plugins_view.as_mut().map(|view| &mut view.screen)
                {
                    input.delete_forward();
                }
                return;
            }
            KeyAction::Left | KeyAction::Right => {
                if let Some(PluginsScreen::AddMarketplace(input)) =
                    self.plugins_view.as_mut().map(|view| &mut view.screen)
                {
                    if key == KeyAction::Left {
                        input.move_left();
                    } else {
                        input.move_right();
                    }
                }
                return;
            }
            _ => return,
        };
        if let Some(source) = source.filter(|source| !source.is_empty()) {
            self.start_plugin_ui_operation(
                "Adding marketplace".to_owned(),
                source.clone(),
                PluginUiMutation::AddMarketplace(source),
            );
        }
    }

    fn close_plugins_view(&mut self) {
        if self.pending_plugin_operation.is_some() {
            return;
        }
        self.plugins_view = None;
        self.input.set("");
    }

    fn move_plugins_tab(&mut self, direction: isize) {
        let Some(view) = self.plugins_view.as_mut() else {
            return;
        };
        view.tab = if direction < 0 {
            view.tab.previous()
        } else {
            view.tab.next()
        };
        view.detail_scroll = 0;
    }

    fn move_plugins_selection(&mut self, direction: isize) {
        let marketplace_count = self.marketplace_selection_count();
        let installed_count = self.config.extensions.installed_plugins.len();
        let Some(view) = self.plugins_view.as_mut() else {
            return;
        };
        match view.tab {
            PluginsTab::Installed => {
                view.selected_installed =
                    move_index(view.selected_installed, direction, installed_count);
            }
            PluginsTab::Marketplaces => {
                view.selected_marketplace =
                    move_index(view.selected_marketplace, direction, marketplace_count);
            }
        }
        view.detail_scroll = 0;
    }

    fn open_selected_plugin_item(&mut self) {
        let Some(view) = self.plugins_view.as_ref() else {
            return;
        };
        match view.tab {
            PluginsTab::Installed => {
                if view.selected_installed < self.config.extensions.installed_plugins.len()
                    && let Some(view) = self.plugins_view.as_mut()
                {
                    view.screen = PluginsScreen::InstalledDetail(view.selected_installed);
                    view.detail_scroll = 0;
                }
            }
            PluginsTab::Marketplaces => {
                match self.marketplace_selection_at(view.selected_marketplace) {
                    Some(MarketplaceSelection::Add) => {
                        if let Some(view) = self.plugins_view.as_mut() {
                            view.screen = PluginsScreen::AddMarketplace(InputState::default());
                            view.detail_scroll = 0;
                        }
                    }
                    Some(MarketplaceSelection::Plugin(index)) => {
                        if let Some(view) = self.plugins_view.as_mut() {
                            view.screen = PluginsScreen::MarketplacePluginDetail(index);
                            view.detail_scroll = 0;
                        }
                    }
                    Some(MarketplaceSelection::Marketplace(_)) | None => {}
                }
            }
        }
    }

    fn toggle_selected_plugin(&mut self) {
        let Some(view) = self.plugins_view.as_ref() else {
            return;
        };
        match view.tab {
            PluginsTab::Installed => {
                let Some(plugin) = self
                    .config
                    .extensions
                    .installed_plugins
                    .get(view.selected_installed)
                else {
                    return;
                };
                if plugin.config_managed {
                    self.show_plugin_ui_error(
                        "Config-managed plugins must be enabled or disabled in config.yaml.",
                    );
                    return;
                }
                let spec = plugin.spec();
                let enabled = !plugin.enabled;
                self.set_plugin_enabled(spec, enabled);
            }
            PluginsTab::Marketplaces => {
                let Some(MarketplaceSelection::Plugin(index)) =
                    self.marketplace_selection_at(view.selected_marketplace)
                else {
                    return;
                };
                let Some(plugin) = self.config.extensions.marketplace_plugins.get(index) else {
                    return;
                };
                let spec = format!("{}@{}", plugin.name, plugin.marketplace);
                let (title, mutation) = if plugin.installed {
                    (
                        "Uninstalling plugin".to_owned(),
                        PluginUiMutation::Uninstall(spec.clone()),
                    )
                } else {
                    (
                        "Installing plugin".to_owned(),
                        PluginUiMutation::Install(spec.clone()),
                    )
                };
                self.start_plugin_ui_operation(title, spec, mutation);
            }
        }
    }

    fn marketplace_selection_count(&self) -> usize {
        1 + self.config.extensions.marketplaces.len()
            + self.config.extensions.marketplace_plugins.len()
    }

    fn marketplace_selection_at(&self, selected: usize) -> Option<MarketplaceSelection> {
        if selected == 0 {
            return Some(MarketplaceSelection::Add);
        }
        let mut row = 1;
        for (marketplace_index, marketplace) in
            self.config.extensions.marketplaces.iter().enumerate()
        {
            if selected == row {
                return Some(MarketplaceSelection::Marketplace(marketplace_index));
            }
            row += 1;
            for (plugin_index, _plugin) in self
                .config
                .extensions
                .marketplace_plugins
                .iter()
                .enumerate()
                .filter(|(_, plugin)| plugin.marketplace == marketplace.name)
            {
                if selected == row {
                    return Some(MarketplaceSelection::Plugin(plugin_index));
                }
                row += 1;
            }
        }
        None
    }

    fn show_plugin_ui_error(&mut self, message: &str) {
        if let Some(view) = self.plugins_view.as_mut() {
            view.screen = PluginsScreen::Operation(PluginOperationView {
                title: "Plugin operation".to_owned(),
                subject: String::new(),
                log: vec![message.to_owned()],
                uses_git: false,
                finished: true,
                failed: true,
            });
        }
    }

    fn set_plugin_enabled(&mut self, spec: String, enabled: bool) {
        let cwd = plugin_command_cwd();
        match PluginManager::set_enabled(
            &self.config.plugins,
            self.config.base_mcp.clone(),
            self.config.base_lsp.clone(),
            &cwd,
            &spec,
            enabled,
        ) {
            Ok(result) => self.activate_plugin_mutation(result),
            Err(error) => self.show_plugin_ui_error(&format!("{error:#}")),
        }
    }

    fn activate_plugin_mutation(&mut self, result: PluginMutationResult) {
        self.runtime.reload_extensions(
            result.load.lsp.clone(),
            result.load.mcp.clone(),
            result.load.catalog.hooks.clone(),
        );
        self.config.apply_plugin_load(result.load);
        self.clamp_plugins_view_selection();
    }

    fn start_plugin_ui_operation(
        &mut self,
        title: String,
        subject: String,
        mutation: PluginUiMutation,
    ) {
        if self.pending_plugin_operation.is_some() {
            return;
        }
        let (sender, events) = std::sync::mpsc::sync_channel(512);
        let plugins = self.config.plugins.clone();
        let mcp = self.config.base_mcp.clone();
        let lsp = self.config.base_lsp.clone();
        let cwd = plugin_command_cwd();
        if let Some(view) = self.plugins_view.as_mut() {
            view.screen = PluginsScreen::Operation(PluginOperationView {
                title,
                subject,
                log: vec!["Preparing plugin operation...".to_owned()],
                uses_git: true,
                finished: false,
                failed: false,
            });
        }
        std::thread::spawn(move || {
            let progress_sender = sender.clone();
            let reporter = std::sync::Arc::new(move |message: String| {
                progress_sender
                    .send(PluginOperationEvent::Progress(message))
                    .ok();
            });
            let result = PluginManager::with_progress(reporter, || match mutation {
                PluginUiMutation::AddMarketplace(source) => {
                    sender
                        .send(PluginOperationEvent::Progress(
                            "Resolving marketplace source and downloading Git data...".to_owned(),
                        ))
                        .ok();
                    PluginManager::add_marketplace(&plugins, mcp, lsp, &cwd, &source)
                }
                PluginUiMutation::Install(spec) => {
                    sender
                        .send(PluginOperationEvent::Progress(
                            "Resolving plugin source and downloading Git data...".to_owned(),
                        ))
                        .ok();
                    PluginManager::install(&plugins, mcp, lsp, &cwd, &spec)
                }
                PluginUiMutation::Uninstall(spec) => {
                    sender
                        .send(PluginOperationEvent::Progress(
                            "Removing plugin registration...".to_owned(),
                        ))
                        .ok();
                    PluginManager::uninstall(&plugins, mcp, lsp, &cwd, &spec)
                }
            });
            sender
                .send(PluginOperationEvent::Finished(Box::new(
                    result.map_err(|error| format!("{error:#}")),
                )))
                .ok();
        });
        self.pending_plugin_operation = Some(PendingPluginOperation { events });
    }

    fn drain_plugin_operation(&mut self) {
        let mut events = Vec::new();
        let mut disconnected = false;
        if let Some(operation) = self.pending_plugin_operation.as_ref() {
            loop {
                match operation.events.try_recv() {
                    Ok(event) => events.push(event),
                    Err(std::sync::mpsc::TryRecvError::Empty) => break,
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        disconnected = true;
                        break;
                    }
                }
            }
        }
        let finished = events
            .iter()
            .any(|event| matches!(event, PluginOperationEvent::Finished(_)));
        for event in events {
            self.handle_plugin_operation_event(event);
        }
        if disconnected && !finished && self.pending_plugin_operation.is_some() {
            self.handle_plugin_operation_event(PluginOperationEvent::Finished(Box::new(Err(
                "plugin operation channel closed before completion".to_owned(),
            ))));
        }
        if finished || disconnected {
            self.pending_plugin_operation = None;
        }
    }

    fn handle_plugin_operation_event(&mut self, event: PluginOperationEvent) {
        match event {
            PluginOperationEvent::Progress(message) => {
                if let Some(PluginsScreen::Operation(operation)) =
                    self.plugins_view.as_mut().map(|view| &mut view.screen)
                {
                    operation.log.push(message);
                    if operation.log.len() > 500 {
                        operation.log.drain(..operation.log.len() - 500);
                    }
                }
            }
            PluginOperationEvent::Finished(result) => match *result {
                Ok(result) => {
                    let message = result.message.clone();
                    self.activate_plugin_mutation(result);
                    if let Some(PluginsScreen::Operation(operation)) =
                        self.plugins_view.as_mut().map(|view| &mut view.screen)
                    {
                        operation.log.push(message);
                        operation
                            .log
                            .push("Changes are active in this session.".to_owned());
                        operation.finished = true;
                    }
                }
                Err(error) => {
                    if let Some(PluginsScreen::Operation(operation)) =
                        self.plugins_view.as_mut().map(|view| &mut view.screen)
                    {
                        operation.log.push(format!("Error: {error}"));
                        operation.finished = true;
                        operation.failed = true;
                    }
                }
            },
        }
    }

    fn clamp_plugins_view_selection(&mut self) {
        let installed_count = self.config.extensions.installed_plugins.len();
        let marketplace_count = self.marketplace_selection_count();
        if let Some(view) = self.plugins_view.as_mut() {
            view.selected_installed = view
                .selected_installed
                .min(installed_count.saturating_sub(1));
            view.selected_marketplace = view
                .selected_marketplace
                .min(marketplace_count.saturating_sub(1));
            view.detail_scroll = 0;
        }
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
        let input = if approval.request.elicitation_schema().is_some()
            && approval.selected == crate::approval::ApprovalChoice::Yes
        {
            let value = if approval.feedback.value.trim().is_empty() {
                serde_json::json!({})
            } else {
                match serde_json::from_str(&approval.feedback.value) {
                    Ok(value) => value,
                    Err(error) => {
                        self.run_notice = Some(format!("Invalid MCP response JSON: {error}"));
                        self.approval = Some(approval);
                        return;
                    }
                }
            };
            if !value.is_object() {
                self.run_notice = Some("MCP response JSON must be an object.".to_owned());
                self.approval = Some(approval);
                return;
            }
            Some(value)
        } else {
            None
        };
        let decision = approval.decision();
        self.runtime
            .handle_command(RuntimeCommand::ApprovalDecision {
                id: approval.request.id,
                decision,
                input,
            });
        self.status = AgentStatus::Responding;
    }

    fn clear_conversation_edit_permission(&mut self) {
        self.runtime
            .handle_command(RuntimeCommand::ClearConversationEditPermission);
    }

    fn update_extension_mouse(&mut self, mouse: ExtensionMouseAction) {
        match mouse {
            ExtensionMouseAction::Resume(action) => self.update_resume_mouse(action),
            ExtensionMouseAction::Mcp(action) => self.update_mcp_mouse(action),
            ExtensionMouseAction::Plugins(action) => self.update_plugins_mouse(action),
        }
    }

    fn update_resume_mouse(&mut self, action: ResumeMouseAction) {
        match action {
            ResumeMouseAction::SelectSession(selected) => {
                if let Some(picker) = self.resume_picker.as_mut()
                    && selected < picker.sessions.len()
                {
                    picker.selected = selected;
                }
            }
            ResumeMouseAction::MoveSelection(direction) => self.move_resume_picker(direction),
            ResumeMouseAction::None => {}
        }
    }

    fn update_mcp_mouse(&mut self, action: McpMouseAction) {
        match action {
            McpMouseAction::SelectServer(selected) => {
                let row_count = self.runtime.mcp_statuses().len() + 1;
                if let Some(view) = self.mcp_view.as_mut()
                    && matches!(view.screen, McpScreen::Browse)
                    && selected < row_count
                {
                    view.selected = selected;
                    view.focus = McpFocus::Servers;
                    view.detail_scroll = 0;
                    view.notice = None;
                }
            }
            McpMouseAction::OpenSelected => self.open_selected_mcp_item(),
            McpMouseAction::MoveServerSelection(direction) => {
                if let Some(view) = self.mcp_view.as_mut()
                    && matches!(view.screen, McpScreen::Browse)
                {
                    view.focus = McpFocus::Servers;
                }
                self.move_or_scroll_mcp(direction);
            }
            McpMouseAction::ScrollDetails(direction) => {
                if let Some(view) = self.mcp_view.as_mut() {
                    if matches!(view.screen, McpScreen::Browse) {
                        if view.selected == 0 {
                            return;
                        }
                        view.focus = McpFocus::Details;
                    }
                    view.detail_scroll = view
                        .detail_scroll
                        .saturating_add_signed(direction)
                        .min(view.detail_max_scroll);
                }
            }
            McpMouseAction::None => {}
        }
    }

    fn update_plugins_mouse(&mut self, action: PluginsMouseAction) {
        match action {
            PluginsMouseAction::SelectTab(tab) => {
                if let Some(view) = self.plugins_view.as_mut()
                    && matches!(
                        view.screen,
                        PluginsScreen::Browse
                            | PluginsScreen::InstalledDetail(_)
                            | PluginsScreen::MarketplacePluginDetail(_)
                    )
                {
                    view.tab = match tab {
                        PluginsMouseTab::Installed => PluginsTab::Installed,
                        PluginsMouseTab::Marketplaces => PluginsTab::Marketplaces,
                    };
                    view.screen = PluginsScreen::Browse;
                    view.detail_scroll = 0;
                }
            }
            PluginsMouseAction::SelectItem(selected) => {
                let installed_count = self.config.extensions.installed_plugins.len();
                let marketplace_count = self.marketplace_selection_count();
                if let Some(view) = self.plugins_view.as_mut()
                    && matches!(view.screen, PluginsScreen::Browse)
                {
                    match view.tab {
                        PluginsTab::Installed if selected < installed_count => {
                            view.selected_installed = selected
                        }
                        PluginsTab::Marketplaces if selected < marketplace_count => {
                            view.selected_marketplace = selected
                        }
                        _ => return,
                    }
                    view.detail_scroll = 0;
                }
            }
            PluginsMouseAction::MoveSelection(direction) => self.move_plugins_selection(direction),
            PluginsMouseAction::OpenSelected => self.open_selected_plugin_item(),
            PluginsMouseAction::ScrollDetails(direction) => {
                if let Some(view) = self.plugins_view.as_mut() {
                    view.detail_scroll = view
                        .detail_scroll
                        .saturating_add_signed(direction)
                        .min(view.detail_max_scroll);
                }
            }
            PluginsMouseAction::None => {}
        }
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
            && self.plugins_view.is_none()
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
        if self.run_plugin_manager_command(&prompt) {
            return;
        }
        if let Some((name, arguments)) = prompt
            .strip_prefix('/')
            .and_then(|command| command.split_once(char::is_whitespace))
            && let Some(command) = self
                .config
                .extensions
                .commands
                .iter()
                .find(|command| command.name == name)
        {
            self.submit_prompt(command.expand(arguments.trim()), true);
            return;
        }
        if let Some(server) = prompt.strip_prefix("/mcp reconnect ").map(str::trim) {
            let response = match self.runtime.reconnect_mcp(server) {
                Ok(()) => format!(
                    "Reconnected MCP server `{server}`.\n\n{}",
                    self.runtime.mcp_status_text()
                ),
                Err(error) => format!("Failed to reconnect MCP server `{server}`: {error:#}"),
            };
            self.show_local_command(&prompt, response);
            return;
        }
        if let Some(server) = prompt.strip_prefix("/mcp auth ").map(str::trim) {
            let response = if server.is_empty() {
                "Usage: /mcp auth <server>".to_owned()
            } else {
                match self.runtime.begin_mcp_oauth(server) {
                    Ok(url) => format!(
                        "OAuth authorization started for `{server}`. Open this URL:\n\n{url}\n\nAfter authorization, copy the complete redirected URL and run:\n`/mcp auth-callback {server} <redirected-url>`"
                    ),
                    Err(error) => {
                        format!("Failed to start OAuth for MCP server `{server}`: {error:#}")
                    }
                }
            };
            self.show_local_command(&prompt, response);
            return;
        }
        if let Some(arguments) = prompt.strip_prefix("/mcp auth-callback ").map(str::trim) {
            let response = match arguments.split_once(char::is_whitespace) {
                Some((server, callback_url)) => {
                    match self.runtime.complete_mcp_oauth(server, callback_url.trim()) {
                        Ok(()) => format!(
                            "Authorized MCP server `{server}`.\n\n{}",
                            self.runtime.mcp_status_text()
                        ),
                        Err(error) => {
                            format!("Failed to complete OAuth for MCP server `{server}`: {error:#}")
                        }
                    }
                }
                None => "Usage: /mcp auth-callback <server> <redirected-url>".to_owned(),
            };
            self.show_local_command(&prompt, response);
            return;
        }
        if let Some(server) = prompt.strip_prefix("/mcp logout ").map(str::trim) {
            let response = if server.is_empty() {
                "Usage: /mcp logout <server>".to_owned()
            } else {
                match self.runtime.logout_mcp_oauth(server) {
                    Ok(()) => format!("Cleared OAuth credentials for MCP server `{server}`."),
                    Err(error) => {
                        format!("Failed to clear OAuth for MCP server `{server}`: {error:#}")
                    }
                }
            };
            self.show_local_command(&prompt, response);
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

    fn run_plugin_manager_command(&mut self, prompt: &str) -> bool {
        if prompt == "/reload-plugins" || prompt == "/plugins reload" {
            let result = PluginManager::refresh(
                &self.config.plugins,
                self.config.base_mcp.clone(),
                self.config.base_lsp.clone(),
                &plugin_command_cwd(),
            );
            self.apply_plugin_mutation(prompt, result);
            return true;
        }
        if prompt == "/plugins marketplace list" {
            self.show_local_command(prompt, self.config.extensions.plugin_status());
            return true;
        }
        if prompt == "/plugins marketplace update"
            || prompt.starts_with("/plugins marketplace update ")
        {
            let result = PluginManager::refresh(
                &self.config.plugins,
                self.config.base_mcp.clone(),
                self.config.base_lsp.clone(),
                &plugin_command_cwd(),
            );
            self.apply_plugin_mutation(prompt, result);
            return true;
        }
        let operation = [
            "/plugins marketplace add ",
            "/plugins marketplace remove ",
            "/plugins install ",
            "/plugins uninstall ",
            "/plugins enable ",
            "/plugins disable ",
        ]
        .into_iter()
        .find_map(|prefix| {
            prompt
                .strip_prefix(prefix)
                .map(|value| (prefix, value.trim()))
        });
        let Some((operation, argument)) = operation else {
            if prompt.starts_with("/plugins ") {
                self.show_local_command(
                    prompt,
                    "Usage: /plugins marketplace add|update|remove, /plugins install|uninstall|enable|disable, or /reload-plugins"
                        .to_owned(),
                );
                return true;
            }
            return false;
        };
        if argument.is_empty() {
            self.show_local_command(
                prompt,
                format!("Missing argument for `{}`", operation.trim()),
            );
            return true;
        }
        let cwd = plugin_command_cwd();
        let result = match operation {
            "/plugins marketplace add " => PluginManager::add_marketplace(
                &self.config.plugins,
                self.config.base_mcp.clone(),
                self.config.base_lsp.clone(),
                &cwd,
                argument,
            ),
            "/plugins marketplace remove " => PluginManager::remove_marketplace(
                &self.config.plugins,
                self.config.base_mcp.clone(),
                self.config.base_lsp.clone(),
                &cwd,
                argument,
            ),
            "/plugins install " => PluginManager::install(
                &self.config.plugins,
                self.config.base_mcp.clone(),
                self.config.base_lsp.clone(),
                &cwd,
                argument,
            ),
            "/plugins uninstall " => PluginManager::uninstall(
                &self.config.plugins,
                self.config.base_mcp.clone(),
                self.config.base_lsp.clone(),
                &cwd,
                argument,
            ),
            "/plugins enable " => PluginManager::set_enabled(
                &self.config.plugins,
                self.config.base_mcp.clone(),
                self.config.base_lsp.clone(),
                &cwd,
                argument,
                true,
            ),
            "/plugins disable " => PluginManager::set_enabled(
                &self.config.plugins,
                self.config.base_mcp.clone(),
                self.config.base_lsp.clone(),
                &cwd,
                argument,
                false,
            ),
            _ => unreachable!(),
        };
        self.apply_plugin_mutation(prompt, result);
        true
    }

    fn apply_plugin_mutation(&mut self, command: &str, result: Result<PluginMutationResult>) {
        let response = match result {
            Ok(result) => {
                self.runtime.reload_extensions(
                    result.load.lsp.clone(),
                    result.load.mcp.clone(),
                    result.load.catalog.hooks.clone(),
                );
                self.config.apply_plugin_load(result.load);
                format!("{}\n\nChanges are active in this session.", result.message)
            }
            Err(error) => format!("Plugin operation failed: {error:#}"),
        };
        self.show_local_command(command, response);
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
            RuntimeEvent::PromptStarted {
                prompt,
                released_progress,
            } => {
                if let Some(progress) = released_progress {
                    self.messages.push(Message::progress(progress));
                }
                self.messages.push(Message::user(prompt));
                self.status = AgentStatus::Thinking;
                self.start_turn_timer();
                self.scroll = 0;
                self.agent_activity = None;
            }
            RuntimeEvent::Blocked { message } => {
                self.run_notice = Some(message);
                self.status = AgentStatus::Idle;
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
                if name == "TodoWrite" {
                    return;
                }
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
                if name == "TodoWrite" {
                    return;
                }
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
                allowed_tool,
            } => {
                self.runtime
                    .sync_conversation_permission(edit_always_allowed, allowed_tool);
            }
            AgentEvent::TodoUpdated(update) => {
                self.runtime.apply_todo_update(update);
                self.agent_activity = Some("Updated progress checklist".to_owned());
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

fn mcp_server_from_form(form: &McpAddForm) -> Result<(String, McpServerConfig)> {
    let name = form.name.value.trim().to_owned();
    if name.is_empty() {
        bail!("server name is required");
    }
    if !name
        .chars()
        .all(|char| char.is_ascii_alphanumeric() || matches!(char, '-' | '_' | '.'))
    {
        bail!("server name may contain only letters, numbers, '-', '_', and '.'");
    }

    let transport = match form.transport {
        McpAddTransport::Stdio => {
            let command = form.command.value.trim().to_owned();
            if command.is_empty() {
                bail!("stdio command is required");
            }
            let arguments = form.arguments.value.trim();
            let args = if arguments.is_empty() {
                Vec::new()
            } else {
                shlex::split(arguments).context("arguments contain an unclosed quote")?
            };
            let env_vars = comma_list(&form.environment_variables.value);
            if let Some(variable) = env_vars.iter().find(|variable| !valid_env_name(variable)) {
                bail!("'{variable}' is not a valid environment variable name");
            }
            McpTransportConfig::Stdio {
                command,
                args,
                env: Default::default(),
                env_vars,
                cwd: optional_text(&form.working_directory.value),
            }
        }
        McpAddTransport::StreamableHttp => {
            let url = required_text(&form.url.value, "server URL")?;
            let bearer_token_env = optional_text(&form.bearer_token_env.value);
            if let Some(variable) = &bearer_token_env
                && !valid_env_name(variable)
            {
                bail!("'{variable}' is not a valid bearer-token environment variable name");
            }
            McpTransportConfig::StreamableHttp {
                url,
                headers: Default::default(),
                bearer_token_env,
                oauth: None,
            }
        }
        McpAddTransport::OAuth => McpTransportConfig::StreamableHttp {
            url: required_text(&form.url.value, "server URL")?,
            headers: Default::default(),
            bearer_token_env: None,
            oauth: Some(McpOAuthConfig {
                redirect_uri: required_text(&form.redirect_uri.value, "OAuth redirect URI")?,
                scopes: comma_list(&form.scopes.value),
            }),
        },
    };
    let server = McpServerConfig {
        enabled: true,
        startup_timeout_ms: 20_000,
        tool_timeout_ms: 60_000,
        approval: form.approval,
        tool_approval: Default::default(),
        enabled_tools: None,
        disabled_tools: Vec::new(),
        transport,
    };
    let config = McpConfig {
        servers: std::collections::BTreeMap::from([(name.clone(), server.clone())]),
    };
    config.validate()?;
    Ok((name, server))
}

fn required_text(value: &str, label: &str) -> Result<String> {
    optional_text(value).with_context(|| format!("{label} is required"))
}

fn optional_text(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_owned())
}

fn comma_list(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .collect()
}

fn valid_env_name(value: &str) -> bool {
    let mut chars = value.chars();
    chars
        .next()
        .is_some_and(|char| char.is_ascii_alphabetic() || char == '_')
        && chars.all(|char| char.is_ascii_alphanumeric() || char == '_')
}

fn previous_mcp_approval(approval: McpApprovalPolicy) -> McpApprovalPolicy {
    match approval {
        McpApprovalPolicy::Allow => McpApprovalPolicy::Deny,
        McpApprovalPolicy::Prompt => McpApprovalPolicy::Allow,
        McpApprovalPolicy::Deny => McpApprovalPolicy::Prompt,
    }
}

fn next_mcp_approval(approval: McpApprovalPolicy) -> McpApprovalPolicy {
    match approval {
        McpApprovalPolicy::Allow => McpApprovalPolicy::Prompt,
        McpApprovalPolicy::Prompt => McpApprovalPolicy::Deny,
        McpApprovalPolicy::Deny => McpApprovalPolicy::Allow,
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

fn plugin_command_cwd() -> PathBuf {
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
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
    use std::fs;

    use crate::{agent::should_auto_compact, commands::SLASH_COMMANDS, plugins::PluginsConfig};

    use super::*;

    fn app() -> App {
        App::test_empty()
    }

    #[test]
    fn plugin_install_and_view_toggle_apply_without_restart_or_log() {
        let root =
            std::env::temp_dir().join(format!("glint-app-plugin-install-{}", uuid::Uuid::new_v4()));
        let plugin = root.join("marketplace/plugins/demo");
        fs::create_dir_all(plugin.join(".claude-plugin")).unwrap();
        fs::create_dir_all(plugin.join("commands")).unwrap();
        fs::create_dir_all(root.join("marketplace/.claude-plugin")).unwrap();
        fs::write(
            plugin.join(".claude-plugin/plugin.json"),
            r#"{"name":"demo","version":"1.0.0"}"#,
        )
        .unwrap();
        fs::write(
            plugin.join("commands/review.md"),
            "---\ndescription: Review code\n---\nReview the workspace.",
        )
        .unwrap();
        fs::write(
            root.join("marketplace/.claude-plugin/marketplace.json"),
            r#"{"name":"demo-market","plugins":[{"name":"demo","source":"./plugins/demo"}]}"#,
        )
        .unwrap();

        let mut app = app();
        app.config.plugins = PluginsConfig {
            marketplaces: vec![root.join("marketplace").to_string_lossy().into_owned()],
            cache_dir: Some(root.join("cache")),
            ..Default::default()
        };
        app.input.set("/plugins install demo@demo-market");
        app.submit();

        assert_eq!(app.config.extensions.plugins[0].name, "demo");
        assert_eq!(app.config.extensions.commands[0].name, "demo:review");
        assert!(
            app.messages
                .last()
                .unwrap()
                .content
                .contains("Changes are active in this session")
        );

        app.open_plugins_view();
        app.update_plugins_view_key(KeyAction::Char(' '));
        assert!(!app.config.extensions.installed_plugins[0].enabled);
        assert!(matches!(
            app.plugins_view.as_ref().map(|view| &view.screen),
            Some(PluginsScreen::Browse)
        ));
        assert!(app.pending_plugin_operation.is_none());

        app.update_plugins_view_key(KeyAction::Char(' '));
        assert!(app.config.extensions.installed_plugins[0].enabled);
        assert!(matches!(
            app.plugins_view.as_ref().map(|view| &view.screen),
            Some(PluginsScreen::Browse)
        ));
        assert!(app.pending_plugin_operation.is_none());
        fs::remove_dir_all(root).ok();
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
        assert!(names.contains(&"/plugins"));
        assert!(!names.contains(&"/plugin"));
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
    fn mcp_command_opens_manager_without_adding_a_message() {
        let mut app = app();
        app.input.set("/mcp");
        let message_count = app.messages.len();

        let command = SLASH_COMMANDS
            .iter()
            .find(|command| command.name == "/mcp")
            .copied()
            .unwrap();
        app.run_slash_command(command);

        assert!(matches!(
            app.mcp_view.as_ref().map(|view| &view.screen),
            Some(McpScreen::Browse)
        ));
        assert_eq!(app.input.value, "/mcp");
        assert_eq!(app.messages.len(), message_count);
    }

    #[test]
    fn mcp_manager_switches_focus_scrolls_and_opens_details() {
        let mut app = app();
        let config = crate::services::mcp::McpConfig {
            servers: std::collections::BTreeMap::from([(
                "docs".to_owned(),
                crate::services::mcp::McpServerConfig {
                    enabled: false,
                    startup_timeout_ms: 20_000,
                    tool_timeout_ms: 60_000,
                    approval: crate::services::mcp::McpApprovalPolicy::Prompt,
                    tool_approval: Default::default(),
                    enabled_tools: None,
                    disabled_tools: Vec::new(),
                    transport: crate::services::mcp::McpTransportConfig::Stdio {
                        command: "docs-server".to_owned(),
                        args: Vec::new(),
                        env: Default::default(),
                        env_vars: Vec::new(),
                        cwd: None,
                    },
                },
            )]),
        };
        app.config.mcp = config.clone();
        app.config.base_mcp = config.clone();
        app.runtime
            .reload_extensions(Default::default(), config, Vec::new());
        app.open_mcp_view();

        app.update_mcp_view_key(KeyAction::Right);
        assert_eq!(
            app.mcp_view.as_ref().map(|view| view.focus),
            Some(McpFocus::Details)
        );
        app.set_mcp_detail_max_scroll(2);
        app.update_mcp_view_key(KeyAction::Down);
        assert_eq!(
            app.mcp_view.as_ref().map(|view| view.detail_scroll),
            Some(1)
        );
        app.update_mcp_view_key(KeyAction::Down);
        app.update_mcp_view_key(KeyAction::Down);
        assert_eq!(
            app.mcp_view.as_ref().map(|view| view.detail_scroll),
            Some(2)
        );
        app.update_mcp_view_key(KeyAction::Up);
        assert_eq!(
            app.mcp_view.as_ref().map(|view| view.detail_scroll),
            Some(1)
        );
        app.set_mcp_detail_max_scroll(0);
        assert_eq!(
            app.mcp_view.as_ref().map(|view| view.detail_scroll),
            Some(0)
        );
        app.update_mcp_view_key(KeyAction::Submit);
        assert!(matches!(
            app.mcp_view.as_ref().map(|view| &view.screen),
            Some(McpScreen::Details)
        ));
        app.update_mcp_view_key(KeyAction::Char('R'));
        assert!(
            app.mcp_view
                .as_ref()
                .and_then(|view| view.notice.as_ref())
                .is_some_and(|notice| notice.failed && notice.message.contains("config.yaml"))
        );
        app.update_mcp_view_key(KeyAction::Escape);
        assert!(matches!(
            app.mcp_view.as_ref().map(|view| &view.screen),
            Some(McpScreen::Browse)
        ));
        app.update_mcp_view_key(KeyAction::Escape);
        assert!(app.mcp_view.is_none());
        assert_eq!(app.input.value, "");
    }

    #[test]
    fn mcp_mouse_detail_scroll_stops_at_calculated_bottom() {
        let mut app = app();
        app.mcp_view = Some(McpView {
            selected: 1,
            detail_scroll: 0,
            detail_max_scroll: 5,
            focus: McpFocus::Details,
            screen: McpScreen::Details,
            notice: None,
        });

        app.update(AppEvent::ExtensionMouse(ExtensionMouseAction::Mcp(
            McpMouseAction::ScrollDetails(3),
        )));
        app.update(AppEvent::ExtensionMouse(ExtensionMouseAction::Mcp(
            McpMouseAction::ScrollDetails(3),
        )));

        assert_eq!(
            app.mcp_view.as_ref().map(|view| view.detail_scroll),
            Some(5)
        );
    }

    #[test]
    fn mcp_oauth_screen_edits_callback_and_cancels() {
        let mut app = app();
        app.mcp_view = Some(McpView {
            selected: 0,
            detail_scroll: 0,
            detail_max_scroll: 0,
            focus: McpFocus::Details,
            screen: McpScreen::OAuth {
                server: "remote".to_owned(),
                authorization_url: "https://example.test/authorize".to_owned(),
                callback: InputState::default(),
            },
            notice: None,
        });

        app.update_mcp_view_key(KeyAction::Char('a'));
        app.update_mcp_view_key(KeyAction::Char('b'));
        app.update_mcp_view_key(KeyAction::Left);
        app.update_mcp_view_key(KeyAction::Char('c'));
        assert!(matches!(
            app.mcp_view.as_ref().map(|view| &view.screen),
            Some(McpScreen::OAuth { callback, .. }) if callback.value == "acb"
        ));

        app.update_mcp_view_key(KeyAction::Escape);
        assert!(matches!(
            app.mcp_view.as_ref().map(|view| &view.screen),
            Some(McpScreen::Browse)
        ));
    }

    #[test]
    fn mcp_add_form_switches_transport_and_edits_visible_fields() {
        let mut app = app();
        app.open_mcp_view();
        app.update_mcp_view_key(KeyAction::Submit);
        assert!(matches!(
            app.mcp_view.as_ref().map(|view| &view.screen),
            Some(McpScreen::Add(form)) if form.transport == McpAddTransport::Stdio
        ));

        app.update_mcp_view_key(KeyAction::Right);
        app.update_mcp_view_key(KeyAction::Down);
        for char in "remote".chars() {
            app.update_mcp_view_key(KeyAction::Char(char));
        }
        app.update_mcp_view_key(KeyAction::Tab);
        for char in "https://example.test/mcp".chars() {
            app.update_mcp_view_key(KeyAction::Char(char));
        }

        assert!(matches!(
            app.mcp_view.as_ref().map(|view| &view.screen),
            Some(McpScreen::Add(form))
                if form.transport == McpAddTransport::StreamableHttp
                    && form.name.value == "remote"
                    && form.url.value == "https://example.test/mcp"
        ));
        app.update_mcp_view_key(KeyAction::Escape);
        assert!(matches!(
            app.mcp_view.as_ref().map(|view| &view.screen),
            Some(McpScreen::Browse)
        ));
    }

    #[test]
    fn mcp_add_form_builds_oauth_configuration() {
        let mut form = McpAddForm {
            transport: McpAddTransport::OAuth,
            approval: McpApprovalPolicy::Allow,
            ..Default::default()
        };
        form.name.set("remote-oauth");
        form.url.set("https://example.test/mcp");
        form.redirect_uri.set("http://127.0.0.1:8765/callback");
        form.scopes.set("read, write");

        let (name, server) = mcp_server_from_form(&form).unwrap();

        assert_eq!(name, "remote-oauth");
        assert_eq!(server.approval, McpApprovalPolicy::Allow);
        let McpTransportConfig::StreamableHttp {
            url,
            bearer_token_env,
            oauth,
            ..
        } = server.transport
        else {
            panic!("expected Streamable HTTP transport");
        };
        assert_eq!(url, "https://example.test/mcp");
        assert!(bearer_token_env.is_none());
        assert_eq!(oauth.unwrap().scopes, ["read", "write"]);
    }

    #[test]
    fn mcp_add_form_persists_and_activates_server() {
        let root = std::env::temp_dir().join(format!("glint-app-add-mcp-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        let config_path = root.join("config.yaml");
        fs::write(&config_path, "llm:\n  provider: demo\n").unwrap();
        let mut app = app();
        let mut form = McpAddForm::default();
        form.name.set("local-docs");
        form.command.set("glint-missing-mcp-test-command");
        form.arguments.set("--stdio \"docs root\"");
        form.environment_variables.set("MCP_TOKEN, OPTIONAL_KEY");
        app.mcp_view = Some(McpView {
            selected: 0,
            detail_scroll: 0,
            detail_max_scroll: 0,
            focus: McpFocus::Servers,
            screen: McpScreen::Add(Box::new(form)),
            notice: None,
        });

        app.add_mcp_server_at(&config_path);

        assert!(app.config.base_mcp.servers.contains_key("local-docs"));
        assert!(app.config.mcp.servers.contains_key("local-docs"));
        assert!(
            app.runtime
                .mcp_statuses()
                .iter()
                .any(|status| status.name == "local-docs")
        );
        assert!(matches!(
            app.mcp_view.as_ref().map(|view| &view.screen),
            Some(McpScreen::Browse)
        ));
        assert_eq!(app.mcp_view.as_ref().map(|view| view.selected), Some(1));
        assert!(
            app.mcp_view
                .as_ref()
                .and_then(|view| view.notice.as_ref())
                .is_some_and(|notice| !notice.failed && notice.message.contains("config.yaml"))
        );
        let persisted = fs::read_to_string(&config_path).unwrap();
        assert!(persisted.contains("    local-docs:\n"));
        assert!(persisted.contains("      command: glint-missing-mcp-test-command\n"));
        assert!(persisted.contains("      - docs root\n"));
        assert!(persisted.contains("      - MCP_TOKEN\n"));
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn mcp_add_form_keeps_validation_errors_in_the_form() {
        let root =
            std::env::temp_dir().join(format!("glint-app-invalid-mcp-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        let config_path = root.join("config.yaml");
        fs::write(&config_path, "llm:\n  provider: demo\n").unwrap();
        let mut app = app();
        let mut form = McpAddForm::default();
        form.name.set("invalid name");
        app.mcp_view = Some(McpView {
            selected: 0,
            detail_scroll: 0,
            detail_max_scroll: 0,
            focus: McpFocus::Servers,
            screen: McpScreen::Add(Box::new(form)),
            notice: None,
        });

        app.add_mcp_server_at(&config_path);

        assert!(matches!(
            app.mcp_view.as_ref().map(|view| &view.screen),
            Some(McpScreen::Add(_))
        ));
        assert!(
            app.mcp_view
                .as_ref()
                .and_then(|view| view.notice.as_ref())
                .is_some_and(|notice| notice.failed && notice.message.contains("server name"))
        );
        assert_eq!(
            fs::read_to_string(&config_path).unwrap(),
            "llm:\n  provider: demo\n"
        );
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn plugins_command_opens_installed_tab() {
        let mut app = app();
        app.input.set("/plugins");

        let command = SLASH_COMMANDS
            .iter()
            .find(|command| command.name == "/plugins")
            .copied()
            .unwrap();
        app.run_slash_command(command);

        assert!(app.plugins_view.is_some());
        assert_eq!(
            app.plugins_view.as_ref().map(|view| view.tab),
            Some(PluginsTab::Installed)
        );
        assert_eq!(app.input.value, "/plugins");
    }

    #[test]
    fn plugins_mouse_switches_tabs_and_clamps_detail_scroll() {
        let mut app = app();
        app.open_plugins_view();
        app.set_plugins_detail_max_scroll(4);

        app.update(AppEvent::ExtensionMouse(ExtensionMouseAction::Plugins(
            PluginsMouseAction::ScrollDetails(3),
        )));
        app.update(AppEvent::ExtensionMouse(ExtensionMouseAction::Plugins(
            PluginsMouseAction::ScrollDetails(3),
        )));
        assert_eq!(
            app.plugins_view.as_ref().map(|view| view.detail_scroll),
            Some(4)
        );

        app.update(AppEvent::ExtensionMouse(ExtensionMouseAction::Plugins(
            PluginsMouseAction::SelectTab(PluginsMouseTab::Marketplaces),
        )));
        assert_eq!(
            app.plugins_view.as_ref().map(|view| view.tab),
            Some(PluginsTab::Marketplaces)
        );
        assert_eq!(
            app.plugins_view.as_ref().map(|view| view.detail_scroll),
            Some(0)
        );
    }

    #[test]
    fn plugins_view_opens_marketplace_input_and_returns() {
        let mut app = app();
        app.open_plugins_view();

        app.update_plugins_view_key(KeyAction::Right);
        assert_eq!(
            app.plugins_view.as_ref().map(|view| view.tab),
            Some(PluginsTab::Marketplaces)
        );

        app.update_plugins_view_key(KeyAction::Submit);
        assert!(matches!(
            app.plugins_view.as_ref().map(|view| &view.screen),
            Some(PluginsScreen::AddMarketplace(_))
        ));
        app.update_plugins_view_key(KeyAction::Char('a'));
        app.update_plugins_view_key(KeyAction::Char('c'));
        assert!(matches!(
            app.plugins_view.as_ref().map(|view| &view.screen),
            Some(PluginsScreen::AddMarketplace(input)) if input.value == "ac"
        ));

        app.update_plugins_view_key(KeyAction::Escape);
        assert!(matches!(
            app.plugins_view.as_ref().map(|view| &view.screen),
            Some(PluginsScreen::Browse)
        ));
        app.update_plugins_view_key(KeyAction::Escape);
        assert!(app.plugins_view.is_none());
        assert_eq!(app.input.value, "");
    }

    #[test]
    fn marketplace_git_activity_is_rendered_in_plugins_operation() {
        let root = std::env::temp_dir().join(format!(
            "glint-app-marketplace-progress-{}",
            uuid::Uuid::new_v4()
        ));
        let repository = root.join("marketplace");
        fs::create_dir_all(repository.join(".claude-plugin")).unwrap();
        fs::write(
            repository.join(".claude-plugin/marketplace.json"),
            r#"{"name":"progress-market","plugins":[]}"#,
        )
        .unwrap();
        init_test_git_repository(&repository);

        let mut app = app();
        app.config.plugins = PluginsConfig {
            cache_dir: Some(root.join("cache")),
            ..Default::default()
        };
        app.open_plugins_view();
        if let Some(view) = app.plugins_view.as_mut() {
            view.tab = PluginsTab::Marketplaces;
        }
        let source = format!("file://{}", repository.display());
        app.start_plugin_ui_operation(
            "Adding marketplace".to_owned(),
            source.clone(),
            PluginUiMutation::AddMarketplace(source),
        );

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while app.pending_plugin_operation.is_some() && std::time::Instant::now() < deadline {
            app.drain_plugin_operation();
            std::thread::yield_now();
        }

        assert!(app.pending_plugin_operation.is_none());
        let PluginsScreen::Operation(operation) = &app.plugins_view.as_ref().unwrap().screen else {
            panic!("expected plugin operation view");
        };
        assert!(operation.finished);
        assert!(!operation.failed);
        assert!(
            operation
                .log
                .iter()
                .any(|line| line.starts_with("git: clone plugin source"))
        );
        assert!(
            operation
                .log
                .iter()
                .any(|line| line.contains("Added marketplace `progress-market`"))
        );
        assert_eq!(
            app.config.extensions.marketplaces[0].name,
            "progress-market"
        );
        fs::remove_dir_all(root).ok();
    }

    fn init_test_git_repository(root: &Path) {
        for arguments in [
            vec!["init", "-b", "main"],
            vec!["add", "."],
            vec![
                "-c",
                "user.name=Glint Test",
                "-c",
                "user.email=glint@example.invalid",
                "commit",
                "-m",
                "test marketplace",
            ],
        ] {
            let output = std::process::Command::new("git")
                .args(arguments)
                .current_dir(root)
                .output()
                .unwrap();
            assert!(output.status.success());
        }
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
