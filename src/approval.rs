use crate::input::InputState;
use serde_json::Value;
use std::collections::BTreeSet;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ApprovalChoice {
    Yes,
    Always,
    No,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ApprovalFocus {
    Options,
    Feedback,
}

#[derive(Clone, Debug)]
pub struct ApprovalPrompt {
    pub request: ApprovalRequest,
    pub selected: ApprovalChoice,
    pub focus: ApprovalFocus,
    pub feedback: InputState,
}

impl ApprovalPrompt {
    pub fn new(request: ApprovalRequest) -> Self {
        Self {
            request,
            selected: ApprovalChoice::Yes,
            focus: ApprovalFocus::Options,
            feedback: InputState::default(),
        }
    }

    pub fn move_up(&mut self) {
        self.focus = ApprovalFocus::Options;
        if self.request.is_mcp_elicitation() {
            self.selected = match self.selected {
                ApprovalChoice::Yes => ApprovalChoice::No,
                ApprovalChoice::Always | ApprovalChoice::No => ApprovalChoice::Yes,
            };
            return;
        }
        self.selected = match self.selected {
            ApprovalChoice::Yes => ApprovalChoice::No,
            ApprovalChoice::Always => ApprovalChoice::Yes,
            ApprovalChoice::No => ApprovalChoice::Always,
        };
    }

    pub fn move_down(&mut self) {
        self.focus = ApprovalFocus::Options;
        if self.request.is_mcp_elicitation() {
            self.selected = match self.selected {
                ApprovalChoice::Yes | ApprovalChoice::Always => ApprovalChoice::No,
                ApprovalChoice::No => ApprovalChoice::Yes,
            };
            return;
        }
        self.selected = match self.selected {
            ApprovalChoice::Yes => ApprovalChoice::Always,
            ApprovalChoice::Always => ApprovalChoice::No,
            ApprovalChoice::No => ApprovalChoice::Yes,
        };
    }

    pub fn focus_feedback(&mut self) {
        if (self.request.is_mcp_elicitation() && self.selected == ApprovalChoice::Yes)
            || (!self.request.is_mcp_elicitation() && self.selected == ApprovalChoice::No)
        {
            self.focus = ApprovalFocus::Feedback;
        }
    }

    pub fn decision(&self) -> ApprovalDecision {
        match self.selected {
            ApprovalChoice::Yes => ApprovalDecision::AllowOnce,
            ApprovalChoice::Always if self.request.tool_name == "Edit" => {
                ApprovalDecision::AllowConversation
            }
            ApprovalChoice::Always if self.request.tool_name == "Bash" => {
                ApprovalDecision::AllowProjectPrefix
            }
            ApprovalChoice::Always => ApprovalDecision::AllowConversationTool,
            ApprovalChoice::No => ApprovalDecision::Deny {
                feedback: self.feedback.value.trim().to_owned(),
            },
        }
    }

    pub fn always_label(&self) -> &'static str {
        if self.request.tool_name == "Edit" {
            "yes, always allow edits in this conversation"
        } else if self.request.tool_name == "Bash" {
            "yes, always allow in this project"
        } else {
            "yes, always allow this tool in this conversation"
        }
    }

    pub fn feedback_label(&self) -> &'static str {
        if self.request.is_mcp_elicitation() {
            "response JSON"
        } else {
            "feedback"
        }
    }
}

#[derive(Clone, Debug)]
pub struct ApprovalRequest {
    pub id: u64,
    pub tool_name: String,
    pub command: String,
    pub explanation: String,
    pub kind: ApprovalKind,
}

#[derive(Clone, Debug)]
pub enum ApprovalKind {
    Tool,
    McpElicitation { input_schema: Option<Value> },
}

impl ApprovalRequest {
    pub fn is_mcp_elicitation(&self) -> bool {
        matches!(self.kind, ApprovalKind::McpElicitation { .. })
    }

    pub fn elicitation_schema(&self) -> Option<&Value> {
        match &self.kind {
            ApprovalKind::McpElicitation { input_schema } => input_schema.as_ref(),
            ApprovalKind::Tool => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ApprovalDecision {
    AllowOnce,
    AllowProjectPrefix,
    AllowConversation,
    AllowConversationTool,
    Deny { feedback: String },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AgentControl {
    ApprovalDecision { id: u64, decision: ApprovalDecision },
    ClearConversationEditPermission,
    Cancel,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ConversationPermissions {
    pub edit_always_allowed: bool,
    pub allowed_tools: BTreeSet<String>,
}
