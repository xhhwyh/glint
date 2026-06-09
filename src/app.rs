use std::sync::mpsc::{self, Receiver};

use crate::{
    agent::{self, AgentEvent, AgentRunInput, AgentStatus, RuntimeContext},
    approval::{AgentControl, ApprovalFocus, ApprovalPrompt, ConversationPermissions},
    config::Config,
    event::{AppEvent, KeyAction, MouseAction},
    input::InputState,
    message::{Message, Role},
};

pub struct App {
    pub should_quit: bool,
    pub messages: Vec<Message>,
    pub input: InputState,
    pub status: AgentStatus,
    pub scroll: u16,
    pub agent_events: Receiver<AgentEvent>,
    pub config: Config,
    pub current_dir: String,
    pub agent_activity: Option<String>,
    pub approval: Option<ApprovalPrompt>,
    pub conversation_permissions: ConversationPermissions,
    agent_control_tx: Option<mpsc::Sender<AgentControl>>,
    agent_tx: mpsc::Sender<AgentEvent>,
}

impl App {
    pub fn new(config: Config) -> Self {
        let (agent_tx, agent_events) = mpsc::channel();
        Self {
            should_quit: false,
            messages: Vec::new(),
            input: InputState::default(),
            status: AgentStatus::Idle,
            scroll: 0,
            agent_events,
            config,
            current_dir: current_dir_label(),
            agent_activity: None,
            approval: None,
            conversation_permissions: ConversationPermissions::default(),
            agent_control_tx: None,
            agent_tx,
        }
    }

    pub fn update(&mut self, event: AppEvent) {
        match event {
            AppEvent::Key(key) => self.update_key(key),
            AppEvent::Mouse(mouse) => self.update_mouse(mouse),
            AppEvent::Agent(event) => self.update_agent(event),
        }
    }

    fn update_key(&mut self, key: KeyAction) {
        if key == KeyAction::Quit {
            self.should_quit = true;
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
            | KeyAction::CancelConversationPermission => {}
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

    fn submit(&mut self) {
        let prompt = self.input.take_trimmed();
        if prompt.is_empty() {
            return;
        }

        let conversation = self.messages.clone();
        self.messages.push(Message::user(prompt.clone()));
        self.status = AgentStatus::Thinking;
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
            AgentEvent::Started => {
                self.status = AgentStatus::Responding;
                self.agent_activity = None;
                self.messages.push(Message::assistant(""));
            }
            AgentEvent::AssistantDelta(delta) => {
                self.agent_activity = None;
                self.append_assistant_delta(&delta);
            }
            AgentEvent::ToolStarted {
                id,
                name,
                input_summary,
            } => {
                self.agent_activity = Some(format!("Running {name}: {input_summary}"));
                self.remove_empty_assistant_tail();
                if name == "Read" && self.merge_read_tool(&input_summary) {
                    return;
                }
                self.messages.push(Message::tool(id, name, input_summary));
            }
            AgentEvent::ToolFinished {
                id,
                name,
                output_summary,
            } => {
                self.agent_activity = Some(format!("Finished {name}: {output_summary}"));
                if name == "Read" {
                    if let Some(message) = self.find_tool_message(&id) {
                        message.tool_finished = true;
                    }
                    return;
                }

                if let Some(message) = self.messages.iter_mut().rev().find(|message| {
                    message.role == Role::Tool && message.tool_call_id.as_deref() == Some(&id)
                }) {
                    message.content = output_summary;
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
                self.status = AgentStatus::Idle;
                self.agent_activity = None;
                self.agent_control_tx = None;
                self.approval = None;
            }
            AgentEvent::Failed(error) => {
                self.append_assistant_delta(&error);
                self.status = AgentStatus::Idle;
                self.agent_activity = None;
                self.agent_control_tx = None;
                self.approval = None;
            }
        }
    }

    fn append_assistant_delta(&mut self, delta: &str) {
        if !matches!(self.messages.last(), Some(message) if message.role == Role::Assistant) {
            self.messages.push(Message::assistant(""));
        }

        if let Some(message) = self.messages.last_mut() {
            message.content.push_str(delta);
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

fn current_dir_label() -> String {
    std::env::current_dir()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|_| "?".to_owned())
}
