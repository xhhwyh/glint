use std::time::{SystemTime, UNIX_EPOCH};

use crate::{agent::provider::ModelMessage, progress::TodoUpdate, tools::ShellToolMode};

const COMMON_TOOL_CONTEXT: &str = "Use paths relative to current_directory for files and directories under current_directory; use absolute paths only for targets outside current_directory. Do not use ~ in tool arguments. Use Read for known file contents. If you do not know the target file path, use narrow Glob or Grep first, then Read the discovered file paths. Use LSP for Rust symbol-aware questions such as definitions, references, hover documentation, document symbols, and workspace symbols. Only batch Read with Glob or Grep when the Read paths are already known from the user request or prior context. Do not start project summaries with broad root Glob patterns like **/*; read orientation files and manifests first. Glob results are capped at 100 files. Glob searches time out after 20 seconds by default, 60 seconds on WSL, or the positive value in GLINT_GLOB_TIMEOUT_SECONDS when set. Large tool outputs may be previewed and persisted outside the model context.";

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
    #[cfg(test)]
    pub fn current(current_dir: impl Into<String>, shell_tool_mode: ShellToolMode) -> Self {
        Self::with_time(current_time_label(), current_dir, shell_tool_mode)
    }

    pub fn with_time(
        current_time: impl Into<String>,
        current_dir: impl Into<String>,
        shell_tool_mode: ShellToolMode,
    ) -> Self {
        Self {
            current_time: current_time.into(),
            current_dir: current_dir.into(),
            shell: std::env::var("SHELL").unwrap_or_else(|_| "unknown".to_owned()),
            app_name: env!("CARGO_PKG_NAME").to_owned(),
            app_version: env!("CARGO_PKG_VERSION").to_owned(),
            tool_mode: tool_mode_context(shell_tool_mode),
        }
    }

    pub fn subagent_with_time(
        current_time: impl Into<String>,
        current_dir: impl Into<String>,
    ) -> Self {
        Self {
            current_time: current_time.into(),
            current_dir: current_dir.into(),
            shell: std::env::var("SHELL").unwrap_or_else(|_| "unknown".to_owned()),
            app_name: env!("CARGO_PKG_NAME").to_owned(),
            app_version: env!("CARGO_PKG_VERSION").to_owned(),
            tool_mode: subagent_tool_mode_context(),
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

fn subagent_tool_mode_context() -> String {
    format!(
        "available tools: Read, Glob, Grep, LSP, Bash, TerminalRun. {COMMON_TOOL_CONTEXT} Use Bash or TerminalRun for non-interactive shell-only commands such as git, build/test, package manager, environment, and process commands. Edit and nested Subagent are unavailable."
    )
}

fn tool_mode_context(shell_tool_mode: ShellToolMode) -> String {
    match shell_tool_mode {
        ShellToolMode::Bash => format!(
            "available tools: Read, Glob, Grep, LSP, Bash, Subagent, TaskList, TaskWait, TaskSend, TaskCancel, Edit, TodoWrite. {COMMON_TOOL_CONTEXT} Use Bash for non-interactive shell-only commands such as git, build/test, package manager, environment, and process commands. TerminalRun is unavailable until the user enables the visible terminal with /terminal."
        ),
        ShellToolMode::TerminalRun => format!(
            "available tools: Read, Glob, Grep, LSP, TerminalRun, Subagent, TaskList, TaskWait, TaskSend, TaskCancel, Edit, TodoWrite. {COMMON_TOOL_CONTEXT} Use TerminalRun for non-interactive shell-only commands such as git, build/test, package manager, environment, and process commands so the command and output are visible in the terminal. Bash is unavailable while terminal mode is enabled."
        ),
    }
}

pub fn build_initial_messages(
    system_prompt: &str,
    runtime_context: &RuntimeContext,
    conversation: &[ModelMessage],
    active_progress: Option<&TodoUpdate>,
    current_user_message: &str,
) -> Vec<ModelMessage> {
    let mut messages = vec![
        ModelMessage::system(system_prompt.trim().to_owned()),
        runtime_context.to_model_message(),
    ];

    messages.extend(conversation.iter().cloned());
    if let Some(progress) = active_progress.filter(|progress| !progress.todos.is_empty()) {
        messages.push(ModelMessage::user(progress.to_model_reminder()));
    }
    messages.push(ModelMessage::user(current_user_message.to_owned()));
    messages
}

pub fn current_time_label() -> String {
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
            tool_mode: tool_mode_context(ShellToolMode::Bash),
        }
    }

    #[test]
    fn places_system_prompt_then_runtime_context_before_history() {
        let messages = build_initial_messages("system", &runtime_context(), &[], None, "hello");

        assert_eq!(messages[0].role, ModelRole::System);
        assert_eq!(messages[0].content.as_deref(), Some("system"));
        assert_eq!(messages[1].role, ModelRole::User);
        assert!(
            messages[1]
                .content
                .as_deref()
                .is_some_and(|content| content.contains("<system-reminder>")
                    && content.contains("<runtime_context>"))
        );
        assert_eq!(messages[2].role, ModelRole::User);
        assert_eq!(messages[2].content.as_deref(), Some("hello"));
    }

    #[test]
    fn runtime_context_describes_active_shell_tool_mode() {
        let bash = RuntimeContext::current("/workspace", ShellToolMode::Bash);
        let terminal = RuntimeContext::current("/workspace", ShellToolMode::TerminalRun);

        assert!(
            bash.tool_mode
                .contains("Bash, Subagent, TaskList, TaskWait, TaskSend, TaskCancel, Edit")
        );
        assert!(bash.tool_mode.contains("TerminalRun is unavailable"));
        assert!(
            terminal
                .tool_mode
                .contains("TerminalRun, Subagent, TaskList, TaskWait, TaskSend, TaskCancel, Edit")
        );
        assert!(terminal.tool_mode.contains("Bash is unavailable"));
    }

    #[test]
    fn subagent_context_describes_limited_tool_surface() {
        let context = RuntimeContext::subagent_with_time("unix_seconds=1", "/workspace");

        assert!(
            context
                .tool_mode
                .contains("available tools: Read, Glob, Grep, LSP, Bash, TerminalRun")
        );
        assert!(
            context
                .tool_mode
                .contains("Edit and nested Subagent are unavailable")
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

        let messages = build_initial_messages(
            "system",
            &runtime_context(),
            &conversation,
            None,
            "second user",
        );

        assert_eq!(messages[2].role, ModelRole::User);
        assert_eq!(messages[2].content.as_deref(), Some("first user"));
        assert_eq!(messages[3].role, ModelRole::Tool);
        assert_eq!(messages[3].content.as_deref(), Some("tool output"));
        assert_eq!(messages[4].role, ModelRole::Assistant);
        assert_eq!(messages[4].content.as_deref(), Some("first assistant"));
        assert_eq!(messages[5].role, ModelRole::User);
        assert_eq!(messages[5].content.as_deref(), Some("second user"));
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

    #[test]
    fn places_progress_reminder_before_current_user() {
        let progress = TodoUpdate::from_tool_arguments(&serde_json::json!({
            "todos": [
                {"content": "Run tests", "active_form": "Running tests", "status": "in_progress"}
            ]
        }))
        .unwrap();

        let messages =
            build_initial_messages("system", &runtime_context(), &[], Some(&progress), "next");

        assert_eq!(messages[2].role, ModelRole::User);
        assert!(
            messages[2]
                .content
                .as_deref()
                .is_some_and(|content| content.contains("Current progress checklist"))
        );
        assert_eq!(messages[3].content.as_deref(), Some("next"));
    }
}
