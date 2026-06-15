use std::process::Command;

use crate::agent::provider::{ToolCall, ToolResult};

mod description;

use super::{
    ToolBehavior,
    utils::{command_result, error, missing_arg, resolve_tool_path, string_arg},
};

pub(super) struct GrepTool;

impl ToolBehavior for GrepTool {
    fn name(&self) -> &'static str {
        "Grep"
    }

    fn description(&self) -> &'static str {
        description::DESCRIPTION
    }

    fn required_args(&self) -> &'static [&'static str] {
        description::REQUIRED_ARGS
    }

    fn execute(&self, call: &ToolCall, is_cancelled: &mut dyn FnMut() -> bool) -> ToolResult {
        grep(call, is_cancelled)
    }

    fn is_concurrency_safe(&self, _call: &ToolCall) -> bool {
        true
    }

    fn input_summary(&self, call: &ToolCall) -> Option<String> {
        string_arg(call, "pattern").map(str::to_owned)
    }
}

fn grep(call: &ToolCall, is_cancelled: &mut dyn FnMut() -> bool) -> ToolResult {
    let Some(pattern) = string_arg(call, "pattern") else {
        return missing_arg(call, "pattern");
    };

    let path = string_arg(call, "path").unwrap_or(".");
    let path = match resolve_tool_path(path) {
        Ok(path) => path,
        Err(message) => return error(call, message),
    };
    let mut command = Command::new("rg");
    command
        .args(["--line-number", "--with-filename", pattern])
        .arg(path);
    if let Some(glob) = string_arg(call, "glob") {
        command.args(["-g", glob]);
    }
    command_result(call, &mut command, is_cancelled)
}
