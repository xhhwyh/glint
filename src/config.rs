use std::{collections::BTreeMap, fmt, path::PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::{
    credentials::CredentialId,
    plugins::{ExtensionCatalog, PluginLoadResult, PluginsConfig},
    services::mcp::McpConfig,
};

pub use crate::provider_catalog::PromptCacheConfig;

pub const CHATGPT_PROVIDER_ID: &str = "chatgpt";
pub const CHATGPT_PROVIDER_NAME: &str = "OpenAI";

pub(crate) const DEFAULT_SYSTEM_PROMPT: &str = include_str!("../prompts/system.md");

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

impl Config {
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

#[derive(Clone)]
pub struct LlmConfig {
    pub reasoning_effort: Option<String>,
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
    pub credential_id: CredentialId,
    pub prompt_cache: PromptCacheConfig,
}

impl LlmConfig {
    pub fn switch_model(
        &mut self,
        provider_name: &str,
        model: &str,
        api_key: Option<String>,
    ) -> Result<()> {
        let provider = self
            .providers
            .iter()
            .find(|provider| provider.name == provider_name)
            .cloned()
            .with_context(|| format!("llm provider '{provider_name}' is not defined"))?;
        if !provider.models.iter().any(|candidate| candidate == model) {
            bail!("model '{model}' is not defined for provider '{provider_name}'");
        }
        let api_key = if provider_name == CHATGPT_PROVIDER_ID {
            String::new()
        } else {
            api_key.with_context(|| {
                format!(
                    "credential '{}' is unavailable",
                    provider.credential_id.as_str()
                )
            })?
        };

        self.reasoning_effort = None;
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

#[derive(Clone, Deserialize, Serialize, PartialEq)]
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
    pub chatgpt: Option<ChatGptConfig>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub reasoning_efforts: BTreeMap<String, BTreeMap<String, String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mcp: Option<serde_yaml::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plugins: Option<serde_yaml::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lsp: Option<serde_yaml::Value>,
    #[serde(default, flatten)]
    extra: BTreeMap<String, serde_yaml::Value>,
}

impl fmt::Debug for UserConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let extra_sections = self.extra.keys().map(String::as_str).collect::<Vec<_>>();
        formatter
            .debug_struct("UserConfig")
            .field("version", &self.version)
            .field("llm", &self.llm)
            .field("configured_providers", &self.configured_providers)
            .field("custom_providers", &self.custom_providers)
            .field("chatgpt", &self.chatgpt)
            .field("mcp_configured", &self.mcp.is_some())
            .field("plugins_configured", &self.plugins.is_some())
            .field("lsp_configured", &self.lsp.is_some())
            .field("extra_sections", &extra_sections)
            .finish()
    }
}

impl Default for UserConfig {
    fn default() -> Self {
        Self {
            version: schema_version(),
            llm: None,
            configured_providers: Vec::new(),
            custom_providers: BTreeMap::new(),
            chatgpt: None,
            reasoning_efforts: BTreeMap::new(),
            mcp: None,
            plugins: None,
            lsp: None,
            extra: BTreeMap::new(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct UserLlmConfig {
    pub provider: String,
    pub model: String,
    pub temperature: f32,
    pub max_tokens: u32,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ChatGptConfig {
    pub models: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub reasoning_efforts: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct CustomProviderConfig {
    pub base_url: String,
    pub models: Vec<String>,
}

fn schema_version() -> u16 {
    1
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LspConfig {
    pub servers: BTreeMap<String, LspServerConfig>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
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

fn default_lsp_startup_timeout_ms() -> u64 {
    20_000
}

fn default_lsp_max_restarts() -> u8 {
    3
}

#[derive(Deserialize)]
struct FileLspConfig {
    #[serde(default)]
    servers: Option<BTreeMap<String, LspServerConfig>>,
}

pub struct RuntimeExtensions {
    pub mcp: McpConfig,
    pub plugins: PluginsConfig,
    pub lsp: LspConfig,
}

impl RuntimeExtensions {
    pub fn from_user_config(config: &UserConfig) -> Result<Self> {
        let mcp: McpConfig =
            parse_optional_section(config.mcp.as_ref(), "mcp")?.unwrap_or_default();
        mcp.validate()
            .context("failed to parse mcp in ~/.glint/config.yaml")?;
        let plugins: PluginsConfig =
            parse_optional_section(config.plugins.as_ref(), "plugins")?.unwrap_or_default();
        let lsp = parse_optional_section::<FileLspConfig>(config.lsp.as_ref(), "lsp")?
            .and_then(|config| config.servers)
            .map(|servers| LspConfig { servers })
            .unwrap_or_default();
        Ok(Self { mcp, plugins, lsp })
    }
}

fn parse_optional_section<T: for<'de> Deserialize<'de>>(
    value: Option<&serde_yaml::Value>,
    section: &str,
) -> Result<Option<T>> {
    value
        .cloned()
        .map(serde_yaml::from_value)
        .transpose()
        .with_context(|| format!("failed to parse {section} in ~/.glint/config.yaml"))
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

#[cfg(test)]
mod tests {
    use std::{fs, path::Path, process::Command, sync::Arc};

    use anyhow::{Result, bail};

    use super::*;
    use crate::{
        paths::GlintPaths,
        persistence::{AtomicFileWriter, UserConfigStore},
    };

    #[test]
    fn user_config_omits_empty_optional_sections() {
        let config = UserConfig {
            llm: Some(UserLlmConfig {
                provider: "deepseek".into(),
                model: "deepseek-v4-flash".into(),
                temperature: 0.7,
                max_tokens: 8196,
            }),
            ..UserConfig::default()
        };
        let yaml = serde_yaml::to_string(&config).unwrap();
        for section in [
            "configured_providers:",
            "custom_providers:",
            "chatgpt:",
            "mcp:",
            "plugins:",
            "lsp:",
        ] {
            assert!(!yaml.contains(section));
        }
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
            "version: 1\nllm:\n  provider: deepseek\n  model: one\n  temperature: 0.7\n  max_tokens: 8196\nmcp:\n  servers: {}\nplugins:\n  entries: []\nlsp:\n  servers: {}\n",
        )
        .unwrap();
        let store = UserConfigStore::new(paths);
        let mut config = store.load().unwrap().unwrap();
        let extensions = (
            config.mcp.clone(),
            config.plugins.clone(),
            config.lsp.clone(),
        );
        config.llm.as_mut().unwrap().model = "two".into();
        store.save(&config).unwrap();
        let saved = store.load().unwrap().unwrap();
        assert_eq!(saved.llm.unwrap().model, "two");
        assert_eq!((saved.mcp, saved.plugins, saved.lsp), extensions);
    }

    #[test]
    fn user_config_rejects_duplicate_known_top_level_keys() {
        let error = serde_yaml::from_str::<UserConfig>("version: 1\nversion: 2\n").unwrap_err();

        assert!(error.to_string().contains("duplicate field `version`"));
    }

    #[test]
    fn user_config_debug_redacts_raw_extension_and_unknown_values() {
        let config: UserConfig = serde_yaml::from_str(
            "version: 1\nllm:\n  provider: deepseek\n  model: safe-model\n  temperature: 0.7\n  max_tokens: 8196\nmcp:\n  token: mcp-sentinel-secret\nplugins:\n  token: plugin-sentinel-secret\nlsp:\n  token: lsp-sentinel-secret\nfuture_auth:\n  token: extra-sentinel-secret\n",
        )
        .unwrap();

        let debug = format!("{config:?}");

        assert!(debug.contains("version: 1"));
        assert!(debug.contains("deepseek"));
        assert!(debug.contains("mcp"));
        assert!(debug.contains("plugins"));
        assert!(debug.contains("lsp"));
        assert!(debug.contains("future_auth"));
        for secret in [
            "mcp-sentinel-secret",
            "plugin-sentinel-secret",
            "lsp-sentinel-secret",
            "extra-sentinel-secret",
        ] {
            assert!(!debug.contains(secret), "Debug leaked {secret}: {debug}");
        }
    }

    #[test]
    fn user_config_keeps_existing_file_when_atomic_writer_fails() {
        let root = temp_dir("atomic-user-config");
        let paths = GlintPaths::from_home(&root);
        fs::create_dir_all(paths.root()).unwrap();
        fs::write(paths.config(), b"version: 1\n").unwrap();
        let store = UserConfigStore::with_writer(paths.clone(), Arc::new(FailingWriter));
        assert!(store.save(&UserConfig::default()).is_err());
        assert_eq!(fs::read(paths.config()).unwrap(), b"version: 1\n");
    }

    #[test]
    fn runtime_extensions_default_and_explicit_lsp_are_distinct() {
        let defaults = RuntimeExtensions::from_user_config(&UserConfig::default()).unwrap();
        assert!(defaults.lsp.servers.contains_key("rust"));

        let explicit = UserConfig {
            lsp: Some(serde_yaml::from_str("servers: {}").unwrap()),
            ..UserConfig::default()
        };
        let explicit = RuntimeExtensions::from_user_config(&explicit).unwrap();
        assert!(explicit.lsp.servers.is_empty());
    }

    #[test]
    fn runtime_extensions_parse_mcp_and_report_section_errors() {
        let configured = UserConfig {
            mcp: Some(
                serde_yaml::from_str(
                    "servers:\n  docs:\n    transport: stdio\n    command: docs-mcp\n",
                )
                .unwrap(),
            ),
            ..UserConfig::default()
        };
        let extensions = RuntimeExtensions::from_user_config(&configured).unwrap();
        assert!(matches!(
            &extensions.mcp.servers["docs"].transport,
            crate::services::mcp::McpTransportConfig::Stdio { command, .. }
                if command == "docs-mcp"
        ));

        let malformed = UserConfig {
            mcp: Some(serde_yaml::Value::String("not a mapping".into())),
            ..UserConfig::default()
        };
        let error = RuntimeExtensions::from_user_config(&malformed)
            .err()
            .expect("invalid MCP section should fail");
        assert!(format!("{error:#}").contains("failed to parse mcp in ~/.glint/config.yaml"));

        let invalid = UserConfig {
            mcp: Some(
                serde_yaml::from_str("servers:\n  docs:\n    transport: stdio\n    command: ''")
                    .unwrap(),
            ),
            ..UserConfig::default()
        };
        let error = RuntimeExtensions::from_user_config(&invalid)
            .err()
            .expect("invalid MCP values should fail");
        assert!(format!("{error:#}").contains("failed to parse mcp in ~/.glint/config.yaml"));
    }

    #[test]
    fn runtime_extension_errors_name_each_fixed_config_section() {
        for (section, config) in [
            (
                "plugins",
                UserConfig {
                    plugins: Some(serde_yaml::Value::String("invalid".into())),
                    ..UserConfig::default()
                },
            ),
            (
                "lsp",
                UserConfig {
                    lsp: Some(serde_yaml::Value::String("invalid".into())),
                    ..UserConfig::default()
                },
            ),
        ] {
            let error = RuntimeExtensions::from_user_config(&config)
                .err()
                .expect("invalid extension section should fail");
            assert!(format!("{error:#}").contains(&format!(
                "failed to parse {section} in ~/.glint/config.yaml"
            )));
        }
    }

    #[cfg(unix)]
    #[test]
    fn user_config_first_save_uses_private_directory_and_file_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let paths = GlintPaths::from_home(temp_dir("private-user-config"));
        UserConfigStore::new(paths.clone())
            .save(&UserConfig::default())
            .unwrap();
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
            UserConfigStore::new(paths.clone())
                .save(&UserConfig::default())
                .unwrap();
            assert_eq!(
                fs::metadata(paths.config()).unwrap().permissions().mode() & 0o777,
                0o600
            );
            return;
        }
        assert!(
            Command::new(std::env::current_exe().unwrap())
                .args(["--exact", TEST_NAME, "--nocapture"])
                .env(CHILD_ENV, "1")
                .status()
                .unwrap()
                .success()
        );
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
        fn write(&self, _: &Path, _: &[u8], _: u32) -> Result<()> {
            bail!("simulated atomic write failure")
        }
    }

    fn temp_dir(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!("glint-{label}-{}", uuid::Uuid::new_v4()))
    }
}
