mod description;

use crate::{
    agent::provider::{ToolCall, ToolResult},
    progress::TodoUpdate,
};

use super::{ToolBehavior, utils};

pub struct TodoWriteTool;

impl ToolBehavior for TodoWriteTool {
    fn name(&self) -> &'static str {
        "TodoWrite"
    }

    fn description(&self) -> &'static str {
        description::DESCRIPTION
    }

    fn required_args(&self) -> &'static [&'static str] {
        &["todos"]
    }

    fn execute(&self, call: &ToolCall, _is_cancelled: &mut dyn FnMut() -> bool) -> ToolResult {
        todo_write(call)
    }

    fn requires_approval(
        &self,
        _call: &ToolCall,
        _bash_prefix_allowed: bool,
        _edit_allowed: bool,
    ) -> bool {
        false
    }

    fn input_summary(&self, call: &ToolCall) -> Option<String> {
        let update = TodoUpdate::from_tool_arguments(&call.arguments).ok()?;
        Some(format!("{} items", update.todos.len()))
    }
}

pub(super) fn todo_write(call: &ToolCall) -> ToolResult {
    match TodoUpdate::from_tool_arguments(&call.arguments) {
        Ok(update) => utils::ok(
            call,
            format!(
                "Progress checklist updated successfully. Continue to use TodoWrite as task status changes. {} item(s).",
                update.todos.len()
            ),
        ),
        Err(error) => utils::error(call, error),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn rejects_stale_pending_only_lists() {
        let result = todo_write(&ToolCall {
            id: "todo".to_owned(),
            name: "TodoWrite".to_owned(),
            arguments: json!({
                "todos": [
                    {"content": "Inspect", "active_form": "Inspecting", "status": "pending"}
                ]
            }),
        });

        assert!(result.is_error);
        assert!(result.content.contains("exactly one in_progress"));
    }

    #[test]
    fn accepts_completed_lists() {
        let result = todo_write(&ToolCall {
            id: "todo".to_owned(),
            name: "TodoWrite".to_owned(),
            arguments: json!({
                "todos": [
                    {"content": "Inspect", "active_form": "Inspecting", "status": "completed"}
                ]
            }),
        });

        assert!(!result.is_error);
        assert!(result.content.contains("updated successfully"));
    }
}
