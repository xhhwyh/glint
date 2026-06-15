use std::fs;

use crate::agent::provider::{ToolCall, ToolResult};

mod description;

use super::{
    ToolBehavior,
    utils::{
        error, missing_arg, ok, path_arg, requires_path_approval, resolve_tool_path, string_arg,
    },
};

pub(super) struct EditTool;

impl ToolBehavior for EditTool {
    fn name(&self) -> &'static str {
        "Edit"
    }

    fn description(&self) -> &'static str {
        description::DESCRIPTION
    }

    fn required_args(&self) -> &'static [&'static str] {
        description::REQUIRED_ARGS
    }

    fn execute(&self, call: &ToolCall, _is_cancelled: &mut dyn FnMut() -> bool) -> ToolResult {
        edit(call)
    }

    fn execute_approved(
        &self,
        call: &ToolCall,
        _is_cancelled: &mut dyn FnMut() -> bool,
    ) -> ToolResult {
        edit_approved(call)
    }

    fn requires_approval(
        &self,
        call: &ToolCall,
        _bash_prefix_allowed: bool,
        edit_allowed: bool,
    ) -> bool {
        requires_path_approval(call) || !edit_allowed
    }

    fn input_summary(&self, call: &ToolCall) -> Option<String> {
        path_arg(call, "file_path")
    }
}

fn edit(call: &ToolCall) -> ToolResult {
    error(
        call,
        "Approval required before editing files with Edit.".to_owned(),
    )
}

fn edit_approved(call: &ToolCall) -> ToolResult {
    let Some(path) = string_arg(call, "file_path") else {
        return missing_arg(call, "file_path");
    };
    let Some(old) = string_arg(call, "old_string") else {
        return missing_arg(call, "old_string");
    };
    let Some(new) = string_arg(call, "new_string") else {
        return missing_arg(call, "new_string");
    };

    let path = match resolve_tool_path(path) {
        Ok(path) => path,
        Err(message) => return error(call, message),
    };

    let Ok(content) = fs::read_to_string(&path) else {
        return error(call, format!("failed to read {}", path.display()));
    };
    let count = content.matches(old).count();
    if count != 1 {
        return error(
            call,
            format!("expected one match in {}, found {count}", path.display()),
        );
    }

    match fs::write(&path, content.replacen(old, new, 1)) {
        Ok(()) => ok(call, format!("Edited {}", path.display())),
        Err(err) => error(call, format!("failed to write {}: {err}", path.display())),
    }
}
