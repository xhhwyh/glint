use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use serde_json::Value;

use crate::config::{LspConfig, LspServerConfig};

use super::instance::LspServerInstance;

#[derive(Clone)]
pub(crate) struct LspManager {
    inner: Arc<Mutex<LspManagerInner>>,
}

struct LspManagerInner {
    root: PathBuf,
    servers: BTreeMap<String, LspServerConfig>,
    extension_to_server: BTreeMap<String, String>,
    instances: BTreeMap<String, LspServerInstance>,
}

impl LspManager {
    pub(crate) fn new(config: LspConfig, root: PathBuf) -> Self {
        Self {
            inner: Arc::new(Mutex::new(LspManagerInner::new(config, root))),
        }
    }

    pub(crate) fn has_server_for_path(&self, path: &Path) -> bool {
        self.inner
            .lock()
            .expect("lsp manager lock")
            .server_name_for_path(path)
            .is_some()
    }

    pub(crate) fn open_file(&self, path: &Path) -> Result<(), String> {
        let mut inner = self.inner.lock().expect("lsp manager lock");
        let server_name = inner.server_name_for_file(path)?;
        inner.instance_mut(&server_name)?.open_file(path)
    }

    pub(crate) fn change_file(&self, path: &Path, text: String) -> Result<(), String> {
        let mut inner = self.inner.lock().expect("lsp manager lock");
        let server_name = inner.server_name_for_file(path)?;
        inner.instance_mut(&server_name)?.change_file(path, text)
    }

    pub(crate) fn save_file(&self, path: &Path) -> Result<(), String> {
        let mut inner = self.inner.lock().expect("lsp manager lock");
        let server_name = inner.server_name_for_file(path)?;
        inner.instance_mut(&server_name)?.save_file(path)
    }

    pub(crate) fn send_request(
        &self,
        path: Option<&Path>,
        method: &str,
        params: Value,
    ) -> Result<Value, String> {
        let mut inner = self.inner.lock().expect("lsp manager lock");
        let server_name = inner.server_name_for_request(path)?;
        inner.instance_mut(&server_name)?.request(method, params)
    }

    pub(crate) fn shutdown(&self) {
        self.inner.lock().expect("lsp manager lock").shutdown();
    }

    #[cfg(test)]
    pub(crate) fn start_count(&self, server_name: &str) -> u64 {
        self.inner
            .lock()
            .expect("lsp manager lock")
            .instances
            .get(server_name)
            .map(LspServerInstance::start_count)
            .unwrap_or(0)
    }
}

impl LspManagerInner {
    fn new(config: LspConfig, root: PathBuf) -> Self {
        let mut extension_to_server = BTreeMap::new();
        for (server_name, server) in &config.servers {
            for extension in server.extension_to_language.keys() {
                extension_to_server
                    .entry(normalize_extension(extension))
                    .or_insert_with(|| server_name.clone());
            }
        }

        Self {
            root,
            servers: config.servers,
            extension_to_server,
            instances: BTreeMap::new(),
        }
    }

    fn server_name_for_file(&self, path: &Path) -> Result<String, String> {
        self.server_name_for_path(path)
            .ok_or_else(|| no_server_for_path(path))
    }

    fn server_name_for_request(&self, path: Option<&Path>) -> Result<String, String> {
        if let Some(path) = path {
            return self.server_name_for_file(path);
        }
        match self.servers.len() {
            0 => Err("no LSP server available".to_owned()),
            1 => self
                .servers
                .keys()
                .next()
                .cloned()
                .ok_or_else(|| "no LSP server available".to_owned()),
            _ => Err(
                "workspaceSymbol needs file_path when multiple LSP servers are configured"
                    .to_owned(),
            ),
        }
    }

    fn server_name_for_path(&self, path: &Path) -> Option<String> {
        extension_key(path).and_then(|extension| self.extension_to_server.get(&extension).cloned())
    }

    fn instance_mut(&mut self, server_name: &str) -> Result<&mut LspServerInstance, String> {
        let config = self
            .servers
            .get(server_name)
            .cloned()
            .ok_or_else(|| format!("LSP server '{server_name}' is not configured"))?;
        let root = self.root.clone();
        Ok(self
            .instances
            .entry(server_name.to_owned())
            .or_insert_with(|| LspServerInstance::new(server_name.to_owned(), config, root)))
    }

    fn shutdown(&mut self) {
        for instance in self.instances.values_mut() {
            instance.shutdown();
        }
    }
}

impl Drop for LspManagerInner {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn no_server_for_path(path: &Path) -> String {
    let extension = extension_key(path).unwrap_or_else(|| "<none>".to_owned());
    format!("no LSP server available for extension '{extension}'")
}

fn extension_key(path: &Path) -> Option<String> {
    path.extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| format!(".{extension}"))
}

fn normalize_extension(extension: &str) -> String {
    if extension.starts_with('.') {
        extension.to_owned()
    } else {
        format!(".{extension}")
    }
}

pub(crate) fn file_uri(path: &Path) -> String {
    format!(
        "file://{}",
        percent_encode_path(&path.display().to_string())
    )
}

pub(crate) fn path_from_file_uri(uri: &str) -> String {
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
    use std::{env, fs};

    use serde_json::json;

    use super::*;

    #[test]
    fn routes_paths_by_extension() {
        let manager = LspManager::new(test_config(), PathBuf::from("/workspace"));

        assert!(manager.has_server_for_path(Path::new("/workspace/src/main.rs")));
        assert!(!manager.has_server_for_path(Path::new("/workspace/src/main.py")));
    }

    #[test]
    fn unknown_extension_reports_no_server() {
        let manager = LspManager::new(test_config(), PathBuf::from("/workspace"));
        let error = manager
            .open_file(Path::new("/workspace/src/main.py"))
            .unwrap_err();

        assert!(error.contains("no LSP server available for extension '.py'"));
    }

    #[test]
    fn workspace_request_needs_file_hint_with_multiple_servers() {
        let mut config = test_config();
        config.servers.insert(
            "python".to_owned(),
            LspServerConfig {
                command: "pyright-langserver".to_owned(),
                args: vec!["--stdio".to_owned()],
                extension_to_language: BTreeMap::from([(".py".to_owned(), "python".to_owned())]),
                startup_timeout_ms: 20_000,
                max_restarts: 3,
            },
        );
        let manager = LspManager::new(config, PathBuf::from("/workspace"));
        let error = manager
            .send_request(None, "workspace/symbol", json!({ "query": "App" }))
            .unwrap_err();

        assert!(error.contains("workspaceSymbol needs file_path"));
    }

    #[test]
    #[ignore]
    fn rust_analyzer_document_symbol_reuses_server() {
        if !program_in_path("rust-analyzer") {
            return;
        }

        let root = env::current_dir().expect("cwd");
        let file = root.join("src/tools/lsp/mod.rs");
        let manager = LspManager::new(LspConfig::default(), root);

        manager.open_file(&file).expect("open file");
        let params = json!({ "textDocument": { "uri": file_uri(&file) } });
        let first = manager
            .send_request(Some(&file), "textDocument/documentSymbol", params.clone())
            .expect("first documentSymbol");
        let second = manager
            .send_request(Some(&file), "textDocument/documentSymbol", params)
            .expect("second documentSymbol");

        assert!(first.is_array());
        assert!(second.is_array());
        assert_eq!(manager.start_count("rust"), 1);
        manager.shutdown();
    }

    fn test_config() -> LspConfig {
        LspConfig {
            servers: BTreeMap::from([(
                "rust".to_owned(),
                LspServerConfig {
                    command: "rust-analyzer".to_owned(),
                    args: Vec::new(),
                    extension_to_language: BTreeMap::from([(".rs".to_owned(), "rust".to_owned())]),
                    startup_timeout_ms: 20_000,
                    max_restarts: 3,
                },
            )]),
        }
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
