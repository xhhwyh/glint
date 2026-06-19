use std::fs;

use crate::{
    agent::provider::{ToolCall, ToolResult},
    services::lsp::LspManager,
};

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
        edit_approved(call, &ReadFileState::new(), None)
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

pub(super) fn edit_approved(
    call: &ToolCall,
    read_file_state: &ReadFileState,
    lsp_manager: Option<&LspManager>,
) -> ToolResult {
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
            let mut message = format!("Edited {}", path.display());
            if let Some(lsp_message) = sync_lsp_after_edit(lsp_manager, &path) {
                message.push('\n');
                message.push_str(&lsp_message);
            }
            ok(call, message)
        }
        Err(err) => error(call, format!("failed to write {}: {err}", path.display())),
    }
}

fn sync_lsp_after_edit(lsp_manager: Option<&LspManager>, path: &std::path::Path) -> Option<String> {
    let manager = lsp_manager?;
    if !manager.has_server_for_path(path) {
        return None;
    }
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) => return Some(format!("LSP sync failed: {err}")),
    };
    if let Err(message) = manager.change_file(path, text) {
        return Some(format!("LSP sync failed: {message}"));
    }
    if let Err(message) = manager.save_file(path) {
        return Some(format!("LSP sync failed: {message}"));
    }
    None
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

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, env, fs};

    use serde_json::json;
    use uuid::Uuid;

    use super::*;
    use crate::config::{LspConfig, LspServerConfig};

    #[test]
    fn edit_success_reports_lsp_sync_warning_without_failing_edit() {
        let path = env::temp_dir().join(format!("glint-edit-lsp-{}.rs", Uuid::new_v4()));
        fs::write(&path, "fn main() {}\n").expect("write fixture");
        let original = fs::read_to_string(&path).expect("read fixture");
        let read_state = ReadFileState::new();
        read_state.record(path.clone(), original, false);
        let manager = LspManager::new(
            LspConfig {
                servers: BTreeMap::from([(
                    "rust".to_owned(),
                    LspServerConfig {
                        command: "/bin/false".to_owned(),
                        args: Vec::new(),
                        extension_to_language: BTreeMap::from([(
                            ".rs".to_owned(),
                            "rust".to_owned(),
                        )]),
                        startup_timeout_ms: 100,
                        max_restarts: 0,
                    },
                )]),
            },
            env::temp_dir(),
        );
        let call = ToolCall {
            id: "edit".to_owned(),
            name: "Edit".to_owned(),
            arguments: json!({
                "file_path": path.to_string_lossy(),
                "old_string": "fn main() {}\n",
                "new_string": "fn main() { println!(\"hi\"); }\n"
            }),
        };

        let result = edit_approved(&call, &read_state, Some(&manager));
        let updated = fs::read_to_string(&path).expect("read updated");
        fs::remove_file(&path).ok();
        manager.shutdown();

        assert!(!result.is_error);
        assert!(result.content.contains("Edited"));
        assert!(result.content.contains("LSP sync failed"));
        assert!(updated.contains("println!"));
    }
}
