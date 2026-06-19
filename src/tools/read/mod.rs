use std::fs;

use crate::agent::provider::{ToolCall, ToolResult};

mod description;

use super::{
    ToolBehavior,
    read_state::ReadFileState,
    utils::{error, missing_arg, ok, path_arg, resolve_tool_path, slice_lines, string_arg},
};

pub(super) struct ReadTool;

impl ToolBehavior for ReadTool {
    fn name(&self) -> &'static str {
        "Read"
    }

    fn description(&self) -> &'static str {
        description::DESCRIPTION
    }

    fn required_args(&self) -> &'static [&'static str] {
        description::REQUIRED_ARGS
    }

    fn execute(&self, call: &ToolCall, _is_cancelled: &mut dyn FnMut() -> bool) -> ToolResult {
        read(call, &ReadFileState::new())
    }

    fn is_concurrency_safe(&self, _call: &ToolCall) -> bool {
        true
    }

    fn input_summary(&self, call: &ToolCall) -> Option<String> {
        path_arg(call, "file_path")
    }
}

pub(super) fn read(call: &ToolCall, read_file_state: &ReadFileState) -> ToolResult {
    let Some(path) = string_arg(call, "file_path") else {
        return missing_arg(call, "file_path");
    };

    let path = match resolve_tool_path(path) {
        Ok(path) => path,
        Err(message) => return error(call, message),
    };

    match fs::read_to_string(&path) {
        Ok(content) => {
            read_file_state.record(path.clone(), content.clone(), is_partial_read(call));
            ok(call, slice_lines(content, call))
        }
        Err(err) => error(call, format!("failed to read {}: {err}", path.display())),
    }
}

fn is_partial_read(call: &ToolCall) -> bool {
    call.arguments.get("offset").is_some() || call.arguments.get("limit").is_some()
}
