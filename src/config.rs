use std::collections::BTreeMap;

use anyhow::{Context, Result, bail};
use serde::Deserialize;

#[derive(Clone)]
pub struct Config {
    pub llm: LlmConfig,
    pub model_catalog: ModelCatalog,
    pub system_prompt: String,
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
}

#[derive(Clone)]
pub struct LlmProviderConfig {
    pub name: String,
    pub base_url: String,
    pub models: Vec<String>,
    pub model_context_windows: BTreeMap<String, u64>,
    pub api_key_env: String,
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
    models: Vec<FileModelConfig>,
    api_key_env: String,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum FileModelConfig {
    Name(String),
    Details(Box<FileModelDetails>),
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
    pub fn load() -> Result<Self> {
        let file = std::fs::read_to_string("config.yaml").context("failed to read config.yaml")?;
        let config: FileConfig =
            serde_yaml::from_str(&file).context("failed to parse config.yaml")?;
        let system_prompt = std::fs::read_to_string("prompts/system.md")
            .context("failed to read prompts/system.md")?;

        let model_catalog = config.llm.model_catalog();
        Ok(Self {
            llm: config
                .llm
                .into_runtime_config(|api_key_env| std::env::var(api_key_env).ok())?,
            model_catalog,
            system_prompt,
        })
    }
}

impl FileLlmConfig {
    fn model_catalog(&self) -> ModelCatalog {
        let mut catalog = ModelCatalog::default();
        for (provider_name, provider) in &self.providers {
            catalog.providers.insert(
                provider_name.clone(),
                ProviderCatalogEntry {
                    description: provider.description.clone(),
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
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn committed_yaml_config_is_valid() {
        let config: FileConfig = serde_yaml::from_str(include_str!("../config.yaml")).unwrap();
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

        let llm = config
            .llm
            .into_runtime_config(|_| Some("secret".to_owned()))
            .unwrap();
        assert_eq!(llm.context_window, Some(1_000_000));
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
}
