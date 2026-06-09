use std::time::{SystemTime, UNIX_EPOCH};

use crate::message::{Message, Role};

use super::provider::ModelMessage;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuntimeContext {
    pub current_time: String,
    pub current_dir: String,
    pub shell: String,
    pub app_name: String,
    pub app_version: String,
    pub tool_mode: String,
}

impl RuntimeContext {
    pub fn current(current_dir: impl Into<String>) -> Self {
        Self {
            current_time: current_time_label(),
            current_dir: current_dir.into(),
            shell: std::env::var("SHELL").unwrap_or_else(|_| "unknown".to_owned()),
            app_name: env!("CARGO_PKG_NAME").to_owned(),
            app_version: env!("CARGO_PKG_VERSION").to_owned(),
            tool_mode: "available tools: Read, Glob, Grep, Bash, Edit. Bash can run read-only or non-destructive commands without approval; modifying commands require approval. Edit always requires approval.".to_owned(),
        }
    }

    pub fn to_model_message(&self) -> ModelMessage {
        ModelMessage::user(format!(
            "<runtime_context>\ncurrent_time: {}\ncurrent_directory: {}\nshell: {}\napp: {} {}\ntool_mode: {}\n</runtime_context>",
            self.current_time,
            self.current_dir,
            self.shell,
            self.app_name,
            self.app_version,
            self.tool_mode
        ))
    }
}

pub fn build_initial_messages(
    system_prompt: &str,
    runtime_context: &RuntimeContext,
    conversation: &[Message],
    current_user_message: &str,
) -> Vec<ModelMessage> {
    let mut messages = vec![
        ModelMessage::system(system_prompt.trim().to_owned()),
        runtime_context.to_model_message(),
    ];

    messages.extend(conversation.iter().filter_map(model_message_from_visible));
    messages.push(ModelMessage::user(current_user_message.to_owned()));
    messages
}

fn model_message_from_visible(message: &Message) -> Option<ModelMessage> {
    match message.role {
        Role::User => Some(ModelMessage::user(message.content.clone())),
        Role::Assistant => Some(ModelMessage::assistant(
            Some(message.content.clone()),
            Vec::new(),
        )),
        Role::Tool => None,
    }
}

fn current_time_label() -> String {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(duration) => format!("unix_seconds={}", duration.as_secs()),
        Err(_) => "unavailable".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::Message;
    use crate::message::Role;

    fn runtime_context() -> RuntimeContext {
        RuntimeContext {
            current_time: "unix_seconds=1".to_owned(),
            current_dir: "/workspace".to_owned(),
            shell: "/bin/zsh".to_owned(),
            app_name: "glint".to_owned(),
            app_version: "0.1.0".to_owned(),
            tool_mode: "available tools: Read, Glob, Grep, Bash, Edit. Bash can run read-only or non-destructive commands without approval; modifying commands require approval. Edit always requires approval.".to_owned(),
        }
    }

    #[test]
    fn places_system_prompt_first_and_runtime_context_second() {
        let messages = build_initial_messages("system", &runtime_context(), &[], "hello");

        assert_eq!(messages[0].role, super::super::provider::ModelRole::System);
        assert_eq!(messages[0].content.as_deref(), Some("system"));
        assert_eq!(messages[1].role, super::super::provider::ModelRole::User);
        assert!(
            messages[1]
                .content
                .as_deref()
                .is_some_and(|content| content.contains("<runtime_context>"))
        );
    }

    #[test]
    fn preserves_prior_visible_history_then_current_user_once() {
        let conversation = vec![
            Message::new(Role::User, "first user"),
            Message::tool("tool-one", "Read", r#"{"file_path":"src/main.rs"}"#),
            Message::new(Role::Assistant, "first assistant"),
        ];

        let messages =
            build_initial_messages("system", &runtime_context(), &conversation, "second user");

        assert_eq!(messages[2].role, super::super::provider::ModelRole::User);
        assert_eq!(messages[2].content.as_deref(), Some("first user"));
        assert_eq!(
            messages[3].role,
            super::super::provider::ModelRole::Assistant
        );
        assert_eq!(messages[3].content.as_deref(), Some("first assistant"));
        assert_eq!(messages[4].role, super::super::provider::ModelRole::User);
        assert_eq!(messages[4].content.as_deref(), Some("second user"));
        assert_eq!(
            messages
                .iter()
                .filter(|message| message.content.as_deref() == Some("second user"))
                .count(),
            1
        );
    }
}
