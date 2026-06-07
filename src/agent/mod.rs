mod openai;

pub use openai::spawn_agent_loop;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum AgentStatus {
    Idle,
    Thinking,
    Responding,
}

pub enum AgentEvent {
    Started,
    AssistantDelta(String),
    AssistantFinished,
    Failed(String),
}
