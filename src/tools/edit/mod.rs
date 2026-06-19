use std::fs;

use crate::agent::provider::{ToolCall, ToolResult};

mod description;

use super::{
    ToolBehavior,
    read_state::ReadFileState,
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
        edit_approved(call, &ReadFileState::new())
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

pub(super) fn edit(call: &ToolCall) -> ToolResult {
    error(
        call,
        "Approval required before editing files with Edit.".to_owned(),
    )
}

pub(super) fn edit_approved(call: &ToolCall, read_file_state: &ReadFileState) -> ToolResult {
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
    if let Err(message) = validate_read_state(&path, &content, read_file_state) {
        return error(call, message);
    }
    let count = content.matches(old).count();
    if count != 1 {
        return error(
            call,
            format!("expected one match in {}, found {count}", path.display()),
        );
    }

    let updated = content.replacen(old, new, 1);
    match fs::write(&path, &updated) {
        Ok(()) => {
            read_file_state.record(path.clone(), updated, false);
            ok(call, format!("Edited {}", path.display()))
        }
        Err(err) => error(call, format!("failed to write {}: {err}", path.display())),
    }
}

fn validate_read_state(
    path: &std::path::Path,
    current_content: &str,
    read_file_state: &ReadFileState,
) -> Result<(), String> {
    let Some(record) = read_file_state.get(path) else {
        return Err(format!(
            "{} has not been read yet. Use Read on the full file before editing it.",
            path.display()
        ));
    };
    if record.partial {
        return Err(format!(
            "{} was only partially read. Use Read without offset or limit before editing it.",
            path.display()
        ));
    }

    let current_modified = fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .ok();
    let modified_after_read = match (record.modified, current_modified) {
        (Some(read_modified), Some(current_modified)) => current_modified > read_modified,
        _ => false,
    };
    if modified_after_read && record.content != current_content {
        return Err(format!(
            "{} changed after it was read. Read it again before editing it.",
            path.display()
        ));
    }

    if record.content != current_content {
        return Err(format!(
            "{} content no longer matches the last full Read result. Read it again before editing it.",
            path.display()
        ));
    }

    Ok(())
}
