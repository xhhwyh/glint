use std::{
    collections::BTreeMap,
    io::Write,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::{
    plugins::{ExtensionCatalog, PluginLoadResult, PluginManager, PluginsConfig},
    services::mcp::McpConfig,
};

pub use crate::provider_catalog::PromptCacheConfig;

const DEFAULT_SYSTEM_PROMPT: &str = include_str!("../prompts/system.md");

#[derive(Clone)]
pub struct Config {
    pub(crate) config_path: PathBuf,
    pub llm: LlmConfig,
    pub lsp: LspConfig,
    pub mcp: McpConfig,
    pub extensions: ExtensionCatalog,
    pub model_catalog: ModelCatalog,
    pub system_prompt: String,
    pub(crate) plugins: PluginsConfig,
    pub(crate) base_lsp: LspConfig,
    pub(crate) base_mcp: McpConfig,
    pub(crate) base_system_prompt: String,
}

#[derive(Clone)]
pub struct LlmConfig {
    pub provider: String,
    pub base_url: String,
    pub model: String,
    pub providers: Vec<LlmProviderConfig>,
    pub temperature: f32,
    pub max_tokens: u32,
    pub context_window: Option<u64>,
    pub api_key: String,
    pub default_context_window: Option<u64>,
    pub prompt_cache: PromptCacheConfig,
}

#[derive(Clone)]
pub struct LlmProviderConfig {
    pub name: String,
    pub base_url: String,
    pub models: Vec<String>,
    pub model_context_windows: BTreeMap<String, u64>,
    pub api_key_env: String,
    pub prompt_cache: PromptCacheConfig,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LspConfig {
    pub servers: BTreeMap<String, LspServerConfig>,
}

#[allow(dead_code)]
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct UserConfig {
    #[serde(default = "schema_version")]
    pub version: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub llm: Option<UserLlmConfig>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub configured_providers: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub custom_providers: BTreeMap<String, CustomProviderConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mcp: Option<serde_yaml::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plugins: Option<serde_yaml::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lsp: Option<serde_yaml::Value>,
}

impl Default for UserConfig {
    fn default() -> Self {
        Self {
            version: schema_version(),
            llm: None,
            configured_providers: Vec::new(),
            custom_providers: BTreeMap::new(),
            mcp: None,
            plugins: None,
            lsp: None,
        }
    }
}

#[allow(dead_code)]
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct UserLlmConfig {
    pub provider: String,
    pub model: String,
    pub temperature: f32,
    pub max_tokens: u32,
}

#[allow(dead_code)]
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct CustomProviderConfig {
    pub base_url: String,
    pub models: Vec<String>,
}

#[allow(dead_code)]
fn schema_version() -> u16 {
    1
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct LspServerConfig {
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub extension_to_language: BTreeMap<String, String>,
    #[serde(default = "default_lsp_startup_timeout_ms")]
    pub startup_timeout_ms: u64,
    #[serde(default = "default_lsp_max_restarts")]
    pub max_restarts: u8,
}

#[derive(Clone, Default, Deserialize)]
pub struct ModelCatalog {
    #[serde(default)]
    pub providers: BTreeMap<String, ProviderCatalogEntry>,
    #[serde(default)]
    pub models: BTreeMap<String, BTreeMap<String, ModelCatalogEntry>>,
}

#[derive(Clone, Default, Deserialize)]
pub struct ProviderCatalogEntry {
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub unit: String,
}

#[derive(Clone, Default, Deserialize)]
pub struct ModelCatalogEntry {
    #[serde(default)]
    pub positioning: String,
    #[serde(default)]
    pub context: String,
    #[serde(default)]
    pub max_tokens: String,
    #[serde(default)]
    pub price: String,
    #[serde(default)]
    pub input: String,
    #[serde(default)]
    pub output: String,
    #[serde(default)]
    pub cache_read: String,
    #[serde(default)]
    pub cache_write: String,
}

#[derive(Deserialize)]
struct FileConfig {
    llm: FileLlmConfig,
    #[serde(default)]
    lsp: Option<FileLspConfig>,
    #[serde(default)]
    mcp: McpConfig,
    #[serde(default)]
    plugins: PluginsConfig,
}

#[derive(Default, Deserialize)]
struct FileLspConfig {
    #[serde(default)]
    servers: Option<BTreeMap<String, LspServerConfig>>,
}

#[derive(Deserialize)]
struct FileLlmConfig {
    provider: String,
    model: String,
    providers: BTreeMap<String, FileProviderConfig>,
    temperature: f32,
    max_tokens: u32,
    context_window: Option<u64>,
}

#[derive(Deserialize)]
struct FileProviderConfig {
    #[serde(default)]
    description: String,
    base_url: String,
    #[serde(default)]
    unit: String,
    models: Vec<FileModelConfig>,
    api_key_env: String,
    #[serde(default)]
    prompt_cache: Option<FilePromptCacheConfig>,
}

#[derive(Clone, Deserialize)]
struct FilePromptCacheConfig {
    #[serde(default)]
    key: Option<String>,
    #[serde(default)]
    retention: Option<String>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum FileModelConfig {
    Name(String),
    Details(Box<FileModelDetails>),
}

struct ConfigPathInput {
    explicit: Option<PathBuf>,
    glint_config: Option<PathBuf>,
    xdg_config_home: Option<PathBuf>,
    home: Option<PathBuf>,
    cwd: PathBuf,
}

struct InitConfigPathInput {
    explicit: Option<PathBuf>,
    glint_config: Option<PathBuf>,
    xdg_config_home: Option<PathBuf>,
    home: Option<PathBuf>,
}

impl InitConfigPathInput {
    fn from_process(explicit: Option<&Path>) -> Self {
        Self {
            explicit: explicit.map(Path::to_path_buf),
            glint_config: non_empty_env_path("GLINT_CONFIG"),
            xdg_config_home: non_empty_env_path("XDG_CONFIG_HOME"),
            home: non_empty_env_path("HOME"),
        }
    }
}

impl ConfigPathInput {
    fn from_process(explicit: Option<&Path>, cwd: PathBuf) -> Self {
        Self {
            explicit: explicit.map(Path::to_path_buf),
            glint_config: non_empty_env_path("GLINT_CONFIG"),
            xdg_config_home: non_empty_env_path("XDG_CONFIG_HOME"),
            home: non_empty_env_path("HOME"),
            cwd,
        }
    }
}

fn resolve_config_path(input: &ConfigPathInput) -> Result<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(path) = &input.explicit {
        candidates.push(path.clone());
    }
    if let Some(path) = &input.glint_config {
        candidates.push(path.clone());
    }
    candidates.push(input.cwd.join(".glint/config.yaml"));
    if let Some(path) = user_config_path(input) {
        candidates.push(path);
    }
    candidates.push(input.cwd.join("config.yaml"));

    if let Some(path) = candidates.iter().find(|path| path.is_file()) {
        return Ok(path.clone());
    }

    let attempted = candidates
        .iter()
        .map(|path| format!("  - {}", path.display()))
        .collect::<Vec<_>>()
        .join("\n");
    bail!(
        "could not find a Glint configuration; tried:\n{attempted}\nrun `glint init` to create one"
    )
}

fn user_config_path(input: &ConfigPathInput) -> Option<PathBuf> {
    input
        .xdg_config_home
        .as_ref()
        .map(|path| path.join("glint/config.yaml"))
        .or_else(|| {
            input
                .home
                .as_ref()
                .map(|path| path.join(".config/glint/config.yaml"))
        })
}

fn non_empty_env_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

fn resolve_init_config_path(input: &InitConfigPathInput) -> Result<PathBuf> {
    input
        .explicit
        .clone()
        .or_else(|| input.glint_config.clone())
        .or_else(|| {
            input
                .xdg_config_home
                .as_ref()
                .map(|path| path.join("glint/config.yaml"))
        })
        .or_else(|| {
            input
                .home
                .as_ref()
                .map(|path| path.join(".config/glint/config.yaml"))
        })
        .context("could not determine a user configuration path; pass `--config PATH`")
}

pub fn init_config(explicit_path: Option<&Path>) -> Result<PathBuf> {
    let input = InitConfigPathInput::from_process(explicit_path);
    let destination = resolve_init_config_path(&input)?;

    let parent = destination
        .parent()
        .context("configuration path does not have a parent directory")?;
    std::fs::create_dir_all(parent)
        .with_context(|| format!("failed to create {}", parent.display()))?;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&destination)
        .with_context(|| {
            if destination.exists() {
                format!("configuration already exists at {}", destination.display())
            } else {
                format!("failed to create {}", destination.display())
            }
        })?;
    file.write_all(include_bytes!("../config.example.yaml"))
        .with_context(|| format!("failed to write {}", destination.display()))?;
    file.flush()
        .with_context(|| format!("failed to flush {}", destination.display()))?;
    Ok(destination)
}

#[derive(Deserialize)]
struct FileModelDetails {
    name: String,
    #[serde(default)]
    positioning: String,
    #[serde(default)]
    context: Option<serde_yaml::Value>,
    #[serde(default)]
    max_tokens: Option<serde_yaml::Value>,
    #[serde(default)]
    price: Option<serde_yaml::Value>,
    #[serde(default)]
    input: Option<serde_yaml::Value>,
    #[serde(default)]
    output: Option<serde_yaml::Value>,
    #[serde(default)]
    cache_read: Option<serde_yaml::Value>,
    #[serde(default)]
    cache_write: Option<serde_yaml::Value>,
}

impl Config {
    pub fn load(explicit_path: Option<&Path>) -> Result<Self> {
        let workspace = std::env::current_dir().context("failed to resolve current directory")?;
        let input = ConfigPathInput::from_process(explicit_path, workspace.clone());
        let config_path = resolve_config_path(&input)?;
        Self::load_from_path(&config_path, &workspace, |api_key_env| {
            std::env::var(api_key_env).ok()
        })
    }

    fn load_from_path(
        config_path: &Path,
        workspace: &Path,
        api_key: impl Fn(&str) -> Option<String>,
    ) -> Result<Self> {
        let file = std::fs::read_to_string(config_path)
            .with_context(|| format!("failed to read {}", config_path.display()))?;
        let config: FileConfig = serde_yaml::from_str(&file)
            .with_context(|| format!("failed to parse {}", config_path.display()))?;
        let base_system_prompt = DEFAULT_SYSTEM_PROMPT.to_owned();

        let model_catalog = config.llm.model_catalog();
        let base_lsp = config.lsp_config();
        let base_mcp = config.mcp.clone();
        let plugins = config.plugins.clone();
        let plugin_result =
            PluginManager::load(&plugins, base_mcp.clone(), base_lsp.clone(), workspace)?;
        let extension_prompt = plugin_result.catalog.system_prompt_fragment();
        let system_prompt = if extension_prompt.is_empty() {
            base_system_prompt.clone()
        } else {
            format!("{base_system_prompt}\n\n{extension_prompt}")
        };
        Ok(Self {
            config_path: config_path.to_path_buf(),
            llm: config.llm.into_runtime_config(api_key)?,
            lsp: plugin_result.lsp,
            mcp: plugin_result.mcp,
            extensions: plugin_result.catalog,
            model_catalog,
            system_prompt,
            plugins,
            base_lsp,
            base_mcp,
            base_system_prompt,
        })
    }

    pub(crate) fn apply_plugin_load(&mut self, result: PluginLoadResult) {
        let extension_prompt = result.catalog.system_prompt_fragment();
        self.system_prompt = if extension_prompt.is_empty() {
            self.base_system_prompt.clone()
        } else {
            format!("{}\n\n{extension_prompt}", self.base_system_prompt)
        };
        self.lsp = result.lsp;
        self.mcp = result.mcp;
        self.extensions = result.catalog;
    }
}

impl Default for LspConfig {
    fn default() -> Self {
        let mut extension_to_language = BTreeMap::new();
        extension_to_language.insert(".rs".to_owned(), "rust".to_owned());

        let mut servers = BTreeMap::new();
        servers.insert(
            "rust".to_owned(),
            LspServerConfig {
                command: "rust-analyzer".to_owned(),
                args: Vec::new(),
                extension_to_language,
                startup_timeout_ms: default_lsp_startup_timeout_ms(),
                max_restarts: default_lsp_max_restarts(),
            },
        );
        Self { servers }
    }
}

impl FileConfig {
    fn lsp_config(&self) -> LspConfig {
        self.lsp
            .as_ref()
            .and_then(|lsp| lsp.servers.clone())
            .map(|servers| LspConfig { servers })
            .unwrap_or_default()
    }
}

fn default_lsp_startup_timeout_ms() -> u64 {
    20_000
}

fn default_lsp_max_restarts() -> u8 {
    3
}

impl FileLlmConfig {
    fn model_catalog(&self) -> ModelCatalog {
        let mut catalog = ModelCatalog::default();
        for (provider_name, provider) in &self.providers {
            catalog.providers.insert(
                provider_name.clone(),
                ProviderCatalogEntry {
                    description: provider.description.clone(),
                    unit: provider.unit.clone(),
                },
            );

            let mut models = BTreeMap::new();
            for model in &provider.models {
                if let Some(entry) = model.catalog_entry() {
                    models.insert(model.name().to_owned(), entry);
                }
            }
            if !models.is_empty() {
                catalog.models.insert(provider_name.clone(), models);
            }
        }
        catalog
    }

    fn into_runtime_config(
        self,
        resolve_api_key: impl Fn(&str) -> Option<String>,
    ) -> Result<LlmConfig> {
        let selected_provider = self.provider;
        let selected_model = self.model;
        let mut providers = Vec::new();
        for (name, provider) in self.providers {
            let model_context_windows = provider
                .models
                .iter()
                .filter_map(|model| {
                    model
                        .context_window()
                        .map(|window| (model.name().to_owned(), window))
                })
                .collect();
            let models = provider
                .models
                .into_iter()
                .map(FileModelConfig::into_name)
                .collect::<Vec<_>>();
            if models.is_empty() {
                bail!("llm provider '{name}' does not define any models");
            }
            providers.push(LlmProviderConfig {
                name,
                base_url: provider.base_url.trim_end_matches('/').to_owned(),
                models,
                model_context_windows,
                api_key_env: provider.api_key_env,
                prompt_cache: provider
                    .prompt_cache
                    .map(PromptCacheConfig::from)
                    .unwrap_or_default(),
            });
        }

        let mut config = LlmConfig {
            provider: String::new(),
            base_url: String::new(),
            model: String::new(),
            providers,
            temperature: self.temperature,
            max_tokens: self.max_tokens,
            context_window: self.context_window,
            api_key: String::new(),
            default_context_window: self.context_window,
            prompt_cache: PromptCacheConfig::default(),
        };
        config.switch_model(&selected_provider, &selected_model, resolve_api_key)?;
        Ok(config)
    }
}

impl FileModelConfig {
    fn name(&self) -> &str {
        match self {
            Self::Name(name) => name,
            Self::Details(details) => &details.name,
        }
    }

    fn into_name(self) -> String {
        match self {
            Self::Name(name) => name,
            Self::Details(details) => details.name,
        }
    }

    fn catalog_entry(&self) -> Option<ModelCatalogEntry> {
        match self {
            Self::Name(_) => None,
            Self::Details(details) => Some(details.catalog_entry()),
        }
        .filter(|entry| !entry.is_empty())
    }

    fn context_window(&self) -> Option<u64> {
        match self {
            Self::Name(_) => None,
            Self::Details(details) => details.context_window(),
        }
    }
}

impl FileModelDetails {
    fn catalog_entry(&self) -> ModelCatalogEntry {
        ModelCatalogEntry {
            positioning: self.positioning.clone(),
            context: self.context.as_ref().map(value_label).unwrap_or_default(),
            max_tokens: self
                .max_tokens
                .as_ref()
                .map(value_label)
                .unwrap_or_default(),
            price: self.price.as_ref().map(value_label).unwrap_or_default(),
            input: self.input.as_ref().map(value_label).unwrap_or_default(),
            output: self.output.as_ref().map(value_label).unwrap_or_default(),
            cache_read: self
                .cache_read
                .as_ref()
                .map(value_label)
                .unwrap_or_default(),
            cache_write: self
                .cache_write
                .as_ref()
                .map(value_label)
                .unwrap_or_default(),
        }
    }

    fn context_window(&self) -> Option<u64> {
        self.context.as_ref().and_then(u64_value)
    }
}

impl ModelCatalogEntry {
    fn is_empty(&self) -> bool {
        self.positioning.is_empty()
            && self.context.is_empty()
            && self.max_tokens.is_empty()
            && self.price.is_empty()
            && self.input.is_empty()
            && self.output.is_empty()
            && self.cache_read.is_empty()
            && self.cache_write.is_empty()
    }
}

fn value_label(value: &serde_yaml::Value) -> String {
    match value {
        serde_yaml::Value::String(value) => value.clone(),
        serde_yaml::Value::Number(value) => value.to_string(),
        serde_yaml::Value::Bool(value) => value.to_string(),
        _ => serde_yaml::to_string(value)
            .unwrap_or_default()
            .trim()
            .to_owned(),
    }
}

fn u64_value(value: &serde_yaml::Value) -> Option<u64> {
    match value {
        serde_yaml::Value::Number(value) => value.as_u64(),
        serde_yaml::Value::String(value) => value.parse().ok(),
        _ => None,
    }
}

impl LlmConfig {
    pub fn switch_model(
        &mut self,
        provider_name: &str,
        model: &str,
        resolve_api_key: impl Fn(&str) -> Option<String>,
    ) -> Result<()> {
        let provider = self
            .providers
            .iter()
            .find(|provider| provider.name == provider_name)
            .cloned()
            .with_context(|| format!("llm provider '{provider_name}' is not defined"))?;
        if provider.models.is_empty() {
            bail!("llm provider '{provider_name}' does not define any models");
        }
        if !provider.models.iter().any(|candidate| candidate == model) {
            bail!("model '{model}' is not defined for provider '{provider_name}'");
        }

        let api_key = resolve_api_key(&provider.api_key_env)
            .with_context(|| format!("{} must be set", provider.api_key_env))?;

        self.provider = provider.name;
        self.base_url = provider.base_url;
        self.model = model.to_owned();
        self.context_window = provider
            .model_context_windows
            .get(model)
            .copied()
            .or(self.default_context_window);
        self.api_key = api_key;
        self.prompt_cache = provider.prompt_cache;
        Ok(())
    }
}

impl From<FilePromptCacheConfig> for PromptCacheConfig {
    fn from(config: FilePromptCacheConfig) -> Self {
        Self {
            key: config.key.and_then(non_empty_string),
            retention: config.retention.and_then(non_empty_string),
        }
    }
}

fn non_empty_string(value: String) -> Option<String> {
    let value = value.trim().to_owned();
    (!value.is_empty()).then_some(value)
}

#[cfg(test)]
mod tests {
    use std::{fs, process::Command, sync::Arc};

    use super::*;
    use crate::{
        paths::GlintPaths,
        persistence::{AtomicFileWriter, UserConfigStore},
    };

    #[test]
    fn user_config_omits_empty_optional_sections() {
        let config = UserConfig {
            version: 1,
            llm: Some(UserLlmConfig {
                provider: "deepseek".into(),
                model: "deepseek-v4-flash".into(),
                temperature: 0.7,
                max_tokens: 8196,
            }),
            ..UserConfig::default()
        };

        let yaml = serde_yaml::to_string(&config).unwrap();
        assert!(!yaml.contains("configured_providers:"));
        assert!(!yaml.contains("custom_providers:"));
        assert!(!yaml.contains("mcp:"));
        assert!(!yaml.contains("plugins:"));
        assert!(!yaml.contains("lsp:"));
    }

    #[test]
    fn malformed_user_config_is_reported_without_replacement() {
        let root = temp_dir("malformed-user-config");
        let paths = GlintPaths::from_home(&root);
        fs::create_dir_all(paths.root()).unwrap();
        fs::write(paths.config(), "llm: [broken\n").unwrap();
        let store = UserConfigStore::new(paths.clone());

        let error = store.load().unwrap_err();

        assert!(format!("{error:#}").contains(&paths.config().display().to_string()));
        assert_eq!(
            fs::read_to_string(paths.config()).unwrap(),
            "llm: [broken\n"
        );
    }

    #[test]
    fn user_config_round_trips_extension_sections_when_model_changes() {
        let root = temp_dir("user-config-extensions");
        let paths = GlintPaths::from_home(&root);
        fs::create_dir_all(paths.root()).unwrap();
        fs::write(
            paths.config(),
            r#"
version: 1
llm:
  provider: deepseek
  model: deepseek-v4-flash
  temperature: 0.7
  max_tokens: 8196
mcp:
  servers:
    docs:
      command: docs-mcp
plugins:
  enabled:
    - example
lsp:
  servers:
    rust:
      command: rust-analyzer
"#,
        )
        .unwrap();
        let store = UserConfigStore::new(paths);
        let mut config = store.load().unwrap().unwrap();
        let mcp = config.mcp.clone();
        let plugins = config.plugins.clone();
        let lsp = config.lsp.clone();

        config.llm.as_mut().unwrap().model = "deepseek-v4-pro".into();
        store.save(&config).unwrap();
        let saved = store.load().unwrap().unwrap();

        assert_eq!(saved.llm.unwrap().model, "deepseek-v4-pro");
        assert_eq!(saved.mcp, mcp);
        assert_eq!(saved.plugins, plugins);
        assert_eq!(saved.lsp, lsp);
    }

    #[test]
    fn user_config_keeps_existing_file_when_atomic_writer_fails() {
        let root = temp_dir("atomic-user-config");
        let paths = GlintPaths::from_home(&root);
        fs::create_dir_all(paths.root()).unwrap();
        let original = b"version: 1\n";
        fs::write(paths.config(), original).unwrap();
        let store = UserConfigStore::with_writer(paths.clone(), Arc::new(FailingWriter));

        let error = store.save(&UserConfig::default()).unwrap_err();

        assert!(format!("{error:#}").contains("simulated atomic write failure"));
        assert_eq!(fs::read(paths.config()).unwrap(), original);
    }

    #[cfg(unix)]
    #[test]
    fn user_config_first_save_uses_private_directory_and_file_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let root = temp_dir("private-user-config");
        let paths = GlintPaths::from_home(&root);
        let store = UserConfigStore::new(paths.clone());

        store.save(&UserConfig::default()).unwrap();

        assert_eq!(
            fs::metadata(paths.root()).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(paths.config()).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[cfg(unix)]
    #[test]
    fn user_config_first_save_sets_mode_despite_restrictive_umask() {
        const CHILD_ENV: &str = "GLINT_TEST_RESTRICTIVE_UMASK";
        const TEST_NAME: &str =
            "config::tests::user_config_first_save_sets_mode_despite_restrictive_umask";

        if std::env::var_os(CHILD_ENV).is_some() {
            use std::os::unix::fs::PermissionsExt;

            let root = temp_dir("restrictive-umask-user-config");
            fs::create_dir_all(&root).unwrap();
            unsafe {
                libc::umask(0o700);
            }
            let paths = GlintPaths::from_home(&root);
            let store = UserConfigStore::new(paths.clone());

            store.save(&UserConfig::default()).unwrap();

            assert_eq!(
                fs::metadata(paths.config()).unwrap().permissions().mode() & 0o777,
                0o600
            );
            return;
        }

        let status = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", TEST_NAME, "--nocapture"])
            .env(CHILD_ENV, "1")
            .status()
            .unwrap();
        assert!(status.success());
    }

    #[cfg(unix)]
    #[test]
    fn user_config_save_preserves_existing_file_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let root = temp_dir("existing-user-config-permissions");
        let paths = GlintPaths::from_home(&root);
        fs::create_dir_all(paths.root()).unwrap();
        fs::write(paths.config(), "version: 1\n").unwrap();
        fs::set_permissions(paths.config(), fs::Permissions::from_mode(0o640)).unwrap();
        let store = UserConfigStore::new(paths.clone());

        store.save(&UserConfig::default()).unwrap();

        assert_eq!(
            fs::metadata(paths.config()).unwrap().permissions().mode() & 0o777,
            0o640
        );
    }

    struct FailingWriter;

    impl AtomicFileWriter for FailingWriter {
        fn write(&self, _path: &Path, _bytes: &[u8], _unix_mode: u32) -> Result<()> {
            bail!("simulated atomic write failure")
        }
    }

    #[test]
    fn resolve_config_path_honors_documented_precedence() {
        let root = temp_dir("config-precedence");
        let cwd = root.join("workspace");
        let xdg = root.join("xdg");
        let home = root.join("home");
        let explicit = root.join("explicit.yaml");
        let from_env = root.join("environment.yaml");
        let project = cwd.join(".glint/config.yaml");
        let user = xdg.join("glint/config.yaml");
        let legacy = cwd.join("config.yaml");

        for path in [&explicit, &from_env, &project, &user, &legacy] {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "config").unwrap();
        }

        let mut input = ConfigPathInput {
            explicit: Some(explicit.clone()),
            glint_config: Some(from_env.clone()),
            xdg_config_home: Some(xdg),
            home: Some(home),
            cwd,
        };

        assert_eq!(resolve_config_path(&input).unwrap(), explicit);
        input.explicit = None;
        assert_eq!(resolve_config_path(&input).unwrap(), from_env);
        input.glint_config = None;
        assert_eq!(resolve_config_path(&input).unwrap(), project);
        std::fs::remove_file(&project).unwrap();
        assert_eq!(resolve_config_path(&input).unwrap(), user);
        std::fs::remove_file(&user).unwrap();
        assert_eq!(resolve_config_path(&input).unwrap(), legacy);
    }

    #[test]
    fn resolve_config_path_falls_back_to_home_when_xdg_is_unset() {
        let root = temp_dir("config-home");
        let home = root.join("home");
        let expected = home.join(".config/glint/config.yaml");
        std::fs::create_dir_all(expected.parent().unwrap()).unwrap();
        std::fs::write(&expected, "config").unwrap();

        let input = ConfigPathInput {
            explicit: None,
            glint_config: None,
            xdg_config_home: None,
            home: Some(home),
            cwd: root.join("workspace"),
        };

        assert_eq!(resolve_config_path(&input).unwrap(), expected);
    }

    #[test]
    fn resolve_config_path_reports_attempts_and_init_hint() {
        let root = temp_dir("config-missing");
        let cwd = root.join("workspace");
        let input = ConfigPathInput {
            explicit: None,
            glint_config: None,
            xdg_config_home: None,
            home: Some(root.join("home")),
            cwd: cwd.clone(),
        };

        let error = resolve_config_path(&input).unwrap_err();
        let message = format!("{error:#}");

        assert!(message.contains("could not find a Glint configuration"));
        assert!(message.contains(&cwd.join(".glint/config.yaml").display().to_string()));
        assert!(message.contains("glint init"));
    }

    #[test]
    fn load_from_path_uses_explicit_file_and_embedded_system_prompt() {
        let root = temp_dir("explicit-load");
        let workspace = root.join("workspace");
        let config_path = root.join("selected.yaml");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::write(
            &config_path,
            r#"
            llm:
              provider: test
              model: test-model
              temperature: 0.7
              max_tokens: 8196
              providers:
                test:
                  base_url: https://example.com/v1
                  models:
                    - test-model
                  api_key_env: TEST_API_KEY
            "#,
        )
        .unwrap();

        let config = Config::load_from_path(&config_path, &workspace, fake_api_key).unwrap();

        assert_eq!(config.config_path, config_path);
        assert_eq!(config.llm.provider, "test");
        assert_eq!(config.system_prompt, include_str!("../prompts/system.md"));
    }

    #[test]
    fn resolve_init_config_path_honors_documented_precedence() {
        let root = temp_dir("init-precedence");
        let explicit = root.join("explicit.yaml");
        let from_env = root.join("environment.yaml");
        let xdg = root.join("xdg");
        let home = root.join("home");
        let mut input = InitConfigPathInput {
            explicit: Some(explicit.clone()),
            glint_config: Some(from_env.clone()),
            xdg_config_home: Some(xdg.clone()),
            home: Some(home.clone()),
        };

        assert_eq!(resolve_init_config_path(&input).unwrap(), explicit);
        input.explicit = None;
        assert_eq!(resolve_init_config_path(&input).unwrap(), from_env);
        input.glint_config = None;
        assert_eq!(
            resolve_init_config_path(&input).unwrap(),
            xdg.join("glint/config.yaml")
        );
        input.xdg_config_home = None;
        assert_eq!(
            resolve_init_config_path(&input).unwrap(),
            home.join(".config/glint/config.yaml")
        );
    }

    #[test]
    fn resolve_init_config_path_requires_an_explicit_or_user_location() {
        let input = InitConfigPathInput {
            explicit: None,
            glint_config: None,
            xdg_config_home: None,
            home: None,
        };

        let error = resolve_init_config_path(&input).unwrap_err();

        assert!(format!("{error:#}").contains("pass `--config PATH`"));
    }

    #[test]
    fn resolves_selected_provider_with_global_defaults() {
        let config: FileConfig = serde_yaml::from_str(
            r#"
            llm:
              provider: deepseek
              model: deepseek-chat
              temperature: 0.7
              max_tokens: 8196
              providers:
                deepseek:
                  base_url: https://api.deepseek.com/
                  models:
                    - deepseek-chat
                    - deepseek-reasoner
                  api_key_env: TEST_API_KEY
            "#,
        )
        .unwrap();

        let llm = config.llm.into_runtime_config(fake_api_key).unwrap();

        assert_eq!(llm.base_url, "https://api.deepseek.com");
        assert_eq!(llm.provider, "deepseek");
        assert_eq!(llm.model, "deepseek-chat");
        assert_eq!(
            llm.providers[0].models,
            ["deepseek-chat", "deepseek-reasoner"]
        );
        assert_eq!(llm.temperature, 0.7);
        assert_eq!(llm.max_tokens, 8196);
        assert_eq!(llm.api_key, "secret");
    }

    #[test]
    fn builds_model_catalog_from_provider_metadata() {
        let config: FileConfig = serde_yaml::from_str(
            r#"
            llm:
              provider: deepseek
              model: deepseek-v4-flash
              temperature: 0.7
              max_tokens: 8196
              providers:
                deepseek:
                  description: DeepSeek official endpoint
                  base_url: https://api.deepseek.com/
                  unit: RMB
                  models:
                    - name: deepseek-v4-flash
                      positioning: Fast chat
                      input: 1.00
                      output: 2.00
                      cache_read: 0.02
                      context: 1000000
                      max_tokens: 384000
                    - deepseek-v4-pro
                  api_key_env: TEST_API_KEY
            "#,
        )
        .unwrap();

        let catalog = config.llm.model_catalog();
        assert_eq!(
            catalog.providers["deepseek"].description,
            "DeepSeek official endpoint"
        );
        assert_eq!(catalog.providers["deepseek"].unit, "RMB");

        let entry = &catalog.models["deepseek"]["deepseek-v4-flash"];
        assert_eq!(entry.positioning, "Fast chat");
        assert_eq!(entry.input, "1.0");
        assert_eq!(entry.output, "2.0");
        assert_eq!(entry.cache_read, "0.02");
        assert_eq!(entry.context, "1000000");
        assert_eq!(entry.max_tokens, "384000");
        assert!(!catalog.models["deepseek"].contains_key("deepseek-v4-pro"));

        let llm = config
            .llm
            .into_runtime_config(|_| Some("secret".to_owned()))
            .unwrap();
        assert_eq!(llm.context_window, Some(1_000_000));
    }

    #[test]
    fn applies_provider_prompt_cache_config() {
        let config: FileConfig = serde_yaml::from_str(
            r#"
            llm:
              provider: openai
              model: gpt-5-codex
              temperature: 0.7
              max_tokens: 8196
              providers:
                openai:
                  base_url: https://api.openai.com/v1
                  models:
                    - gpt-5-codex
                  api_key_env: TEST_API_KEY
                  prompt_cache:
                    key: glint-coding
                    retention: 24h
                other:
                  base_url: https://example.com
                  models:
                    - other-model
                  api_key_env: TEST_API_KEY
            "#,
        )
        .unwrap();

        let mut llm = config.llm.into_runtime_config(fake_api_key).unwrap();

        assert_eq!(llm.prompt_cache.key.as_deref(), Some("glint-coding"));
        assert_eq!(llm.prompt_cache.retention.as_deref(), Some("24h"));

        llm.switch_model("other", "other-model", fake_api_key)
            .unwrap();
        assert_eq!(llm.prompt_cache, PromptCacheConfig::default());
    }

    #[test]
    fn committed_yaml_configs_are_valid() {
        for yaml in [
            include_str!("../config.yaml"),
            include_str!("../config.example.yaml"),
        ] {
            let config: FileConfig = serde_yaml::from_str(yaml).unwrap();
            let provider = config
                .llm
                .providers
                .get(&config.llm.provider)
                .expect("selected provider should exist");

            assert!(
                provider
                    .models
                    .iter()
                    .any(|model| model.name() == config.llm.model)
            );

            config
                .llm
                .into_runtime_config(|_| Some("secret".to_owned()))
                .unwrap();
        }
    }

    #[test]
    fn lsp_config_defaults_to_rust_server_when_omitted() {
        let config: FileConfig = serde_yaml::from_str(
            r#"
            llm:
              provider: deepseek
              model: deepseek-chat
              temperature: 0.7
              max_tokens: 8196
              providers:
                deepseek:
                  base_url: https://api.deepseek.com
                  models:
                    - deepseek-chat
                  api_key_env: TEST_API_KEY
            "#,
        )
        .unwrap();

        let lsp = config.lsp_config();
        let rust = &lsp.servers["rust"];

        assert_eq!(rust.command, "rust-analyzer");
        assert_eq!(rust.args, Vec::<String>::new());
        assert_eq!(rust.extension_to_language[".rs"], "rust");
        assert_eq!(rust.startup_timeout_ms, 20_000);
        assert_eq!(rust.max_restarts, 3);
    }

    #[test]
    fn configured_lsp_servers_replace_default() {
        let config: FileConfig = serde_yaml::from_str(
            r#"
            llm:
              provider: deepseek
              model: deepseek-chat
              temperature: 0.7
              max_tokens: 8196
              providers:
                deepseek:
                  base_url: https://api.deepseek.com
                  models:
                    - deepseek-chat
                  api_key_env: TEST_API_KEY
            lsp:
              servers:
                python:
                  command: pyright-langserver
                  args: ["--stdio"]
                  extension_to_language:
                    .py: python
                  startup_timeout_ms: 10000
                  max_restarts: 1
            "#,
        )
        .unwrap();

        let lsp = config.lsp_config();

        assert!(!lsp.servers.contains_key("rust"));
        assert_eq!(lsp.servers["python"].command, "pyright-langserver");
        assert_eq!(lsp.servers["python"].args, ["--stdio"]);
        assert_eq!(lsp.servers["python"].extension_to_language[".py"], "python");
        assert_eq!(lsp.servers["python"].startup_timeout_ms, 10_000);
        assert_eq!(lsp.servers["python"].max_restarts, 1);
    }

    #[test]
    fn reports_missing_selected_provider() {
        let config: FileConfig = serde_yaml::from_str(
            r#"
            llm:
              provider: missing
              model: deepseek-chat
              temperature: 0.7
              max_tokens: 8196
              providers:
                deepseek:
                  base_url: https://api.deepseek.com
                  models:
                    - deepseek-chat
                  api_key_env: TEST_API_KEY
            "#,
        )
        .unwrap();

        let error = match config.llm.into_runtime_config(fake_api_key) {
            Ok(_) => panic!("missing provider should fail"),
            Err(error) => error,
        };

        assert!(format!("{error:#}").contains("llm provider 'missing' is not defined"));
    }

    #[test]
    fn reports_missing_selected_model() {
        let config: FileConfig = serde_yaml::from_str(
            r#"
            llm:
              provider: deepseek
              model: missing
              temperature: 0.7
              max_tokens: 8196
              providers:
                deepseek:
                  base_url: https://api.deepseek.com
                  models:
                    - deepseek-chat
                  api_key_env: TEST_API_KEY
            "#,
        )
        .unwrap();

        let error = match config.llm.into_runtime_config(fake_api_key) {
            Ok(_) => panic!("missing model should fail"),
            Err(error) => error,
        };

        assert!(format!("{error:#}").contains("model 'missing' is not defined"));
    }

    #[test]
    fn failed_switch_preserves_current_model() {
        let config: FileConfig = serde_yaml::from_str(
            r#"
            llm:
              provider: deepseek
              model: deepseek-chat
              temperature: 0.7
              max_tokens: 8196
              providers:
                deepseek:
                  base_url: https://api.deepseek.com
                  models:
                    - deepseek-chat
                  api_key_env: TEST_API_KEY
                other:
                  base_url: https://example.com
                  models:
                    - other-model
                  api_key_env: MISSING_API_KEY
            "#,
        )
        .unwrap();

        let mut llm = config.llm.into_runtime_config(fake_api_key).unwrap();
        let error = llm
            .switch_model("other", "other-model", fake_api_key)
            .unwrap_err();

        assert!(format!("{error:#}").contains("MISSING_API_KEY must be set"));
        assert_eq!(llm.provider, "deepseek");
        assert_eq!(llm.model, "deepseek-chat");
        assert_eq!(llm.api_key, "secret");
    }

    fn fake_api_key(api_key_env: &str) -> Option<String> {
        (api_key_env == "TEST_API_KEY").then(|| "secret".to_owned())
    }

    fn temp_dir(label: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("glint-{label}-{}", uuid::Uuid::new_v4()))
    }
}
