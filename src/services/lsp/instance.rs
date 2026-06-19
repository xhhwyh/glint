use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

use serde_json::{Value, json};

use crate::config::LspServerConfig;

use super::{client::LspClient, manager::file_uri};

const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LspServerState {
    Stopped,
    Starting,
    Running,
    Error,
}

struct OpenFile {
    version: i32,
    text: String,
}

pub(super) struct LspServerInstance {
    name: String,
    config: LspServerConfig,
    root: PathBuf,
    state: LspServerState,
    client: Option<LspClient>,
    failed_starts: u8,
    start_count: u64,
    last_error: Option<String>,
    open_files: BTreeMap<PathBuf, OpenFile>,
}

impl LspServerInstance {
    pub(super) fn new(name: String, config: LspServerConfig, root: PathBuf) -> Self {
        Self {
            name,
            config,
            root,
            state: LspServerState::Stopped,
            client: None,
            failed_starts: 0,
            start_count: 0,
            last_error: None,
            open_files: BTreeMap::new(),
        }
    }

    pub(super) fn open_file(&mut self, path: &Path) -> Result<(), String> {
        let text = fs::read_to_string(path)
            .map_err(|err| format!("failed to read {} for LSP: {err}", path.display()))?;
        if self.open_files.contains_key(path) {
            return self.refresh_open_file(path, text);
        }
        self.open_file_with_text(path, text)
    }

    pub(super) fn change_file(&mut self, path: &Path, text: String) -> Result<(), String> {
        if !self.open_files.contains_key(path) {
            self.open_file_with_text(path, text.clone())?;
        }

        let version = self
            .open_files
            .get(path)
            .map(|file| file.version.saturating_add(1))
            .unwrap_or(1);
        self.notify(
            "textDocument/didChange",
            json!({
                "textDocument": {
                    "uri": file_uri(path),
                    "version": version,
                },
                "contentChanges": [
                    { "text": text.clone() }
                ]
            }),
        )?;
        self.open_files
            .insert(path.to_path_buf(), OpenFile { version, text });
        Ok(())
    }

    pub(super) fn save_file(&mut self, path: &Path) -> Result<(), String> {
        if !self.open_files.contains_key(path) {
            self.open_file(path)?;
        }
        let text = self.open_files.get(path).map(|file| file.text.clone());
        self.notify(
            "textDocument/didSave",
            json!({
                "textDocument": { "uri": file_uri(path) },
                "text": text.unwrap_or_default(),
            }),
        )
    }

    pub(super) fn request(&mut self, method: &str, params: Value) -> Result<Value, String> {
        self.ensure_running()?;
        let result = self
            .client
            .as_mut()
            .expect("running LSP server should have a client")
            .request(method, params, DEFAULT_REQUEST_TIMEOUT);
        match result {
            Ok(result) => Ok(result),
            Err(message) => {
                self.mark_error(message.clone());
                Err(format!(
                    "LSP server '{}' request failed: {message}",
                    self.name
                ))
            }
        }
    }

    pub(super) fn shutdown(&mut self) {
        if let Some(mut client) = self.client.take() {
            client.shutdown();
        }
        self.state = LspServerState::Stopped;
        self.open_files.clear();
    }

    #[cfg(test)]
    pub(super) fn start_count(&self) -> u64 {
        self.start_count
    }

    #[cfg(test)]
    fn open_file_record(&self, path: &Path) -> Option<(i32, &str)> {
        self.open_files
            .get(path)
            .map(|file| (file.version, file.text.as_str()))
    }

    fn open_file_with_text(&mut self, path: &Path, text: String) -> Result<(), String> {
        let language_id = self.language_id_for_path(path).ok_or_else(|| {
            format!(
                "LSP server '{}' does not handle {}",
                self.name,
                path.display()
            )
        })?;
        self.notify(
            "textDocument/didOpen",
            json!({
                "textDocument": {
                    "uri": file_uri(path),
                    "languageId": language_id,
                    "version": 1,
                    "text": text.clone(),
                }
            }),
        )?;
        self.open_files
            .insert(path.to_path_buf(), OpenFile { version: 1, text });
        Ok(())
    }

    fn refresh_open_file(&mut self, path: &Path, text: String) -> Result<(), String> {
        let Some(open_file) = self.open_files.get(path) else {
            return self.open_file_with_text(path, text);
        };
        if open_file.text == text {
            return Ok(());
        }

        self.change_file(path, text)
    }

    fn notify(&mut self, method: &str, params: Value) -> Result<(), String> {
        self.ensure_running()?;
        let result = self
            .client
            .as_mut()
            .expect("running LSP server should have a client")
            .notify(method, params);
        match result {
            Ok(()) => Ok(()),
            Err(message) => {
                self.mark_error(message.clone());
                Err(format!(
                    "LSP server '{}' notification failed: {message}",
                    self.name
                ))
            }
        }
    }

    fn ensure_running(&mut self) -> Result<(), String> {
        if self.state == LspServerState::Running && self.client.is_some() {
            return Ok(());
        }
        if self.failed_starts > self.config.max_restarts {
            let detail = self
                .last_error
                .as_deref()
                .unwrap_or("server start failed too many times");
            return Err(format!(
                "LSP server '{}' is unavailable after {} failed starts: {detail}",
                self.name, self.failed_starts
            ));
        }
        self.start()
    }

    fn start(&mut self) -> Result<(), String> {
        self.state = LspServerState::Starting;
        let timeout = Duration::from_millis(self.config.startup_timeout_ms.max(1));
        let result = LspClient::start(&self.config.command, &self.config.args, &self.root)
            .and_then(|mut client| {
                client.initialize(&file_uri(&self.root), timeout)?;
                Ok(client)
            });

        match result {
            Ok(client) => {
                self.client = Some(client);
                self.state = LspServerState::Running;
                self.start_count += 1;
                self.last_error = None;
                Ok(())
            }
            Err(message) => {
                self.client = None;
                self.state = LspServerState::Error;
                self.failed_starts = self.failed_starts.saturating_add(1);
                self.last_error = Some(message.clone());
                Err(format!(
                    "failed to start LSP server '{}': {message}",
                    self.name
                ))
            }
        }
    }

    fn mark_error(&mut self, message: String) {
        if let Some(mut client) = self.client.take() {
            client.shutdown();
        }
        self.state = LspServerState::Error;
        self.last_error = Some(message);
        self.open_files.clear();
    }

    fn language_id_for_path(&self, path: &Path) -> Option<String> {
        let extension = path.extension()?.to_str()?;
        let dotted = format!(".{extension}");
        self.config
            .extension_to_language
            .get(&dotted)
            .or_else(|| self.config.extension_to_language.get(extension))
            .cloned()
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, env, fs};

    use uuid::Uuid;

    use super::*;

    #[cfg(unix)]
    #[test]
    fn refresh_open_file_updates_cached_text_when_disk_content_changes() {
        let path = env::temp_dir().join(format!("glint-lsp-stale-cache-{}.rs", Uuid::new_v4()));
        fs::write(&path, "fn old() {}\n").expect("write fixture");
        let mut instance = test_instance();
        instance.open_file(&path).expect("open file");

        fs::write(&path, "fn new() {}\n").expect("rewrite fixture");
        instance.open_file(&path).expect("refresh open file");
        let (version, text) = {
            let record = instance.open_file_record(&path).expect("open file record");
            (record.0, record.1.to_owned())
        };
        fs::remove_file(&path).ok();
        instance.shutdown();

        assert_eq!(version, 2);
        assert_eq!(text, "fn new() {}\n");
    }

    #[cfg(unix)]
    #[test]
    fn refresh_open_file_keeps_version_when_disk_content_matches_cache() {
        let path = env::temp_dir().join(format!("glint-lsp-fresh-cache-{}.rs", Uuid::new_v4()));
        fs::write(&path, "fn same() {}\n").expect("write fixture");
        let mut instance = test_instance();
        instance.open_file(&path).expect("open file");

        instance.open_file(&path).expect("refresh open file");
        let (version, text) = {
            let record = instance.open_file_record(&path).expect("open file record");
            (record.0, record.1.to_owned())
        };
        fs::remove_file(&path).ok();
        instance.shutdown();

        assert_eq!(version, 1);
        assert_eq!(text, "fn same() {}\n");
    }

    #[cfg(unix)]
    fn test_instance() -> LspServerInstance {
        LspServerInstance {
            name: "rust".to_owned(),
            config: LspServerConfig {
                command: "cat".to_owned(),
                args: Vec::new(),
                extension_to_language: BTreeMap::from([(".rs".to_owned(), "rust".to_owned())]),
                startup_timeout_ms: 20_000,
                max_restarts: 3,
            },
            root: env::temp_dir(),
            state: LspServerState::Stopped,
            client: None,
            failed_starts: 0,
            start_count: 0,
            last_error: None,
            open_files: BTreeMap::new(),
        }
    }
}
