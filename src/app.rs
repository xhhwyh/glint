use std::{
    path::{Path, PathBuf},
    sync::mpsc::{self, Receiver},
    time::{Duration, Instant},
};

use anyhow::Result;

use crate::{
    agent::{
        self, AgentEvent, AgentRunInput, AgentStatus, CompactRunInput, RuntimeContext, TokenUsage,
        provider::FinishReason,
    },
    approval::{AgentControl, ApprovalFocus, ApprovalPrompt, ConversationPermissions},
    config::Config,
    event::{AppEvent, KeyAction, MouseAction},
    input::InputState,
    message::{Message, Role},
    transcript::{AssistantTranscript, CompactTrigger, TranscriptSessionSummary, TranscriptStore},
};

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
    pub agent_events: Receiver<AgentEvent>,
    pub config: Config,
    pub current_dir: String,
    pub agent_activity: Option<String>,
    pub run_notice: Option<String>,
    pub approval: Option<ApprovalPrompt>,
    pub conversation_permissions: ConversationPermissions,
    turn_started_at: Option<Instant>,
    transcript: TranscriptStore,
    transcript_cwd: String,
    agent_control_tx: Option<mpsc::Sender<AgentControl>>,
    agent_tx: mpsc::Sender<AgentEvent>,
    pending_prompt_after_compact: Option<String>,
    auto_compact_failures: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SlashCommand {
    pub name: &'static str,
    pub description: &'static str,
    kind: SlashCommandKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SlashCommandKind {
    Compact,
    Model,
    Resume,
}

const SLASH_COMMANDS: [SlashCommand; 3] = [
    SlashCommand {
        name: "/compact",
        description: "Summarize earlier conversation and continue compacted",
        kind: SlashCommandKind::Compact,
    },
    SlashCommand {
        name: "/model",
        description: "Switch provider and model",
        kind: SlashCommandKind::Model,
    },
    SlashCommand {
        name: "/resume",
        description: "Resume a saved session",
        kind: SlashCommandKind::Resume,
    },
];

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

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ConversationUsage {
    pub last_usage: Option<TokenUsage>,
    pub total_prompt_tokens: u64,
    pub total_completion_tokens: u64,
    pub total_tokens: u64,
}

impl ConversationUsage {
    pub fn record(self, usage: TokenUsage) -> Self {
        Self {
            last_usage: Some(usage),
            total_prompt_tokens: self.total_prompt_tokens + usage.prompt_tokens,
            total_completion_tokens: self.total_completion_tokens + usage.completion_tokens,
            total_tokens: self.total_tokens + usage.total_tokens,
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

fn percent(value: u64, total: u64) -> u8 {
    (value.saturating_mul(100) / total).min(100) as u8
}

fn usage_from_transcript(transcript: &TranscriptStore) -> ConversationUsage {
    transcript
        .token_usages()
        .fold(ConversationUsage::default(), |usage, item| {
            usage.record(item)
        })
}

impl App {
    pub fn new(config: Config) -> Result<Self> {
        let (agent_tx, agent_events) = mpsc::channel();
        let current_dir = current_dir_label();
        let transcript_cwd = std::env::current_dir()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|_| current_dir.clone());
        let transcript = TranscriptStore::load_or_create(
            &transcript_cwd,
            &config.llm.provider,
            &config.llm.model,
        )?;
        let messages = transcript.ui_messages();
        let usage = usage_from_transcript(&transcript);

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
            agent_events,
            config,
            current_dir,
            agent_activity: None,
            run_notice: None,
            approval: None,
            conversation_permissions: ConversationPermissions::default(),
            turn_started_at: None,
            transcript,
            transcript_cwd,
            agent_control_tx: None,
            agent_tx,
            pending_prompt_after_compact: None,
            auto_compact_failures: 0,
        })
    }

    pub fn update(&mut self, event: AppEvent) {
        match event {
            AppEvent::Key(key) => self.update_key(key),
            AppEvent::Mouse(mouse) => self.update_mouse(mouse),
            AppEvent::Agent(event) => self.update_agent(event),
        }
    }

    pub fn processing_elapsed(&self) -> Option<Duration> {
        self.turn_started_at.map(|started_at| started_at.elapsed())
    }

    fn update_key(&mut self, key: KeyAction) {
        if key == KeyAction::Quit {
            if self.status == AgentStatus::Idle {
                self.should_quit = true;
            } else {
                self.cancel_current_turn();
            }
            return;
        }
        if key == KeyAction::CancelConversationPermission {
            self.clear_conversation_edit_permission();
            return;
        }
        if self.approval.is_some() {
            self.update_approval_key(key);
            return;
        }
        if self.resume_picker.is_some() {
            self.update_resume_picker_key(key);
            return;
        }
        if self.model_picker.is_some() {
            self.update_model_picker_key(key);
            return;
        }
        if self.slash_menu_visible()
            && matches!(key, KeyAction::Submit | KeyAction::Up | KeyAction::Down)
        {
            self.update_slash_menu_key(key);
            return;
        }

        match key {
            KeyAction::Quit => self.should_quit = true,
            KeyAction::Submit if self.status == AgentStatus::Idle => self.submit(),
            KeyAction::Newline if self.status == AgentStatus::Idle => self.input.newline(),
            KeyAction::Char(char) if self.status == AgentStatus::Idle => self.input.push(char),
            KeyAction::Backspace if self.status == AgentStatus::Idle => self.input.backspace(),
            KeyAction::Left if self.status == AgentStatus::Idle => self.input.move_left(),
            KeyAction::Right if self.status == AgentStatus::Idle => self.input.move_right(),
            KeyAction::Up if self.status == AgentStatus::Idle => self.input.move_up(),
            KeyAction::Down if self.status == AgentStatus::Idle => self.input.move_down(),
            KeyAction::Up => self.scroll = self.scroll.saturating_add(1),
            KeyAction::Down => self.scroll = self.scroll.saturating_sub(1),
            KeyAction::None
            | KeyAction::Submit
            | KeyAction::Newline
            | KeyAction::Char(_)
            | KeyAction::Backspace
            | KeyAction::Left
            | KeyAction::Right
            | KeyAction::Tab
            | KeyAction::Escape
            | KeyAction::CancelConversationPermission => {}
        }
        self.clamp_slash_command_selection();
    }

    pub fn slash_query(&self) -> Option<&str> {
        if self.status != AgentStatus::Idle
            || self.model_picker.is_some()
            || self.resume_picker.is_some()
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
        SLASH_COMMANDS
            .into_iter()
            .filter(|command| command.name[1..].starts_with(query))
            .take(5)
            .collect()
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
                self.slash_command_selection = self.slash_command_selection.saturating_sub(1);
            }
            KeyAction::Down if !matches.is_empty() => {
                self.slash_command_selection =
                    (self.slash_command_selection + 1).min(matches.len() - 1);
            }
            _ => {}
        }
    }

    fn run_slash_command(&mut self, command: SlashCommand) {
        match command.kind {
            SlashCommandKind::Compact => self.run_compact(),
            SlashCommandKind::Model => self.open_model_picker(),
            SlashCommandKind::Resume => self.open_resume_picker(),
        }
    }

    fn submit_unknown_slash_command(&mut self) {
        let command = self.input.take_trimmed();
        if command.is_empty() {
            return;
        }
        self.record_user(command.clone());
        self.messages.push(Message::user(command.clone()));
        let response = format!("Unknown slash command `{command}`");
        self.record_assistant(response.clone(), Vec::new(), None, FinishReason::Stop, None);
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
        self.record_user(command.clone());
        self.messages.push(Message::user(command));

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
        self.record_assistant(result.clone(), Vec::new(), None, FinishReason::Stop, None);
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
        let sessions = TranscriptStore::sessions(&self.transcript_cwd).unwrap_or_default();
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
        let Ok(transcript) = TranscriptStore::load_path(path) else {
            self.close_resume_picker();
            return;
        };
        self.messages = transcript.ui_messages();
        self.usage = usage_from_transcript(&transcript);
        self.transcript = transcript;
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
        if decision == crate::approval::ApprovalDecision::AllowConversation {
            self.conversation_permissions.edit_always_allowed = true;
        }
        if let Some(tx) = &self.agent_control_tx {
            tx.send(AgentControl::ApprovalDecision {
                id: approval.request.id,
                decision,
            })
            .ok();
        }
        self.status = AgentStatus::Responding;
    }

    fn clear_conversation_edit_permission(&mut self) {
        self.conversation_permissions.edit_always_allowed = false;
        if let Some(tx) = &self.agent_control_tx {
            tx.send(AgentControl::ClearConversationEditPermission).ok();
        }
    }

    fn update_mouse(&mut self, mouse: MouseAction) {
        match mouse {
            MouseAction::ScrollUp => self.scroll = self.scroll.saturating_add(3),
            MouseAction::ScrollDown => self.scroll = self.scroll.saturating_sub(3),
            MouseAction::None => {}
        }
    }

    fn run_compact(&mut self) {
        let _command = self.input.take_trimmed();
        self.run_notice = None;
        self.pending_prompt_after_compact = None;
        self.reset_agent_channel();
        let conversation = self.transcript.model_history();
        if conversation.is_empty() {
            self.run_notice = Some("No messages to compact.".to_owned());
            return;
        }

        self.status = AgentStatus::Compacting;
        self.turn_started_at = Some(Instant::now());
        self.agent_activity = Some("Compacting conversation".to_owned());
        self.scroll = 0;
        self.agent_control_tx = None;

        agent::spawn_compact_loop(
            CompactRunInput {
                llm: self.config.llm.clone(),
                conversation,
                pre_prompt_tokens: self.usage.last_usage.map(|usage| usage.prompt_tokens),
            },
            self.agent_tx.clone(),
        );
    }

    fn submit(&mut self) {
        let prompt = self.input.take_trimmed();
        if prompt.is_empty() {
            return;
        }

        if agent::should_auto_compact(
            &self.config.llm,
            self.usage.last_usage.map(|usage| usage.prompt_tokens),
            self.auto_compact_failures,
        ) {
            let conversation = self.transcript.model_history();
            if !conversation.is_empty() {
                self.start_auto_compact(prompt, conversation);
                return;
            }
        }

        self.submit_prompt(prompt, true);
    }

    fn start_auto_compact(
        &mut self,
        prompt: String,
        conversation: Vec<agent::provider::ModelMessage>,
    ) {
        self.run_notice = None;
        self.reset_agent_channel();
        self.pending_prompt_after_compact = Some(prompt);
        self.status = AgentStatus::Compacting;
        self.turn_started_at = Some(Instant::now());
        self.agent_activity = Some("Auto-compacting conversation".to_owned());
        self.scroll = 0;
        self.agent_control_tx = None;

        agent::spawn_compact_loop(
            CompactRunInput {
                llm: self.config.llm.clone(),
                conversation,
                pre_prompt_tokens: self.usage.last_usage.map(|usage| usage.prompt_tokens),
            },
            self.agent_tx.clone(),
        );
    }

    fn submit_prompt(&mut self, prompt: String, clear_notice: bool) {
        if clear_notice {
            self.run_notice = None;
        }
        self.reset_agent_channel();
        let conversation = self.transcript.model_history();
        self.transcript
            .start_turn(
                self.transcript_cwd.clone(),
                self.config.llm.provider.clone(),
                self.config.llm.model.clone(),
            )
            .ok();
        self.record_user(prompt.clone());
        self.messages.push(Message::user(prompt.clone()));
        self.status = AgentStatus::Thinking;
        self.turn_started_at = Some(Instant::now());
        self.scroll = 0;
        self.agent_activity = None;

        let (control_tx, control_rx) = mpsc::channel();
        self.agent_control_tx = Some(control_tx);

        agent::spawn_agent_loop(
            AgentRunInput {
                llm: self.config.llm.clone(),
                system_prompt: self.config.system_prompt.clone(),
                runtime_context: RuntimeContext::current(self.current_dir.clone()),
                conversation_permissions: self.conversation_permissions,
                conversation,
                current_user_message: prompt,
            },
            self.agent_tx.clone(),
            control_rx,
        );
    }

    fn update_agent(&mut self, event: AgentEvent) {
        match event {
            AgentEvent::CompactStarted => {
                self.status = AgentStatus::Compacting;
                self.agent_activity = Some(if self.pending_prompt_after_compact.is_some() {
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
                self.record_tool(id.clone(), output.clone(), is_error);
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
                self.conversation_permissions.edit_always_allowed = edit_always_allowed;
            }
            AgentEvent::AssistantFinished => {
                self.transcript.complete_turn().ok();
                self.status = AgentStatus::Idle;
                self.turn_started_at = None;
                self.agent_activity = None;
                self.agent_control_tx = None;
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
                self.transcript.abort_turn(error).ok();
                self.status = AgentStatus::Idle;
                self.turn_started_at = None;
                self.agent_activity = None;
                self.agent_control_tx = None;
                self.approval = None;
            }
        }
    }

    fn finish_compact(
        &mut self,
        summary: String,
        pre_prompt_tokens: Option<u64>,
    ) -> Option<String> {
        let pending_prompt = self.pending_prompt_after_compact.take();
        let trigger = if pending_prompt.is_some() {
            CompactTrigger::Auto
        } else {
            CompactTrigger::Manual
        };
        self.transcript
            .append_compact_boundary(trigger, summary, pre_prompt_tokens)
            .ok();
        self.messages = self.transcript.ui_messages();
        self.status = AgentStatus::Idle;
        self.turn_started_at = None;
        self.agent_activity = None;
        self.agent_control_tx = None;
        self.approval = None;
        if pending_prompt.is_some() {
            self.auto_compact_failures = 0;
            self.run_notice = Some("Auto-compacted conversation.".to_owned());
        } else {
            self.run_notice = Some("Compacted conversation.".to_owned());
        }
        pending_prompt
    }

    fn fail_compact(&mut self, error: String) -> Option<String> {
        let pending_prompt = self.pending_prompt_after_compact.take();
        self.status = AgentStatus::Idle;
        self.turn_started_at = None;
        self.agent_activity = None;
        self.agent_control_tx = None;
        self.approval = None;
        if pending_prompt.is_some() {
            self.auto_compact_failures = self.auto_compact_failures.saturating_add(1);
            self.run_notice = Some("Auto-compact failed; continuing without compact.".to_owned());
        } else {
            self.run_notice = Some(format!("Compact failed: {error}"));
        }
        pending_prompt
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

    fn record_user(&mut self, content: String) {
        self.transcript.append_user(content).ok();
    }

    fn record_assistant(
        &mut self,
        content: String,
        tool_calls: Vec<agent::provider::ToolCall>,
        usage: Option<TokenUsage>,
        finish_reason: FinishReason,
        error: Option<String>,
    ) {
        self.transcript
            .append_assistant(AssistantTranscript {
                content,
                provider: self.config.llm.provider.clone(),
                model: self.config.llm.model.clone(),
                tool_calls,
                usage,
                finish_reason,
                error,
            })
            .ok();
    }

    fn record_tool(&mut self, call_id: String, content: String, is_error: bool) {
        self.transcript.append_tool(call_id, content, is_error).ok();
    }

    fn cancel_current_turn(&mut self) {
        if let Some(tx) = &self.agent_control_tx {
            tx.send(AgentControl::Cancel).ok();
        }
        let was_compacting = self.status == AgentStatus::Compacting;
        self.reset_agent_channel();
        if !was_compacting {
            self.transcript.abort_turn("cancelled".to_owned()).ok();
        }
        self.status = AgentStatus::Idle;
        self.turn_started_at = None;
        self.agent_activity = None;
        self.agent_control_tx = None;
        self.approval = None;
        self.pending_prompt_after_compact = None;
        self.remove_empty_assistant_tail();
        self.run_notice = Some(if was_compacting {
            "Stopped compact.".to_owned()
        } else {
            "Stopped this turn.".to_owned()
        });
    }

    fn reset_agent_channel(&mut self) {
        let (agent_tx, agent_events) = mpsc::channel();
        self.agent_tx = agent_tx;
        self.agent_events = agent_events;
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
    use crate::config::{LlmConfig, LlmProviderConfig, ModelCatalog};

    use super::*;

    fn app() -> App {
        let (agent_tx, agent_events) = mpsc::channel();
        App {
            should_quit: false,
            messages: Vec::new(),
            input: InputState::default(),
            status: AgentStatus::Idle,
            scroll: 0,
            usage: ConversationUsage::default(),
            slash_command_selection: 0,
            model_picker: None,
            resume_picker: None,
            agent_events,
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
                    }],
                    temperature: 0.0,
                    max_tokens: 100,
                    context_window: Some(1000),
                    api_key: "test-key".to_owned(),
                    default_context_window: Some(1000),
                },
                model_catalog: ModelCatalog::default(),
                system_prompt: "system".to_owned(),
            },
            current_dir: "/workspace".to_owned(),
            agent_activity: None,
            run_notice: None,
            approval: None,
            conversation_permissions: ConversationPermissions::default(),
            turn_started_at: None,
            transcript: TranscriptStore::test_empty(
                std::env::temp_dir().join(format!("glint-app-test-{}.jsonl", uuid::Uuid::new_v4())),
            ),
            transcript_cwd: "/workspace".to_owned(),
            agent_control_tx: None,
            agent_tx,
            pending_prompt_after_compact: None,
            auto_compact_failures: 0,
        }
    }

    #[test]
    fn slash_commands_include_compact() {
        let names = SLASH_COMMANDS
            .iter()
            .map(|command| command.name)
            .collect::<Vec<_>>();

        assert!(names.contains(&"/compact"));
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
        assert_eq!(app.transcript.model_history().len(), 0);
        assert_eq!(app.run_notice.as_deref(), Some("No messages to compact."));
    }

    #[test]
    fn compact_finished_records_boundary_and_notice() {
        let mut app = app();
        app.transcript.append_user("old user".to_owned()).unwrap();
        app.messages = app.transcript.ui_messages();
        app.status = AgentStatus::Compacting;
        app.turn_started_at = Some(Instant::now());

        let pending_prompt = app.finish_compact("important summary".to_owned(), Some(20));

        let history = app.transcript.model_history();
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
    fn auto_compact_finished_records_auto_boundary_and_returns_pending_prompt() {
        let mut app = app();
        app.transcript.append_user("old user".to_owned()).unwrap();
        app.pending_prompt_after_compact = Some("new prompt".to_owned());
        app.auto_compact_failures = 2;
        app.status = AgentStatus::Compacting;

        let pending_prompt = app.finish_compact("auto summary".to_owned(), Some(900));

        assert_eq!(pending_prompt.as_deref(), Some("new prompt"));
        assert_eq!(app.auto_compact_failures, 0);
        assert_eq!(
            app.run_notice.as_deref(),
            Some("Auto-compacted conversation.")
        );
        assert!(
            app.transcript
                .model_history()
                .first()
                .and_then(|message| message.content.as_deref())
                .is_some_and(|content| content.contains("auto summary"))
        );
    }

    #[test]
    fn auto_compact_failure_returns_pending_prompt_and_increments_failures() {
        let mut app = app();
        app.pending_prompt_after_compact = Some("new prompt".to_owned());
        app.auto_compact_failures = 2;
        app.status = AgentStatus::Compacting;

        let pending_prompt = app.fail_compact("network error".to_owned());

        assert_eq!(pending_prompt.as_deref(), Some("new prompt"));
        assert_eq!(app.auto_compact_failures, 3);
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

        assert!(agent::should_auto_compact(
            &app.config.llm,
            app.usage.last_usage.map(|usage| usage.prompt_tokens),
            app.auto_compact_failures
        ));

        app.auto_compact_failures = 3;
        assert!(!agent::should_auto_compact(
            &app.config.llm,
            app.usage.last_usage.map(|usage| usage.prompt_tokens),
            app.auto_compact_failures
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
