use std::{
    cell::RefCell,
    env,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::Duration,
};

use serde_json::Value;

use crate::agent::provider::{ToolCall, ToolResult};

const POLL_INTERVAL: Duration = Duration::from_millis(50);

thread_local! {
    static TOOL_CWD: RefCell<Option<PathBuf>> = const { RefCell::new(None) };
}

pub(crate) fn with_tool_cwd<R>(cwd: PathBuf, f: impl FnOnce() -> R) -> R {
    let previous = TOOL_CWD.with(|slot| slot.replace(Some(cwd)));
    let result = f();
    TOOL_CWD.with(|slot| {
        slot.replace(previous);
    });
    result
}

pub(super) fn requires_path_approval(call: &ToolCall) -> bool {
    let path = string_arg(call, "file_path").or_else(|| string_arg(call, "path"));
    path.is_some_and(|path| {
        is_protected_path(path)
            || resolve_tool_path(path)
                .ok()
                .is_some_and(|path| is_protected_path(&path.display().to_string()))
    })
}

pub(super) fn is_protected_path(path: &str) -> bool {
    Path::new(path).components().any(|component| {
        let text = component.as_os_str().to_string_lossy();
        text.starts_with(".env")
            || text.starts_with(".npmrc")
            || text.starts_with(".pypirc")
            || matches!(text.as_ref(), ".glint" | ".git" | ".envrc")
    }) || path.contains("settings.local.json")
        || path.contains("id_rsa")
        || path.contains("id_ed25519")
}

pub(super) fn command_result(
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

pub(super) fn command_error_message(
    status: std::process::ExitStatus,
    stderr_lines: Vec<String>,
) -> String {
    if stderr_lines.is_empty() {
        format!("exit status: {status}")
    } else {
        stderr_lines.join("\n")
    }
}

pub(super) fn child_output(call: &ToolCall, child: std::process::Child) -> ToolResult {
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
pub(super) fn prepare_killable_command(command: &mut Command) {
    use std::os::unix::process::CommandExt;

    command.process_group(0);
}

#[cfg(not(unix))]
pub(super) fn prepare_killable_command(_command: &mut Command) {}

#[cfg(unix)]
pub(super) fn kill_child_tree(child: &mut std::process::Child) {
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
pub(super) fn kill_child_tree(child: &mut std::process::Child) {
    child.kill().ok();
    child.wait().ok();
}

pub(super) fn program_in_path(program: &str) -> bool {
    let names = program_names(program);
    env::var_os("PATH").is_some_and(|path| {
        env::split_paths(&path)
            .any(|dir| names.iter().any(|name| is_executable_file(&dir.join(name))))
    })
}

#[cfg(windows)]
pub(super) fn program_names(program: &str) -> Vec<String> {
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
pub(super) fn program_names(program: &str) -> Vec<String> {
    vec![program.to_owned()]
}

#[cfg(unix)]
pub(super) fn is_executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;

    path.metadata()
        .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
pub(super) fn is_executable_file(path: &Path) -> bool {
    path.is_file()
}

pub(super) fn slice_lines(content: String, call: &ToolCall) -> String {
    let offset = usize_arg(call, "offset").unwrap_or(0);
    let limit = usize_arg(call, "limit").unwrap_or(usize::MAX);
    content
        .lines()
        .skip(offset)
        .take(limit)
        .collect::<Vec<_>>()
        .join("\n")
}

pub(super) fn resolve_tool_path(path: &str) -> Result<PathBuf, String> {
    let cwd = current_tool_dir()?;
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

pub(super) fn expand_home_path(path: &str) -> PathBuf {
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

pub(super) fn string_arg<'a>(call: &'a ToolCall, name: &str) -> Option<&'a str> {
    call.arguments.get(name).and_then(Value::as_str)
}

pub(super) fn normalize_path_argument(arguments: &mut Value, name: &str) {
    let Value::Object(properties) = arguments else {
        return;
    };
    let Some(Value::String(path)) = properties.get_mut(name) else {
        return;
    };
    *path = display_path(path);
}

pub(super) fn path_arg(call: &ToolCall, name: &str) -> Option<String> {
    string_arg(call, name).map(display_path)
}

pub(super) fn glob_summary(call: &ToolCall) -> Option<String> {
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

pub(super) fn truncate_summary(output: &str) -> String {
    const MAX_SUMMARY_CHARS: usize = 120;

    if output.chars().count() <= MAX_SUMMARY_CHARS {
        return output.to_owned();
    }

    format!(
        "{}...",
        output.chars().take(MAX_SUMMARY_CHARS).collect::<String>()
    )
}

pub(super) fn display_path(path: &str) -> String {
    let cwd = current_tool_dir().unwrap_or_else(|_| PathBuf::from("."));
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

pub(super) fn current_tool_dir() -> Result<PathBuf, String> {
    if let Some(cwd) = TOOL_CWD.with(|slot| slot.borrow().clone()) {
        return Ok(cwd);
    }
    std::env::current_dir().map_err(|err| format!("failed to read current directory: {err}"))
}

pub(super) fn display_relative_path(path: &Path) -> String {
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

pub(super) fn display_path_string(path: &Path) -> String {
    path.display().to_string().replace('\\', "/")
}

pub(super) fn usize_arg(call: &ToolCall, name: &str) -> Option<usize> {
    call.arguments
        .get(name)
        .and_then(Value::as_u64)
        .map(|value| value as usize)
}

pub(super) fn missing_arg(call: &ToolCall, name: &str) -> ToolResult {
    error(call, format!("missing required argument '{name}'"))
}

pub(super) fn ok(call: &ToolCall, content: String) -> ToolResult {
    ToolResult {
        call_id: call.id.clone(),
        content,
        is_error: false,
    }
}

pub(super) fn error(call: &ToolCall, content: String) -> ToolResult {
    ToolResult {
        call_id: call.id.clone(),
        content,
        is_error: true,
    }
}
