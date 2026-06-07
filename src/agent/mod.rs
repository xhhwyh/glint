mod fake_loop;

pub use fake_loop::spawn_fake_loop;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum AgentStatus {
    Idle,
    Thinking,
    Responding,
}

impl AgentStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Thinking => "thinking",
            Self::Responding => "responding",
        }
    }
}

pub enum AgentEvent {
    Started,
    AssistantDelta(String),
    AssistantFinished,
}
