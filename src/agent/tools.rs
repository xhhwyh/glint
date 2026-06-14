use std::{
    env, fs,
    io::{BufRead, BufReader, Read},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::mpsc::{self, Receiver, Sender},
    thread,
    time::{Duration, Instant},
};

use serde_json::{Value, json};

use super::provider::{ToolCall, ToolResult, ToolSpec};

const GLOB_DEFAULT_LIMIT: usize = 100;
const GLOB_MAX_LIMIT: usize = 100;
const GLOB_DEFAULT_TIMEOUT_SECONDS: u64 = 20;
const GLOB_WSL_TIMEOUT_SECONDS: u64 = 60;
const GLOB_TIMEOUT_ENV: &str = "CLAUDE_CODE_GLOB_TIMEOUT_SECONDS";
const POLL_INTERVAL: Duration = Duration::from_millis(50);
const DEFAULT_GLOB_EXCLUDES: &[&str] = &[
    "!.git/**",
    "!target/**",
    "!.worktree/**",
    "!node_modules/**",
    "!dist/**",
    "!build/**",
    "!vendor/**",
    "!.venv/**",
];

#[derive(Clone, Copy)]
pub struct ToolRegistry;

impl ToolRegistry {
    pub fn new() -> Self {
        Self
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

trait ToolBehavior: Sync {
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

struct ReadTool;
struct GlobTool;
struct GrepTool;
struct BashTool;
struct EditTool;

static READ_TOOL: ReadTool = ReadTool;
static GLOB_TOOL: GlobTool = GlobTool;
static GREP_TOOL: GrepTool = GrepTool;
static BASH_TOOL: BashTool = BashTool;
static EDIT_TOOL: EditTool = EditTool;
static TOOLS: [&dyn ToolBehavior; 5] = [&READ_TOOL, &GLOB_TOOL, &GREP_TOOL, &BASH_TOOL, &EDIT_TOOL];

fn tool_for_name(name: &str) -> Option<&'static dyn ToolBehavior> {
    TOOLS.iter().copied().find(|tool| tool.name() == name)
}

impl ToolBehavior for ReadTool {
    fn name(&self) -> &'static str {
        "Read"
    }

    fn description(&self) -> &'static str {
        "Read a UTF-8 text file. Use current-directory-relative paths for files under the current directory; use absolute paths only outside it."
    }

    fn required_args(&self) -> &'static [&'static str] {
        &["file_path"]
    }

    fn execute(&self, call: &ToolCall, _is_cancelled: &mut dyn FnMut() -> bool) -> ToolResult {
        read(call)
    }

    fn is_concurrency_safe(&self, _call: &ToolCall) -> bool {
        true
    }

    fn input_summary(&self, call: &ToolCall) -> Option<String> {
        path_arg(call, "file_path")
    }
}

impl ToolBehavior for GlobTool {
    fn name(&self) -> &'static str {
        "Glob"
    }

    fn description(&self) -> &'static str {
        "Find files by narrow glob pattern. Use current-directory-relative paths for directories under the current directory; use absolute paths only outside it. Returns at most 100 files with a truncation note when more matches exist. Common generated, dependency, VCS, and worktree directories are excluded by default. Searches time out after 20 seconds, or 60 seconds on WSL; set CLAUDE_CODE_GLOB_TIMEOUT_SECONDS to override."
    }

    fn required_args(&self) -> &'static [&'static str] {
        &["pattern"]
    }

    fn execute(&self, call: &ToolCall, is_cancelled: &mut dyn FnMut() -> bool) -> ToolResult {
        glob(call, is_cancelled)
    }

    fn is_concurrency_safe(&self, _call: &ToolCall) -> bool {
        true
    }

    fn input_summary(&self, call: &ToolCall) -> Option<String> {
        glob_summary(call)
    }
}

impl ToolBehavior for GrepTool {
    fn name(&self) -> &'static str {
        "Grep"
    }

    fn description(&self) -> &'static str {
        "Search file contents by text or regex pattern. Use current-directory-relative paths for files and directories under the current directory; use absolute paths only outside it."
    }

    fn required_args(&self) -> &'static [&'static str] {
        &["pattern"]
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

impl ToolBehavior for BashTool {
    fn name(&self) -> &'static str {
        "Bash"
    }

    fn description(&self) -> &'static str {
        "Run shell-only commands such as git, build/test, package, environment, and process commands."
    }

    fn required_args(&self) -> &'static [&'static str] {
        &["command"]
    }

    fn execute(&self, call: &ToolCall, is_cancelled: &mut dyn FnMut() -> bool) -> ToolResult {
        bash(call, is_cancelled)
    }

    fn execute_approved(
        &self,
        call: &ToolCall,
        is_cancelled: &mut dyn FnMut() -> bool,
    ) -> ToolResult {
        bash_approved(call, is_cancelled)
    }

    fn requires_approval(
        &self,
        call: &ToolCall,
        bash_prefix_allowed: bool,
        _edit_allowed: bool,
    ) -> bool {
        if requires_path_approval(call) {
            return true;
        }
        string_arg(call, "command").is_none_or(|command| {
            let analysis = analyze_bash_command(command);
            analysis.parse_error
                || (analysis.dedicated_tool_replacement.is_none()
                    && (analysis.has_sensitive_path
                        || (analysis.requires_approval
                            && (!bash_prefix_allowed || analysis.has_shell_control))))
        })
    }

    fn input_summary(&self, call: &ToolCall) -> Option<String> {
        string_arg(call, "command").map(str::to_owned)
    }

    fn input_description(&self, call: &ToolCall) -> Option<String> {
        string_arg(call, "description").map(str::to_owned)
    }
}

impl ToolBehavior for EditTool {
    fn name(&self) -> &'static str {
        "Edit"
    }

    fn description(&self) -> &'static str {
        "Request approval to replace one exact string in a UTF-8 text file. Use current-directory-relative paths for files under the current directory; use absolute paths only outside it."
    }

    fn required_args(&self) -> &'static [&'static str] {
        &["file_path", "old_string", "new_string"]
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

fn requires_path_approval(call: &ToolCall) -> bool {
    let path = string_arg(call, "file_path").or_else(|| string_arg(call, "path"));
    path.is_some_and(|path| {
        is_protected_path(path)
            || resolve_tool_path(path)
                .ok()
                .is_some_and(|path| is_protected_path(&path.display().to_string()))
    })
}

fn is_protected_path(path: &str) -> bool {
    Path::new(path).components().any(|component| {
        let text = component.as_os_str().to_string_lossy();
        text.starts_with(".env")
            || text.starts_with(".npmrc")
            || text.starts_with(".pypirc")
            || matches!(text.as_ref(), ".glint" | ".git" | ".claude" | ".envrc")
    }) || path.contains("settings.local.json")
        || path.contains("id_rsa")
        || path.contains("id_ed25519")
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

    if name == "Bash"
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

fn read(call: &ToolCall) -> ToolResult {
    let Some(path) = string_arg(call, "file_path") else {
        return missing_arg(call, "file_path");
    };

    let path = match resolve_tool_path(path) {
        Ok(path) => path,
        Err(message) => return error(call, message),
    };

    match fs::read_to_string(&path) {
        Ok(content) => ok(call, slice_lines(content, call)),
        Err(err) => error(call, format!("failed to read {}: {err}", path.display())),
    }
}

fn glob(call: &ToolCall, is_cancelled: &mut dyn FnMut() -> bool) -> ToolResult {
    let Some(pattern) = string_arg(call, "pattern") else {
        return missing_arg(call, "pattern");
    };

    let path = string_arg(call, "path").unwrap_or(".");
    let path = match resolve_tool_path(path) {
        Ok(path) => path,
        Err(message) => return error(call, message),
    };
    if !program_in_path("rg") {
        return missing_ripgrep(call);
    }

    let mut command = Command::new("rg");
    command
        .args(["--files", "--glob", pattern, "--sort=modified"])
        .args(
            DEFAULT_GLOB_EXCLUDES
                .iter()
                .flat_map(|pattern| ["--glob", *pattern]),
        )
        .arg(path);

    match run_limited_glob_command(&mut command, is_cancelled, glob_limit(call), glob_timeout()) {
        Ok(output) => ok(call, format_glob_output(output)),
        Err(message) => error(call, message),
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

fn bash(call: &ToolCall, is_cancelled: &mut dyn FnMut() -> bool) -> ToolResult {
    let Some(command) = string_arg(call, "command") else {
        return missing_arg(call, "command");
    };

    if bash_requires_approval(command) {
        return error(
            call,
            format!("Approval required before running Bash command: {command}"),
        );
    }

    run_bash(call, command, is_cancelled)
}

fn bash_approved(call: &ToolCall, is_cancelled: &mut dyn FnMut() -> bool) -> ToolResult {
    let Some(command) = string_arg(call, "command") else {
        return missing_arg(call, "command");
    };

    run_bash(call, command, is_cancelled)
}

fn run_bash(call: &ToolCall, command: &str, is_cancelled: &mut dyn FnMut() -> bool) -> ToolResult {
    if let Some(replacement) = dedicated_tool_replacement(command) {
        return error(
            call,
            format!(
                "Bash command was not run because a dedicated tool should handle this action. {replacement}"
            ),
        );
    }

    let shell = std::env::var("SHELL").unwrap_or_else(|_| "sh".to_owned());
    command_result(
        call,
        Command::new(shell).args(["-lc", command]),
        is_cancelled,
    )
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

#[derive(Clone, Debug)]
struct BashCommandAnalysis {
    parse_error: bool,
    has_shell_control: bool,
    has_sensitive_path: bool,
    requires_approval: bool,
    dedicated_tool_replacement: Option<&'static str>,
}

fn analyze_bash_command(command: &str) -> BashCommandAnalysis {
    let words = shlex::split(command);
    let parse_error = words.is_none();
    let words = words.unwrap_or_default();
    let has_shell_control = contains_shell_control(command);
    let has_sensitive_path = command_words_have_sensitive_path(&words);
    let dedicated_tool_replacement = dedicated_tool_replacement_from_words(&words, command);
    let read_only = !parse_error && is_preapproved_read_only_words(&words, has_shell_control);

    BashCommandAnalysis {
        parse_error,
        has_shell_control,
        has_sensitive_path,
        requires_approval: parse_error || (dedicated_tool_replacement.is_none() && !read_only),
        dedicated_tool_replacement,
    }
}

fn bash_requires_approval(command: &str) -> bool {
    analyze_bash_command(command).requires_approval
}

fn command_words_have_sensitive_path(words: &[String]) -> bool {
    words
        .iter()
        .skip(1)
        .filter(|word| !is_shell_operator(word))
        .any(|word| sensitive_arg(word))
}

fn sensitive_arg(arg: &str) -> bool {
    arg.starts_with('/') || arg.starts_with('~') || arg.contains("..") || is_protected_path(arg)
}

fn is_preapproved_read_only_words(words: &[String], has_shell_control: bool) -> bool {
    if words.is_empty() || has_shell_control {
        return false;
    }

    let program = program_name(&words[0]);
    if words
        .iter()
        .skip(1)
        .filter(|word| !is_shell_operator(word))
        .any(|part| {
            sensitive_arg(part)
                || matches!(
                    part.as_str(),
                    "--hidden" | "--no-ignore" | "--no-ignore-vcs" | "-uuu" | "-uu" | "-a"
                )
        })
    {
        return false;
    }

    match program {
        "git" => matches!(
            words.get(1).map(|word| word.as_str()),
            Some("status" | "rev-parse" | "ls-files")
        ),
        "pwd" => words.len() == 1,
        "which" => words.len() >= 2,
        "rustc" => words.get(1).map(|word| word.as_str()) == Some("--version"),
        _ => false,
    }
}

fn contains_shell_control(command: &str) -> bool {
    scan_shell_control(command).has_control || shlex::split(command).is_none()
}

fn dedicated_tool_replacement(command: &str) -> Option<&'static str> {
    let words = shlex::split(command)?;
    dedicated_tool_replacement_from_words(&words, command)
}

fn dedicated_tool_replacement_from_words(words: &[String], command: &str) -> Option<&'static str> {
    for (index, word) in words.iter().enumerate() {
        let program = program_name(word);
        let next = words.get(index + 1).map(|word| program_name(word));

        match program {
            "|" | "||" | "&&" | ";" | ">" | ">>" | "<" | "<<" => {}
            "cat" | "head" | "tail" | "less" | "more" => {
                return Some("Use Read to inspect file contents.");
            }
            "sed" | "awk" => {
                return Some("Use Read for inspection or Edit for file modifications.");
            }
            "find" | "fd" | "ls" | "tree" => return Some("Use Glob to find or list files."),
            "grep" | "rg" => return Some("Use Grep to search file contents."),
            "git" if next == Some("grep") => return Some("Use Grep to search file contents."),
            "echo" | "printf" if scan_shell_control(command).has_output_redirect => {
                return Some("Use Edit for file modifications.");
            }
            "echo" | "printf" => {
                return Some("Output text directly to the user instead of running echo or printf.");
            }
            _ => {}
        }
    }

    None
}

fn is_shell_operator(word: &str) -> bool {
    matches!(
        word,
        "|" | "||" | "&&" | ";" | ">" | ">>" | "<" | "<<" | "2>" | "2>>" | "&>"
    )
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct ShellControl {
    has_control: bool,
    has_output_redirect: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum QuoteState {
    None,
    Single,
    Double,
}

fn scan_shell_control(command: &str) -> ShellControl {
    let mut quote = QuoteState::None;
    let mut escaped = false;
    let mut control = ShellControl::default();

    for ch in command.chars() {
        if escaped {
            escaped = false;
            continue;
        }

        match quote {
            QuoteState::None => match ch {
                '\'' => quote = QuoteState::Single,
                '"' => quote = QuoteState::Double,
                '\\' => control.has_control = true,
                '>' => {
                    control.has_control = true;
                    control.has_output_redirect = true;
                }
                '|' | '<' | ';' | '&' | '$' | '`' | '*' | '?' | '[' | ']' | '{' | '}' | '('
                | ')' | '\n' => control.has_control = true,
                _ => {}
            },
            QuoteState::Single => {
                if ch == '\'' {
                    quote = QuoteState::None;
                }
            }
            QuoteState::Double => match ch {
                '"' => quote = QuoteState::None,
                '\\' => {
                    escaped = true;
                    control.has_control = true;
                }
                '$' | '`' | '\n' => control.has_control = true,
                _ => {}
            },
        }
    }

    if quote != QuoteState::None {
        control.has_control = true;
    }

    control
}

fn program_name(word: &str) -> &str {
    Path::new(word)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(word)
}

fn command_result(
    call: &ToolCall,
    command: &mut Command,
    is_cancelled: &mut dyn FnMut() -> bool,
) -> ToolResult {
    prepare_killable_command(command);
    let mut child = match command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(err) => return error(call, format!("failed to run command: {err}")),
    };

    loop {
        if is_cancelled() {
            kill_child_tree(&mut child);
            return error(call, "cancelled".to_owned());
        }

        match child.try_wait() {
            Ok(Some(_)) => return child_output(call, child),
            Ok(None) => thread::sleep(POLL_INTERVAL),
            Err(err) => return error(call, format!("failed to wait for command: {err}")),
        }
    }
}

fn run_limited_glob_command(
    command: &mut Command,
    is_cancelled: &mut dyn FnMut() -> bool,
    limit: usize,
    timeout: Duration,
) -> Result<GlobSearchOutput, String> {
    prepare_killable_command(command);
    let mut child = match command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(err) => return Err(format!("failed to run command: {err}")),
    };

    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let reader_count = usize::from(stdout.is_some()) + usize::from(stderr.is_some());
    let (tx, rx) = mpsc::channel();
    let stdout_reader = stdout
        .map(|stdout| spawn_line_reader(stdout, OutputStream::Stdout, tx.clone(), Some(limit + 1)));
    let stderr_reader =
        stderr.map(|stderr| spawn_line_reader(stderr, OutputStream::Stderr, tx, None));
    let started = Instant::now();
    let mut stdout_lines = Vec::new();
    let mut stderr_lines = Vec::new();
    let mut done_readers = 0;

    loop {
        if drain_output_events(
            &rx,
            &mut stdout_lines,
            &mut stderr_lines,
            &mut done_readers,
            limit,
        ) {
            kill_child_tree(&mut child);
            wait_for_reader(stdout_reader);
            wait_for_reader(stderr_reader);
            drain_output_events(
                &rx,
                &mut stdout_lines,
                &mut stderr_lines,
                &mut done_readers,
                limit,
            );
            return Ok(GlobSearchOutput {
                files: stdout_lines,
                truncated: true,
                timed_out: false,
            });
        }

        if is_cancelled() {
            kill_child_tree(&mut child);
            wait_for_reader(stdout_reader);
            wait_for_reader(stderr_reader);
            return Err("cancelled".to_owned());
        }

        if started.elapsed() >= timeout {
            kill_child_tree(&mut child);
            wait_for_reader(stdout_reader);
            wait_for_reader(stderr_reader);
            let reached_limit = drain_output_events(
                &rx,
                &mut stdout_lines,
                &mut stderr_lines,
                &mut done_readers,
                limit,
            );
            if stdout_lines.is_empty() {
                return Err(format!(
                    "Ripgrep search timed out after {} seconds. The search may have matched files but did not complete in time. Try searching a more specific path or pattern.",
                    timeout.as_secs()
                ));
            }
            return Ok(GlobSearchOutput {
                files: stdout_lines,
                truncated: reached_limit,
                timed_out: true,
            });
        }

        match child.try_wait() {
            Ok(Some(status)) => {
                let mut reached_limit = false;
                while done_readers < reader_count {
                    match rx.recv_timeout(POLL_INTERVAL) {
                        Ok(event) => {
                            reached_limit |= record_output_event(
                                event,
                                &mut stdout_lines,
                                &mut stderr_lines,
                                &mut done_readers,
                                limit,
                            );
                        }
                        Err(mpsc::RecvTimeoutError::Timeout) => {}
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    }
                }
                wait_for_reader(stdout_reader);
                wait_for_reader(stderr_reader);
                if reached_limit {
                    return Ok(GlobSearchOutput {
                        files: stdout_lines,
                        truncated: true,
                        timed_out: false,
                    });
                }
                if status.success() || status.code() == Some(1) {
                    return Ok(GlobSearchOutput {
                        files: stdout_lines,
                        truncated: false,
                        timed_out: false,
                    });
                }
                return Err(command_error_message(status, stderr_lines));
            }
            Ok(None) => thread::sleep(POLL_INTERVAL),
            Err(err) => return Err(format!("failed to wait for command: {err}")),
        }
    }
}

struct GlobSearchOutput {
    files: Vec<String>,
    truncated: bool,
    timed_out: bool,
}

#[derive(Clone, Copy)]
enum OutputStream {
    Stdout,
    Stderr,
}

enum OutputEvent {
    Line(OutputStream, String),
    Done,
}

fn spawn_line_reader<R: Read + Send + 'static>(
    reader: R,
    stream: OutputStream,
    tx: Sender<OutputEvent>,
    line_limit: Option<usize>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        for (line_count, line) in BufReader::new(reader).lines().enumerate() {
            if line_limit.is_some_and(|limit| line_count >= limit) {
                break;
            }
            let Ok(line) = line else {
                break;
            };
            if tx.send(OutputEvent::Line(stream, line)).is_err() {
                return;
            }
        }
        tx.send(OutputEvent::Done).ok();
    })
}

fn drain_output_events(
    rx: &Receiver<OutputEvent>,
    stdout_lines: &mut Vec<String>,
    stderr_lines: &mut Vec<String>,
    done_readers: &mut usize,
    limit: usize,
) -> bool {
    let mut reached_limit = false;
    while let Ok(event) = rx.try_recv() {
        reached_limit |=
            record_output_event(event, stdout_lines, stderr_lines, done_readers, limit);
    }
    reached_limit
}

fn record_output_event(
    event: OutputEvent,
    stdout_lines: &mut Vec<String>,
    stderr_lines: &mut Vec<String>,
    done_readers: &mut usize,
    limit: usize,
) -> bool {
    match event {
        OutputEvent::Line(OutputStream::Stdout, line) => {
            if stdout_lines.len() < limit {
                stdout_lines.push(line);
                false
            } else {
                true
            }
        }
        OutputEvent::Line(OutputStream::Stderr, line) => {
            stderr_lines.push(line);
            false
        }
        OutputEvent::Done => {
            *done_readers += 1;
            false
        }
    }
}

fn wait_for_reader(reader: Option<thread::JoinHandle<()>>) {
    if let Some(reader) = reader {
        reader.join().ok();
    }
}

fn format_glob_output(output: GlobSearchOutput) -> String {
    if output.files.is_empty() {
        return "No files found".to_owned();
    }

    let mut content = output.files.join("\n");
    if output.truncated {
        content
            .push_str("\n(Results are truncated. Consider using a more specific path or pattern.)");
    }
    if output.timed_out {
        content.push_str(
            "\n(Search timed out before completing. Consider using a more specific path or pattern.)",
        );
    }
    content
}

fn command_error_message(status: std::process::ExitStatus, stderr_lines: Vec<String>) -> String {
    if stderr_lines.is_empty() {
        format!("exit status: {status}")
    } else {
        stderr_lines.join("\n")
    }
}

fn child_output(call: &ToolCall, child: std::process::Child) -> ToolResult {
    match child.wait_with_output() {
        Ok(output) => {
            let mut content = String::new();
            content.push_str(&String::from_utf8_lossy(&output.stdout));
            content.push_str(&String::from_utf8_lossy(&output.stderr));
            if content.is_empty() {
                content = format!("exit status: {}", output.status);
            }
            ToolResult {
                call_id: call.id.clone(),
                content,
                is_error: !output.status.success(),
            }
        }
        Err(err) => error(call, format!("failed to collect command output: {err}")),
    }
}

#[cfg(unix)]
fn prepare_killable_command(command: &mut Command) {
    use std::os::unix::process::CommandExt;

    command.process_group(0);
}

#[cfg(not(unix))]
fn prepare_killable_command(_command: &mut Command) {}

#[cfg(unix)]
fn kill_child_tree(child: &mut std::process::Child) {
    let pgid = child.id() as libc::pid_t;
    // The child was started in its own process group, so this kills the shell
    // and any subprocesses it spawned for this tool call.
    unsafe {
        libc::killpg(pgid, libc::SIGKILL);
    }
    child.kill().ok();
    child.wait().ok();
}

#[cfg(not(unix))]
fn kill_child_tree(child: &mut std::process::Child) {
    child.kill().ok();
    child.wait().ok();
}

fn missing_ripgrep(call: &ToolCall) -> ToolResult {
    error(
        call,
        "Missing dependency: ripgrep (`rg`) is required for Glob but was not found in PATH."
            .to_owned(),
    )
}

fn program_in_path(program: &str) -> bool {
    let names = program_names(program);
    env::var_os("PATH").is_some_and(|path| {
        env::split_paths(&path)
            .any(|dir| names.iter().any(|name| is_executable_file(&dir.join(name))))
    })
}

#[cfg(windows)]
fn program_names(program: &str) -> Vec<String> {
    if Path::new(program).extension().is_some() {
        return vec![program.to_owned()];
    }

    env::var_os("PATHEXT")
        .map(|extensions| {
            extensions
                .to_string_lossy()
                .split(';')
                .filter(|extension| !extension.is_empty())
                .map(|extension| format!("{program}{extension}"))
                .chain(std::iter::once(program.to_owned()))
                .collect()
        })
        .unwrap_or_else(|| vec![format!("{program}.exe"), program.to_owned()])
}

#[cfg(not(windows))]
fn program_names(program: &str) -> Vec<String> {
    vec![program.to_owned()]
}

#[cfg(unix)]
fn is_executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;

    path.metadata()
        .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable_file(path: &Path) -> bool {
    path.is_file()
}

fn slice_lines(content: String, call: &ToolCall) -> String {
    let offset = usize_arg(call, "offset").unwrap_or(0);
    let limit = usize_arg(call, "limit").unwrap_or(usize::MAX);
    content
        .lines()
        .skip(offset)
        .take(limit)
        .collect::<Vec<_>>()
        .join("\n")
}

fn resolve_tool_path(path: &str) -> Result<PathBuf, String> {
    let cwd = std::env::current_dir()
        .map_err(|err| format!("failed to read current directory: {err}"))?;
    let path = expand_home_path(path);
    let absolute = if path.is_absolute() {
        path
    } else {
        cwd.join(path)
    };
    absolute.canonicalize().map_err(|err| {
        format!(
            "failed to resolve path {}: {err}. Use a path relative to current_directory for files under it, or an absolute path for files outside it.",
            display_path_string(&absolute)
        )
    })
}

fn expand_home_path(path: &str) -> PathBuf {
    let Some(home) = env::var_os("HOME").map(PathBuf::from) else {
        return PathBuf::from(path);
    };

    if path == "~" {
        return home;
    }

    path.strip_prefix("~/")
        .or_else(|| path.strip_prefix("~\\"))
        .map(|relative| home.join(relative))
        .unwrap_or_else(|| PathBuf::from(path))
}

fn string_arg<'a>(call: &'a ToolCall, name: &str) -> Option<&'a str> {
    call.arguments.get(name).and_then(Value::as_str)
}

fn normalize_path_argument(arguments: &mut Value, name: &str) {
    let Value::Object(properties) = arguments else {
        return;
    };
    let Some(Value::String(path)) = properties.get_mut(name) else {
        return;
    };
    *path = display_path(path);
}

fn path_arg(call: &ToolCall, name: &str) -> Option<String> {
    string_arg(call, name).map(display_path)
}

fn glob_summary(call: &ToolCall) -> Option<String> {
    let pattern = string_arg(call, "pattern")?;
    let Some(path) = string_arg(call, "path") else {
        return Some(pattern.to_owned());
    };

    let display_path = display_path(path);
    if display_path == "." {
        Some(pattern.to_owned())
    } else {
        Some(format!("{display_path} ｜ {pattern}"))
    }
}

fn truncate_summary(output: &str) -> String {
    const MAX_SUMMARY_CHARS: usize = 120;

    if output.chars().count() <= MAX_SUMMARY_CHARS {
        return output.to_owned();
    }

    format!(
        "{}...",
        output.chars().take(MAX_SUMMARY_CHARS).collect::<String>()
    )
}

fn display_path(path: &str) -> String {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let cwd = cwd.canonicalize().unwrap_or(cwd);
    let path = expand_home_path(path);
    let absolute = if path.is_absolute() {
        path
    } else {
        cwd.join(path)
    };
    let display_path = absolute.canonicalize().unwrap_or(absolute);

    display_path
        .strip_prefix(&cwd)
        .map(display_relative_path)
        .unwrap_or_else(|_| display_path_string(&display_path))
}

fn display_relative_path(path: &Path) -> String {
    let display = display_path_string(path);
    if display.is_empty() {
        return ".".to_owned();
    }

    display
        .strip_prefix("./")
        .unwrap_or(&display)
        .trim_end_matches('/')
        .to_owned()
}

fn display_path_string(path: &Path) -> String {
    path.display().to_string().replace('\\', "/")
}

fn usize_arg(call: &ToolCall, name: &str) -> Option<usize> {
    call.arguments
        .get(name)
        .and_then(Value::as_u64)
        .map(|value| value as usize)
}

fn glob_limit(call: &ToolCall) -> usize {
    usize_arg(call, "limit")
        .unwrap_or(GLOB_DEFAULT_LIMIT)
        .clamp(1, GLOB_MAX_LIMIT)
}

fn glob_timeout() -> Duration {
    let env_timeout = env::var(GLOB_TIMEOUT_ENV).ok();
    glob_timeout_from(env_timeout.as_deref(), running_on_wsl())
}

fn glob_timeout_from(env_timeout: Option<&str>, running_on_wsl: bool) -> Duration {
    env_timeout
        .and_then(parse_glob_timeout_override)
        .unwrap_or_else(|| Duration::from_secs(default_glob_timeout_seconds(running_on_wsl)))
}

fn parse_glob_timeout_override(value: &str) -> Option<Duration> {
    let seconds = value.trim().parse::<u64>().ok()?;
    (seconds > 0).then(|| Duration::from_secs(seconds))
}

fn default_glob_timeout_seconds(running_on_wsl: bool) -> u64 {
    if running_on_wsl {
        GLOB_WSL_TIMEOUT_SECONDS
    } else {
        GLOB_DEFAULT_TIMEOUT_SECONDS
    }
}

fn running_on_wsl() -> bool {
    env::var_os("WSL_DISTRO_NAME").is_some()
        || fs::read_to_string("/proc/version").is_ok_and(|content| {
            let content = content.to_ascii_lowercase();
            content.contains("microsoft") || content.contains("wsl")
        })
}

fn missing_arg(call: &ToolCall, name: &str) -> ToolResult {
    error(call, format!("missing required argument '{name}'"))
}

fn ok(call: &ToolCall, content: String) -> ToolResult {
    ToolResult {
        call_id: call.id.clone(),
        content,
        is_error: false,
    }
}

fn error(call: &ToolCall, content: String) -> ToolResult {
    ToolResult {
        call_id: call.id.clone(),
        content,
        is_error: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exposes_claude_style_tool_names() {
        let names = ToolRegistry::new()
            .specs()
            .into_iter()
            .map(|spec| spec.name)
            .collect::<Vec<_>>();

        assert_eq!(names, ["Read", "Glob", "Grep", "Bash", "Edit"]);
    }

    #[test]
    fn bash_schema_includes_user_facing_description() {
        let specs = ToolRegistry::new().specs();
        let bash = specs
            .iter()
            .find(|spec| spec.name == "Bash")
            .expect("Bash spec should exist");
        let read = specs
            .iter()
            .find(|spec| spec.name == "Read")
            .expect("Read spec should exist");

        assert!(bash.parameters["properties"]["description"].is_object());
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
    fn glob_timeout_defaults_follow_claude_code_shape() {
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
