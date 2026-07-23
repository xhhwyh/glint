use crate::progress::TodoUpdate;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    User,
    Assistant,
    Tool,
    Progress,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Message {
    pub role: Role,
    pub content: String,
    pub tool_call_id: Option<String>,
    pub tool_name: Option<String>,
    pub tool_input: Option<String>,
    pub tool_description: Option<String>,
    pub tool_finished: bool,
    pub progress: Option<TodoUpdate>,
}

impl Message {
    pub fn new(role: Role, content: impl Into<String>) -> Self {
        Self {
            role,
            content: content.into(),
            tool_call_id: None,
            tool_name: None,
            tool_input: None,
            tool_description: None,
            tool_finished: false,
            progress: None,
        }
    }

    pub fn user(content: impl Into<String>) -> Self {
        Self::new(Role::User, content)
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self::new(Role::Assistant, content)
    }

    pub fn tool_with_description(
        id: impl Into<String>,
        name: impl Into<String>,
        input: impl Into<String>,
        description: Option<String>,
    ) -> Self {
        Self {
            role: Role::Tool,
            content: String::new(),
            tool_call_id: Some(id.into()),
            tool_name: Some(name.into()),
            tool_input: Some(input.into()),
            tool_description: description,
            tool_finished: false,
            progress: None,
        }
    }

    pub fn progress(update: TodoUpdate) -> Self {
        Self {
            role: Role::Progress,
            content: String::new(),
            tool_call_id: None,
            tool_name: None,
            tool_input: None,
            tool_description: None,
            tool_finished: true,
            progress: Some(update),
        }
    }
}
