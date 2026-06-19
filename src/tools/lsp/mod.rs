use std::{
    fs,
    io::{self, BufRead, BufReader, Read as _, Write as _},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
    sync::mpsc::{self, Receiver},
    thread,
    time::{Duration, Instant},
};

use serde_json::{Value, json};

use crate::agent::provider::{ToolCall, ToolResult};

mod description;

use super::{
    ToolBehavior,
    utils::{
        display_path, error, missing_arg, path_arg, program_in_path, resolve_tool_path, string_arg,
    },
};

const LSP_TIMEOUT: Duration = Duration::from_secs(20);

pub(super) struct LspTool;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LspOperation {
    Definition,
    References,
    Hover,
    DocumentSymbols,
    WorkspaceSymbols,
}

impl LspOperation {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "definition" => Some(Self::Definition),
            "references" => Some(Self::References),
            "hover" => Some(Self::Hover),
            "document_symbols" => Some(Self::DocumentSymbols),
            "workspace_symbols" => Some(Self::WorkspaceSymbols),
            _ => None,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Definition => "definition",
            Self::References => "references",
            Self::Hover => "hover",
            Self::DocumentSymbols => "document symbols",
            Self::WorkspaceSymbols => "workspace symbols",
        }
    }

    fn method(self) -> &'static str {
        match self {
            Self::Definition => "textDocument/definition",
            Self::References => "textDocument/references",
            Self::Hover => "textDocument/hover",
            Self::DocumentSymbols => "textDocument/documentSymbol",
            Self::WorkspaceSymbols => "workspace/symbol",
        }
    }

    fn needs_file(self) -> bool {
        !matches!(self, Self::WorkspaceSymbols)
    }

    fn needs_position(self) -> bool {
        matches!(self, Self::Definition | Self::References | Self::Hover)
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
        lsp(call)
    }

    fn is_concurrency_safe(&self, _call: &ToolCall) -> bool {
        true
    }

    fn input_summary(&self, call: &ToolCall) -> Option<String> {
        let operation = string_arg(call, "operation")?;
        if operation == "workspace_symbols" {
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

fn lsp(call: &ToolCall) -> ToolResult {
    match lsp_result(call) {
        Ok(content) => ToolResult {
            call_id: call.id.clone(),
            content,
            is_error: false,
        },
        Err(message) => error(call, message),
    }
}

fn lsp_result(call: &ToolCall) -> Result<String, String> {
    if !program_in_path("rust-analyzer") {
        return Err("Missing dependency: rust-analyzer is required for LSP.".to_owned());
    }

    let operation = operation_arg(call)?;
    let file = if operation.needs_file() {
        Some(file_arg(call)?)
    } else {
        None
    };
    if let Some(path) = &file
        && path.extension().and_then(|extension| extension.to_str()) != Some("rs")
    {
        return Err(format!(
            "LSP currently supports Rust files through rust-analyzer, got {}.",
            path.display()
        ));
    }

    let cwd = std::env::current_dir().map_err(|err| format!("failed to read cwd: {err}"))?;
    let root_uri = file_uri(&cwd);
    let mut session = LspSession::start(&cwd)?;
    session.initialize(&root_uri)?;

    if let Some(path) = &file {
        session.open_file(path)?;
    }

    let params = request_params(call, operation, file.as_deref())?;
    let result = session.request(operation.method(), params)?;
    session.shutdown();
    Ok(format_result(operation, &result))
}

fn operation_arg(call: &ToolCall) -> Result<LspOperation, String> {
    let Some(operation) = string_arg(call, "operation") else {
        return Err(missing_arg(call, "operation").content);
    };
    LspOperation::parse(operation).ok_or_else(|| {
        format!(
            "unsupported LSP operation '{operation}'. Use definition, references, hover, document_symbols, or workspace_symbols."
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
    if operation == LspOperation::WorkspaceSymbols {
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

    if operation == LspOperation::References {
        params["context"] = json!({ "includeDeclaration": true });
    }

    Ok(params)
}

struct LspSession {
    child: Child,
    stdin: ChildStdin,
    rx: Receiver<Value>,
    next_id: u64,
}

impl LspSession {
    fn start(cwd: &Path) -> Result<Self, String> {
        let mut child = Command::new("rust-analyzer")
            .current_dir(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|err| format!("failed to start rust-analyzer: {err}"))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| "failed to open rust-analyzer stdin".to_owned())?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "failed to open rust-analyzer stdout".to_owned())?;
        let rx = spawn_lsp_reader(stdout);

        Ok(Self {
            child,
            stdin,
            rx,
            next_id: 1,
        })
    }

    fn initialize(&mut self, root_uri: &str) -> Result<(), String> {
        let _ = self.request_with_params(
            "initialize",
            json!({
                "processId": null,
                "rootUri": root_uri,
                "capabilities": {},
                "workspaceFolders": [
                    { "uri": root_uri, "name": "workspace" }
                ]
            }),
        )?;
        self.notify("initialized", json!({}))?;
        Ok(())
    }

    fn open_file(&mut self, path: &Path) -> Result<(), String> {
        let text = fs::read_to_string(path)
            .map_err(|err| format!("failed to read {} for LSP: {err}", path.display()))?;
        self.notify(
            "textDocument/didOpen",
            json!({
                "textDocument": {
                    "uri": file_uri(path),
                    "languageId": "rust",
                    "version": 1,
                    "text": text,
                }
            }),
        )
    }

    fn request(&mut self, method: &str, params: Value) -> Result<Value, String> {
        self.request_with_params(method, params)
    }

    fn request_with_params(&mut self, method: &str, params: Value) -> Result<Value, String> {
        let id = self.next_id;
        self.next_id += 1;
        send_lsp_message(
            &mut self.stdin,
            &json!({
                "jsonrpc": "2.0",
                "id": id,
                "method": method,
                "params": params,
            }),
        )?;
        wait_for_response(&self.rx, &mut self.stdin, id, LSP_TIMEOUT)
    }

    fn notify(&mut self, method: &str, params: Value) -> Result<(), String> {
        send_lsp_message(
            &mut self.stdin,
            &json!({
                "jsonrpc": "2.0",
                "method": method,
                "params": params,
            }),
        )
    }

    fn shutdown(&mut self) {
        if let Ok(id) = self.send_shutdown_request() {
            let _ = wait_for_response(&self.rx, &mut self.stdin, id, Duration::from_millis(500));
        }
        let _ = self.notify("exit", json!(null));
        let _ = self.child.kill();
        let _ = self.child.wait();
    }

    fn send_shutdown_request(&mut self) -> Result<u64, String> {
        let id = self.next_id;
        self.next_id += 1;
        send_lsp_message(
            &mut self.stdin,
            &json!({
                "jsonrpc": "2.0",
                "id": id,
                "method": "shutdown",
                "params": null,
            }),
        )?;
        Ok(id)
    }
}

fn spawn_lsp_reader(stdout: ChildStdout) -> Receiver<Value> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        while let Ok(Some(message)) = read_lsp_message(&mut reader) {
            if tx.send(message).is_err() {
                break;
            }
        }
    });
    rx
}

fn read_lsp_message(reader: &mut BufReader<ChildStdout>) -> io::Result<Option<Value>> {
    let mut content_length = None;
    loop {
        let mut line = String::new();
        let bytes = reader.read_line(&mut line)?;
        if bytes == 0 {
            return Ok(None);
        }
        let line = line.trim_end_matches(['\r', '\n']);
        if line.is_empty() {
            break;
        }
        if let Some(value) = line.strip_prefix("Content-Length:") {
            content_length = value.trim().parse::<usize>().ok();
        }
    }

    let Some(content_length) = content_length else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "missing LSP Content-Length header",
        ));
    };
    let mut body = vec![0; content_length];
    reader.read_exact(&mut body)?;
    serde_json::from_slice(&body)
        .map(Some)
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))
}

fn send_lsp_message(stdin: &mut ChildStdin, message: &Value) -> Result<(), String> {
    let body = message.to_string();
    write!(stdin, "Content-Length: {}\r\n\r\n{body}", body.len())
        .and_then(|_| stdin.flush())
        .map_err(|err| format!("failed to send LSP message: {err}"))
}

fn wait_for_response(
    rx: &Receiver<Value>,
    stdin: &mut ChildStdin,
    id: u64,
    timeout: Duration,
) -> Result<Value, String> {
    let deadline = Instant::now() + timeout;
    loop {
        let now = Instant::now();
        if now >= deadline {
            return Err(format!("LSP request {id} timed out"));
        }
        let message = rx
            .recv_timeout(deadline - now)
            .map_err(|_| format!("LSP request {id} timed out"))?;

        if is_server_request(&message, id) {
            respond_to_server_request(stdin, &message)?;
            continue;
        }

        if message.get("id").and_then(Value::as_u64) != Some(id) {
            continue;
        }

        if let Some(error) = message.get("error") {
            return Err(format!("LSP request failed: {error}"));
        }
        return Ok(message.get("result").cloned().unwrap_or(Value::Null));
    }
}

fn is_server_request(message: &Value, awaited_id: u64) -> bool {
    message.get("method").is_some()
        && message.get("id").is_some()
        && message.get("id").and_then(Value::as_u64) != Some(awaited_id)
}

fn respond_to_server_request(stdin: &mut ChildStdin, message: &Value) -> Result<(), String> {
    let id = message.get("id").cloned().unwrap_or(Value::Null);
    send_lsp_message(
        stdin,
        &json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": null,
        }),
    )
}

fn format_result(operation: LspOperation, result: &Value) -> String {
    match operation {
        LspOperation::Definition | LspOperation::References => format_locations(operation, result),
        LspOperation::Hover => format_hover(result),
        LspOperation::DocumentSymbols => format_document_symbols(result),
        LspOperation::WorkspaceSymbols => format_workspace_symbols(result),
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

fn file_uri(path: &Path) -> String {
    format!(
        "file://{}",
        percent_encode_path(&path.display().to_string())
    )
}

fn path_from_file_uri(uri: &str) -> String {
    percent_decode(uri.strip_prefix("file://").unwrap_or(uri))
}

fn percent_encode_path(path: &str) -> String {
    let mut encoded = String::new();
    for byte in path.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(byte as char);
            }
            _ => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    encoded
}

fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%'
            && index + 2 < bytes.len()
            && let Ok(hex) = std::str::from_utf8(&bytes[index + 1..index + 3])
            && let Ok(byte) = u8::from_str_radix(hex, 16)
        {
            decoded.push(byte);
            index += 3;
            continue;
        }
        decoded.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&decoded).into_owned()
}

#[cfg(test)]
mod tests {
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
            format_locations(LspOperation::Definition, &result),
            "/tmp/project/src/main.rs:10:5"
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

        let file_path = Path::new("src/tools/lsp/mod.rs");
        let call = ToolCall {
            id: "lsp".to_owned(),
            name: "LSP".to_owned(),
            arguments: json!({
                "operation": "document_symbols",
                "file_path": file_path
            }),
        };

        let output = lsp_result(&call).expect("live document_symbols should succeed");

        assert!(output.contains("LspSession"));
    }
}
