use crate::input::InputState;

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
        self.selected = match self.selected {
            ApprovalChoice::Yes => ApprovalChoice::No,
            ApprovalChoice::Always => ApprovalChoice::Yes,
            ApprovalChoice::No => ApprovalChoice::Always,
        };
    }

    pub fn move_down(&mut self) {
        self.focus = ApprovalFocus::Options;
        self.selected = match self.selected {
            ApprovalChoice::Yes => ApprovalChoice::Always,
            ApprovalChoice::Always => ApprovalChoice::No,
            ApprovalChoice::No => ApprovalChoice::Yes,
        };
    }

    pub fn focus_feedback(&mut self) {
        if self.selected == ApprovalChoice::No {
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
            ApprovalChoice::Always => ApprovalDecision::AllowOnce,
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
            "yes"
        }
    }
}

#[derive(Clone, Debug)]
pub struct ApprovalRequest {
    pub id: u64,
    pub tool_name: String,
    pub command: String,
    pub explanation: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ApprovalDecision {
    AllowOnce,
    AllowProjectPrefix,
    AllowConversation,
    Deny { feedback: String },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AgentControl {
    ApprovalDecision { id: u64, decision: ApprovalDecision },
    ClearConversationEditPermission,
    Cancel,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ConversationPermissions {
    pub edit_always_allowed: bool,
}
