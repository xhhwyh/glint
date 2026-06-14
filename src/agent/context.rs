use std::time::{SystemTime, UNIX_EPOCH};

use super::provider::ModelMessage;

const TOOL_MODE_CONTEXT: &str = "available tools: Read, Glob, Grep, Bash, Edit. Use paths relative to current_directory for files and directories under current_directory; use absolute paths only for targets outside current_directory. Do not use ~ in tool arguments. Use Read for known file contents. If you do not know the target file path, use narrow Glob or Grep first, then Read the discovered file paths. Only batch Read with Glob or Grep when the Read paths are already known from the user request or prior context. Do not start project summaries with broad root Glob patterns like **/*; read orientation files and manifests first. Glob results are capped at 100 files. Glob searches time out after 20 seconds by default, 60 seconds on WSL, or the positive value in CLAUDE_CODE_GLOB_TIMEOUT_SECONDS when set. Large tool outputs may be previewed and persisted outside the model context. Use Bash only for shell-only commands such as git, build/test, package manager, environment, and process commands.";

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
            tool_mode: TOOL_MODE_CONTEXT.to_owned(),
        }
    }

    pub fn to_model_message(&self) -> ModelMessage {
        ModelMessage::user(format!(
            "<system-reminder>\nAs you answer the user's current request, you can use the following runtime context. This context may or may not be relevant; do not respond to it directly unless it is relevant.\n<runtime_context>\ncurrent_time: {}\ncurrent_directory: {}\nshell: {}\napp: {} {}\ntool_mode: {}\n</runtime_context>\n</system-reminder>",
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
    conversation: &[ModelMessage],
    current_user_message: &str,
) -> Vec<ModelMessage> {
    let mut messages = vec![
        ModelMessage::system(system_prompt.trim().to_owned()),
        runtime_context.to_model_message(),
    ];

    messages.extend(conversation.iter().cloned());
    messages.push(ModelMessage::user(format!(
        "<current_user_request>\n{}\n</current_user_request>",
        current_user_message
    )));
    messages
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
    use crate::agent::provider::{ModelRole, ToolResult};

    fn runtime_context() -> RuntimeContext {
        RuntimeContext {
            current_time: "unix_seconds=1".to_owned(),
            current_dir: "/workspace".to_owned(),
            shell: "/bin/zsh".to_owned(),
            app_name: "glint".to_owned(),
            app_version: "0.1.0".to_owned(),
            tool_mode: TOOL_MODE_CONTEXT.to_owned(),
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
                .is_some_and(|content| content.contains("<system-reminder>")
                    && content.contains("<runtime_context>"))
        );
    }

    #[test]
    fn preserves_prior_visible_history_then_current_user_once() {
        let conversation = vec![
            ModelMessage::user("first user"),
            ModelMessage::tool_result(&ToolResult {
                call_id: "tool-one".to_owned(),
                content: "tool output".to_owned(),
                is_error: false,
            }),
            ModelMessage::assistant(Some("first assistant".to_owned()), Vec::new()),
        ];

        let messages =
            build_initial_messages("system", &runtime_context(), &conversation, "second user");

        assert_eq!(messages[2].role, ModelRole::User);
        assert_eq!(messages[2].content.as_deref(), Some("first user"));
        assert_eq!(messages[3].role, ModelRole::Tool);
        assert_eq!(messages[3].content.as_deref(), Some("tool output"));
        assert_eq!(messages[4].role, ModelRole::Assistant);
        assert_eq!(messages[4].content.as_deref(), Some("first assistant"));
        assert_eq!(messages[5].role, ModelRole::User);
        assert_eq!(
            messages[5].content.as_deref(),
            Some("<current_user_request>\nsecond user\n</current_user_request>")
        );
        assert_eq!(
            messages
                .iter()
                .filter(|message| message
                    .content
                    .as_deref()
                    .is_some_and(|content| content.contains("second user")))
                .count(),
            1
        );
    }
}
