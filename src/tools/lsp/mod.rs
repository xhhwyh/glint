use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::{
    agent::provider::{ToolCall, ToolResult},
    services::lsp::{LspManager, file_uri, path_from_file_uri},
};

mod description;

use super::{
    ToolBehavior,
    utils::{display_path, error, missing_arg, path_arg, resolve_tool_path, string_arg},
};

pub(super) struct LspTool;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LspOperation {
    GoToDefinition,
    FindReferences,
    Hover,
    DocumentSymbol,
    WorkspaceSymbol,
}

impl LspOperation {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "goToDefinition" => Some(Self::GoToDefinition),
            "findReferences" => Some(Self::FindReferences),
            "hover" => Some(Self::Hover),
            "documentSymbol" => Some(Self::DocumentSymbol),
            "workspaceSymbol" => Some(Self::WorkspaceSymbol),
            _ => None,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::GoToDefinition => "definition",
            Self::FindReferences => "references",
            Self::Hover => "hover",
            Self::DocumentSymbol => "document symbols",
            Self::WorkspaceSymbol => "workspace symbols",
        }
    }

    fn method(self) -> &'static str {
        match self {
            Self::GoToDefinition => "textDocument/definition",
            Self::FindReferences => "textDocument/references",
            Self::Hover => "textDocument/hover",
            Self::DocumentSymbol => "textDocument/documentSymbol",
            Self::WorkspaceSymbol => "workspace/symbol",
        }
    }

    fn needs_file(self) -> bool {
        !matches!(self, Self::WorkspaceSymbol)
    }

    fn needs_position(self) -> bool {
        matches!(
            self,
            Self::GoToDefinition | Self::FindReferences | Self::Hover
        )
    }
}

impl ToolBehavior for LspTool {
    fn name(&self) -> &'static str {
        "LSP"
    }

    fn description(&self) -> &'static str {
        description::DESCRIPTION
    }

    fn required_args(&self) -> &'static [&'static str] {
        description::REQUIRED_ARGS
    }

    fn execute(&self, call: &ToolCall, _is_cancelled: &mut dyn FnMut() -> bool) -> ToolResult {
        lsp(call, None)
    }

    fn is_concurrency_safe(&self, _call: &ToolCall) -> bool {
        true
    }

    fn input_summary(&self, call: &ToolCall) -> Option<String> {
        let operation = string_arg(call, "operation")?;
        if LspOperation::parse(operation) == Some(LspOperation::WorkspaceSymbol) {
            return string_arg(call, "query")
                .map(|query| format!("{operation}: {query}"))
                .or_else(|| Some(operation.to_owned()));
        }

        let path = path_arg(call, "file_path").unwrap_or_else(|| "?".to_owned());
        let line = call.arguments.get("line").and_then(Value::as_u64);
        let character = call.arguments.get("character").and_then(Value::as_u64);
        Some(match (line, character) {
            (Some(line), Some(character)) => format!("{operation}: {path}:{line}:{character}"),
            _ => format!("{operation}: {path}"),
        })
    }
}

pub(super) fn lsp(call: &ToolCall, manager: Option<&LspManager>) -> ToolResult {
    let Some(manager) = manager else {
        return error(
            call,
            "LSP manager is unavailable for this session.".to_owned(),
        );
    };
    match lsp_result(call, manager) {
        Ok(content) => ToolResult {
            call_id: call.id.clone(),
            content,
            is_error: false,
        },
        Err(message) => error(call, message),
    }
}

fn lsp_result(call: &ToolCall, manager: &LspManager) -> Result<String, String> {
    let operation = operation_arg(call)?;
    let file = if operation.needs_file() || string_arg(call, "file_path").is_some() {
        Some(file_arg(call)?)
    } else {
        None
    };

    if let Some(path) = &file {
        manager.open_file(path)?;
    }

    let params = request_params(call, operation, file.as_deref())?;
    let result = manager.send_request(file.as_deref(), operation.method(), params)?;
    Ok(format_result(operation, &result))
}

fn operation_arg(call: &ToolCall) -> Result<LspOperation, String> {
    let Some(operation) = string_arg(call, "operation") else {
        return Err(missing_arg(call, "operation").content);
    };
    LspOperation::parse(operation).ok_or_else(|| {
        format!(
            "unsupported LSP operation '{operation}'. Use goToDefinition, findReferences, hover, documentSymbol, or workspaceSymbol."
        )
    })
}

fn file_arg(call: &ToolCall) -> Result<PathBuf, String> {
    let Some(path) = string_arg(call, "file_path") else {
        return Err(missing_arg(call, "file_path").content);
    };
    resolve_tool_path(path)
}

fn position_arg(call: &ToolCall, name: &str) -> Result<u64, String> {
    let Some(value) = call.arguments.get(name).and_then(Value::as_u64) else {
        return Err(missing_arg(call, name).content);
    };
    if value == 0 {
        return Err(format!("{name} is 1-based and must be greater than 0"));
    }
    Ok(value - 1)
}

fn request_params(
    call: &ToolCall,
    operation: LspOperation,
    file: Option<&Path>,
) -> Result<Value, String> {
    if operation == LspOperation::WorkspaceSymbol {
        let Some(query) = string_arg(call, "query") else {
            return Err(missing_arg(call, "query").content);
        };
        return Ok(json!({ "query": query }));
    }

    let file = file.expect("file required for non-workspace LSP operations");
    let uri = file_uri(file);
    let mut params = json!({
        "textDocument": { "uri": uri }
    });

    if operation.needs_position() {
        params["position"] = json!({
            "line": position_arg(call, "line")?,
            "character": position_arg(call, "character")?,
        });
    }

    if operation == LspOperation::FindReferences {
        params["context"] = json!({ "includeDeclaration": true });
    }

    Ok(params)
}

fn format_result(operation: LspOperation, result: &Value) -> String {
    match operation {
        LspOperation::GoToDefinition | LspOperation::FindReferences => {
            format_locations(operation, result)
        }
        LspOperation::Hover => format_hover(result),
        LspOperation::DocumentSymbol => format_document_symbols(result),
        LspOperation::WorkspaceSymbol => format_workspace_symbols(result),
    }
}

fn format_locations(operation: LspOperation, result: &Value) -> String {
    let locations = locations_from_result(result);
    if locations.is_empty() {
        return format!("No {} found.", operation.label());
    }
    locations.join("\n")
}

fn locations_from_result(result: &Value) -> Vec<String> {
    match result {
        Value::Array(items) => items.iter().filter_map(format_location).collect(),
        Value::Object(_) => format_location(result).into_iter().collect(),
        _ => Vec::new(),
    }
}

fn format_location(value: &Value) -> Option<String> {
    let uri = value
        .get("uri")
        .or_else(|| value.get("targetUri"))
        .and_then(Value::as_str)?;
    let range = value
        .get("range")
        .or_else(|| value.get("targetSelectionRange"))
        .or_else(|| value.get("targetRange"))?;
    let start = range.get("start")?;
    Some(format!(
        "{}:{}:{}",
        display_path(&path_from_file_uri(uri)),
        one_based_position(start, "line"),
        one_based_position(start, "character")
    ))
}

fn format_hover(result: &Value) -> String {
    let Some(contents) = result.get("contents") else {
        return "No hover information found.".to_owned();
    };
    let text = hover_text(contents).trim().to_owned();
    if text.is_empty() {
        "No hover information found.".to_owned()
    } else {
        text
    }
}

fn hover_text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Array(items) => items.iter().map(hover_text).collect::<Vec<_>>().join("\n"),
        Value::Object(object) => object
            .get("value")
            .or_else(|| object.get("language"))
            .map(hover_text)
            .unwrap_or_else(|| value.to_string()),
        _ => String::new(),
    }
}

fn format_document_symbols(result: &Value) -> String {
    let Value::Array(items) = result else {
        return "No document symbols found.".to_owned();
    };
    let mut lines = Vec::new();
    for item in items {
        push_document_symbol(item, 0, &mut lines);
    }
    if lines.is_empty() {
        "No document symbols found.".to_owned()
    } else {
        lines.join("\n")
    }
}

fn push_document_symbol(symbol: &Value, depth: usize, lines: &mut Vec<String>) {
    if let Some(name) = symbol.get("name").and_then(Value::as_str) {
        let line = symbol
            .get("selectionRange")
            .or_else(|| symbol.get("range"))
            .and_then(|range| range.get("start"))
            .map(|start| one_based_position(start, "line"))
            .unwrap_or(0);
        lines.push(format!("{}{}:{}", "  ".repeat(depth), name, line));
    }
    if let Some(children) = symbol.get("children").and_then(Value::as_array) {
        for child in children {
            push_document_symbol(child, depth + 1, lines);
        }
    }
    if symbol.get("location").is_some()
        && let Some(name) = symbol.get("name").and_then(Value::as_str)
        && let Some(location) = symbol.get("location").and_then(format_location)
    {
        lines.push(format!("{name} -> {location}"));
    }
}

fn format_workspace_symbols(result: &Value) -> String {
    let Value::Array(items) = result else {
        return "No workspace symbols found.".to_owned();
    };
    let lines = items
        .iter()
        .filter_map(|symbol| {
            let name = symbol.get("name").and_then(Value::as_str)?;
            let location = symbol.get("location").and_then(format_location)?;
            Some(format!("{name} -> {location}"))
        })
        .collect::<Vec<_>>();
    if lines.is_empty() {
        "No workspace symbols found.".to_owned()
    } else {
        lines.join("\n")
    }
}

fn one_based_position(value: &Value, key: &str) -> u64 {
    value.get(key).and_then(Value::as_u64).unwrap_or(0) + 1
}

#[cfg(test)]
mod tests {
    use std::{env, fs};

    use super::*;

    #[test]
    fn formats_location_results() {
        let result = json!([
            {
                "uri": "file:///tmp/project/src/main.rs",
                "range": { "start": { "line": 9, "character": 4 } }
            }
        ]);

        assert_eq!(
            format_locations(LspOperation::GoToDefinition, &result),
            "/tmp/project/src/main.rs:10:5"
        );
    }

    #[test]
    fn operations_map_to_lsp_methods() {
        assert_eq!(
            LspOperation::parse("goToDefinition").map(LspOperation::method),
            Some("textDocument/definition")
        );
        assert_eq!(
            LspOperation::parse("findReferences").map(LspOperation::method),
            Some("textDocument/references")
        );
        assert_eq!(
            LspOperation::parse("documentSymbol").map(LspOperation::method),
            Some("textDocument/documentSymbol")
        );
        assert_eq!(
            LspOperation::parse("workspaceSymbol").map(LspOperation::method),
            Some("workspace/symbol")
        );
    }

    #[test]
    fn formats_nested_document_symbols() {
        let result = json!([
            {
                "name": "App",
                "selectionRange": { "start": { "line": 4, "character": 0 } },
                "children": [
                    {
                        "name": "update",
                        "selectionRange": { "start": { "line": 25, "character": 4 } }
                    }
                ]
            }
        ]);

        let output = format_document_symbols(&result);

        assert!(output.contains("App:5"));
        assert!(output.contains("  update:26"));
    }

    #[test]
    fn lsp_request_params_use_one_based_user_positions() {
        let call = ToolCall {
            id: "lsp".to_owned(),
            name: "LSP".to_owned(),
            arguments: json!({
                "operation": "hover",
                "file_path": "src/app.rs",
                "line": 10,
                "character": 5
            }),
        };
        let params = request_params(
            &call,
            LspOperation::Hover,
            Some(Path::new("/tmp/project/src/app.rs")),
        )
        .expect("params");

        assert_eq!(params["position"]["line"], 9);
        assert_eq!(params["position"]["character"], 4);
    }

    #[test]
    #[ignore]
    fn lsp_document_symbols_live_smoke() {
        if !program_in_path("rust-analyzer") {
            return;
        }

        let root = env::current_dir().expect("cwd");
        let manager = LspManager::new(crate::config::LspConfig::default(), root.clone());
        let file_path = root.join("src/tools/lsp/mod.rs");
        let call = ToolCall {
            id: "lsp".to_owned(),
            name: "LSP".to_owned(),
            arguments: json!({
                "operation": "documentSymbol",
                "file_path": file_path.to_string_lossy()
            }),
        };

        let output = lsp_result(&call, &manager).expect("live documentSymbol should succeed");

        assert!(output.contains("LspTool"));
        manager.shutdown();
    }

    fn program_in_path(program: &str) -> bool {
        env::var_os("PATH").is_some_and(|path| {
            env::split_paths(&path).any(|dir| {
                let path = dir.join(program);
                fs::metadata(path).is_ok_and(|metadata| metadata.is_file())
            })
        })
    }
}
