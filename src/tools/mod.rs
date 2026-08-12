mod bash;
mod edit;
mod glob;
mod grep;
mod lsp;
mod read;
mod read_state;
mod subagent;
mod task_control;
mod terminal_run;
mod todo_write;
mod utils;

use std::{collections::BTreeMap, sync::Arc, sync::mpsc::Sender};

use serde_json::{Value, json};

use crate::agent::provider::{ToolCall, ToolResult, ToolSpec};
use crate::services::lsp::LspManager;
use crate::terminal::TerminalRequest;

use bash::BashTool;
use edit::EditTool;
use glob::{GLOB_MAX_LIMIT, GlobTool};
use grep::GrepTool;
use lsp::LspTool;
use read::ReadTool;
pub use read_state::ReadFileState;
use subagent::SubagentTool;
use task_control::{TaskCancelTool, TaskListTool, TaskSendTool, TaskWaitTool};
use terminal_run::TerminalRunTool;
use todo_write::TodoWriteTool;
pub(crate) use utils::with_tool_cwd;
use utils::{error, normalize_path_argument, requires_path_approval, truncate_summary};

pub(crate) fn sanitize_tool_name(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '_' {
                character
            } else {
                '_'
            }
        })
        .collect()
}

#[derive(Clone)]
pub struct ToolRegistry {
    terminal_requests: Option<Sender<TerminalRequest>>,
    lsp_manager: Option<LspManager>,
    shell_tool_mode: ShellToolMode,
    read_file_state: ReadFileState,
    dynamic_tools: Arc<BTreeMap<String, Arc<dyn DynamicTool>>>,
    subagent: bool,
}

pub trait DynamicTool: Send + Sync {
    fn spec(&self) -> ToolSpec;
    fn execute(&self, call: &ToolCall, is_cancelled: &mut dyn FnMut() -> bool) -> ToolResult;

    fn requires_approval(&self, _call: &ToolCall) -> bool {
        true
    }

    fn is_concurrency_safe(&self, _call: &ToolCall) -> bool {
        false
    }

    fn input_summary(&self, call: &ToolCall) -> String {
        call.arguments.to_string()
    }

    fn input_description(&self, _call: &ToolCall) -> Option<String> {
        None
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShellToolMode {
    Bash,
    TerminalRun,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self {
            terminal_requests: None,
            lsp_manager: None,
            shell_tool_mode: ShellToolMode::Bash,
            read_file_state: ReadFileState::new(),
            dynamic_tools: Arc::new(BTreeMap::new()),
            subagent: false,
        }
    }

    #[cfg(test)]
    pub fn with_terminal_requests(terminal_requests: Sender<TerminalRequest>) -> Self {
        Self {
            terminal_requests: Some(terminal_requests),
            lsp_manager: None,
            shell_tool_mode: ShellToolMode::TerminalRun,
            read_file_state: ReadFileState::new(),
            dynamic_tools: Arc::new(BTreeMap::new()),
            subagent: false,
        }
    }

    pub fn with_shell_tool(
        shell_tool_mode: ShellToolMode,
        terminal_requests: Option<Sender<TerminalRequest>>,
    ) -> Self {
        Self {
            terminal_requests,
            lsp_manager: None,
            shell_tool_mode,
            read_file_state: ReadFileState::new(),
            dynamic_tools: Arc::new(BTreeMap::new()),
            subagent: false,
        }
    }

    pub fn for_subagent(terminal_requests: Option<Sender<TerminalRequest>>) -> Self {
        Self {
            terminal_requests,
            lsp_manager: None,
            shell_tool_mode: ShellToolMode::TerminalRun,
            read_file_state: ReadFileState::new(),
            dynamic_tools: Arc::new(BTreeMap::new()),
            subagent: true,
        }
    }

    pub fn with_lsp_manager(mut self, lsp_manager: LspManager) -> Self {
        self.lsp_manager = Some(lsp_manager);
        self
    }

    pub fn with_read_file_state(mut self, read_file_state: ReadFileState) -> Self {
        self.read_file_state = read_file_state;
        self
    }

    pub fn with_dynamic_tools(mut self, tools: Vec<Arc<dyn DynamicTool>>) -> Self {
        let mut registered = BTreeMap::new();
        for tool in tools {
            let name = tool.spec().name;
            if tool_metadata_for_name(&name).is_none() {
                registered.insert(name, tool);
            }
        }
        self.dynamic_tools = Arc::new(registered);
        self
    }

    pub fn specs(&self) -> Vec<ToolSpec> {
        let mut specs = self
            .tools()
            .into_iter()
            .map(|tool| tool.spec())
            .collect::<Vec<_>>();
        specs.extend(self.dynamic_tools.values().map(|tool| tool.spec()));
        specs
    }

    #[cfg(test)]
    pub fn execute(&self, call: &ToolCall) -> ToolResult {
        self.execute_with_cancel(call, &mut || false)
    }

    pub fn execute_with_cancel(
        &self,
        call: &ToolCall,
        is_cancelled: &mut dyn FnMut() -> bool,
    ) -> ToolResult {
        if call.name == "TerminalRun" {
            if self.shell_tool_mode != ShellToolMode::TerminalRun && !self.subagent {
                return error(call, "Tool 'TerminalRun' is not registered.".to_owned());
            }
            return terminal_run::terminal_run(
                call,
                self.terminal_requests.as_ref(),
                is_cancelled,
                false,
            );
        }
        if call.name == "Bash" && self.shell_tool_mode != ShellToolMode::Bash && !self.subagent {
            return error(call, "Tool 'Bash' is not registered.".to_owned());
        }
        if call.name == "Subagent" {
            if self.subagent {
                return error(call, "Nested Subagent is not registered.".to_owned());
            }
            return subagent::subagent(call, self.terminal_requests.as_ref());
        }
        if matches!(
            call.name.as_str(),
            "TaskList" | "TaskWait" | "TaskSend" | "TaskCancel"
        ) {
            if self.subagent {
                return error(
                    call,
                    "Task control is not registered inside subagents.".to_owned(),
                );
            }
            return task_control::execute(call, self.terminal_requests.as_ref(), is_cancelled);
        }
        if call.name == "Read" {
            return read::read(call, &self.read_file_state);
        }
        if call.name == "LSP" {
            return lsp::lsp(call, self.lsp_manager.as_ref());
        }
        if call.name == "Edit" {
            if self.subagent {
                return error(call, "Edit is not registered inside subagents.".to_owned());
            }
            return edit::edit(call);
        }
        if call.name == "TodoWrite" {
            return todo_write::todo_write(call);
        }
        if let Some(tool) = self.dynamic_tools.get(&call.name) {
            return tool.execute(call, is_cancelled);
        }
        self.tool_for_name(&call.name)
            .map(|tool| tool.execute(call, is_cancelled))
            .unwrap_or_else(|| error(call, format!("Tool '{}' is not registered.", call.name)))
    }

    pub fn requires_approval(
        &self,
        call: &ToolCall,
        bash_prefix_allowed: bool,
        edit_allowed: bool,
    ) -> bool {
        self.dynamic_tools.get(&call.name).map_or_else(
            || {
                self.tool_for_name(&call.name).is_some_and(|tool| {
                    tool.requires_approval(call, bash_prefix_allowed, edit_allowed)
                })
            },
            |tool| tool.requires_approval(call),
        )
    }

    #[cfg(test)]
    pub fn execute_approved(&self, call: &ToolCall) -> ToolResult {
        self.execute_approved_with_cancel(call, &mut || false)
    }

    pub fn execute_approved_with_cancel(
        &self,
        call: &ToolCall,
        is_cancelled: &mut dyn FnMut() -> bool,
    ) -> ToolResult {
        if call.name == "TerminalRun" {
            if self.shell_tool_mode != ShellToolMode::TerminalRun && !self.subagent {
                return error(call, "Tool 'TerminalRun' is not registered.".to_owned());
            }
            return terminal_run::terminal_run(
                call,
                self.terminal_requests.as_ref(),
                is_cancelled,
                true,
            );
        }
        if call.name == "Bash" && self.shell_tool_mode != ShellToolMode::Bash && !self.subagent {
            return error(call, "Tool 'Bash' is not registered.".to_owned());
        }
        if call.name == "Subagent" {
            if self.subagent {
                return error(call, "Nested Subagent is not registered.".to_owned());
            }
            return subagent::subagent(call, self.terminal_requests.as_ref());
        }
        if matches!(
            call.name.as_str(),
            "TaskList" | "TaskWait" | "TaskSend" | "TaskCancel"
        ) {
            if self.subagent {
                return error(
                    call,
                    "Task control is not registered inside subagents.".to_owned(),
                );
            }
            return task_control::execute(call, self.terminal_requests.as_ref(), is_cancelled);
        }
        if call.name == "Read" {
            return read::read(call, &self.read_file_state);
        }
        if call.name == "LSP" {
            return lsp::lsp(call, self.lsp_manager.as_ref());
        }
        if call.name == "Edit" {
            if self.subagent {
                return error(call, "Edit is not registered inside subagents.".to_owned());
            }
            return edit::edit_approved(call, &self.read_file_state, self.lsp_manager.as_ref());
        }
        if call.name == "TodoWrite" {
            return todo_write::todo_write(call);
        }
        if let Some(tool) = self.dynamic_tools.get(&call.name) {
            return tool.execute(call, is_cancelled);
        }
        self.tool_for_name(&call.name)
            .map(|tool| tool.execute_approved(call, is_cancelled))
            .unwrap_or_else(|| self.execute_with_cancel(call, is_cancelled))
    }

    pub fn is_concurrency_safe(&self, call: &ToolCall) -> bool {
        self.dynamic_tools.get(&call.name).map_or_else(
            || {
                self.tool_for_name(&call.name)
                    .is_some_and(|tool| tool.is_concurrency_safe(call))
            },
            |tool| tool.is_concurrency_safe(call),
        )
    }

    pub fn input_summary(&self, call: &ToolCall) -> String {
        let summary = self
            .dynamic_tools
            .get(&call.name)
            .map(|tool| tool.input_summary(call))
            .or_else(|| {
                tool_metadata_for_name(&call.name).and_then(|tool| tool.input_summary(call))
            })
            .unwrap_or_else(|| call.arguments.to_string());
        truncate_summary(&summary)
    }

    pub fn input_description(&self, call: &ToolCall) -> Option<String> {
        self.dynamic_tools
            .get(&call.name)
            .and_then(|tool| tool.input_description(call))
            .or_else(|| {
                tool_metadata_for_name(&call.name).and_then(|tool| tool.input_description(call))
            })
            .map(|description| truncate_summary(&description))
    }

    pub fn output_summary(&self, output: &str) -> String {
        truncate_summary(output)
    }

    pub fn normalize_for_context(&self, call: &ToolCall) -> ToolCall {
        let mut call = call.clone();
        match call.name.as_str() {
            "Read" | "Edit" => normalize_path_argument(&mut call.arguments, "file_path"),
            "Glob" | "Grep" => normalize_path_argument(&mut call.arguments, "path"),
            "LSP" => normalize_path_argument(&mut call.arguments, "file_path"),
            "Subagent" => normalize_path_argument(&mut call.arguments, "cwd"),
            _ => {}
        }
        call
    }

    fn tools(&self) -> Vec<&'static dyn ToolBehavior> {
        if self.subagent {
            return vec![
                &READ_TOOL,
                &GLOB_TOOL,
                &GREP_TOOL,
                &LSP_TOOL,
                &BASH_TOOL,
                &TERMINAL_RUN_TOOL,
            ];
        }
        let shell_tool: &'static dyn ToolBehavior = match self.shell_tool_mode {
            ShellToolMode::Bash => &BASH_TOOL,
            ShellToolMode::TerminalRun => &TERMINAL_RUN_TOOL,
        };
        vec![
            &READ_TOOL,
            &GLOB_TOOL,
            &GREP_TOOL,
            &LSP_TOOL,
            shell_tool,
            &SUBAGENT_TOOL,
            &TASK_LIST_TOOL,
            &TASK_WAIT_TOOL,
            &TASK_SEND_TOOL,
            &TASK_CANCEL_TOOL,
            &EDIT_TOOL,
            &TODO_WRITE_TOOL,
        ]
    }

    fn tool_for_name(&self, name: &str) -> Option<&'static dyn ToolBehavior> {
        self.tools().into_iter().find(|tool| tool.name() == name)
    }
}

pub(super) trait ToolBehavior: Sync {
    fn name(&self) -> &'static str;
    fn description(&self) -> &'static str;
    fn required_args(&self) -> &'static [&'static str];
    fn execute(&self, call: &ToolCall, is_cancelled: &mut dyn FnMut() -> bool) -> ToolResult;

    fn spec(&self) -> ToolSpec {
        spec(self.name(), self.description(), self.required_args())
    }

    fn execute_approved(
        &self,
        call: &ToolCall,
        is_cancelled: &mut dyn FnMut() -> bool,
    ) -> ToolResult {
        self.execute(call, is_cancelled)
    }

    fn requires_approval(
        &self,
        call: &ToolCall,
        _bash_prefix_allowed: bool,
        _edit_allowed: bool,
    ) -> bool {
        requires_path_approval(call)
    }

    fn is_concurrency_safe(&self, _call: &ToolCall) -> bool {
        false
    }

    fn input_summary(&self, _call: &ToolCall) -> Option<String> {
        None
    }

    fn input_description(&self, _call: &ToolCall) -> Option<String> {
        None
    }
}

static READ_TOOL: ReadTool = ReadTool;
static GLOB_TOOL: GlobTool = GlobTool;
static GREP_TOOL: GrepTool = GrepTool;
static LSP_TOOL: LspTool = LspTool;
static BASH_TOOL: BashTool = BashTool;
static EDIT_TOOL: EditTool = EditTool;
static TERMINAL_RUN_TOOL: TerminalRunTool = TerminalRunTool;
static SUBAGENT_TOOL: SubagentTool = SubagentTool;
static TASK_LIST_TOOL: TaskListTool = TaskListTool;
static TASK_WAIT_TOOL: TaskWaitTool = TaskWaitTool;
static TASK_SEND_TOOL: TaskSendTool = TaskSendTool;
static TASK_CANCEL_TOOL: TaskCancelTool = TaskCancelTool;
static TODO_WRITE_TOOL: TodoWriteTool = TodoWriteTool;

static ALL_TOOLS: [&dyn ToolBehavior; 13] = [
    &READ_TOOL,
    &GLOB_TOOL,
    &GREP_TOOL,
    &LSP_TOOL,
    &TERMINAL_RUN_TOOL,
    &BASH_TOOL,
    &SUBAGENT_TOOL,
    &TASK_LIST_TOOL,
    &TASK_WAIT_TOOL,
    &TASK_SEND_TOOL,
    &TASK_CANCEL_TOOL,
    &EDIT_TOOL,
    &TODO_WRITE_TOOL,
];

fn tool_metadata_for_name(name: &str) -> Option<&'static dyn ToolBehavior> {
    ALL_TOOLS.iter().copied().find(|tool| tool.name() == name)
}

fn spec(name: &str, description: &str, required: &[&str]) -> ToolSpec {
    let mut properties = json!({
        "file_path": { "type": "string", "description": "File path. Use a path relative to current_directory when the file is under current_directory; use an absolute path only outside it. Do not use ~." },
        "pattern": { "type": "string" },
        "path": { "type": "string", "description": "Search path. Use a path relative to current_directory when the directory is under current_directory; use an absolute path only outside it. Do not use ~." },
        "glob": { "type": "string", "description": "Optional file glob filter." },
        "operation": { "type": "string" },
        "line": { "type": "integer", "minimum": 1 },
        "character": { "type": "integer", "minimum": 1 },
        "query": { "type": "string" },
        "command": { "type": "string", "description": "Shell-only command. Do not use for file reading, listing, searching, or edits." },
        "old_string": { "type": "string" },
        "new_string": { "type": "string" },
        "offset": { "type": "integer", "minimum": 0 },
        "limit": { "type": "integer", "minimum": 1 }
    });

    if matches!(name, "Bash" | "TerminalRun")
        && let Value::Object(properties) = &mut properties
    {
        properties.insert(
            "description".to_owned(),
            json!({
                "type": "string",
                "description": "Brief user-facing label for this command in active voice."
            }),
        );
    }
    if name == "TerminalRun"
        && let Value::Object(properties) = &mut properties
    {
        properties.insert(
            "command".to_owned(),
            json!({
                "type": "string",
                "description": "Non-interactive shell command to run visibly in the agent terminal."
            }),
        );
        properties.insert(
            "timeout_ms".to_owned(),
            json!({
                "type": "integer",
                "minimum": 1,
                "maximum": crate::terminal::TERMINAL_RUN_MAX_TIMEOUT_MS,
                "description": "Optional command timeout in milliseconds. Defaults to 120000 and cannot exceed 600000."
            }),
        );
    }
    if name == "Subagent"
        && let Value::Object(properties) = &mut properties
    {
        properties.insert(
            "description".to_owned(),
            json!({
                "type": "string",
                "description": "Short label for the delegated Codex task."
            }),
        );
        properties.insert(
            "prompt".to_owned(),
            json!({
                "type": "string",
                "description": "Complete instructions for the Codex subagent. Include all context it needs."
            }),
        );
        properties.insert(
            "backend".to_owned(),
            json!({
                "type": "string",
                "enum": ["codex"],
                "description": "Subagent backend. Omit to use codex."
            }),
        );
        properties.insert(
            "agent".to_owned(),
            json!({
                "type": "string",
                "description": "Optional namespaced plugin agent definition, such as plugin-name:reviewer."
            }),
        );
        properties.insert(
            "cwd".to_owned(),
            json!({
                "type": "string",
                "description": "Optional working directory. Use a path relative to current_directory or an absolute path. Do not use ~."
            }),
        );
    }
    if matches!(name, "TaskList" | "TaskWait" | "TaskSend" | "TaskCancel")
        && let Value::Object(properties) = &mut properties
    {
        properties.clear();
    }
    if matches!(name, "TaskWait" | "TaskSend" | "TaskCancel")
        && let Value::Object(properties) = &mut properties
    {
        properties.insert(
            "task_id".to_owned(),
            json!({
                "type": "string",
                "description": "Delegated task ID returned by Subagent or TaskList."
            }),
        );
    }
    if name == "TaskWait"
        && let Value::Object(properties) = &mut properties
    {
        properties.remove("task_id");
        properties.insert(
            "task_ids".to_owned(),
            json!({
                "type": "array",
                "minItems": 1,
                "items": { "type": "string" },
                "description": "One or more delegated task IDs to wait for."
            }),
        );
        properties.insert(
            "timeout_ms".to_owned(),
            json!({
                "type": "integer",
                "minimum": 1,
                "maximum": task_control::MAX_WAIT_TIMEOUT_MS,
                "description": "Optional wait timeout in milliseconds. Defaults to 30000."
            }),
        );
    }
    if name == "TaskSend"
        && let Value::Object(properties) = &mut properties
    {
        properties.insert(
            "message".to_owned(),
            json!({
                "type": "string",
                "description": "Additional instructions for the running task."
            }),
        );
    }
    if name == "TodoWrite"
        && let Value::Object(properties) = &mut properties
    {
        properties.clear();
        properties.insert(
            "explanation".to_owned(),
            json!({
                "type": "string",
                "description": "Optional short reason for this checklist update."
            }),
        );
        properties.insert(
            "todos".to_owned(),
            json!({
                "type": "array",
                "description": "The full updated checklist. Use an empty list only when the checklist is no longer relevant.",
                "items": {
                    "type": "object",
                    "properties": {
                        "content": {
                            "type": "string",
                            "description": "Imperative task description, such as \"Run tests\"."
                        },
                        "active_form": {
                            "type": "string",
                            "description": "Present-progress form shown while active, such as \"Running tests\"."
                        },
                        "status": {
                            "type": "string",
                            "enum": ["pending", "in_progress", "completed"],
                            "description": "Current task status."
                        }
                    },
                    "required": ["content", "active_form", "status"],
                    "additionalProperties": false
                }
            }),
        );
    }
    if name == "Glob"
        && let Value::Object(properties) = &mut properties
    {
        properties.insert(
            "pattern".to_owned(),
            json!({
                "type": "string",
                "description": "Narrow glob pattern for purposeful file discovery. Avoid broad root patterns such as **/* unless the user asked for a full inventory."
            }),
        );
        properties.insert(
            "limit".to_owned(),
            json!({
                "type": "integer",
                "minimum": 1,
                "maximum": GLOB_MAX_LIMIT,
                "description": "Maximum number of matching files to return. Defaults to 100 and cannot exceed 100."
            }),
        );
    }
    if name == "LSP"
        && let Value::Object(properties) = &mut properties
    {
        properties.insert(
            "operation".to_owned(),
            json!({
                "type": "string",
                "enum": ["goToDefinition", "findReferences", "hover", "documentSymbol", "workspaceSymbol"],
                "description": "Semantic operation to perform."
            }),
        );
        properties.insert(
            "file_path".to_owned(),
            json!({
                "type": "string",
                "description": "File path for goToDefinition, findReferences, hover, and documentSymbol. Optional route hint for workspaceSymbol when multiple LSP servers are configured. Use current_directory-relative paths for files under current_directory."
            }),
        );
        properties.insert(
            "line".to_owned(),
            json!({
                "type": "integer",
                "minimum": 1,
                "description": "1-based line number for goToDefinition, findReferences, and hover."
            }),
        );
        properties.insert(
            "character".to_owned(),
            json!({
                "type": "integer",
                "minimum": 1,
                "description": "1-based character offset for goToDefinition, findReferences, and hover."
            }),
        );
        properties.insert(
            "query".to_owned(),
            json!({
                "type": "string",
                "description": "Workspace symbol query for workspaceSymbol."
            }),
        );
    }

    ToolSpec {
        name: name.to_owned(),
        description: description.to_owned(),
        parameters: json!({
            "type": "object",
            "properties": properties,
            "required": required,
            "additionalProperties": false
        }),
    }
}

#[cfg(test)]
mod tests {
    use std::{env, fs, path::PathBuf, sync::mpsc, thread, time::Duration};

    use serde_json::json;

    use super::{
        bash::{bash_requires_approval, contains_shell_control},
        glob::{
            GLOB_DEFAULT_LIMIT, GLOB_DEFAULT_TIMEOUT_SECONDS, GLOB_MAX_LIMIT, GLOB_TIMEOUT_ENV,
            GLOB_WSL_TIMEOUT_SECONDS, GlobSearchOutput, OutputEvent, OutputStream,
            format_glob_output, glob_limit, glob_timeout_from, missing_ripgrep,
            record_output_event,
        },
        utils::{display_path, display_path_string, resolve_tool_path},
        *,
    };
    use crate::{
        agent::provider::{ToolCall, ToolResult, ToolSpec},
        terminal::{TerminalRequest, TerminalRunResult},
    };

    struct RuntimeEchoTool;

    impl DynamicTool for RuntimeEchoTool {
        fn spec(&self) -> ToolSpec {
            ToolSpec {
                name: "mcp__demo__echo".to_owned(),
                description: "Echo dynamic input".to_owned(),
                parameters: json!({"type":"object"}),
            }
        }

        fn execute(&self, call: &ToolCall, _is_cancelled: &mut dyn FnMut() -> bool) -> ToolResult {
            ToolResult {
                call_id: call.id.clone(),
                content: call.arguments.to_string(),
                is_error: false,
            }
        }

        fn requires_approval(&self, _call: &ToolCall) -> bool {
            false
        }

        fn is_concurrency_safe(&self, _call: &ToolCall) -> bool {
            true
        }
    }

    #[test]
    fn runtime_tools_join_specs_dispatch_and_policy() {
        let registry =
            ToolRegistry::new().with_dynamic_tools(vec![std::sync::Arc::new(RuntimeEchoTool)]);
        let call = ToolCall {
            id: "dynamic".to_owned(),
            name: "mcp__demo__echo".to_owned(),
            arguments: json!({"message":"hello"}),
        };

        assert!(registry.specs().iter().any(|spec| spec.name == call.name));
        assert_eq!(registry.execute(&call).content, r#"{"message":"hello"}"#);
        assert!(!registry.requires_approval(&call, false, false));
        assert!(registry.is_concurrency_safe(&call));
    }

    #[test]
    fn exposes_glint_tool_names() {
        let names = ToolRegistry::new()
            .specs()
            .into_iter()
            .map(|spec| spec.name)
            .collect::<Vec<_>>();

        assert_eq!(
            names,
            [
                "Read",
                "Glob",
                "Grep",
                "LSP",
                "Bash",
                "Subagent",
                "TaskList",
                "TaskWait",
                "TaskSend",
                "TaskCancel",
                "Edit",
                "TodoWrite"
            ]
        );
    }

    #[test]
    fn terminal_mode_swaps_bash_for_terminal_run() {
        let (terminal_tx, _terminal_rx) = mpsc::channel();
        let names = ToolRegistry::with_terminal_requests(terminal_tx)
            .specs()
            .into_iter()
            .map(|spec| spec.name)
            .collect::<Vec<_>>();

        assert_eq!(
            names,
            [
                "Read",
                "Glob",
                "Grep",
                "LSP",
                "TerminalRun",
                "Subagent",
                "TaskList",
                "TaskWait",
                "TaskSend",
                "TaskCancel",
                "Edit",
                "TodoWrite"
            ]
        );
    }

    #[test]
    fn task_control_tools_have_narrow_schemas_and_need_no_approval() {
        let registry = ToolRegistry::new();
        let wait = registry
            .specs()
            .into_iter()
            .find(|spec| spec.name == "TaskWait")
            .unwrap();
        let properties = wait.parameters["properties"].as_object().unwrap();
        assert_eq!(properties.len(), 2);
        assert!(properties.contains_key("task_ids"));
        assert!(properties.contains_key("timeout_ms"));

        let call = ToolCall {
            id: "wait".to_owned(),
            name: "TaskWait".to_owned(),
            arguments: json!({"task_ids": ["a1"]}),
        };
        assert!(!registry.requires_approval(&call, false, false));
        assert!(registry.is_concurrency_safe(&call));
    }

    #[test]
    fn subagent_registry_exposes_limited_tool_surface() {
        let (terminal_tx, _terminal_rx) = mpsc::channel();
        let registry = ToolRegistry::for_subagent(Some(terminal_tx));
        let names = registry
            .specs()
            .into_iter()
            .map(|spec| spec.name)
            .collect::<Vec<_>>();

        assert_eq!(
            names,
            ["Read", "Glob", "Grep", "LSP", "Bash", "TerminalRun"]
        );

        let edit = ToolCall {
            id: "edit".to_owned(),
            name: "Edit".to_owned(),
            arguments: json!({ "file_path": "src/main.rs", "old_string": "a", "new_string": "b" }),
        };
        let result = registry.execute_approved(&edit);
        assert!(result.is_error);
        assert!(result.content.contains("not registered"));

        let nested = ToolCall {
            id: "subagent".to_owned(),
            name: "Subagent".to_owned(),
            arguments: json!({ "description": "nested", "prompt": "work" }),
        };
        let result = registry.execute_approved(&nested);
        assert!(result.is_error);
        assert!(result.content.contains("Nested Subagent"));
    }

    #[test]
    fn shell_tools_are_rejected_outside_active_mode() {
        let bash_mode = ToolRegistry::new();
        let terminal_run = ToolCall {
            id: "terminal".to_owned(),
            name: "TerminalRun".to_owned(),
            arguments: json!({
                "command": "git status --short",
                "description": "Check status"
            }),
        };

        let result = bash_mode.execute_approved(&terminal_run);

        assert!(result.is_error);
        assert!(result.content.contains("not registered"));

        let (terminal_tx, _terminal_rx) = mpsc::channel();
        let terminal_mode = ToolRegistry::with_terminal_requests(terminal_tx);
        let bash = ToolCall {
            id: "bash".to_owned(),
            name: "Bash".to_owned(),
            arguments: json!({
                "command": "git status --short",
                "description": "Check status"
            }),
        };

        let result = terminal_mode.execute_approved(&bash);

        assert!(result.is_error);
        assert!(result.content.contains("not registered"));
    }

    #[test]
    fn shell_schemas_include_user_facing_description() {
        let bash_specs = ToolRegistry::new().specs();
        let bash = bash_specs
            .iter()
            .find(|spec| spec.name == "Bash")
            .expect("Bash spec should exist");
        let (terminal_tx, _terminal_rx) = mpsc::channel();
        let terminal_specs = ToolRegistry::with_terminal_requests(terminal_tx).specs();
        let terminal_run = terminal_specs
            .iter()
            .find(|spec| spec.name == "TerminalRun")
            .expect("TerminalRun spec should exist");
        let read = bash_specs
            .iter()
            .find(|spec| spec.name == "Read")
            .expect("Read spec should exist");

        assert!(bash.parameters["properties"]["description"].is_object());
        assert!(terminal_run.parameters["properties"]["description"].is_object());
        assert!(terminal_run.parameters["properties"]["timeout_ms"].is_object());
        assert_eq!(
            terminal_run.parameters["required"],
            json!(["command", "description"])
        );
        assert!(
            read.parameters["properties"]["file_path"]["description"]
                .as_str()
                .expect("file_path should have a description")
                .contains("relative to current_directory")
        );
    }

    #[test]
    fn display_path_relativizes_paths_under_current_directory() {
        let cwd = env::current_dir().expect("cwd should exist");
        let cargo_toml = cwd.join("Cargo.toml");

        assert_eq!(
            display_path(cargo_toml.to_str().expect("utf-8 path")),
            "Cargo.toml"
        );
        assert_eq!(display_path("Cargo.toml"), "Cargo.toml");
    }

    #[test]
    fn display_path_relativizes_home_paths_under_current_directory() {
        let Some(home) = env::var_os("HOME").map(PathBuf::from) else {
            return;
        };
        let cwd = env::current_dir().expect("cwd should exist");
        let Ok(relative_cwd) = cwd.strip_prefix(home) else {
            return;
        };
        let path = format!("~/{}/Cargo.toml", display_path_string(relative_cwd));

        assert_eq!(display_path(&path), "Cargo.toml");
    }

    #[test]
    fn resolve_tool_path_allows_absolute_paths_outside_current_directory() {
        let path = env::temp_dir().join(format!(
            "glint-tool-path-test-{}-read.txt",
            std::process::id()
        ));
        fs::write(&path, "outside").expect("write temp file");
        let canonical = path.canonicalize().expect("canonical temp file");
        let resolved = resolve_tool_path(path.to_str().expect("utf-8 path"));
        fs::remove_file(&path).ok();

        assert_eq!(resolved.expect("path should resolve"), canonical);
    }

    #[test]
    fn normalize_for_context_rewrites_cwd_paths_but_keeps_external_absolute_paths() {
        let registry = ToolRegistry::new();
        let cwd = env::current_dir().expect("cwd should exist");
        let cargo_toml = cwd.join("Cargo.toml");
        let call = ToolCall {
            id: "read".to_owned(),
            name: "Read".to_owned(),
            arguments: json!({ "file_path": cargo_toml }),
        };

        let normalized = registry.normalize_for_context(&call);

        assert_eq!(normalized.arguments["file_path"], "Cargo.toml");

        let external = env::temp_dir().join(format!(
            "glint-tool-path-test-{}-context.txt",
            std::process::id()
        ));
        fs::write(&external, "outside").expect("write temp file");
        let canonical = external.canonicalize().expect("canonical temp file");
        if canonical.starts_with(&cwd) {
            fs::remove_file(&canonical).ok();
            return;
        }
        let call = ToolCall {
            id: "read".to_owned(),
            name: "Read".to_owned(),
            arguments: json!({ "file_path": external }),
        };
        let normalized = registry.normalize_for_context(&call);
        fs::remove_file(&canonical).ok();

        assert_eq!(
            normalized.arguments["file_path"],
            display_path_string(&canonical)
        );
    }

    #[test]
    fn glob_schema_describes_limits() {
        let specs = ToolRegistry::new().specs();
        let glob = specs
            .iter()
            .find(|spec| spec.name == "Glob")
            .expect("Glob spec should exist");

        assert!(glob.description.contains("at most 100 files"));
        assert!(glob.description.contains("20 seconds"));
        assert!(glob.description.contains("60 seconds"));
        assert!(glob.description.contains(GLOB_TIMEOUT_ENV));
        assert_eq!(
            glob.parameters["properties"]["limit"]["maximum"],
            GLOB_MAX_LIMIT
        );
        assert!(
            glob.parameters["properties"]["pattern"]["description"]
                .as_str()
                .expect("pattern should have a description")
                .contains("Narrow glob pattern")
        );
    }

    #[test]
    fn lsp_schema_describes_operations() {
        let specs = ToolRegistry::new().specs();
        let lsp = specs
            .iter()
            .find(|spec| spec.name == "LSP")
            .expect("LSP spec should exist");

        assert_eq!(lsp.parameters["required"], json!(["operation"]));
        assert_eq!(
            lsp.parameters["properties"]["operation"]["enum"],
            json!([
                "goToDefinition",
                "findReferences",
                "hover",
                "documentSymbol",
                "workspaceSymbol"
            ])
        );
        assert!(
            lsp.description
                .contains("language-server semantic information")
        );
    }

    #[test]
    fn glob_limit_defaults_and_caps_at_maximum() {
        let default = ToolCall {
            id: "glob".to_owned(),
            name: "Glob".to_owned(),
            arguments: json!({ "pattern": "**/*" }),
        };
        let smaller = ToolCall {
            id: "glob".to_owned(),
            name: "Glob".to_owned(),
            arguments: json!({ "pattern": "**/*", "limit": 50 }),
        };
        let larger = ToolCall {
            id: "glob".to_owned(),
            name: "Glob".to_owned(),
            arguments: json!({ "pattern": "**/*", "limit": 500 }),
        };

        assert_eq!(glob_limit(&default), GLOB_DEFAULT_LIMIT);
        assert_eq!(glob_limit(&smaller), 50);
        assert_eq!(glob_limit(&larger), GLOB_MAX_LIMIT);
    }

    #[test]
    fn glob_timeout_defaults_follow_platform_shape() {
        assert_eq!(
            glob_timeout_from(None, false),
            Duration::from_secs(GLOB_DEFAULT_TIMEOUT_SECONDS)
        );
        assert_eq!(
            glob_timeout_from(None, true),
            Duration::from_secs(GLOB_WSL_TIMEOUT_SECONDS)
        );
    }

    #[test]
    fn glob_timeout_env_override_must_be_positive_seconds() {
        assert_eq!(
            glob_timeout_from(Some("45"), false),
            Duration::from_secs(45)
        );
        assert_eq!(
            glob_timeout_from(Some(" 30 "), true),
            Duration::from_secs(30)
        );
        assert_eq!(
            glob_timeout_from(Some("0"), false),
            Duration::from_secs(GLOB_DEFAULT_TIMEOUT_SECONDS)
        );
        assert_eq!(
            glob_timeout_from(Some("bad"), true),
            Duration::from_secs(GLOB_WSL_TIMEOUT_SECONDS)
        );
    }

    #[test]
    fn glob_output_limit_triggers_only_after_extra_line() {
        let mut stdout_lines = Vec::new();
        let mut stderr_lines = Vec::new();
        let mut done_readers = 0;

        assert!(!record_output_event(
            OutputEvent::Line(OutputStream::Stdout, "one".to_owned()),
            &mut stdout_lines,
            &mut stderr_lines,
            &mut done_readers,
            2
        ));
        assert!(!record_output_event(
            OutputEvent::Line(OutputStream::Stdout, "two".to_owned()),
            &mut stdout_lines,
            &mut stderr_lines,
            &mut done_readers,
            2
        ));
        assert!(record_output_event(
            OutputEvent::Line(OutputStream::Stdout, "three".to_owned()),
            &mut stdout_lines,
            &mut stderr_lines,
            &mut done_readers,
            2
        ));

        assert_eq!(stdout_lines, ["one", "two"]);
    }

    #[test]
    fn formats_truncated_glob_output_like_tool_result() {
        let output = format_glob_output(GlobSearchOutput {
            files: vec!["src/main.rs".to_owned()],
            truncated: true,
            timed_out: false,
        });

        assert!(output.contains("src/main.rs"));
        assert!(output.contains("Results are truncated"));
        assert!(!output.contains("timed out"));
    }

    #[test]
    fn formats_partial_timeout_glob_output() {
        let output = format_glob_output(GlobSearchOutput {
            files: vec!["src/main.rs".to_owned()],
            truncated: false,
            timed_out: true,
        });

        assert!(output.contains("src/main.rs"));
        assert!(output.contains("Search timed out before completing"));
    }

    #[test]
    fn bash_allows_read_only_commands_without_approval() {
        assert!(!bash_requires_approval("git status --short"));
        assert!(!bash_requires_approval("pwd"));
        assert!(!bash_requires_approval("rustc --version"));
    }

    #[test]
    fn bash_requires_approval_for_commands_that_can_modify_files() {
        assert!(bash_requires_approval("cargo fmt"));
        assert!(bash_requires_approval("cargo test"));
        assert!(bash_requires_approval("rm -rf target"));
        assert!(bash_requires_approval("rm file.txt"));
        assert!(bash_requires_approval("mv a b"));
        assert!(bash_requires_approval("cp a b"));
        assert!(bash_requires_approval("chmod 600 file"));
        assert!(bash_requires_approval("mkdir tmp"));
        assert!(bash_requires_approval("touch file"));
        assert!(bash_requires_approval("git diff --output=file.patch"));
        assert!(bash_requires_approval("git show HEAD:.env.local"));
        assert!(bash_requires_approval(
            "python3 -c 'open(\"x\",\"w\").write(\"y\")'"
        ));
        assert!(bash_requires_approval("git status --short; rm -rf target"));
        assert!(bash_requires_approval("git status \"$(rm -rf target)\""));
        assert!(bash_requires_approval("git status 'unterminated"));
    }

    #[test]
    fn bash_shell_control_scanner_respects_plain_quotes() {
        assert!(!contains_shell_control("git commit -m \"hello world\""));
        assert!(!contains_shell_control("printf 'literal $HOME'"));
        assert!(contains_shell_control("git status && cargo test"));
        assert!(contains_shell_control("echo \"$(date)\""));
        assert!(contains_shell_control("echo hi > file.txt"));
    }

    #[test]
    fn bash_refuses_commands_covered_by_dedicated_tools_without_approval() {
        let registry = ToolRegistry::new();

        for (command, expected) in [
            ("rg TODO src", "Use Grep"),
            ("grep -R TODO src", "Use Grep"),
            ("find . -name '*.rs'", "Use Glob"),
            ("ls src", "Use Glob"),
            ("cat src/main.rs", "Use Read"),
            ("sed -n '1,20p' src/main.rs", "Use Read"),
            ("git grep TODO", "Use Grep"),
            ("echo hello", "Output text directly"),
            ("echo hi > file.txt", "Use Edit"),
            ("pwd && ls -la", "Use Glob"),
            ("pwd; find . -name '*.rs'", "Use Glob"),
            ("git status && rg TODO src", "Use Grep"),
            ("pwd && cat Cargo.toml", "Use Read"),
        ] {
            let call = ToolCall {
                id: "bash".to_owned(),
                name: "Bash".to_owned(),
                arguments: json!({ "command": command }),
            };

            assert!(!registry.requires_approval(&call, false, false));
            let result = registry.execute_approved(&call);
            assert!(result.is_error);
            assert!(result.content.contains(expected));
        }
    }

    #[test]
    fn terminal_run_reuses_bash_approval_rules() {
        let (terminal_tx, _terminal_rx) = mpsc::channel();
        let registry = ToolRegistry::with_terminal_requests(terminal_tx);
        let read_only = ToolCall {
            id: "terminal".to_owned(),
            name: "TerminalRun".to_owned(),
            arguments: json!({
                "command": "git status --short",
                "description": "Check git status"
            }),
        };
        let mutating = ToolCall {
            id: "terminal".to_owned(),
            name: "TerminalRun".to_owned(),
            arguments: json!({
                "command": "cargo test",
                "description": "Run tests"
            }),
        };
        let shell_control = ToolCall {
            id: "terminal".to_owned(),
            name: "TerminalRun".to_owned(),
            arguments: json!({
                "command": "cargo test; rm -rf target",
                "description": "Run tests"
            }),
        };
        let sensitive = ToolCall {
            id: "terminal".to_owned(),
            name: "TerminalRun".to_owned(),
            arguments: json!({
                "command": "rm -rf ~/.ssh",
                "description": "Remove ssh keys"
            }),
        };

        assert!(!registry.requires_approval(&read_only, false, false));
        assert!(registry.requires_approval(&mutating, false, false));
        assert!(!registry.requires_approval(&mutating, true, false));
        assert!(registry.requires_approval(&shell_control, true, false));
        assert!(registry.requires_approval(&sensitive, true, false));
    }

    #[test]
    fn terminal_run_sends_request_and_formats_result() {
        let (terminal_tx, terminal_rx) = mpsc::channel();
        let registry = ToolRegistry::with_terminal_requests(terminal_tx);
        let call = ToolCall {
            id: "terminal".to_owned(),
            name: "TerminalRun".to_owned(),
            arguments: json!({
                "command": "echo glint-terminal-test",
                "description": "Print terminal smoke test",
                "timeout_ms": 500
            }),
        };
        let worker_call = call.clone();
        let handle = thread::spawn(move || registry.execute_approved(&worker_call));

        let request = terminal_rx.recv().expect("terminal request should arrive");
        let TerminalRequest::Run {
            command,
            description,
            timeout,
            response,
        } = request
        else {
            panic!("expected run request");
        };
        assert_eq!(command, "echo glint-terminal-test");
        assert_eq!(description, "Print terminal smoke test");
        assert_eq!(timeout, Duration::from_millis(500));
        response
            .send(TerminalRunResult {
                command,
                output: "glint-terminal-test".to_owned(),
                exit_code: Some(0),
                timed_out: false,
                error: None,
            })
            .unwrap();

        let result = handle.join().expect("tool worker should finish");

        assert!(!result.is_error);
        assert!(result.content.contains("command: echo glint-terminal-test"));
        assert!(result.content.contains("exit_code: 0"));
        assert!(result.content.contains("output:\nglint-terminal-test"));
    }

    #[test]
    fn terminal_run_still_refuses_echo_redirection() {
        let (terminal_tx, _terminal_rx) = mpsc::channel();
        let registry = ToolRegistry::with_terminal_requests(terminal_tx);
        let call = ToolCall {
            id: "terminal".to_owned(),
            name: "TerminalRun".to_owned(),
            arguments: json!({
                "command": "echo hi > file.txt",
                "description": "Write file"
            }),
        };

        let result = registry.execute_approved(&call);

        assert!(result.is_error);
        assert!(result.content.contains("Use Edit"));
    }

    #[test]
    fn bash_command_can_be_cancelled() {
        let registry = ToolRegistry::new();
        let call = ToolCall {
            id: "bash".to_owned(),
            name: "Bash".to_owned(),
            arguments: json!({ "command": "sleep 5" }),
        };

        let result = registry.execute_approved_with_cancel(&call, &mut || true);

        assert!(result.is_error);
        assert_eq!(result.content, "cancelled");
    }

    #[test]
    fn persisted_prefixes_do_not_bypass_shell_control_approval() {
        let registry = ToolRegistry::new();
        let call = ToolCall {
            id: "bash".to_owned(),
            name: "Bash".to_owned(),
            arguments: json!({ "command": "cargo test --lib; rm -rf target" }),
        };

        assert!(registry.requires_approval(&call, true, false));

        let call = ToolCall {
            id: "bash".to_owned(),
            name: "Bash".to_owned(),
            arguments: json!({ "command": "rm -rf ~/.ssh" }),
        };

        assert!(registry.requires_approval(&call, true, false));
    }

    #[test]
    fn protected_paths_require_approval() {
        let registry = ToolRegistry::new();
        let call = ToolCall {
            id: "read".to_owned(),
            name: "Read".to_owned(),
            arguments: json!({ "file_path": ".glint/settings.local.json" }),
        };

        assert!(registry.requires_approval(&call, false, false));
    }

    #[test]
    fn edit_requires_prior_full_read() {
        let registry = ToolRegistry::new();
        let path = env::temp_dir().join(format!(
            "glint-edit-requires-read-{}.txt",
            uuid::Uuid::new_v4()
        ));
        fs::write(&path, "old").expect("write temp file");
        let edit = ToolCall {
            id: "edit".to_owned(),
            name: "Edit".to_owned(),
            arguments: json!({
                "file_path": path,
                "old_string": "old",
                "new_string": "new"
            }),
        };

        let result = registry.execute_approved(&edit);

        assert!(result.is_error);
        assert!(result.content.contains("has not been read yet"));
        let path = edit.arguments["file_path"].as_str().expect("path");
        fs::remove_file(path).ok();
    }

    #[test]
    fn edit_succeeds_after_full_read_and_refreshes_state() {
        let registry = ToolRegistry::new();
        let path = env::temp_dir().join(format!(
            "glint-edit-after-read-{}.txt",
            uuid::Uuid::new_v4()
        ));
        fs::write(&path, "old").expect("write temp file");
        let read = ToolCall {
            id: "read".to_owned(),
            name: "Read".to_owned(),
            arguments: json!({ "file_path": path }),
        };
        let edit = ToolCall {
            id: "edit".to_owned(),
            name: "Edit".to_owned(),
            arguments: json!({
                "file_path": read.arguments["file_path"].clone(),
                "old_string": "old",
                "new_string": "new"
            }),
        };
        let edit_again = ToolCall {
            id: "edit-again".to_owned(),
            name: "Edit".to_owned(),
            arguments: json!({
                "file_path": read.arguments["file_path"].clone(),
                "old_string": "new",
                "new_string": "final"
            }),
        };

        assert!(!registry.execute(&read).is_error);
        assert!(!registry.execute_approved(&edit).is_error);
        assert!(!registry.execute_approved(&edit_again).is_error);

        let path = read.arguments["file_path"].as_str().expect("path");
        assert_eq!(fs::read_to_string(path).expect("read temp file"), "final");
        fs::remove_file(path).ok();
    }

    #[test]
    fn edit_rejects_partial_read_state() {
        let registry = ToolRegistry::new();
        let path = env::temp_dir().join(format!(
            "glint-edit-partial-read-{}.txt",
            uuid::Uuid::new_v4()
        ));
        fs::write(&path, "old\nsecond").expect("write temp file");
        let read = ToolCall {
            id: "read".to_owned(),
            name: "Read".to_owned(),
            arguments: json!({
                "file_path": path,
                "limit": 1
            }),
        };
        let edit = ToolCall {
            id: "edit".to_owned(),
            name: "Edit".to_owned(),
            arguments: json!({
                "file_path": read.arguments["file_path"].clone(),
                "old_string": "old",
                "new_string": "new"
            }),
        };

        assert!(!registry.execute(&read).is_error);
        let result = registry.execute_approved(&edit);

        assert!(result.is_error);
        assert!(result.content.contains("only partially read"));
        let path = read.arguments["file_path"].as_str().expect("path");
        fs::remove_file(path).ok();
    }

    #[test]
    fn missing_ripgrep_reports_dependency_error() {
        let result = missing_ripgrep(&ToolCall {
            id: "glob".to_owned(),
            name: "Glob".to_owned(),
            arguments: json!({ "pattern": "Cargo.toml" }),
        });

        assert_eq!(result.call_id, "glob");
        assert!(result.is_error);
        assert_eq!(
            result.content,
            "Missing dependency: ripgrep (`rg`) is required for Glob but was not found in PATH."
        );
    }
}
