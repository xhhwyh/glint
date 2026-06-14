use crate::approval::ApprovalRequest;
use serde::{Deserialize, Serialize};

mod context;
mod openai;
pub(crate) mod provider;
mod runner;
mod tools;

pub use context::RuntimeContext;
pub use runner::{AgentRunInput, spawn_agent_loop};

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
pub struct TokenUsage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
    pub cached_prompt_tokens: Option<u64>,
}

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
    AssistantTurn {
        usage: Option<TokenUsage>,
        finish_reason: provider::FinishReason,
        tool_calls: Vec<provider::ToolCall>,
    },
    ToolStarted {
        id: String,
        name: String,
        input_summary: String,
        input_description: Option<String>,
    },
    ToolFinished {
        id: String,
        name: String,
        output: String,
        is_error: bool,
        output_summary: String,
    },
    ToolApprovalRequested(ApprovalRequest),
    ConversationPermissionChanged {
        edit_always_allowed: bool,
    },
    AssistantFinished,
    Failed(String),
}
