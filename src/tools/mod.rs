mod bash;
mod edit;
mod glob;
mod grep;
mod read;
mod terminal_run;
mod utils;

use std::sync::mpsc::Sender;

use serde_json::{Value, json};

use crate::agent::provider::{ToolCall, ToolResult, ToolSpec};
use crate::terminal::TerminalRequest;

use bash::BashTool;
use edit::EditTool;
use glob::{GLOB_MAX_LIMIT, GlobTool};
use grep::GrepTool;
use read::ReadTool;
use terminal_run::TerminalRunTool;
use utils::{error, normalize_path_argument, requires_path_approval, truncate_summary};

#[derive(Clone)]
pub struct ToolRegistry {
    terminal_requests: Option<Sender<TerminalRequest>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self {
            terminal_requests: None,
        }
    }

    pub fn with_terminal_requests(terminal_requests: Sender<TerminalRequest>) -> Self {
        Self {
            terminal_requests: Some(terminal_requests),
        }
    }

    pub fn specs(&self) -> Vec<ToolSpec> {
        TOOLS.iter().map(|tool| tool.spec()).collect()
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
            return terminal_run::terminal_run(
                call,
                self.terminal_requests.as_ref(),
                is_cancelled,
                false,
            );
        }
        tool_for_name(&call.name)
            .map(|tool| tool.execute(call, is_cancelled))
            .unwrap_or_else(|| error(call, format!("Tool '{}' is not registered.", call.name)))
    }

    pub fn requires_approval(
        &self,
        call: &ToolCall,
        bash_prefix_allowed: bool,
        edit_allowed: bool,
    ) -> bool {
        tool_for_name(&call.name)
            .is_some_and(|tool| tool.requires_approval(call, bash_prefix_allowed, edit_allowed))
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
            return terminal_run::terminal_run(
                call,
                self.terminal_requests.as_ref(),
                is_cancelled,
                true,
            );
        }
        tool_for_name(&call.name)
            .map(|tool| tool.execute_approved(call, is_cancelled))
            .unwrap_or_else(|| self.execute_with_cancel(call, is_cancelled))
    }

    pub fn is_concurrency_safe(&self, call: &ToolCall) -> bool {
        tool_for_name(&call.name).is_some_and(|tool| tool.is_concurrency_safe(call))
    }

    pub fn input_summary(&self, call: &ToolCall) -> String {
        let summary = tool_for_name(&call.name)
            .and_then(|tool| tool.input_summary(call))
            .unwrap_or_else(|| call.arguments.to_string());
        truncate_summary(&summary)
    }

    pub fn input_description(&self, call: &ToolCall) -> Option<String> {
        tool_for_name(&call.name)
            .and_then(|tool| tool.input_description(call))
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
            _ => {}
        }
        call
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
static BASH_TOOL: BashTool = BashTool;
static EDIT_TOOL: EditTool = EditTool;
static TERMINAL_RUN_TOOL: TerminalRunTool = TerminalRunTool;
static TOOLS: [&dyn ToolBehavior; 6] = [
    &READ_TOOL,
    &GLOB_TOOL,
    &GREP_TOOL,
    &TERMINAL_RUN_TOOL,
    &BASH_TOOL,
    &EDIT_TOOL,
];

fn tool_for_name(name: &str) -> Option<&'static dyn ToolBehavior> {
    TOOLS.iter().copied().find(|tool| tool.name() == name)
}

fn spec(name: &str, description: &str, required: &[&str]) -> ToolSpec {
    let mut properties = json!({
        "file_path": { "type": "string", "description": "File path. Use a path relative to current_directory when the file is under current_directory; use an absolute path only outside it. Do not use ~." },
        "pattern": { "type": "string" },
        "path": { "type": "string", "description": "Search path. Use a path relative to current_directory when the directory is under current_directory; use an absolute path only outside it. Do not use ~." },
        "glob": { "type": "string", "description": "Optional file glob filter." },
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
        agent::provider::ToolCall,
        terminal::{TerminalRequest, TerminalRunResult},
    };

    #[test]
    fn exposes_glint_tool_names() {
        let names = ToolRegistry::new()
            .specs()
            .into_iter()
            .map(|spec| spec.name)
            .collect::<Vec<_>>();

        assert_eq!(
            names,
            ["Read", "Glob", "Grep", "TerminalRun", "Bash", "Edit"]
        );
    }

    #[test]
    fn shell_schemas_include_user_facing_description() {
        let specs = ToolRegistry::new().specs();
        let bash = specs
            .iter()
            .find(|spec| spec.name == "Bash")
            .expect("Bash spec should exist");
        let terminal_run = specs
            .iter()
            .find(|spec| spec.name == "TerminalRun")
            .expect("TerminalRun spec should exist");
        let read = specs
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
        let registry = ToolRegistry::new();
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
        let registry = ToolRegistry::new();
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
