use std::{
    io::{self, BufRead, BufReader, Read as _, Write as _},
    path::Path,
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
    sync::mpsc::{self, Receiver},
    thread,
    time::{Duration, Instant},
};

use serde_json::{Value, json};

pub(super) struct LspClient {
    child: Child,
    stdin: ChildStdin,
    rx: Receiver<Value>,
    next_id: u64,
    stopped: bool,
}

impl LspClient {
    pub(super) fn start(command: &str, args: &[String], cwd: &Path) -> Result<Self, String> {
        let mut command = Command::new(command);
        command
            .args(args)
            .current_dir(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        prepare_lsp_command(&mut command);

        let mut child = command
            .spawn()
            .map_err(|err| format!("failed to start LSP server: {err}"))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| "failed to open LSP server stdin".to_owned())?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "failed to open LSP server stdout".to_owned())?;
        let rx = spawn_lsp_reader(stdout);

        Ok(Self {
            child,
            stdin,
            rx,
            next_id: 1,
            stopped: false,
        })
    }

    pub(super) fn initialize(&mut self, root_uri: &str, timeout: Duration) -> Result<(), String> {
        let _ = self.request_with_timeout(
            "initialize",
            json!({
                "processId": null,
                "rootUri": root_uri,
                "capabilities": {
                    "textDocument": {
                        "synchronization": {
                            "didSave": true
                        }
                    }
                },
                "workspaceFolders": [
                    { "uri": root_uri, "name": "workspace" }
                ]
            }),
            timeout,
        )?;
        self.notify("initialized", json!({}))?;
        Ok(())
    }

    pub(super) fn request(
        &mut self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, String> {
        self.request_with_timeout(method, params, timeout)
    }

    pub(super) fn notify(&mut self, method: &str, params: Value) -> Result<(), String> {
        send_lsp_message(
            &mut self.stdin,
            &json!({
                "jsonrpc": "2.0",
                "method": method,
                "params": params,
            }),
        )
    }

    pub(super) fn shutdown(&mut self) {
        if let Ok(id) = self.send_shutdown_request() {
            let _ = wait_for_response(&self.rx, &mut self.stdin, id, Duration::from_millis(500));
        }
        let _ = self.notify("exit", json!(null));
        self.kill();
    }

    fn kill(&mut self) {
        if self.stopped {
            return;
        }
        kill_lsp_child(&mut self.child);
        self.stopped = true;
    }

    fn request_with_timeout(
        &mut self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, String> {
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
        wait_for_response(&self.rx, &mut self.stdin, id, timeout)
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

impl Drop for LspClient {
    fn drop(&mut self) {
        self.kill();
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
        let message = match rx.recv_timeout(deadline - now) {
            Ok(message) => message,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                return Err(format!("LSP request {id} timed out"));
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err(format!("LSP server closed before response {id}"));
            }
        };

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

#[cfg(unix)]
fn prepare_lsp_command(command: &mut Command) {
    use std::os::unix::process::CommandExt;

    command.process_group(0);
}

#[cfg(not(unix))]
fn prepare_lsp_command(_command: &mut Command) {}

#[cfg(unix)]
fn kill_lsp_child(child: &mut Child) {
    let pgid = child.id() as libc::pid_t;
    unsafe {
        libc::killpg(pgid, libc::SIGKILL);
    }
    child.kill().ok();
    child.wait().ok();
}

#[cfg(not(unix))]
fn kill_lsp_child(child: &mut Child) {
    child.kill().ok();
    child.wait().ok();
}
