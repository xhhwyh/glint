use crate::approval::ApprovalRequest;
use serde::{Deserialize, Serialize};

mod compact;
pub(crate) mod openai;
pub(crate) mod provider;

pub use crate::context::RuntimeContext;
pub use crate::query::{AgentRunInput, spawn_agent_loop, spawn_subagent_loop};
pub use compact::{CompactRunInput, should_auto_compact, spawn_compact_loop};

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
pub struct TokenUsage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
    pub cached_prompt_tokens: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentStatus {
    Idle,
    Thinking,
    Compacting,
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
        allowed_tool: Option<String>,
    },
    CompactStarted,
    CompactFinished {
        summary: String,
        pre_prompt_tokens: Option<u64>,
    },
    CompactFailed(String),
    AssistantFinished,
    Failed(String),
}
