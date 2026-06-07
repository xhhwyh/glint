use std::sync::mpsc::{self, Receiver};

use crate::{
    agent::{self, AgentEvent, AgentStatus},
    event::{AppEvent, KeyAction},
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
    agent_tx: mpsc::Sender<AgentEvent>,
}

impl Default for App {
    fn default() -> Self {
        let (agent_tx, agent_events) = mpsc::channel();
        Self {
            should_quit: false,
            messages: vec![Message::system(
                "Enter a task. The fake agent will stream a response.",
            )],
            input: InputState::default(),
            status: AgentStatus::Idle,
            scroll: 0,
            agent_events,
            agent_tx,
        }
    }
}

impl App {
    pub fn update(&mut self, event: AppEvent) {
        match event {
            AppEvent::Key(key) => self.update_key(key),
            AppEvent::Agent(event) => self.update_agent(event),
        }
    }

    fn update_key(&mut self, key: KeyAction) {
        match key {
            KeyAction::Quit => self.should_quit = true,
            KeyAction::Submit if self.status == AgentStatus::Idle => self.submit(),
            KeyAction::Char(char) if self.status == AgentStatus::Idle => self.input.push(char),
            KeyAction::Backspace if self.status == AgentStatus::Idle => self.input.backspace(),
            KeyAction::ScrollUp => self.scroll = self.scroll.saturating_add(1),
            KeyAction::ScrollDown => self.scroll = self.scroll.saturating_sub(1),
            KeyAction::None | KeyAction::Submit | KeyAction::Char(_) | KeyAction::Backspace => {}
        }
    }

    fn submit(&mut self) {
        let prompt = self.input.take_trimmed();
        if prompt.is_empty() {
            return;
        }

        self.messages.push(Message::user(prompt.clone()));
        self.status = AgentStatus::Thinking;
        self.scroll = 0;
        agent::spawn_fake_loop(prompt, self.agent_tx.clone());
    }

    fn update_agent(&mut self, event: AgentEvent) {
        match event {
            AgentEvent::Started => {
                self.status = AgentStatus::Responding;
                self.messages.push(Message::assistant(""));
            }
            AgentEvent::AssistantDelta(delta) => self.append_assistant_delta(&delta),
            AgentEvent::AssistantFinished => self.status = AgentStatus::Idle,
        }
    }

    fn append_assistant_delta(&mut self, delta: &str) {
        if let Some(message) = self
            .messages
            .last_mut()
            .filter(|message| message.role == Role::Assistant)
        {
            message.content.push_str(delta);
        }
    }
}
