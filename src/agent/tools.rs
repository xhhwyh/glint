use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use serde_json::{Value, json};

use super::provider::{ToolCall, ToolResult, ToolSpec};

pub struct ToolRegistry;

impl ToolRegistry {
    pub fn new() -> Self {
        Self
    }

    pub fn specs(&self) -> Vec<ToolSpec> {
        vec![
            spec(
                "Read",
                "Read a UTF-8 text file inside the workspace.",
                &["file_path"],
            ),
            spec(
                "Glob",
                "List files matching a glob pattern inside the workspace, respecting ignore files where possible.",
                &["pattern"],
            ),
            spec(
                "Grep",
                "Search file contents for a text or regex pattern inside the workspace.",
                &["pattern"],
            ),
            spec(
                "Bash",
                "Run a read-only or non-destructive shell command. Commands that can modify files require approval.",
                &["command"],
            ),
            spec(
                "Edit",
                "Request approval to replace one exact string in a UTF-8 text file inside the workspace.",
                &["file_path", "old_string", "new_string"],
            ),
        ]
    }

    pub fn execute(&self, call: &ToolCall) -> ToolResult {
        match call.name.as_str() {
            "Read" => read(call),
            "Glob" => glob(call),
            "Grep" => grep(call),
            "Bash" => bash(call),
            "Edit" => edit(call),
            _ => error(call, format!("Tool '{}' is not registered.", call.name)),
        }
    }

    pub fn requires_approval(
        &self,
        call: &ToolCall,
        bash_prefix_allowed: bool,
        edit_allowed: bool,
    ) -> bool {
        if requires_path_approval(call) {
            return true;
        }
        match call.name.as_str() {
            "Bash" => string_arg(call, "command").is_none_or(|command| {
                command_has_sensitive_path(command)
                    || (bash_requires_approval(command)
                        && (!bash_prefix_allowed || contains_shell_control(command)))
            }),
            "Edit" => !edit_allowed,
            _ => false,
        }
    }

    pub fn execute_approved(&self, call: &ToolCall) -> ToolResult {
        match call.name.as_str() {
            "Bash" => bash_approved(call),
            "Edit" => edit_approved(call),
            _ => self.execute(call),
        }
    }
}

fn requires_path_approval(call: &ToolCall) -> bool {
    let path = string_arg(call, "file_path").or_else(|| string_arg(call, "path"));
    path.is_some_and(|path| {
        is_protected_path(path)
            || workspace_path(path)
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
    ToolSpec {
        name: name.to_owned(),
        description: description.to_owned(),
        parameters: json!({
            "type": "object",
            "properties": {
                "file_path": { "type": "string" },
                "pattern": { "type": "string" },
                "path": { "type": "string" },
                "glob": { "type": "string" },
                "command": { "type": "string", "description": "Read-only or non-destructive shell command; modifying commands require approval." },
                "old_string": { "type": "string" },
                "new_string": { "type": "string" },
                "offset": { "type": "integer", "minimum": 0 },
                "limit": { "type": "integer", "minimum": 1 }
            },
            "required": required,
            "additionalProperties": false
        }),
    }
}

fn read(call: &ToolCall) -> ToolResult {
    let Some(path) = string_arg(call, "file_path") else {
        return missing_arg(call, "file_path");
    };

    let Ok(path) = workspace_path(path) else {
        return error(call, format!("path is outside the workspace: {path}"));
    };

    match fs::read_to_string(&path) {
        Ok(content) => ok(call, slice_lines(content, call)),
        Err(err) => error(call, format!("failed to read {}: {err}", path.display())),
    }
}

fn glob(call: &ToolCall) -> ToolResult {
    let Some(pattern) = string_arg(call, "pattern") else {
        return missing_arg(call, "pattern");
    };

    let path = string_arg(call, "path").unwrap_or(".");
    let Ok(path) = workspace_path(path) else {
        return error(call, format!("path is outside the workspace: {path}"));
    };
    command_result(
        call,
        Command::new("rg")
            .args(["--files", "-g", pattern])
            .arg(path),
    )
}

fn grep(call: &ToolCall) -> ToolResult {
    let Some(pattern) = string_arg(call, "pattern") else {
        return missing_arg(call, "pattern");
    };

    let path = string_arg(call, "path").unwrap_or(".");
    let Ok(path) = workspace_path(path) else {
        return error(call, format!("path is outside the workspace: {path}"));
    };
    let mut command = Command::new("rg");
    command
        .args(["--line-number", "--with-filename", pattern])
        .arg(path);
    if let Some(glob) = string_arg(call, "glob") {
        command.args(["-g", glob]);
    }
    command_result(call, &mut command)
}

fn bash(call: &ToolCall) -> ToolResult {
    let Some(command) = string_arg(call, "command") else {
        return missing_arg(call, "command");
    };

    if bash_requires_approval(command) {
        return error(
            call,
            format!("Approval required before running Bash command: {command}"),
        );
    }

    run_bash(call, command)
}

fn bash_approved(call: &ToolCall) -> ToolResult {
    let Some(command) = string_arg(call, "command") else {
        return missing_arg(call, "command");
    };

    run_bash(call, command)
}

fn run_bash(call: &ToolCall, command: &str) -> ToolResult {
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "sh".to_owned());
    command_result(call, Command::new(shell).args(["-lc", command]))
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

    let Ok(path) = workspace_path(path) else {
        return error(call, format!("path is outside the workspace: {path}"));
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

fn bash_requires_approval(command: &str) -> bool {
    !is_preapproved_read_only_command(command)
}

fn command_has_sensitive_path(command: &str) -> bool {
    command.split_whitespace().skip(1).any(sensitive_arg)
}

fn sensitive_arg(arg: &str) -> bool {
    arg.starts_with('/') || arg.starts_with('~') || arg.contains("..") || is_protected_path(arg)
}

fn is_preapproved_read_only_command(command: &str) -> bool {
    let trimmed = command.trim();
    if trimmed.is_empty() || contains_shell_control(trimmed) {
        return false;
    }

    let parts = trimmed.split_whitespace().collect::<Vec<_>>();
    let Some(program) = parts.first().copied() else {
        return false;
    };

    if parts.iter().skip(1).any(|part| {
        sensitive_arg(part)
            || matches!(
                *part,
                "--hidden" | "--no-ignore" | "--no-ignore-vcs" | "-uuu" | "-uu" | "-a"
            )
    }) {
        return false;
    }

    match program {
        "git" => matches!(
            parts.get(1).copied(),
            Some("status" | "rev-parse" | "ls-files")
        ),
        "pwd" | "which" => true,
        "rustc" => parts.get(1).copied() == Some("--version"),
        _ => false,
    }
}

fn contains_shell_control(command: &str) -> bool {
    command.contains('|')
        || command.contains('>')
        || command.contains('<')
        || command.contains(';')
        || command.contains('&')
        || command.contains('$')
        || command.contains('*')
        || command.contains('?')
        || command.contains('[')
        || command.contains(']')
        || command.contains('{')
        || command.contains('}')
        || command.contains('\\')
        || command.contains('"')
        || command.contains('\'')
        || command.contains('`')
        || command.contains("\n")
}

fn command_result(call: &ToolCall, command: &mut Command) -> ToolResult {
    match command.output() {
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
        Err(err) => error(call, format!("failed to run command: {err}")),
    }
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

fn workspace_path(path: &str) -> Result<PathBuf, ()> {
    let cwd = std::env::current_dir().map_err(|_| ())?;
    let absolute = if Path::new(path).is_absolute() {
        PathBuf::from(path)
    } else {
        cwd.join(path)
    };
    let canonical = absolute.canonicalize().map_err(|_| ())?;
    if canonical.starts_with(&cwd) {
        Ok(canonical)
    } else {
        Err(())
    }
}

fn string_arg<'a>(call: &'a ToolCall, name: &str) -> Option<&'a str> {
    call.arguments.get(name).and_then(Value::as_str)
}

fn usize_arg(call: &ToolCall, name: &str) -> Option<usize> {
    call.arguments
        .get(name)
        .and_then(Value::as_u64)
        .map(|value| value as usize)
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
    fn bash_allows_read_only_commands_without_approval() {
        assert!(!bash_requires_approval("git status --short"));
        assert!(!bash_requires_approval("pwd"));
        assert!(!bash_requires_approval("rustc --version"));
    }

    #[test]
    fn bash_requires_approval_for_commands_that_can_modify_files() {
        assert!(bash_requires_approval("cargo fmt"));
        assert!(bash_requires_approval("cargo test"));
        assert!(bash_requires_approval("echo hi > file.txt"));
        assert!(bash_requires_approval("rm -rf target"));
        assert!(bash_requires_approval("rm file.txt"));
        assert!(bash_requires_approval("mv a b"));
        assert!(bash_requires_approval("cp a b"));
        assert!(bash_requires_approval("chmod 600 file"));
        assert!(bash_requires_approval("mkdir tmp"));
        assert!(bash_requires_approval("touch file"));
        assert!(bash_requires_approval("git diff --output=file.patch"));
        assert!(bash_requires_approval("cat ~/.ssh/id_rsa"));
        assert!(bash_requires_approval("grep -R . ~/.ssh"));
        assert!(bash_requires_approval("ls $HOME/.ssh"));
        assert!(bash_requires_approval("rg PRIVATE $HOME/.ssh/*"));
        assert!(bash_requires_approval("rg SECRET .env.local"));
        assert!(bash_requires_approval("rg --hidden --no-ignore TOKEN ."));
        assert!(bash_requires_approval("grep -R TOKEN ."));
        assert!(bash_requires_approval("git show HEAD:.env.local"));
        assert!(bash_requires_approval("echo ok; rm -rf target"));
        assert!(bash_requires_approval(
            "python3 -c 'open(\"x\",\"w\").write(\"y\")'"
        ));
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
}
