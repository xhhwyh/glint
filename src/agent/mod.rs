use crate::approval::ApprovalRequest;

mod context;
mod openai;
pub(crate) mod provider;
mod runner;
mod tools;

pub use context::RuntimeContext;
pub use runner::{AgentRunInput, spawn_agent_loop};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum AgentStatus {
    Idle,
    Thinking,
    Responding,
    AwaitingApproval,
}

pub enum AgentEvent {
    Started,
    AssistantDelta(String),
    ToolStarted {
        id: String,
        name: String,
        input_summary: String,
    },
    ToolFinished {
        id: String,
        name: String,
        output_summary: String,
    },
    ToolApprovalRequested(ApprovalRequest),
    ConversationPermissionChanged {
        edit_always_allowed: bool,
    },
    AssistantFinished,
    Failed(String),
}
