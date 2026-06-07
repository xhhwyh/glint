use std::sync::mpsc::{self, Receiver};

use crate::{
    agent::{self, AgentEvent, AgentStatus},
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
            | KeyAction::Right => {}
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

        self.messages.push(Message::user(prompt.clone()));
        self.status = AgentStatus::Thinking;
        self.scroll = 0;
        agent::spawn_agent_loop(prompt, self.config.llm.clone(), self.agent_tx.clone());
    }

    fn update_agent(&mut self, event: AgentEvent) {
        match event {
            AgentEvent::Started => {
                self.status = AgentStatus::Responding;
                self.messages.push(Message::assistant(""));
            }
            AgentEvent::AssistantDelta(delta) => self.append_assistant_delta(&delta),
            AgentEvent::AssistantFinished => self.status = AgentStatus::Idle,
            AgentEvent::Failed(error) => {
                self.append_assistant_delta(&error);
                self.status = AgentStatus::Idle;
            }
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

fn current_dir_label() -> String {
    std::env::current_dir()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|_| "?".to_owned())
}
